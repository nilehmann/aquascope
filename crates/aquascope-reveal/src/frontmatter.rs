//! YAML front matter at the top of a deck.
//!
//! The shape follows `reveal-md`, the usual markdown-first wrapper around
//! reveal.js, so its documentation carries over: deck-level keys at the top,
//! reveal's own options nested under `revealOptions`.
//!
//! ```markdown
//! ---
//! title: Ownership and Borrowing
//! theme: white
//! revealOptions:
//!   navigationMode: default
//!   slideNumber: "c/t"
//! ---
//!
//! # First slide
//! ```

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{Map, Value};

/// reveal options this deck must not set, because the rest of the pipeline
/// depends on them. Silently ignoring an option a deck asked for is worse than
/// refusing it, so these are errors.
const RESERVED: [(&str, &str); 2] = [
  (
    "disableLayout",
    "the deck relies on reveal not scaling slides: Aquascope draws its arrows \
     in document coordinates and does not compensate for a scaled ancestor",
  ),
  (
    "plugins",
    "plugins are live JavaScript objects and cannot be expressed in YAML",
  ),
];

#[derive(Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontMatter {
  /// Page title. Overridden by `--title`.
  pub title: Option<String>,
  /// reveal.js theme name. Overridden by `--theme`.
  pub theme: Option<String>,
  /// Merged into the object passed to `Reveal.initialize`.
  #[serde(default)]
  pub reveal_options: Map<String, Value>,
}

impl FrontMatter {
  /// The reveal options as the JSON literal embedded in the page.
  pub fn reveal_options_json(&self) -> String {
    Value::Object(self.reveal_options.clone()).to_string()
  }
}

/// Splits a deck into its front matter and its markdown body.
///
/// A block is only recognised when the file *starts* with `---`, which keeps it
/// unambiguous: anywhere else in the document that line is a slide separator.
/// The body is returned as a borrowed slice so the byte ranges the Aquascope
/// preprocessor computes against it stay meaningful.
pub fn split(content: &str) -> Result<(FrontMatter, &str)> {
  let Some(rest) = content
    .strip_prefix("---\n")
    .or_else(|| content.strip_prefix("---\r\n"))
  else {
    return Ok((FrontMatter::default(), content));
  };

  let mut offset = 0;
  for line in rest.split_inclusive('\n') {
    let trimmed = line.trim_end();
    // `...` ends a YAML document just as `---` does.
    if trimmed == "---" || trimmed == "..." {
      let yaml = &rest[.. offset];
      let body = &rest[offset + line.len() ..];

      let front: FrontMatter = if yaml.trim().is_empty() {
        FrontMatter::default()
      } else {
        serde_yaml::from_str(yaml).context("parsing front matter")?
      };

      for (key, why) in RESERVED {
        if front.reveal_options.contains_key(key) {
          bail!("front matter may not set revealOptions.{key}: {why}");
        }
      }

      return Ok((front, body));
    }
    offset += line.len();
  }

  bail!("front matter opened with `---` but was never closed")
}

#[cfg(test)]
mod test {
  use super::split;

  #[test]
  fn absent_front_matter_leaves_content_alone() {
    let (front, body) = split("# Title\n\n---\n\n# Next\n").unwrap();
    assert!(front.title.is_none());
    // The `---` further down is a slide separator, not a front matter fence.
    assert!(body.contains("# Next"));
    assert!(front.reveal_options.is_empty());
  }

  #[test]
  fn parses_deck_options() {
    let (front, body) = split(
      "---\ntitle: Deck\ntheme: black\nrevealOptions:\n  navigationMode: default\n  slideNumber: \"c/t\"\n---\n\n# Slide\n",
    )
    .unwrap();
    assert_eq!(front.title.as_deref(), Some("Deck"));
    assert_eq!(front.theme.as_deref(), Some("black"));
    assert_eq!(body.trim(), "# Slide");
    assert_eq!(
      front.reveal_options_json(),
      r#"{"navigationMode":"default","slideNumber":"c/t"}"#
    );
  }

  #[test]
  fn empty_block_is_fine() {
    let (front, body) = split("---\n---\n# Slide\n").unwrap();
    assert!(front.title.is_none());
    assert_eq!(body.trim(), "# Slide");
  }

  #[test]
  fn rejects_reserved_options() {
    let err = split("---\nrevealOptions:\n  disableLayout: false\n---\n")
      .unwrap_err()
      .to_string();
    assert!(err.contains("disableLayout"), "{err}");
  }

  #[test]
  fn rejects_unclosed_and_unknown() {
    assert!(split("---\ntitle: Deck\n\n# Slide\n").is_err());
    assert!(split("---\nnope: 1\n---\n").is_err());
  }
}
