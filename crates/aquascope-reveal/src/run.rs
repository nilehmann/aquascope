//! The Run button's backend: compile and execute a snippet with the local
//! toolchain.
//!
//! Aquascope's editor posts snippets to the Rust playground's
//! `/evaluate.json`. A deck is given in a lecture room whose wifi cannot be
//! trusted, so `--serve` answers that endpoint itself. Only the part of the
//! contract the editor uses is implemented: `{version, optimize, code,
//! edition}` in, `{result}` out.
//!
//! Under `--serve` the deck's own ```origins Run buttons use a second
//! endpoint instead, [`stream`], which sends the same output a piece at a
//! time as the program writes it -- so a demo of two threads interleaving is
//! seen interleaving, rather than arriving all at once when they are done.
//! The editor's Run button keeps the playground's contract above.
//!
//! Two consequences of how the editor renders the answer shape everything
//! here. It shows `result` and nothing else, so *every* outcome -- a compile
//! error, a timeout, a missing `rustc` -- is a 200 whose result is the text to
//! display. And it assigns that text to `innerHTML`, which is what lets
//! rustc's colours reach the slide: diagnostics are compiled with
//! `--color=always` and the escape codes are converted to spans here.
//!
//! Everything in `result` is therefore HTML, and every piece of it is escaped
//! at the point it is built -- [`plain`] for text, [`ansi`] for anything
//! carrying escape codes. Only rustc's own output goes through [`ansi`]; a
//! snippet's stdout is [`plain`], so a program printing escape codes cannot
//! style the slide.

use std::{
  env, fs,
  io::Read,
  path::{Path, PathBuf},
  process::{Child, Command, Stdio},
  sync::{
    atomic::{AtomicU64, Ordering},
    mpsc,
  },
  thread,
  time::{Duration, Instant},
};

use serde::Deserialize;

/// A snippet that loops forever should not survive the slide. Long enough that
/// nothing a lecture demonstrates hits it by accident.
const TIMEOUT: Duration = Duration::from_secs(10);

/// How often the child is checked against the deadline.
const POLL: Duration = Duration::from_millis(20);

/// Names inside the scratch directory. `main.rs` is what the playground calls
/// the snippet, and what its error messages point at.
const SOURCE: &str = "main.rs";
const BINARY: &str = "snippet";

/// Prefix of the CSS variables the colours are emitted as. Kept in step with
/// the `--aq-ansi-*` defaults in `assets/aquascope-reveal.css`.
const VAR_PREFIX: &str = "aq-ansi-";

/// The editor sends the playground's parameters. `version` is ignored: there
/// is one toolchain here, whichever one `rustc` resolves to.
#[derive(Deserialize)]
struct Request {
  code: String,
  edition: Option<String>,
  optimize: Option<String>,
}

/// Answers one POST to `/evaluate.json`, returning the response body.
pub fn evaluate(body: &[u8]) -> Vec<u8> {
  let mut result = String::new();
  run(body, &mut |html| {
    result.push_str(html);
    true
  });

  // `result` is the only field the editor reads. serde_json does the quoting,
  // which is the part worth not hand-rolling.
  serde_json::json!({ "result": result })
    .to_string()
    .into_bytes()
}

/// Answers one POST to [`crate::serve::STREAM_ENDPOINT`]: the same run as
/// [`evaluate`], its output passed to `sink` as it is produced -- one line of
/// JSON per piece, `{"html": …}`, so that the browser can tell where one
/// piece ends however the network splits them.
pub fn stream(body: &[u8], sink: &mut dyn FnMut(&[u8]) -> bool) {
  run(body, &mut |html| {
    let mut line = serde_json::json!({ "html": html }).to_string().into_bytes();
    line.push(b'\n');
    sink(&line)
  });
}

fn run(body: &[u8], sink: Sink) {
  match serde_json::from_slice::<Request>(body) {
    Ok(request) => compile_and_run(&request, sink),
    Err(e) => {
      sink(&plain(&format!("Malformed request: {e}")));
    }
  }
}

/// Whether `code` has a `main` at the top level, which decides whether it is
/// compiled as a program or as a set of items.
fn defines_main(code: &str) -> bool {
  code.lines().any(|line| {
    let line = line.trim_start();
    line.starts_with("fn main") || line.starts_with("pub fn main")
  })
}

