//! reveal's `<!-- .element: -->` and `<!-- .slide: -->` attribute comments.
//!
//! These are features of reveal's markdown plugin, which renders in the
//! browser. This crate renders markdown in Rust, so the plugin never runs and
//! the comments would otherwise pass through inert. Applying them here gives
//! the same authoring experience at build time:
//!
//! ```markdown
//! <!-- .slide: class="center" -->
//!
//! - first  <!-- .element: class="fragment" -->
//! - second <!-- .element: class="fragment" -->
//! ```
//!
//! `.element` attaches to the element the comment sits in or directly after;
//! `.slide` is collected for the enclosing `<section>`.

use std::collections::HashMap;

/// Elements that never have a closing tag, so they must not be pushed onto the
/// nesting stack when pairing tags up.
const VOID: [&str; 13] = [
  "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta",
  "source", "track", "wbr",
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
  Open,
  Close,
  Void,
}

#[derive(Debug)]
struct Tag<'a> {
  name: &'a str,
  start: usize,
  end: usize,
  kind: Kind,
}

/// Every HTML tag in `html`, in source order. Comments are skipped so that an
/// attribute comment is never mistaken for markup.
fn tags(html: &str) -> Vec<Tag<'_>> {
  let bytes = html.as_bytes();
  let mut out = Vec::new();
  let mut i = 0;

  while let Some(offset) = html[i ..].find('<') {
    let start = i + offset;
    if html[start ..].starts_with("<!--") {
      i = match html[start ..].find("-->") {
        Some(n) => start + n + 3,
        None => break,
      };
      continue;
    }

    let Some(len) = html[start ..].find('>') else {
      break;
    };
    let end = start + len + 1;

    let closing = bytes.get(start + 1) == Some(&b'/');
    let name_at = if closing { start + 2 } else { start + 1 };
    let name_len = html[name_at .. end]
      .find(|c: char| !c.is_ascii_alphanumeric())
      .unwrap_or(0);
    let name = &html[name_at .. name_at + name_len];

    if !name.is_empty() {
      let self_closing = html[start .. end].trim_end_matches('>').ends_with('/');
      let kind = if closing {
        Kind::Close
      } else if self_closing || VOID.contains(&name) {
        Kind::Void
      } else {
        Kind::Open
      };
      out.push(Tag { name, start, end, kind });
    }
    i = end;
  }

  out
}

/// Maps each closing tag to the index of the tag that opened it.
fn pairs(tags: &[Tag]) -> HashMap<usize, usize> {
  let mut stack: Vec<usize> = Vec::new();
  let mut out = HashMap::new();

  for (i, tag) in tags.iter().enumerate() {
    match tag.kind {
      Kind::Open => stack.push(i),
      Kind::Void => {}
      Kind::Close => {
        // Tolerate unbalanced markup by unwinding to the nearest same-named
        // opening tag rather than assuming the top of the stack matches.
        if let Some(pos) = stack.iter().rposition(|&j| tags[j].name == tag.name)
        {
          out.insert(i, stack[pos]);
          stack.truncate(pos);
        }
      }
    }
  }

  out
}

/// Splits an attribute list such as `class="fragment" data-x="1"` into pairs.
fn parse_attrs(s: &str) -> Vec<(String, String)> {
  let mut out = Vec::new();
  let mut rest = s.trim();

  while let Some(eq) = rest.find('=') {
    let name = rest[.. eq].trim().to_string();
    let after = rest[eq + 1 ..].trim_start();
    let (value, tail) = match after.chars().next() {
      Some(quote @ ('"' | '\'')) => {
        let body = &after[1 ..];
        match body.find(quote) {
          Some(n) => (&body[.. n], &body[n + 1 ..]),
          None => (body, ""),
        }
      }
      _ => {
        let n = after.find(char::is_whitespace).unwrap_or(after.len());
        (&after[.. n], &after[n ..])
      }
    };
    if !name.is_empty() {
      out.push((name, value.to_string()));
    }
    rest = tail.trim_start();
  }

  out
}

/// Rewrites an opening tag to carry `attrs`. `class` is merged with whatever
/// the tag already has -- replacing it would silently drop classes the renderer
/// emitted -- while any other attribute is replaced.
fn with_attrs(tag: &str, attrs: &[(String, String)]) -> String {
  let inner = tag
    .trim_start_matches('<')
    .trim_end_matches('>')
    .trim_end_matches('/');
  let name_len = inner
    .find(|c: char| !c.is_ascii_alphanumeric())
    .unwrap_or(inner.len());
  let name = &inner[.. name_len];
  let mut existing = parse_attrs(&inner[name_len ..]);

  for (name, value) in attrs {
    match existing.iter_mut().find(|(n, _)| n == name) {
      Some((_, old)) if name == "class" => {
        *old = format!("{old} {value}");
      }
      Some((_, old)) => *old = value.clone(),
      None => existing.push((name.clone(), value.clone())),
    }
  }

  let rendered = existing
    .iter()
    .map(|(n, v)| format!(" {n}=\"{v}\""))
    .collect::<String>();
  let close = if tag.ends_with("/>") { " />" } else { ">" };

  format!("<{name}{rendered}{close}")
}

