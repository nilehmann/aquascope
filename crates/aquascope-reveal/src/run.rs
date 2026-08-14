//! The Run button's backend: compile and execute a snippet with the local
//! toolchain.
//!
//! Aquascope's editor posts snippets to the Rust playground's
//! `/evaluate.json`. A deck is given in a lecture room whose wifi cannot be
//! trusted, so `--serve` answers that endpoint itself. Only the part of the
//! contract the editor uses is implemented: `{version, optimize, code,
//! edition}` in, `{result}` out.
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
  sync::atomic::{AtomicU64, Ordering},
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
  let result = match serde_json::from_slice::<Request>(body) {
    Ok(request) => compile_and_run(&request),
    Err(e) => plain(&format!("Malformed request: {e}")),
  };

  // `result` is the only field the editor reads. serde_json does the quoting,
  // which is the part worth not hand-rolling.
  serde_json::json!({ "result": result }).to_string().into_bytes()
}

fn compile_and_run(request: &Request) -> String {
  let dir = match scratch_dir() {
    Ok(dir) => dir,
    Err(e) => return plain(&format!("Could not create a build directory: {e}")),
  };

  let result = build_in(&dir, request);

  // Nothing can be done about a failed cleanup, and reporting it would bury
  // the program's own output.
  let _ = fs::remove_dir_all(&dir);
  result
}

fn build_in(dir: &Path, request: &Request) -> String {
  let source = dir.join(SOURCE);
  if let Err(e) = fs::write(&source, &request.code) {
    return plain(&format!("Could not write {}: {e}", source.display()));
  }

  // Compiled from inside the scratch directory and given relative paths, so
  // that diagnostics read `main.rs:4:9` rather than naming a temp directory
  // nobody in the room can do anything with.
  let compile = Command::new("rustc")
    .current_dir(dir)
    .arg("--edition")
    .arg(edition(request.edition.as_deref()))
    .arg("-C")
    .arg(format!("opt-level={}", opt_level(request.optimize.as_deref())))
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
      return plain(&format!(
        "Could not run rustc: {e}\n\
         The Run button compiles locally, so rustc has to be on PATH."
      ))
    }
  };

  let diagnostics = ansi(&String::from_utf8_lossy(&compile.stderr));
  if !compile.status.success() {
    // A snippet that does not compile is the expected case for half the
    // slides in an ownership lecture, not an error on our side.
    return diagnostics;
  }

  // Warnings come first, the way they do in a terminal.
  diagnostics + &execute(dir)
}

fn execute(dir: &Path) -> String {
  let child = Command::new(dir.join(BINARY))
    .current_dir(dir)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn();

  match child {
    Ok(child) => plain(&wait_with_timeout(child)),
    Err(e) => plain(&format!("Could not start the compiled program: {e}")),
  }
}

/// Waits for `child`, killing it if it outruns [`TIMEOUT`].
///
/// The pipes are drained on their own threads throughout: a program that fills
/// the pipe buffer blocks until someone reads it, so waiting first and reading
/// afterwards would hang exactly on the runaway programs the timeout exists
/// for.
fn wait_with_timeout(mut child: Child) -> String {
  let stdout = child.stdout.take().map(drain);
  let stderr = child.stderr.take().map(drain);

  let deadline = Instant::now() + TIMEOUT;
  let timed_out = loop {
    match child.try_wait() {
      Ok(Some(_)) => break false,
      Err(e) => return format!("Could not wait for the program: {e}"),
      Ok(None) => {}
    }
    if Instant::now() >= deadline {
      let _ = child.kill();
      let _ = child.wait();
      break true;
    }
    thread::sleep(POLL);
  };

  let join = |handle: Option<thread::JoinHandle<String>>| {
    handle.and_then(|h| h.join().ok()).unwrap_or_default()
  };

  let mut output = join(stderr) + &join(stdout);
  if timed_out {
    if !output.is_empty() && !output.ends_with('\n') {
      output.push('\n');
    }
    output.push_str(&format!(
      "Timeout: the program was killed after {} seconds.\n",
      TIMEOUT.as_secs()
    ));
  }
  output
}

fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<String> {
  thread::spawn(move || {
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
  })
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
  let dir = env::temp_dir().join(format!(
    "aquascope-reveal-run-{}-{n}",
    std::process::id()
  ));
  fs::create_dir_all(&dir)?;
  Ok(dir)
}

/// Text with no markup of its own, escaped for `innerHTML`.
fn plain(s: &str) -> String {
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
fn ansi(s: &str) -> String {
  ansi_to_html::Converter::new()
    // Decides only how "reverse video" is rendered, which rustc does not use;
    // set anyway because the slides are a light background.
    .theme(ansi_to_html::Theme::Light)
    .four_bit_var_prefix(Some(VAR_PREFIX.to_string()))
    .convert(s)
    .unwrap_or_else(|_| plain(&strip_ansi(s)))
}

/// Last-resort removal of SGR sequences, for the fallback above.
fn strip_ansi(s: &str) -> String {
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
    let body = r#"{"code":"fn main() { let x: i32 = \"s\"; }","edition":"2021"}"#;
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
    assert!(result.ends_with("\x1b[31m&lt;b&gt;hi&lt;/b&gt;"), "{result}");
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
  fn a_malformed_request_is_reported_not_panicked_on() {
    assert!(result_of("not json").starts_with("Malformed request"));
  }

  #[test]
  fn rejects_unknown_flag_values() {
    assert_eq!(opt_level(Some("; rm -rf /")), "0");
    assert_eq!(opt_level(Some("2")), "2");
  }
}
