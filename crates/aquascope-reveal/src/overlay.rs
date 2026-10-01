//! The build-error overlay shown by `--watch`.
//!
//! A failed rebuild leaves the last good `index.html` in place, so without
//! help the browser keeps showing a deck that no longer matches the source and
//! says nothing about why. Instead, each build ends with [`publish`]: a failed
//! one writes [`ERROR_FILE`] before bumping the stamp, and `livereload.js`
//! draws its contents in a modal over the stale deck; a successful one removes
//! the file before bumping the stamp, and the page reloads as usual.
//!
//! Only `--watch` does any of this. A one-off build still fails with a
//! non-zero exit and writes nothing, so a broken deck cannot be deployed by
//! accident.

use std::{fmt, fs, io::IsTerminal, path::Path};

use anyhow::Result;

use crate::run::{ansi, plain, strip_ansi};

/// Written next to `index.html` while the last build failed. An HTML fragment,
/// escaped here, that the overlay inserts as is.
pub const ERROR_FILE: &str = "build-error.html";
const STAMP_FILE: &str = "build-stamp.txt";

const LIVERELOAD_JS: &[u8] = include_bytes!("../assets/livereload.js");

/// One thing wrong with the deck.
pub struct Problem {
  /// `lecture.md:42`, or `None` for a failure that is not about one place.
  pub location: Option<String>,
  pub message: String,
  /// rustc's output, colour escapes included.
  pub diagnostic: Option<String>,
  /// Raw output from whatever failed. Collapsed in the overlay when there is
  /// a diagnostic to read instead.
  pub details: Option<String>,
}

impl Problem {
  /// A failure with nothing to show but its message.
  pub fn general(error: &anyhow::Error) -> Self {
    Problem {
      location: None,
      message: format!("{error:#}"),
      diagnostic: None,
      details: None,
    }
  }

  /// Prints the problem the way the rest of the build log reads. rustc's
  /// colours are kept for a terminal and stripped for anything else.
  pub fn print(&self) {
    let colour = std::io::stderr().is_terminal();
    match &self.location {
      Some(location) => eprintln!("error: {location}: {}", self.message),
      None => eprintln!("error: {}", self.message),
    }
    let body = match (&self.diagnostic, &self.details) {
      (Some(d), _) if colour => d.clone(),
      (Some(d), _) => strip_ansi(d),
      (None, Some(details)) => details.clone(),
      (None, None) => return,
    };
    for line in body.lines() {
      eprintln!("        {line}");
    }
  }

  fn to_html(&self) -> String {
    let mut html = String::from("<section class=\"problem\">\n<header>");
    if let Some(location) = &self.location {
      html.push_str(&format!(
        "<span class=\"location\">{}</span>",
        plain(location)
      ));
    }
    html.push_str(&format!(
      "<span class=\"message\">{}</span></header>\n",
      plain(&self.message)
    ));
    if let Some(diagnostic) = &self.diagnostic {
      html.push_str(&format!(
        "<pre class=\"diagnostic\">{}</pre>\n",
        ansi(diagnostic)
      ));
    }
    if let Some(details) = &self.details {
      let open = if self.diagnostic.is_none() { " open" } else { "" };
      html.push_str(&format!(
        "<details{open}><summary>Raw output</summary><pre>{}</pre></details>\n",
        plain(details)
      ));
    }
    html.push_str("</section>\n");
    html
  }
}

/// The error a build returns when one or more blocks are broken. The problems
/// themselves have already been printed; this carries them to the overlay.
#[derive(Debug)]
pub struct Failed(pub Vec<Problem>);

impl fmt::Debug for Problem {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "{:?}: {}", self.location, self.message)
  }
}

impl fmt::Display for Failed {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self.0.len() {
      1 => write!(f, "1 code block failed to build"),
      n => write!(f, "{n} code blocks failed to build"),
    }
  }
}

impl std::error::Error for Failed {}

/// The problems behind a failed build: the blocks it collected, or the one
/// error that stopped it.
pub fn problems_of(error: &anyhow::Error) -> Vec<&Problem> {
  match error.downcast_ref::<Failed>() {
    Some(Failed(problems)) => problems.iter().collect(),
    None => Vec::new(),
  }
}

/// Records the outcome of a build for the open page: `problems` empty means it
/// succeeded. Called after `index.html` is written, if it is, so the stamp is
/// the last thing to change and the page never reloads into a half-built
/// directory.
///
/// Also writes `livereload.js` itself, because a build that fails early never
/// reaches the point where the deck's assets are written. And if there is no
/// deck yet at all -- the first build failed -- it writes a placeholder page
/// for the overlay to sit on.
pub fn publish(out: &Path, problems: &[&Problem]) -> Result<()> {
  fs::create_dir_all(out.join("aquascope"))?;
  fs::write(out.join("aquascope/livereload.js"), LIVERELOAD_JS)?;

  let error_file = out.join(ERROR_FILE);
  if problems.is_empty() {
    if error_file.exists() {
      fs::remove_file(&error_file)?;
    }
  } else {
    let html: String = problems.iter().map(|p| p.to_html()).collect();
    fs::write(&error_file, html)?;

    let index = out.join("index.html");
    if !index.exists() {
      fs::write(index, PLACEHOLDER)?;
    }
  }

  fs::write(out.join(STAMP_FILE), stamp())?;
  Ok(())
}

/// A value that changes on every build. Nanosecond resolution is well past
/// what the poll interval can distinguish, so successive builds never collide.
fn stamp() -> String {
  std::time::SystemTime::now()
    .duration_since(std::time::SystemTime::UNIX_EPOCH)
    .map(|d| d.as_nanos().to_string())
    .unwrap_or_default()
}

const PLACEHOLDER: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <title>Build failed</title>
</head>
<body>
  <p>No successful build yet. This page reloads when there is one.</p>
  <script src="aquascope/livereload.js"></script>
</body>
</html>
"#;

#[cfg(test)]
mod test {
  use super::*;

  #[test]
  fn everything_in_the_fragment_is_escaped() {
    let problem = Problem {
      location: Some("<a>.md:1".into()),
      message: "<script>".into(),
      diagnostic: Some("\x1b[31m<b>\x1b[0m".into()),
      details: Some("</pre>".into()),
    };
    let html = problem.to_html();
    assert!(!html.contains("<script>"), "{html}");
    assert!(!html.contains("<b>"), "{html}");
    assert!(!html.contains("<a>"), "{html}");
    assert!(html.contains("&lt;/pre&gt;"), "{html}");
    assert!(!html.contains('\x1b'), "{html}");
  }

  #[test]
  fn details_open_only_without_a_diagnostic() {
    let mut problem = Problem {
      location: None,
      message: "m".into(),
      diagnostic: None,
      details: Some("d".into()),
    };
    assert!(problem.to_html().contains("<details open>"));
    problem.diagnostic = Some("error".into());
    assert!(problem.to_html().contains("<details>"));
  }
}
