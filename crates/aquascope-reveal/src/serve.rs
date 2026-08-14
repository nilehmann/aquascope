//! A minimal static file server, enough to preview a built deck.
//!
//! Deliberately small: one thread per connection, no keep-alive, no ranges, no
//! compression. A deck is a handful of files served to one browser on
//! localhost, so the interesting part is only that it is correct about paths
//! and content types.
//!
//! The one dynamic route is [`RUN_ENDPOINT`], which stands in for the Rust
//! playground so that the Run button works without a network. See [`crate::run`].

use std::{
  fs,
  io::{BufRead, BufReader, Read, Write},
  net::{TcpListener, TcpStream},
  path::{Component, Path, PathBuf},
  thread,
};

use anyhow::{Context, Result};

use crate::run;

/// Where the editor posts snippets. The path is the playground's, because the
/// editor's default is the playground and it only varies the origin.
pub const RUN_ENDPOINT: &str = "/evaluate.json";

/// A snippet is a slide's worth of code. Anything larger is not a snippet.
const MAX_BODY: usize = 1 << 20;

/// Binds the port and serves `root` on a background thread.
///
/// Bound to loopback only: a lecture deck has no business being reachable from
/// the rest of the network.
pub fn spawn(root: PathBuf, port: u16) -> Result<()> {
  let listener = TcpListener::bind(("127.0.0.1", port))
    .with_context(|| format!("binding 127.0.0.1:{port}"))?;

  println!("Serving {} at http://127.0.0.1:{port}", root.display());

  thread::spawn(move || {
    for stream in listener.incoming() {
      let Ok(stream) = stream else {
        continue;
      };
      let root = root.clone();
      thread::spawn(move || {
        if let Err(e) = handle(stream, &root) {
          // A browser closing a connection mid-response is routine.
          if e.kind() != std::io::ErrorKind::BrokenPipe {
            eprintln!("serve: {e}");
          }
        }
      });
    }
  });

  Ok(())
}

fn handle(mut stream: TcpStream, root: &Path) -> std::io::Result<()> {
  let mut reader = BufReader::new(stream.try_clone()?);
  let mut request_line = String::new();
  reader.read_line(&mut request_line)?;

  let mut parts = request_line.split_whitespace();
  let method = parts.next().unwrap_or_default();
  let target = parts.next().unwrap_or("/");

  if method == "POST" {
    return post(&mut stream, &mut reader, target);
  }

  if method != "GET" && method != "HEAD" {
    return respond(&mut stream, 405, "text/plain", b"method not allowed", true);
  }
  let head_only = method == "HEAD";

  let Some(path) = resolve(root, target) else {
    return respond(&mut stream, 403, "text/plain", b"forbidden", head_only);
  };

  match fs::read(&path) {
    Ok(body) => {
      respond(&mut stream, 200, content_type(&path), &body, head_only)
    }
    Err(_) => respond(&mut stream, 404, "text/plain", b"not found", head_only),
  }
}

/// The only POST route is the snippet runner.
fn post(
  stream: &mut TcpStream,
  reader: &mut BufReader<TcpStream>,
  target: &str,
) -> std::io::Result<()> {
  let path = target.split(['?', '#']).next().unwrap_or("/");
  let length = content_length(reader)?;

  if path != RUN_ENDPOINT {
    // The body has to come off the socket either way, or the browser sees the
    // response as a broken connection rather than a 404.
    let _ = read_body(reader, length);
    return respond(stream, 404, "text/plain", b"not found", false);
  }

  let Some(body) = read_body(reader, length)? else {
    return respond(stream, 413, "text/plain", b"body too large", false);
  };

  let json = run::evaluate(&body);
  respond(stream, 200, "application/json; charset=utf-8", &json, false)
}

/// Consumes the request headers, returning the declared body length. Anything
/// unparsable reads as no body, which the runner reports as a malformed
/// request.
fn content_length(reader: &mut BufReader<TcpStream>) -> std::io::Result<usize> {
  let mut length = 0;
  loop {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
      break;
    }
    let line = line.trim_end();
    if line.is_empty() {
      break;
    }
    if let Some((name, value)) = line.split_once(':') {
      if name.eq_ignore_ascii_case("content-length") {
        length = value.trim().parse().unwrap_or(0);
      }
    }
  }
  Ok(length)
}

/// `None` if the client declared more than [`MAX_BODY`].
fn read_body(
  reader: &mut BufReader<TcpStream>,
  length: usize,
) -> std::io::Result<Option<Vec<u8>>> {
  if length > MAX_BODY {
    return Ok(None);
  }
  let mut body = vec![0; length];
  reader.read_exact(&mut body)?;
  Ok(Some(body))
}