/// Compiles `code` without running or linking it, for the build-time checks on
/// ```origins blocks and on ```aquascope blocks Aquascope could not render.
/// `Ok(())` means it compiled; the error is rustc's own output with its colour
/// escapes left in, for [`ansi`] to turn into the build-error overlay and for
/// a terminal to show as it is.
///
/// `--emit=metadata` stops before codegen, which is what keeps this cheap
/// enough to run over every block on every build without a cache.
pub fn check(code: &str) -> Result<(), String> {
  let dir = scratch_dir().map_err(|e| format!("no build directory: {e}"))?;
  let source = dir.join(SOURCE);
  let result = (|| {
    fs::write(&source, code)
      .map_err(|e| format!("could not write {}: {e}", source.display()))?;

    let output = Command::new("rustc")
      .current_dir(&dir)
      .arg("--edition")
      .arg(edition(None))
      .arg("--emit=metadata")
      .arg("--crate-type")
      // A block is either a whole program or a set of items -- a struct and
      // an impl, say. `bin` reports E0601 for a missing `main` even under
      // `--emit=metadata`, so an item-only block has to be a `lib` or every
      // one of them would read as broken.
      .arg(if defines_main(code) { "bin" } else { "lib" })
      .arg("--color=always")
      .arg("-A")
      // A slide shows the code that makes its point and nothing else, so
      // unused names are the rule rather than a mistake.
      .arg("unused")
      .arg(SOURCE)
      .output()
      .map_err(|e| {
        format!(
          "could not run rustc: {e}\n\
           Code blocks are compiled at build time, so rustc has to be on PATH."
        )
      })?;

    if output.status.success() {
      Ok(())
    } else {
      Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
  })();

  let _ = fs::remove_dir_all(&dir);
  result
}

/// Where output goes as it is produced: a piece of HTML at a time, escaped
/// already. Returns `false` once nobody is listening -- the browser closed the
/// stream -- which stops the program rather than running it out for no one.
type Sink<'a> = &'a mut dyn FnMut(&str) -> bool;

fn compile_and_run(request: &Request, sink: Sink) {
  let dir = match scratch_dir() {
    Ok(dir) => dir,
    Err(e) => {
      sink(&plain(&format!("Could not create a build directory: {e}")));
      return;
    }
  };

  build_in(&dir, request, sink);

  // Nothing can be done about a failed cleanup, and reporting it would bury
  // the program's own output.
  let _ = fs::remove_dir_all(&dir);
}

fn build_in(dir: &Path, request: &Request, sink: Sink) {
  let source = dir.join(SOURCE);
  if let Err(e) = fs::write(&source, &request.code) {
    sink(&plain(&format!("Could not write {}: {e}", source.display())));
    return;
  }

  // Compiled from inside the scratch directory and given relative paths, so
  // that diagnostics read `main.rs:4:9` rather than naming a temp directory
  // nobody in the room can do anything with.
  let compile = Command::new("rustc")
    .current_dir(dir)
    .arg("--edition")
    .arg(edition(request.edition.as_deref()))
    .arg("-C")
    .arg(format!(
      "opt-level={}",
      opt_level(request.optimize.as_deref())
    ))
    // rustc suppresses colour when stderr is not a terminal, which a pipe
    // never is. Ask for it explicitly and turn it into spans below.
    .arg("--color=always")
    .arg("-o")
    .arg(BINARY)
    .arg(SOURCE)
    .output();

  let compile = match compile {
    Ok(output) => output,
    Err(e) => {
      sink(&plain(&format!(
        "Could not run rustc: {e}\n\
         The Run button compiles locally, so rustc has to be on PATH."
      )));
      return;
    }
  };

  // Warnings come first, the way they do in a terminal, and as one piece:
  // the converter needs a whole diagnostic to pair up its escape codes.
  let diagnostics = ansi(&String::from_utf8_lossy(&compile.stderr));
  if !diagnostics.is_empty() && !sink(&diagnostics) {
    return;
  }
  // A snippet that does not compile is the expected case for half the slides
  // in an ownership lecture, not an error on our side.
  if compile.status.success() {
    execute(dir, sink);
  }
}

fn execute(dir: &Path, sink: Sink) {
  let child = Command::new(dir.join(BINARY))
    .current_dir(dir)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn();

  match child {
    Ok(child) => run_with_timeout(child, sink),
    Err(e) => {
      sink(&plain(&format!("Could not start the compiled program: {e}")));
    }
  }
}

/// Passes `child`'s output on as it is written, killing the child if it
/// outruns [`TIMEOUT`] or if the sink stops listening.
///
/// The pipes are read on their own threads and their pieces arrive here in
/// the order they were read, so a program's stdout and stderr interleave as
/// they did when it ran. Reading from the start is also what keeps a runaway
/// program from blocking on a full pipe buffer before the timeout catches it.
fn run_with_timeout(mut child: Child, sink: Sink) {
  let (tx, rx) = mpsc::channel();
  let mut open = 0;
  if let Some(pipe) = child.stdout.take() {
    forward(pipe, tx.clone());
    open += 1;
  }
  if let Some(pipe) = child.stderr.take() {
    forward(pipe, tx.clone());
    open += 1;
  }
  drop(tx);

  let deadline = Instant::now() + TIMEOUT;
  let mut ends_line = true;
  let timed_out = loop {
    let left = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(left.min(POLL)) {
      Ok(Some(text)) => {
        ends_line = text.ends_with('\n');
        if !sink(&plain(&text)) {
          let _ = child.kill();
          let _ = child.wait();
          return;
        }
      }
      // One pipe closed. Both closed means the program is done writing,
      // which is the end of its output even before it has exited.
      Ok(None) => {
        open -= 1;
        if open == 0 {
          break false;
        }
      }
      Err(mpsc::RecvTimeoutError::Disconnected) => break false,
      Err(mpsc::RecvTimeoutError::Timeout) => {
        if Instant::now() >= deadline {
          break true;
        }
      }
    }
  };

  if timed_out {
    let _ = child.kill();
  }
  let _ = child.wait();
  if timed_out {
    let newline = if ends_line { "" } else { "\n" };
    sink(&plain(&format!(
      "{newline}Timeout: the program was killed after {} seconds.\n",
      TIMEOUT.as_secs()
    )));
  }
}

/// Reads `pipe` on a thread of its own, sending each piece as soon as it is
/// read, and `None` at the end. A piece that ends partway through a UTF-8
/// character keeps that character's first bytes for the next one, so no
/// character is ever split into two replacement characters.
fn forward(mut pipe: impl Read + Send + 'static, tx: mpsc::Sender<Option<String>>) {
  thread::spawn(move || {
    let mut pending = Vec::new();
    let mut buf = [0; 4096];
    loop {
      let n = match pipe.read(&mut buf) {
        Ok(0) | Err(_) => break,
        Ok(n) => n,
      };
      pending.extend_from_slice(&buf[.. n]);
      let complete = match std::str::from_utf8(&pending) {
        Ok(_) => pending.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => pending.len(),
      };
      if complete > 0 {
        let text = String::from_utf8_lossy(&pending[.. complete]).into_owned();
        pending.drain(.. complete);
        if tx.send(Some(text)).is_err() {
          return;
        }
      }
    }
    if !pending.is_empty() {
      let _ = tx.send(Some(String::from_utf8_lossy(&pending).into_owned()));
    }
    let _ = tx.send(None);
  });
}

/// Both of these go onto a command line, so neither is taken on trust.
fn edition(requested: Option<&str>) -> &str {
  match requested {
    Some("2015") => "2015",
    Some("2018") => "2018",
    Some("2024") => "2024",
    _ => "2021",
  }
}

fn opt_level(requested: Option<&str>) -> &str {
  match requested {
    Some("1") => "1",
    Some("2") => "2",
    Some("3") => "3",
    _ => "0",
  }
}

/// A fresh directory per request, so two clicks in quick succession cannot
/// compile over each other.
fn scratch_dir() -> std::io::Result<PathBuf> {
  static COUNTER: AtomicU64 = AtomicU64::new(0);
  let n = COUNTER.fetch_add(1, Ordering::Relaxed);
  let dir = env::temp_dir()
    .join(format!("aquascope-reveal-run-{}-{n}", std::process::id()));
  fs::create_dir_all(&dir)?;
  Ok(dir)
}

/// Text with no markup of its own, escaped for `innerHTML`.
pub fn plain(s: &str) -> String {
  s.replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
}

/// rustc's coloured output as HTML. The converter escapes the text itself.
///
/// Colours come out as `var(--aq-ansi-<name>, <terminal default>)`, so a deck
/// restyles them by setting those variables; the defaults for a white slide
/// live in `assets/aquascope-reveal.css`. Nothing here can fail in practice --
/// the input is rustc's own output -- but falling back to the escaped text
/// with the codes stripped beats losing the diagnostic.
pub fn ansi(s: &str) -> String {
  ansi_to_html::Converter::new()
    // Decides only how "reverse video" is rendered, which rustc does not use;
    // set anyway because the slides are a light background.
    .theme(ansi_to_html::Theme::Light)
    .four_bit_var_prefix(Some(VAR_PREFIX.to_string()))
    .convert(s)
    .unwrap_or_else(|_| plain(&strip_ansi(s)))
}

/// Last-resort removal of SGR sequences, for the fallback above.
pub fn strip_ansi(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  let mut chars = s.chars();
  while let Some(c) = chars.next() {
    if c != '\x1b' {
      out.push(c);
      continue;
    }
    // Skip up to and including the sequence's final byte.
    for c in chars.by_ref() {
      if c.is_ascii_alphabetic() {
        break;
      }
    }
  }
  out
}

#[cfg(test)]
mod test {
  use super::{evaluate, opt_level, strip_ansi};

  fn result_of(body: &str) -> String {
    let response = evaluate(body.as_bytes());
    let json: serde_json::Value = serde_json::from_slice(&response).unwrap();
    json["result"].as_str().unwrap().to_string()
  }

  #[test]
  fn runs_a_program() {
    let body = r#"{"code":"fn main() { println!(\"hi\"); }","edition":"2021"}"#;
    assert_eq!(result_of(body), "hi\n");
  }

  #[test]
  fn reports_compile_errors_as_output() {
    let body =
      r#"{"code":"fn main() { let x: i32 = \"s\"; }","edition":"2021"}"#;
    let result = result_of(body);
    assert!(result.contains("mismatched types"), "{result}");
    // rustc's colours arrive as themeable variables, not literal escapes.
    assert!(result.contains("var(--aq-ansi-"), "{result}");
    assert!(!result.contains('\x1b'), "{result}");
  }

  #[test]
  fn program_output_cannot_style_the_slide() {
    // A snippet printing markup or escape codes is text, not HTML: only
    // rustc's own output is trusted with colour.
    let body = r#"{"code":"fn main() { print!(\"\\x1b[31m<b>hi</b>\"); }","edition":"2021"}"#;
    let result = result_of(body);
    assert!(
      result.ends_with("\x1b[31m&lt;b&gt;hi&lt;/b&gt;"),
      "{result}"
    );
  }

  #[test]
  fn strips_escapes_when_conversion_fails() {
    assert_eq!(strip_ansi("\x1b[1m\x1b[91merror\x1b[0m: bad"), "error: bad");
  }

  #[test]
  fn kills_a_runaway_program() {
    // Cheap enough to keep in the suite only because the timeout is the whole
    // point of the code under test; it takes TIMEOUT seconds to pass.
    let body = r#"{"code":"fn main() { loop {} }","edition":"2021"}"#;
    assert!(result_of(body).contains("Timeout"));
  }

  #[test]
  fn streams_output_as_the_program_writes_it() {
    // Two lines a pause apart arrive as two pieces, the first well before
    // the program is done -- which is the whole point of streaming.
    let body = r#"{"code":"fn main() { println!(\"a\"); std::thread::sleep(std::time::Duration::from_millis(600)); println!(\"b\"); }"}"#;
    let mut pieces: Vec<(std::time::Instant, String)> = Vec::new();
    super::stream(body.as_bytes(), &mut |line| {
      let json: serde_json::Value = serde_json::from_slice(line).unwrap();
      pieces.push((
        std::time::Instant::now(),
        json["html"].as_str().unwrap().to_string(),
      ));
      true
    });
    let a = pieces.iter().find(|(_, html)| html == "a\n").expect("a");
    let b = pieces.iter().find(|(_, html)| html == "b\n").expect("b");
    assert!(b.0 - a.0 >= std::time::Duration::from_millis(400), "{pieces:?}");
  }

  #[test]
  fn a_listener_that_leaves_stops_the_program() {
    // The sink refuses the first piece; the program, which would otherwise
    // run into the timeout, is killed then and there.
    let body = r#"{"code":"fn main() { loop { println!(\"x\"); std::thread::sleep(std::time::Duration::from_millis(10)); } }"}"#;
    let start = std::time::Instant::now();
    super::stream(body.as_bytes(), &mut |_| false);
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
  }

  #[test]
  fn a_malformed_request_is_reported_not_panicked_on() {
    assert!(result_of("not json").starts_with("Malformed request"));
  }

  #[test]
  fn rejects_unknown_flag_values() {
    assert_eq!(opt_level(Some("; rm -rf /")), "0");
    assert_eq!(opt_level(Some("2")), "2");
  }
}
