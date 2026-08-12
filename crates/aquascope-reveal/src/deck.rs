//! Splitting a markdown document into reveal.js slides.
//!
//! A line containing only `---` starts a new slide; a line containing only `--`
//! starts a new *vertical* slide underneath the current one. Both must be
//! surrounded by blank lines, which keeps them from colliding with setext
//! headings (`Title` followed by `---`) and with `--` appearing in prose.
//! Separators inside fenced code blocks are ignored.

use pulldown_cmark::{html, Options, Parser};

/// A deck is a list of horizontal slides, each of which is a non-empty list of
/// vertical slides. A slide with no vertical children is a one-element list.
pub struct Deck {
  pub slides: Vec<Vec<String>>,
}

#[derive(PartialEq)]
enum Sep {
  None,
  Horizontal,
  Vertical,
}

/// Tracks whether we are inside a fenced code block so that separators in code
/// are treated as ordinary text. Handles both ``` and ~~~ fences, and requires
/// the closing fence to be at least as long as the opening one, per CommonMark.
struct FenceState {
  open: Option<(char, usize)>,
}

impl FenceState {
  fn new() -> Self {
    FenceState { open: None }
  }

  fn in_code(&self) -> bool {
    self.open.is_some()
  }

  fn update(&mut self, line: &str) {
    let trimmed = line.trim_start();
    let ch = match trimmed.chars().next() {
      Some(c @ ('`' | '~')) => c,
      _ => return,
    };
    let len = trimmed.chars().take_while(|c| *c == ch).count();
    if len < 3 {
      return;
    }

    match self.open {
      // A closing fence must match the opening character, be at least as long,
      // and carry no info string.
      Some((open_ch, open_len))
        if open_ch == ch
          && len >= open_len
          && trimmed[len ..].trim().is_empty() =>
      {
        self.open = None;
      }
      Some(_) => {}
      None => self.open = Some((ch, len)),
    }
  }
}

fn separator(line: &str) -> Sep {
  match line.trim() {
    "---" => Sep::Horizontal,
    "--" => Sep::Vertical,
    _ => Sep::None,
  }
}

impl Deck {
  /// Splits `content` into slides. `content` is markdown that has already had
  /// its Aquascope blocks replaced by HTML, so the byte ranges those
  /// replacements were computed against are no longer needed here.
  pub fn parse(content: &str) -> Deck {
    let lines: Vec<&str> = content.lines().collect();
    let mut fences = FenceState::new();

    // `slides` accumulates finished horizontal groups; `group` the vertical
    // slides of the group being built; `cur` the lines of the current slide.
    let mut slides: Vec<Vec<String>> = Vec::new();
    let mut group: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();

    let blank = |i: usize| -> bool {
      i == 0 || i > lines.len() || lines[i - 1].trim().is_empty()
    };

    for (i, line) in lines.iter().enumerate() {
      fences.update(line);

      let sep = if fences.in_code() {
        Sep::None
      } else {
        separator(line)
      };

      // The separator must sit on its own, between blank lines (or at a
      // boundary of the document).
      let isolated = sep != Sep::None
        && blank(i)
        && (i + 1 >= lines.len() || lines[i + 1].trim().is_empty());

      if !isolated {
        cur.push(line);
        continue;
      }

      group.push(cur.join("\n"));
      cur.clear();

      if sep == Sep::Horizontal {
        slides.push(std::mem::take(&mut group));
      }
    }

    group.push(cur.join("\n"));
    slides.push(group);

    // Drop slides that are entirely whitespace, which is what leading or
    // trailing separators produce.
    let slides = slides
      .into_iter()
      .map(|group| {
        group
          .into_iter()
          .filter(|s| !s.trim().is_empty())
          .collect::<Vec<_>>()
      })
      .filter(|group| !group.is_empty())
      .collect();

    Deck { slides }
  }

  /// Renders the deck as the contents of reveal's `<div class="slides">`.
  pub fn to_html(&self) -> String {
    let mut out = String::new();
    for group in &self.slides {
      match group.as_slice() {
        [only] => {
          out.push_str(&section(only));
        }
        many => {
          out.push_str("<section>\n");
          for slide in many {
            out.push_str(&section(slide));
          }
          out.push_str("</section>\n");
        }
      }
    }
    out
  }
}

fn section(markdown: &str) -> String {
  let (html, slide_attrs) = crate::attributes::apply(&to_html(markdown));
  format!(
    "<section{}>\n{html}</section>\n",
    crate::attributes::render(&slide_attrs)
  )
}

fn to_html(markdown: &str) -> String {
  let mut options = Options::empty();
  options.insert(Options::ENABLE_TABLES);
  options.insert(Options::ENABLE_FOOTNOTES);
  options.insert(Options::ENABLE_STRIKETHROUGH);
  options.insert(Options::ENABLE_TASKLISTS);

  let mut out = String::new();
  html::push_html(&mut out, Parser::new_ext(markdown, options));
  out
}

#[cfg(test)]
mod test {
  use super::Deck;

  #[test]
  fn splits_on_isolated_separators() {
    let deck = Deck::parse("a\n\n---\n\nb\n\n--\n\nc\n");
    assert_eq!(deck.slides.len(), 2);
    assert_eq!(deck.slides[0].len(), 1);
    assert_eq!(deck.slides[1].len(), 2);
  }

  #[test]
  fn ignores_separators_in_code() {
    let deck = Deck::parse("a\n\n```text\n\n---\n\n```\n");
    assert_eq!(deck.slides.len(), 1);
  }

  #[test]
  fn ignores_setext_headings() {
    // No blank line before `---`, so this is an h2 rather than a separator.
    let deck = Deck::parse("Title\n---\n\nbody\n");
    assert_eq!(deck.slides.len(), 1);
    assert!(deck.to_html().contains("<h2>"));
  }

  #[test]
  fn passes_through_raw_html() {
    let deck = Deck::parse("<div class=\"aquascope-embed\" data-x=\"&quot;\"></div>\n");
    assert!(deck.to_html().contains("data-x=\"&quot;\""));
  }
}
