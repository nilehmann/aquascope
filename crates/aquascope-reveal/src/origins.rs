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

/// What a block asks for beyond being rendered.
#[derive(Debug, Default, PartialEq)]
pub struct Options {
  /// Give the block a Run button.
  run: bool,
  /// The block is expected not to compile, and says so on the slide.
  should_fail: bool,
  /// The block is not a program: a bare signature, or a body elided to
  /// `{ ... }`. Not compiled at all.
  notation: bool,
}

impl Options {
  fn parse(spec: &str) -> Result<Options> {
    let mut opts = Options::default();
    for entry in spec.split(',').map(str::trim).filter(|e| !e.is_empty()) {
      match entry {
        "run" => opts.run = true,
        "shouldFail" => opts.should_fail = true,
        "notation" => opts.notation = true,
        _ => bail!(
          "unknown ```origins specifier `{entry}`, expected one of run, \
           shouldFail, notation"
        ),
      }
    }
    if opts.notation && (opts.run || opts.should_fail) {
      bail!(
        "`notation` says the block is not a program, so it cannot also be \
         `run` or `shouldFail`"
      );
    }
    Ok(opts)
  }

  /// Whether the block is compiled at build time. Checking is the default: a
  /// block that is really code should be able to prove it.
  fn checked(&self) -> bool {
    !self.notation
  }
}

/// A block's compilable form, for the build-time check.
#[derive(Debug)]
pub struct Program {
  /// Line the fence opened on, for diagnostics.
  pub line: usize,
  pub code: String,
  pub should_fail: bool,
}

/// A `<span>` to wrap around a byte range of the code.
#[derive(Debug)]
struct Span {
  range: Range<usize>,
  class: String,
  /// Marker boxes sort outside token spans covering the same range, so that a
  /// box framing exactly one token frames the highlighting rather than
  /// splitting it.
  is_marker: bool,
}

/// Splits a block body into the lines that are shown and the whole program.
///
/// A line whose first non-blank characters are `# ` is compiled but not
/// displayed, which is how mdBook and Rust By Example carry the context a
/// snippet needs without putting it on the page: a `use`, a helper the slide
/// is not about, the `fn main` around a fragment. `##` at the start of a line
/// is an escaped `#`, for the rare block that means to show one.
fn split_hidden(body: &str) -> (String, String) {
  let (mut shown, mut all) = (Vec::new(), Vec::new());
  for line in body.split('\n') {
    let indent = &line[.. line.len() - line.trim_start().len()];
    let rest = line.trim_start();
    if let Some(code) = rest
      .strip_prefix("# ")
      .or(rest.strip_prefix("#").filter(|r| r.is_empty()))
    {
      all.push(format!("{indent}{code}"));
    } else if let Some(code) = rest.strip_prefix("##") {
      shown.push(format!("{indent}#{code}"));
      all.push(format!("{indent}#{code}"));
    } else {
      shown.push(line.to_string());
      all.push(line.to_string());
    }
  }
  (shown.join("\n"), all.join("\n"))
}