/// Applies the attribute comments in one slide's HTML.
///
/// Returns the HTML with the comments removed and `.element` attributes
/// applied, plus the `.slide` attributes for the enclosing `<section>`.
pub fn apply(html: &str) -> (String, Vec<(String, String)>) {
  let all = tags(html);
  let paired = pairs(&all);

  let mut slide_attrs: Vec<(String, String)> = Vec::new();
  // Edits as (range to remove or replace, replacement).
  let mut edits: Vec<(usize, usize, String)> = Vec::new();

  let mut i = 0;
  while let Some(offset) = html[i ..].find("<!--") {
    let start = i + offset;
    let Some(len) = html[start ..].find("-->") else {
      break;
    };
    let end = start + len + 3;
    let body = html[start + 4 .. start + len].trim();
    i = end;

    let (kind, rest) = if let Some(r) = body.strip_prefix(".element:") {
      ("element", r)
    } else if let Some(r) = body.strip_prefix(".slide:") {
      ("slide", r)
    } else {
      continue;
    };

    let attrs = parse_attrs(rest);
    // The comment itself always goes away.
    edits.push((start, end, String::new()));

    if kind == "slide" {
      slide_attrs.extend(attrs);
      continue;
    }

    // `- item <!-- .element: ... -->` puts the comment inside the element, so a
    // closing tag straight after means the target is the element it closes.
    // Otherwise the comment follows a sibling, as in a paragraph on its own
    // line, and the target is whatever ended just before it.
    let after = html[end ..].trim_start();
    let target = if after.starts_with("</") {
      let at = html.len() - after.len();
      all
        .iter()
        .position(|t| t.start == at && t.kind == Kind::Close)
        .and_then(|close| paired.get(&close).copied())
    } else {
      let before = html[.. start].trim_end();
      all.iter().position(|t| t.end == before.len()).and_then(|j| {
        match all[j].kind {
          Kind::Close => paired.get(&j).copied(),
          // An image or other void element is its own target.
          Kind::Void => Some(j),
          Kind::Open => None,
        }
      })
    };

    if let Some(t) = target {
      let tag = &all[t];
      edits.push((
        tag.start,
        tag.end,
        with_attrs(&html[tag.start .. tag.end], &attrs),
      ));
    }
  }

  // Apply back to front so earlier ranges stay valid.
  edits.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
  let mut out = html.to_string();
  for (start, end, replacement) in edits {
    out.replace_range(start .. end, &replacement);
  }

  (out, slide_attrs)
}

/// Renders attributes for a `<section>` opening tag, including the leading
/// space, or an empty string when there are none.
pub fn render(attrs: &[(String, String)]) -> String {
  let mut merged: Vec<(String, String)> = Vec::new();
  for (name, value) in attrs {
    match merged.iter_mut().find(|(n, _)| n == name) {
      Some((_, old)) if name == "class" => *old = format!("{old} {value}"),
      Some((_, old)) => *old = value.clone(),
      None => merged.push((name.clone(), value.clone())),
    }
  }
  merged
    .iter()
    .map(|(n, v)| format!(" {n}=\"{v}\""))
    .collect()
}

#[cfg(test)]
mod test {
  use super::{apply, render};

  #[test]
  fn attaches_to_the_containing_element() {
    let (html, slide) =
      apply("<ul>\n<li>one <!-- .element: class=\"fragment\" --></li>\n</ul>");
    assert_eq!(
      html,
      "<ul>\n<li class=\"fragment\">one </li>\n</ul>",
      "list item should carry the class"
    );
    assert!(slide.is_empty());
  }

  #[test]
  fn attaches_to_the_preceding_sibling() {
    let (html, _) = apply("<p>text</p>\n<!-- .element: class=\"fragment\" -->");
    assert_eq!(html.trim_end(), "<p class=\"fragment\">text</p>");
  }

  #[test]
  fn merges_with_existing_classes() {
    let (html, _) =
      apply("<p class=\"note\">t</p>\n<!-- .element: class=\"fragment\" -->");
    assert!(html.contains("class=\"note fragment\""), "{html}");
  }

  #[test]
  fn collects_slide_attributes() {
    let (html, slide) =
      apply("<!-- .slide: class=\"center\" -->\n<h1>Title</h1>");
    assert_eq!(html.trim(), "<h1>Title</h1>");
    assert_eq!(render(&slide), " class=\"center\"");
  }

  #[test]
  fn handles_nesting_and_multiple_attributes() {
    let (html, _) = apply(
      "<ul>\n<li>a<ul><li>b</li></ul></li>\n</ul>\n<!-- .element: class=\"f\" data-fragment-index=\"2\" -->",
    );
    // The outer list is the preceding sibling, not the inner one.
    assert!(html.starts_with("<ul class=\"f\" data-fragment-index=\"2\">"), "{html}");
  }

  #[test]
  fn a_comment_inside_an_element_targets_that_element() {
    // Matches reveal, where the comment is a child node and the attributes go
    // to its parent. An image alone in a paragraph is targeted via the <p>.
    let (html, _) = apply(
      "<p><img src=\"a.svg\" /> <!-- .element: class=\"fragment\" --></p>",
    );
    assert!(html.starts_with("<p class=\"fragment\">"), "{html}");
  }

  #[test]
  fn targets_a_void_element_as_a_sibling() {
    let (html, _) =
      apply("<img src=\"a.svg\" />\n<!-- .element: class=\"fragment\" -->");
    assert!(html.starts_with("<img src=\"a.svg\" class=\"fragment\" />"), "{html}");
  }

  #[test]
  fn leaves_other_comments_alone() {
    let (html, slide) = apply("<p>x</p>\n<!-- just a note -->");
    assert!(html.contains("<!-- just a note -->"));
    assert!(slide.is_empty());
  }
}