/// Maps a request target onto a path inside `root`, or `None` if it tries to
/// escape. Rejecting `..` outright is stricter than resolving and then
/// comparing, and needs no canonicalization.
fn resolve(root: &Path, target: &str) -> Option<PathBuf> {
  let path = target.split(['?', '#']).next().unwrap_or("/");
  let decoded = percent_decode(path);

  let mut out = root.to_path_buf();
  for component in Path::new(&decoded).components() {
    match component {
      Component::Normal(part) => out.push(part),
      Component::RootDir | Component::CurDir => {}
      Component::ParentDir | Component::Prefix(_) => return None,
    }
  }

  if out.is_dir() {
    out.push("index.html");
  }
  Some(out)
}

fn percent_decode(s: &str) -> String {
  let bytes = s.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'%' && i + 2 < bytes.len() {
      let hex = std::str::from_utf8(&bytes[i + 1 ..= i + 2]).ok();
      if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
        out.push(byte);
        i += 3;
        continue;
      }
    }
    out.push(bytes[i]);
    i += 1;
  }
  String::from_utf8_lossy(&out).into_owned()
}

fn content_type(path: &Path) -> &'static str {
  match path.extension().and_then(|e| e.to_str()).unwrap_or_default() {
    "html" => "text/html; charset=utf-8",
    "css" => "text/css; charset=utf-8",
    "js" | "mjs" => "text/javascript; charset=utf-8",
    "json" | "map" => "application/json; charset=utf-8",
    "txt" => "text/plain; charset=utf-8",
    "svg" => "image/svg+xml",
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "gif" => "image/gif",
    "ico" => "image/x-icon",
    "webp" => "image/webp",
    "woff2" => "font/woff2",
    "woff" => "font/woff",
    "ttf" => "font/ttf",
    "otf" => "font/otf",
    "eot" => "application/vnd.ms-fontobject",
    "wasm" => "application/wasm",
    _ => "application/octet-stream",
  }
}

/// Everything is sent `no-store`. This is a preview server whose whole job is
/// to show the file that is on disk right now -- and `--watch` depends on the
/// browser not serving a cached copy of a rebuilt asset.
fn respond(
  stream: &mut TcpStream,
  status: u16,
  content_type: &str,
  body: &[u8],
  head_only: bool,
) -> std::io::Result<()> {
  let reason = match status {
    200 => "OK",
    403 => "Forbidden",
    404 => "Not Found",
    413 => "Payload Too Large",
    _ => "Method Not Allowed",
  };

  write!(
    stream,
    "HTTP/1.1 {status} {reason}\r\n\
     Content-Type: {content_type}\r\n\
     Content-Length: {}\r\n\
     Cache-Control: no-store\r\n\
     Connection: close\r\n\r\n",
    body.len()
  )?;

  if !head_only {
    stream.write_all(body)?;
  }
  stream.flush()
}

#[cfg(test)]
mod test {
  use std::path::Path;

  use super::{content_type, percent_decode, resolve};

  #[test]
  fn rejects_traversal() {
    let root = Path::new("/deck");
    assert!(resolve(root, "/../etc/passwd").is_none());
    assert!(resolve(root, "/img/../../etc/passwd").is_none());
    // Encoded traversal is decoded before the components are inspected.
    assert!(resolve(root, "/%2e%2e/etc/passwd").is_none());
  }

  #[test]
  fn maps_normal_paths() {
    let root = Path::new("/deck");
    assert_eq!(resolve(root, "/img/a.svg").unwrap(), root.join("img/a.svg"));
    // Query strings and fragments are not part of the path.
    assert_eq!(
      resolve(root, "/build-stamp.txt?t=1").unwrap(),
      root.join("build-stamp.txt")
    );
    assert_eq!(resolve(root, "/a%20b.png").unwrap(), root.join("a b.png"));
  }

  #[test]
  fn decodes_percent_escapes() {
    assert_eq!(percent_decode("/a%20b"), "/a b");
    // A stray percent is left alone rather than swallowing the next chars.
    assert_eq!(percent_decode("/100%"), "/100%");
    assert_eq!(percent_decode("/%zz"), "/%zz");
  }

  #[test]
  fn types_the_assets_a_deck_uses() {
    assert_eq!(content_type(Path::new("a.woff2")), "font/woff2");
    assert_eq!(content_type(Path::new("a.svg")), "image/svg+xml");
    assert_eq!(content_type(Path::new("a")), "application/octet-stream");
  }
}