/// The Rust a block stands for: markers taken out, and the notation in them
/// turned back into the code it annotates.
///
/// The sigils already say which lifetimes are annotation and which are real
/// Rust, which is the whole reason this translation is possible. A concrete
/// origin is the lifetime the compiler infers, so `'!a` becomes `'_`; a
/// generic parameter is a parameter, so `'?a` becomes `'a`.
fn program(all: &str) -> String {
  let mut out = String::new();
  let bytes = all.as_bytes();
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i ..].starts_with(b"[[")
      && i + 4 <= bytes.len()
      && bytes[i + 3] == b':'
      && (bytes[i + 2].is_ascii_lowercase()
        || bytes[i + 2] == b'*'
        || bytes[i + 2] == b'?')
    {
      i += 4;
      continue;
    }
    if bytes[i ..].starts_with(b":]]") {
      i += 3;
      continue;
    }
    if bytes[i] == b'\''
      && matches!(bytes.get(i + 1), Some(b'?' | b'!'))
      && bytes
        .get(i + 2)
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
    {
      let concrete = bytes[i + 1] == b'!';
      i += 2;
      let start = i;
      while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
      {
        i += 1;
      }
      // A concrete origin is not nameable in Rust where the notation writes
      // it -- inside a `let`, say -- and does not need to be: it is exactly
      // the lifetime inference would pick.
      out.push_str(if concrete { "'_" } else { "'" });
      if !concrete {
        out.push_str(&all[start .. i]);
      }
      continue;
    }
    let ch = all[i ..].chars().next().unwrap();
    out.push(ch);
    i += ch.len_utf8();
  }
  out
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
pub fn render(src: &str, opts: &Options) -> Result<String> {
  // The block's last line ends in a newline that belongs to the closing fence,
  // not to the code. Exactly that one comes off, so the rendered block has the
  // lines the author wrote -- blank ones at either end included.
  let src = src.strip_suffix('\n').unwrap_or(src);
  let src = src.strip_suffix('\r').unwrap_or(src);
  let (shown, _) = split_hidden(src);
  let (code, mut markers) = strip(&shown)?;

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

  // The crab for a block that says it does not compile, the same markup
  // `aquascope-embed` emits. It goes in the wrapper rather than in the `pre`:
  // the container is positioned absolutely, and `pre.code` carries `.hljs`,
  // whose `overflow-x: auto` makes it a scroll box that clips its
  // absolutely-positioned children and swallows their clicks. The wrapper also
  // keeps the crab out of the code's font size, so `4.5em` measures the same
  // here as it does inside `.aquascope`.
  //
  // Deliberately not the `does_not_compile` class that mdBook's `ferris.js`
  // looks for. That script inserts its own container as a *sibling* of the
  // block, which is what put the crab outside it -- and finding the class here
  // too would then give a block two crabs.
  let crab = if opts.should_fail {
    r#"<div class="ferris-container"><img src="img/ferris/does_not_compile.svg" title="This code does not compile!" class="ferris ferris-large" /></div>"#
  } else {
    ""
  };

  // The program is not what is on the slide -- hidden lines are missing from
  // it and the markers are still in it -- so a runnable block carries its own
  // source. Newlines are escaped along with everything else: an attribute
  // spanning lines would put a blank line inside the raw-HTML block, which is
  // the one thing that breaks it.
  let run = if opts.run {
    let (_, all) = split_hidden(src);
    format!(" data-run-code=\"{}\"", attribute(&program(&all)))
  } else {
    String::new()
  };

  // The wrapper is what `.aquascope` is to an editor: it carries the frame,
  // it is the positioned element the crab and the Run button hang off, and it
  // is what the run output is appended to -- so the output lands inside the
  // block's border rather than under it.
  Ok(format!(
    r#"<div class="origins-block"{run}>{crab}<pre class="code hljs">{body}</pre></div>"#
  ))
}

/// Escapes a string for use as an HTML attribute value, newlines included.
fn attribute(text: &str) -> String {
  let mut out = String::new();
  for ch in text.chars() {
    match ch {
      '&' => out.push_str("&amp;"),
      '<' => out.push_str("&lt;"),
      '>' => out.push_str("&gt;"),
      '"' => out.push_str("&quot;"),
      '\n' => out.push_str("&#10;"),
      '\r' => out.push_str("&#13;"),
      _ => out.push(ch),
    }
  }
  out
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
) -> Result<(Vec<Replacement>, Vec<Program>)> {
  /// The backtick count of a fence line, and whatever follows it.
  fn fence(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let ticks = trimmed.chars().take_while(|c| *c == '`').count();
    // Backticks are one byte, so slicing by their count is safe.
    (ticks >= 3).then(|| (ticks, &trimmed[ticks ..]))
  }

  let mut out = Vec::new();
  let mut programs = Vec::new();
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

    let line = first_line + n;
    let opts = Options::parse(spec)
      .with_context(|| format!("in the ```origins block at line {line}"))?;

    let html = render(&body, &opts)
      .with_context(|| format!("in the ```origins block at line {line}"))?;
    out.push((start .. end, html));

    if opts.checked() {
      let (_, all) = split_hidden(body.strip_suffix('\n').unwrap_or(&body));
      programs.push(Program {
        line,
        code: program(&all),
        should_fail: opts.should_fail,
      });
    }
  }

  Ok((out, programs))
}

