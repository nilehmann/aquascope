//! ```origins fences: Rust carrying origin markers, baked into a
//! syntax-highlighted `<pre>` at build time.
//!
//! A deck teaching lifetimes wants to draw a coloured box around the code a
//! reference borrows from, and to draw the lifetime in a type as that same
//! box. Both are `<span>`s *inside* the code, which is why the block has to be
//! baked here rather than highlighted in the browser: reveal's highlight
//! plugin rewrites the innerHTML of every `pre code` it finds, which would
//! strip them. The output is a `pre` with no `code` child, which that plugin
//! never selects, carrying highlight.css's own class names -- so a baked block
//! is indistinguishable from the Aquascope-highlighted ones on other slides.
//!
//! ````markdown
//! ```origins
//! fn longest<'?a>(v1: &'?a Vec<i32>, v2: &'?a Vec<i32>) -> &'?a Vec<i32> {
//!     if v1.len() > v2.len() { v1 } else { v2 }
//! }
//!
//! let v1: Vec<i32> = [[a:vec![1, 2, 3]:]];
//! let r: &'!a Vec<i32> = longest(&v1, &v2);
//! ```
//! ````
//!
//! **No box appears without a marker.** A bare `'a` is ordinary Rust syntax;
//! every box on the slide is something the source asked for, which is what
//! lets one block hold a generic definition and the concrete origins a caller
//! instantiates it with.
//!
//! ```text
//! 'a             a lifetime, as ordinary syntax: no box
//! '?a            neutral box: a variable ranging over origins, which is what
//!                a generic lifetime parameter on a definition is
//! '!a            box in origin a's colour: the concrete origin `a`
//! [[a:TEXT:]]    frame TEXT in origin a's box; nests
//! [[?:TEXT:]]    frame TEXT in a box with no origin colour
//! [[*:TEXT:]]    tight dashed box: what is actually borrowed, where the
//!                origin around it over-approximates
//! ```
//!
//! The sigils are sugar: `'!a` is `[[a:'a:]]` and `'?a` is `[[?:'a:]]`. They
//! exist because a deck writes far more lifetimes than frames, and the bracket
//! form around each one would bury the code. The general form still buys
//! something the sigils cannot say -- `[[b:'a:]]` boxes a lifetime *named* `a`
//! in origin *b*'s colour, so a lifetime's spelling need not be its origin's
//! letter.
//!
//! `'?` and `'!` are markers only when an identifier character follows, so
//! `let c = '!';` is still a char literal.
//!
//! The frame close is `:]]` rather than `]]` because framed text is usually an
//! expression ending in a bracket, as in `vec![1, 2, 3]`. Everything between
//! the colons is kept verbatim, spaces included: it lands inside the box, so
//! padding there would shift the code away from the lines around it.
//!
//! Colours are the deck's own: `.origin-a`, `.origin-b`, ... on `.oframe` and
//! `.oname`, with `.oexact` for the dashed box. A box with no `origin-*` class
//! draws neutral, which is how a generic parameter is shown. Only the letters
//! the deck's stylesheet defines have colours, so `'!x` for an undefined
//! letter draws neutral too -- indistinguishable from `'?x` while claiming
//! something different. That is a deck-level concern; this crate cannot see
//! the stylesheet.

use std::{cmp::Reverse, ops::Range};

use anyhow::{bail, Context, Result};
use ra_ap_rustc_lexer::{tokenize, FrontmatterAllowed, LiteralKind, TokenKind};

pub type Replacement = (Range<usize>, String);

/// Words highlight.css paints as keywords. Deliberately not every Rust
/// keyword: the list matches what the deck's snippets actually use, and an
/// unknown word simply renders unstyled.
const KEYWORDS: &[&str] = &[
  "let", "mut", "fn", "struct", "impl", "if", "else", "return", "move", "for",
  "in", "while", "loop", "match", "pub", "use", "as", "ref", "const", "static",
  "enum", "trait", "where", "break", "continue",
];