/// Compiles every checked block, reporting what disagreed with its fence.
///
/// Checking is the default because a block that is really Rust should be able
/// to prove it: a typo on a slide is otherwise found in the lecture. The two
/// escapes say which kind of not-compiling a block means -- `shouldFail` for a
/// block whose error is the point, `notation` for one that is not a program.
pub fn check(programs: &[Program]) -> Vec<String> {
  let mut problems = Vec::new();
  for p in programs {
    match (crate::run::check(&p.code), p.should_fail) {
      (Ok(()), false) | (Err(_), true) => {}
      (Ok(()), true) => problems.push(format!(
        "{}: the ```origins block is marked shouldFail but compiles",
        p.line
      )),
      (Err(e), false) => problems.push(format!(
        "{}: the ```origins block does not compile. Mark it shouldFail if \
         that is the point of the slide, or notation if it is not a \
         program.\n{}",
        p.line,
        e.lines()
          .map(|l| format!("        {l}"))
          .collect::<Vec<_>>()
          .join("\n")
      )),
    }
  }
  problems
}

#[cfg(test)]
mod test {
  use super::*;

  /// The single rendered block in `md`.
  fn one(md: &str) -> String {
    let (reps, _) = replacements(md, 1).unwrap();
    assert_eq!(reps.len(), 1, "{reps:?}");
    reps.into_iter().next().unwrap().1
  }

  /// The programs `md`'s blocks would be checked as.
  fn programs(md: &str) -> Vec<Program> {
    replacements(md, 1).unwrap().1
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
    assert!(html.contains(r#"<pre class="code hljs">"#), "{html}");
    assert!(!html.contains("<code"), "{html}");
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
    let (reps, _) = replacements(md, 1).unwrap();
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
    assert!(html.ends_with("</pre></div>"), "{html}");
    assert!(!html.contains('`'), "{html}");
  }

  #[test]
  fn leaves_a_nested_example_as_text() {
    // A deck documenting the notation writes an ```origins block inside a
    // longer fence; only real blocks are rendered.
    let md = "````markdown\n```origins\nlet a = [[a:1:]];\n```\n````\n\n```origins\nlet b = 2;\n```\n";
    let (reps, _) = replacements(md, 1).unwrap();
    assert_eq!(reps.len(), 1, "{reps:?}");
    assert_eq!(&md[reps[0].0.clone()], "```origins\nlet b = 2;\n```");
  }

  #[test]
  fn a_hidden_line_is_compiled_but_not_shown() {
    let md = "```origins\n# fn main() {\nlet a = 1;\n# }\n```\n";
    let html = one(md);
    assert!(!html.contains("main"), "{html}");
    assert!(
      html.contains("<span class=\"hljs-number\">1</span>"),
      "{html}"
    );
    assert_eq!(programs(md)[0].code, "fn main() {\nlet a = 1;\n}");
  }

  #[test]
  fn a_doubled_hash_shows_one() {
    let html = one("```origins\n## [derive(Debug)]\nstruct S;\n```\n");
    // The highlighter has been over it, so look for the escaped `#` alone.
    assert!(html.contains("># ["), "{html}");
  }

  #[test]
  fn the_program_is_the_notation_translated_back_into_rust() {
    // A concrete origin is the lifetime inference would pick, so it becomes
    // `'_`; a generic one is a real parameter; a frame is annotation only.
    let md = "```origins\nfn f<'?a>(v: &'?a Vec<i32>) -> &'?a i32 { &[[a:v:]][0] }\nlet r: &'!a i32 = f(&v);\n```\n";
    assert_eq!(
      programs(md)[0].code,
      "fn f<'a>(v: &'a Vec<i32>) -> &'a i32 { &v[0] }\nlet r: &'_ i32 = f(&v);"
    );
  }

  #[test]
  fn run_carries_the_program_in_an_attribute() {
    let html =
      one("```origins,run\n# fn main() {\nlet r: &'!a i32 = &1;\n# }\n```\n");
    // Newlines escaped too: an attribute spanning lines would put a blank
    // line inside the raw-HTML block.
    assert!(
      html.contains(
        r#"data-run-code="fn main() {&#10;let r: &amp;'_ i32 = &amp;1;&#10;}""#
      ),
      "{html}"
    );
    assert!(!html.contains('\n'), "{html:?}");
  }

  #[test]
  fn should_fail_asks_for_the_crab_and_is_not_run() {
    let html = one("```origins,shouldFail\nlet a: i32 = \"s\";\n```\n");
    // In the wrapper, before the `pre`: `.hljs` makes the `pre` a scroll box
    // that would clip it and swallow its clicks.
    assert!(
      html.starts_with(
        r#"<div class="origins-block"><div class="ferris-container">"#
      ),
      "{html}"
    );
    assert!(!html.contains("data-run-code"), "{html}");
    assert!(
      programs("```origins,shouldFail\nlet a = 1;\n```\n")[0].should_fail
    );
  }

  #[test]
  fn notation_is_not_compiled_at_all() {
    assert!(
      programs("```origins,notation\nfn f() -> &'a str\n```\n").is_empty()
    );
    // Every other block is, without asking.
    assert_eq!(programs("```origins\nfn main() {}\n```\n").len(), 1);
  }

  #[test]
  fn notation_cannot_also_run_or_fail() {
    for spec in ["notation,run", "notation,shouldFail"] {
      let md = format!("```origins,{spec}\nfn f() {{}}\n```\n");
      // `{:#}` for the whole chain: the outer layer is only the line number.
      let err = format!("{:#}", replacements(&md, 1).unwrap_err());
      assert!(err.contains("not a program"), "{spec}: {err}");
    }
  }

  #[test]
  fn check_reports_what_disagrees_with_the_fence() {
    let good = Program {
      line: 1,
      code: "fn main() {}".into(),
      should_fail: false,
    };
    let bad = Program {
      line: 2,
      code: "fn main() { let x: i32 = \"s\"; }".into(),
      should_fail: false,
    };
    let expected = Program {
      line: 3,
      code: "fn main() { let x: i32 = \"s\"; }".into(),
      should_fail: true,
    };
    let surprise = Program {
      line: 4,
      code: "fn main() {}".into(),
      should_fail: true,
    };

    assert!(check(&[good, expected]).is_empty());
    let problems = check(&[bad, surprise]);
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(
      problems[0].starts_with("2: ")
        && problems[0].contains("does not compile")
    );
    assert!(problems[1].starts_with("4: ") && problems[1].contains("compiles"));
  }

  #[test]
  fn an_unused_name_is_not_a_failure() {
    // A slide shows what makes its point and nothing else.
    let problems = check(&[Program {
      line: 1,
      code: "fn helper() {}\nfn main() { let x = 1; }".into(),
      should_fail: false,
    }]);
    assert!(problems.is_empty(), "{problems:?}");
  }

  #[test]
  fn ignores_other_fences() {
    assert!(replacements("```rust\nlet a = 1;\n```\n", 1)
      .unwrap()
      .0
      .is_empty());
    assert!(
      replacements("```aquascope,permissions\nfn main() {}\n```\n", 1)
        .unwrap()
        .0
        .is_empty()
    );
  }

  #[test]
  fn rejects_a_bad_specifier_and_an_unclosed_marker() {
    assert!(replacements("```origins\nlet a = [[a:1;\n```\n", 1).is_err());
    assert!(replacements("```origins\nlet a = 1;\n", 1).is_err());
    // The notation modes are gone; only block metadata remains.
    // `{:#}` for the whole chain: the outer layer is only the line number.
    let err = format!(
      "{:#}",
      replacements("```origins,generic\nfn f() {}\n```\n", 1).unwrap_err()
    );
    assert!(
      err.contains("unknown ```origins specifier `generic`"),
      "{err}"
    );
  }
}