/// Primitives, which hljs paints as types. Every other CamelCase name is
/// treated as a type too, which is what hljs does with a user-defined one.
const PRIMS: &[&str] = &[
  "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128",
  "usize", "isize", "f32", "f64", "bool", "char", "str",
];

/// Lifetimes drawn as neutral boxes.
#[derive(Debug)]
struct Span {
  range: Range<usize>,
  class: String,
  /// Marker boxes sort outside token spans covering the same range, so that a
  /// box framing exactly one token frames the highlighting rather than
  /// splitting it.
  is_marker: bool,
}

/// The class a frame key asks for. `*` is the dashed box, `?` a box with no
/// origin colour, and a letter that origin's colour.
fn frame_class(key: char) -> String {
  match key {
    '*' => "oexact".to_string(),
    '?' => "oframe".to_string(),
    letter => format!("oframe origin-{letter}"),
  }
}

/// Strips the markers, returning the code as it should read and the spans they
/// asked for, in that code's byte offsets.
fn strip(src: &str) -> Result<(String, Vec<Span>)> {
  let (mut code, mut spans, mut open) = (String::new(), Vec::new(), Vec::new());
  let bytes = src.as_bytes();
  let mut i = 0;

  while i < bytes.len() {
    // `[[x:` opens a frame. Anything else starting with `[[` is ordinary code,
    // such as a nested index.
    if bytes[i ..].starts_with(b"[[")
      && i + 4 <= bytes.len()
      && bytes[i + 3] == b':'
      && (bytes[i + 2].is_ascii_lowercase()
        || bytes[i + 2] == b'*'
        || bytes[i + 2] == b'?')
    {
      open.push((code.len(), frame_class(bytes[i + 2] as char)));
      i += 4;
      continue;
    }

    // `'?a` and `'!a`: a boxed lifetime. Only a marker when an identifier
    // character follows the sigil, so `'!'` stays a char literal.
    if bytes[i] == b'\''
      && matches!(bytes.get(i + 1), Some(b'?' | b'!'))
      && bytes
        .get(i + 2)
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
    {
      let start = code.len();
      let key = if bytes[i + 1] == b'?' {
        '?'
      } else {
        bytes[i + 2] as char
      };
      code.push('\'');
      i += 2;
      while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
      {
        code.push(bytes[i] as char);
        i += 1;
      }
      spans.push(Span {
        range: start .. code.len(),
        class: frame_class(key),
        is_marker: true,
      });
      continue;
    }

    if bytes[i ..].starts_with(b":]]") {
      let (start, class) = open
        .pop()
        .context("`:]]` with no matching `[[` origin marker")?;
      spans.push(Span {
        range: start .. code.len(),
        class,
        is_marker: true,
      });
      i += 3;
      continue;
    }

    let ch = src[i ..].chars().next().unwrap();
    code.push(ch);
    i += ch.len_utf8();
  }

  anyhow::ensure!(open.is_empty(), "unclosed origin marker");
  Ok((code, spans))
}

/// The highlight.css class for each token of `code`, as spans.
///
/// The lexer is rustc's own, so comments, strings, char literals, raw strings
/// and lifetimes are delimited exactly as rustc delimits them. What it does
/// not tell us is which identifiers are keywords, types or call sites; those
/// stay the same heuristics hljs itself applies.
fn highlight(code: &str) -> Vec<Span> {
  // Lex once into (range, kind), so the passes below can look either way.
  let mut toks: Vec<(Range<usize>, TokenKind)> = Vec::new();
  let mut pos = 0;
  for token in tokenize(code, FrontmatterAllowed::No) {
    let len = token.len as usize;
    toks.push((pos .. pos + len, token.kind));
    pos += len;
  }

  let significant =
    |t: &&(Range<usize>, TokenKind)| t.1 != TokenKind::Whitespace;
  let mut spans = Vec::new();

  for (n, (range, kind)) in toks.iter().enumerate() {
    let text = &code[range.clone()];
    let next = toks[n + 1 ..].iter().find(significant).map(|t| &t.1);
    let prev = toks[.. n].iter().rev().find(significant);

    let class = match kind {
      TokenKind::LineComment { .. } | TokenKind::BlockComment { .. } => {
        "hljs-comment".to_string()
      }
      TokenKind::Literal { kind, .. } => match kind {
        LiteralKind::Int { .. } | LiteralKind::Float { .. } => {
          "hljs-number".to_string()
        }
        // Every flavour of string, char and byte literal reads as a string.
        _ => "hljs-string".to_string(),
      },
      // Ordinary syntax. A lifetime is boxed only where the source asked
      // for it, with `'?a`, `'!a` or a frame around it.
      TokenKind::Lifetime { .. } | TokenKind::RawLifetime => {
        "hljs-symbol".to_string()
      }
      TokenKind::Ident | TokenKind::RawIdent => {
        // `foo!` is an ident and a `!`; hljs paints the pair as one built_in.
        if matches!(next, Some(TokenKind::Bang)) {
          let bang = toks[n + 1 ..].iter().find(significant).unwrap();
          spans.push(Span {
            range: range.start .. bang.0.end,
            class: "hljs-built_in".to_string(),
            is_marker: false,
          });
          continue;
        }
        let after_fn = prev.is_some_and(|(r, k)| {
          *k == TokenKind::Ident && &code[r.clone()] == "fn"
        });
        if KEYWORDS.contains(&text) {
          "hljs-keyword".to_string()
        } else if PRIMS.contains(&text) {
          "hljs-type".to_string()
        } else if text.starts_with(char::is_uppercase) {
          "hljs-built_in".to_string()
        } else if matches!(next, Some(TokenKind::OpenParen)) || after_fn {
          // Call sites and fn definitions: hljs paints both as titles.
          "hljs-title".to_string()
        } else {
          continue;
        }
      }
      // The `!` of a macro, already folded into the name above.
      TokenKind::Bang
        if prev.is_some_and(|(_, k)| matches!(k, TokenKind::Ident)) =>
      {
        continue
      }
      _ => continue,
    };

    spans.push(Span {
      range: range.clone(),
      class,
      is_marker: false,
    });
  }

  spans
}

/// Splits every token span at any marker boundary falling strictly inside it,
/// so that the two sets of spans are jointly well nested and can be emitted
/// with a single stack. A box drawn around part of a token is unusual but
/// legal, and splitting is what makes it render.
fn split_at_markers(spans: Vec<Span>, markers: &[Span]) -> Vec<Span> {
  let mut cuts: Vec<usize> = markers
    .iter()
    .flat_map(|m| [m.range.start, m.range.end])
    .collect();
  cuts.sort_unstable();
  cuts.dedup();

  spans
    .into_iter()
    .flat_map(|span| {
      let inner: Vec<usize> = cuts
        .iter()
        .copied()
        .filter(|c| span.range.start < *c && *c < span.range.end)
        .collect();
      let mut pieces = Vec::with_capacity(inner.len() + 1);
      let mut start = span.range.start;
      for cut in inner.into_iter().chain([span.range.end]) {
        pieces.push(Span {
          range: start .. cut,
          class: span.class.clone(),
          is_marker: false,
        });
        start = cut;
      }
      pieces
    })
    .collect()
}

/// Whether `text` is exactly one lifetime token and nothing else.
fn is_one_lifetime(text: &str) -> bool {
  let mut toks = tokenize(text, FrontmatterAllowed::No);
  let first = toks.next();
  toks.next().is_none()
    && matches!(
      first.map(|t| t.kind),
      Some(TokenKind::Lifetime { .. } | TokenKind::RawLifetime)
    )
}

fn push_escaped(out: &mut String, text: &str) {
  for ch in text.chars() {
    match ch {
      '&' => out.push_str("&amp;"),
      '<' => out.push_str("&lt;"),
      '>' => out.push_str("&gt;"),
      _ => out.push(ch),
    }
  }
}

/// Renders one block's worth of marked-up Rust.
pub fn render(src: &str) -> Result<String> {
  // The block's last line ends in a newline that belongs to the closing fence,
  // not to the code. Exactly that one comes off, so the rendered block has the
  // lines the author wrote -- blank ones at either end included.
  let src = src.strip_suffix('\n').unwrap_or(src);
  let src = src.strip_suffix('\r').unwrap_or(src);
  let (code, mut markers) = strip(src)?;

  // A frame whose text is exactly one lifetime is that lifetime's own box, so
  // it takes `.oname` -- the same box with the name written in it, rather than
  // `.oframe`, a box drawn around a span of code. This is what makes the
  // sigils sugar for the bracket form: `[[a:'a:]]` and `'!a` are one thing.
  let mut boxed: Vec<Range<usize>> = Vec::new();
  for marker in &mut markers {
    if let Some(rest) = marker.class.strip_prefix("oframe") {
      if is_one_lifetime(&code[marker.range.clone()]) {
        marker.class = format!("oname{rest}");
        boxed.push(marker.range.clone());
      }
    }
  }

  // `.oname` carries the colour and the weight, so a boxed lifetime must not
  // also carry its token class: the inner span's colour would win over the
  // origin's.
  let tokens = highlight(&code)
    .into_iter()
    .filter(|span| span.class != "hljs-symbol" || !boxed.contains(&span.range))
    .collect();

  let mut spans = split_at_markers(tokens, &markers);
  spans.extend(markers);
  // Opened outermost-first at each offset, and a marker outside a token span
  // covering the same range.
  spans.sort_by_key(|s| (s.range.start, Reverse(s.range.end), !s.is_marker));

  let mut out = String::new();
  let mut stack: Vec<usize> = Vec::new();
  let mut at = 0;

  for span in &spans {
    while let Some(&end) = stack.last() {
      if end > span.range.start {
        break;
      }
      push_escaped(&mut out, &code[at .. end]);
      at = end;
      out.push_str("</span>");
      stack.pop();
    }
    push_escaped(&mut out, &code[at .. span.range.start]);
    at = span.range.start;
    out.push_str(&format!(r#"<span class="{}">"#, span.class));
    stack.push(span.range.end);
  }
  while let Some(end) = stack.pop() {
    push_escaped(&mut out, &code[at .. end]);
    at = end;
    out.push_str("</span>");
  }
  push_escaped(&mut out, &code[at ..]);

  // A line with nothing but whitespace on it has its first character escaped,
  // because two different parsers would otherwise eat the line:
  //
  //  - CommonMark ends a raw-HTML block at a blank line, and counts a line of
  //    only spaces or tabs as blank -- so neither an empty line nor a line of
  //    literal spaces will do, and both have to be escaped. The
  //    rest of the `pre` would be re-parsed as markdown, wrapped in a `<p>`
  //    and stripped of its indentation. An entity is not whitespace, so the
  //    line keeps the block open;
  //  - the HTML parser drops a newline immediately following `<pre>`, so a
  //    leading blank line would vanish in the browser however faithfully it
  //    was emitted. Having something on that first line stops the newline
  //    being first.
  //
  // Both decode back to one space, which is what a blank line inside a `pre`
  // is anyway.
  let body = out
    .split('\n')
    .map(|line| {
      if !line.trim().is_empty() {
        return line.to_string();
      }
      // Escaping the first character is enough to make the line non-blank,
      // and escaping it numerically keeps whatever it was -- so a line of
      // three spaces stays three columns wide. An empty line has no first
      // character, and one space is what it renders as inside a `pre`.
      match line.chars().next() {
        Some(first) => {
          format!("&#{};{}", first as u32, &line[first.len_utf8() ..])
        }
        None => "&#32;".to_string(),
      }
    })
    .collect::<Vec<_>>()
    .join("\n");

  Ok(format!(r#"<pre class="code hljs">{body}</pre>"#))
}

/// Every ```origins fence in `content`, as a byte range to splice HTML over.
///
/// Ranges cover the fence lines themselves, and are computed against
/// `content` as given -- the same slice `main` applies them to. `first_line`
/// is the line number `content` starts at in the file on disk, so that a
/// diagnostic names the line the author sees rather than one shifted by the
/// front matter `main` stripped first.
pub fn replacements(
  content: &str,
  first_line: usize,
) -> Result<Vec<Replacement>> {
  /// The backtick count of a fence line, and whatever follows it.
  fn fence(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let ticks = trimmed.chars().take_while(|c| *c == '`').count();
    // Backticks are one byte, so slicing by their count is safe.
    (ticks >= 3).then(|| (ticks, &trimmed[ticks ..]))
  }

  let mut out = Vec::new();
  let mut lines = content.split_inclusive('\n').enumerate();
  let mut offset = 0;

  while let Some((n, line)) = lines.next() {
    let start = offset;
    offset += line.len();

    let Some((ticks, info)) = fence(line) else {
      continue;
    };

    // Every fence is consumed to its close, whether or not it is ours, so
    // that an ```origins example inside a longer markdown fence is left as
    // the text it is.
    let spec = info.trim().strip_prefix("origins");

    let mut body = String::new();
    let mut end = None;
    for (_, line) in lines.by_ref() {
      offset += line.len();
      // A closing fence matches the opening backtick count, is at least as
      // long, and carries no info string of its own.
      if let Some((close, rest)) = fence(line) {
        if close >= ticks && rest.trim().is_empty() {
          end = Some(
            offset - (line.len() - line.trim_end_matches(['\r', '\n']).len()),
          );
          break;
        }
      }
      body.push_str(line);
    }

    let Some(spec) = spec else {
      continue;
    };
    let Some(end) = end else {
      bail!("{}: unclosed ```origins fence", first_line + n);
    };

    if !spec.is_empty() {
      bail!(
        "{}: ```origins takes no specifiers, but found `{}`. Every box is \
         asked for in the code now: mark a generic lifetime `'?a`, a concrete \
         one `'!a`, and leave a lifetime that is just syntax bare",
        first_line + n,
        spec.trim_start_matches(',')
      );
    }

    let html = render(&body).with_context(|| {
      format!("in the ```origins block at line {}", first_line + n)
    })?;
    out.push((start .. end, html));
  }

  Ok(out)
}

#[cfg(test)]
mod test {
  use super::*;

  fn one(md: &str) -> String {
    let reps = replacements(md, 1).unwrap();
    assert_eq!(reps.len(), 1, "{reps:?}");
    reps.into_iter().next().unwrap().1
  }

  #[test]
  fn frames_and_tints_by_letter() {
    let html = one("```origins\nlet r: &'!a Vec<i32> = &[[a:v:]];\n```\n");
    assert!(
      html.contains(r#"<span class="oname origin-a">'a</span>"#),
      "{html}"
    );
    assert!(
      html.contains(r#"<span class="oframe origin-a">v</span>"#),
      "{html}"
    );
    // No `code` child, or reveal's highlight plugin would strip the spans.
    assert!(
      html.starts_with(r#"<pre class="code hljs">"#) && !html.contains("<code")
    );
  }

  #[test]
  fn a_bare_lifetime_gets_no_box() {
    let html = one("```origins\nfn f<'a>(x: &'a str) {}\n```\n");
    assert!(
      html.contains(r#"<span class="hljs-symbol">'a</span>"#),
      "{html}"
    );
    assert!(
      !html.contains("oname") && !html.contains("oframe"),
      "{html}"
    );
  }

  #[test]
  fn question_mark_draws_a_neutral_box() {
    let html = one("```origins\nfn f<'?a>(x: &'?a str) {}\n```\n");
    assert_eq!(
      html.matches(r#"<span class="oname">'a</span>"#).count(),
      2,
      "{html}"
    );
    assert!(!html.contains("origin-a"), "{html}");
  }

  #[test]
  fn generic_and_concrete_coexist_in_one_block() {
    // The case the block-level specifiers could not express: a definition
    // whose `'a` is a variable, beside a caller whose `'a` is origin a.
    let html = one(
      "```origins\nfn longest<'?a>(v: &'?a Vec<i32>) -> &'?a Vec<i32> { v }\nlet r: &'!a Vec<i32> = longest(&[[a:v:]]);\n```\n",
    );
    assert_eq!(
      html.matches(r#"<span class="oname">'a</span>"#).count(),
      3,
      "{html}"
    );
    assert_eq!(
      html
        .matches(r#"<span class="oname origin-a">'a</span>"#)
        .count(),
      1,
      "{html}"
    );
  }

  #[test]
  fn a_sigil_is_sugar_for_the_bracket_form() {
    assert_eq!(
      one("```origins\nlet r: &'!a str = x;\n```\n"),
      one("```origins\nlet r: &[[a:'a:]] str = x;\n```\n")
    );
    assert_eq!(
      one("```origins\nlet r: &'?a str = x;\n```\n"),
      one("```origins\nlet r: &[[?:'a:]] str = x;\n```\n")
    );
  }

  #[test]
  fn a_frame_can_box_a_lifetime_in_another_origins_colour() {
    // What the sigils cannot say: a lifetime's spelling need not be its
    // origin's letter.
    let html = one("```origins\nlet r: &[[b:'a:]] str = x;\n```\n");
    assert!(
      html.contains(r#"<span class="oname origin-b">'a</span>"#),
      "{html}"
    );
  }

  #[test]
  fn a_sigil_needs_an_identifier_after_it() {
    // `'!'` and `'?'` are char literals, not markers.
    let html = one("```origins\nlet c = '!'; let d = '?';\n```\n");
    assert_eq!(html.matches(r#"class="hljs-string""#).count(), 2, "{html}");
    assert!(
      !html.contains("oname") && !html.contains("oframe"),
      "{html}"
    );
  }

  #[test]
  fn dashed_box_nests_inside_an_origin() {
    let html = one("```origins\nlet n = [[a:vec![[[*:1:]], 2]:]];\n```\n");
    let exact = html.find(r#"<span class="oexact">"#).unwrap();
    let frame = html.find(r#"<span class="oframe origin-a">"#).unwrap();
    assert!(frame < exact, "{html}");
  }

  #[test]
  fn a_marker_frames_the_highlighting_of_a_token_it_matches() {
    let html = one("```origins\n[[a:vec!:]][1]\n```\n");
    assert!(
      html.contains(
        r#"<span class="oframe origin-a"><span class="hljs-built_in">vec!</span></span>"#
      ),
      "{html}"
    );
  }

  #[test]
  fn lexes_what_a_regex_gets_wrong() {
    // A char literal is not a lifetime; a raw string is one token; a suffixed
    // integer is a number; `//` inside a string is not a comment.
    let html = one(
      "```origins\nlet c = 'x'; let r = r#\"a \"q\" b\"#; let n = 1_000i64;\nlet s = \"// no\";\n```\n",
    );
    assert!(
      html.contains(r#"<span class="hljs-string">'x'</span>"#),
      "{html}"
    );
    assert!(
      html.contains(r##"<span class="hljs-string">r#"a "q" b"#</span>"##),
      "{html}"
    );
    assert!(
      html.contains(r#"<span class="hljs-number">1_000i64</span>"#),
      "{html}"
    );
    assert!(!html.contains("hljs-comment"), "{html}");
    assert!(!html.contains("oname"), "{html}");
    assert!(!html.contains("hljs-symbol"), "{html}");
  }

  #[test]
  fn keeps_the_framed_text_verbatim() {
    // Padding inside a box would shift the code away from the lines around it,
    // so the markers are written tight and nothing is trimmed.
    let html = one("```origins\nlet a = [[a: 1 :]];\n```\n");
    assert!(html.contains(r#"<span class="oframe origin-a"> <span class="hljs-number">1</span> </span>"#), "{html}");
  }

  #[test]
  fn escapes_html() {
    let html = one("```origins\nlet v: Vec<i32> = f(&x);\n```\n");
    assert!(
      html.contains("&lt;") && html.contains("&gt;") && html.contains("&amp;"),
      "{html}"
    );
    assert!(!html.contains("<i32>"), "{html}");
  }

  #[test]
  fn the_rendered_block_has_the_lines_the_author_wrote() {
    // Blank lines at either end are content, not padding. Only the newline
    // belonging to the closing fence comes off.
    // The rendered text, with the highlighting tags taken back out.
    let inner = |md: &str| {
      let html = one(md);
      let mut text = String::new();
      let mut depth = 0;
      for ch in html.chars() {
        match ch {
          '<' => depth += 1,
          '>' => depth -= 1,
          c if depth == 0 => text.push(c),
          _ => {}
        }
      }
      text
    };
    assert_eq!(inner("```origins\nlet a = 1;\n```\n"), "let a = 1;");
    assert_eq!(
      inner("```origins\nlet a = 1;\n\n\n```\n"),
      "let a = 1;\n&#32;\n&#32;"
    );
    assert_eq!(
      inner("```origins\n\nlet a = 1;\n```\n"),
      "&#32;\nlet a = 1;"
    );
    // Every line count is the source's, once the space stand-ins are read as
    // the empty lines they are.
    for body in ["a", "a\n\nb", "\na\n", "\n\na\n\n"] {
      let md = format!("```origins\n{body}\n```\n");
      assert_eq!(
        inner(&md).split('\n').count(),
        body.split('\n').count(),
        "{body:?}"
      );
    }
  }

  #[test]
  fn blank_lines_become_an_entity_so_the_html_block_survives() {
    let html = one("```origins\nlet a = 1;\n\nlet b = 2;\n```\n");
    assert!(html.contains(";\n&#32;\n<span"), "{html:?}");
  }

  #[test]
  fn leaves_ordinary_double_brackets_alone() {
    let html = one("```origins\nlet x = m[[i]];\n```\n");
    assert!(html.contains("m[[i]]"), "{html}");
    assert!(!html.contains("oframe"), "{html}");
  }

  #[test]
  fn replaces_only_the_fence_and_finds_every_block() {
    let md = "before\n\n```origins\nlet a = 1;\n```\n\nbetween\n\n```origins\nfn f<'?a>() {}\n```\n\nafter\n";
    let reps = replacements(md, 1).unwrap();
    assert_eq!(reps.len(), 2);
    assert_eq!(&md[reps[0].0.clone()], "```origins\nlet a = 1;\n```");
    assert_eq!(&md[reps[1].0.clone()], "```origins\nfn f<'?a>() {}\n```");
  }

  #[test]
  fn names_the_line_in_the_file_rather_than_in_the_body() {
    let err = replacements("```origins\nlet a = [[a:1;\n```\n", 4)
      .unwrap_err()
      .to_string();
    assert!(err.contains("at line 4"), "{err}");
  }

  #[test]
  fn accepts_a_closing_fence_with_trailing_whitespace() {
    let html = one("```origins\nlet a = 1;\n```   \n");
    assert!(html.ends_with("</pre>"), "{html}");
    assert!(!html.contains('`'), "{html}");
  }

  #[test]
  fn leaves_a_nested_example_as_text() {
    // A deck documenting the notation writes an ```origins block inside a
    // longer fence; only real blocks are rendered.
    let md = "````markdown\n```origins\nlet a = [[a:1:]];\n```\n````\n\n```origins\nlet b = 2;\n```\n";
    let reps = replacements(md, 1).unwrap();
    assert_eq!(reps.len(), 1, "{reps:?}");
    assert_eq!(&md[reps[0].0.clone()], "```origins\nlet b = 2;\n```");
  }

  #[test]
  fn ignores_other_fences() {
    assert!(replacements("```rust\nlet a = 1;\n```\n", 1)
      .unwrap()
      .is_empty());
    assert!(
      replacements("```aquascope,permissions\nfn main() {}\n```\n", 1)
        .unwrap()
        .is_empty()
    );
  }

  #[test]
  fn rejects_a_bad_specifier_and_an_unclosed_marker() {
    assert!(replacements("```origins\nlet a = [[a:1;\n```\n", 1).is_err());
    assert!(replacements("```origins\nlet a = 1;\n", 1).is_err());
    // The specifiers are gone; a block still carrying one says so.
    let err = replacements("```origins,generic\nfn f() {}\n```\n", 1)
      .unwrap_err()
      .to_string();
    assert!(err.contains("takes no specifiers"), "{err}");
    assert!(err.contains("'?a"), "{err}");
  }
}
