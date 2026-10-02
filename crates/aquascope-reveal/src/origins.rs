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
//! `[[+N:TEXT:]]` is not a box either: it reveals `TEXT` on click `N`, as a
//! reveal fragment. `#N ` in front of a line is the same for the whole line.
//!
//! `[[=:TEXT:]]` puts `TEXT` in focus, fading the rest of the block, and
//! `[[=N:TEXT:]]` does so from click `N`. The focus moves from step to step,
//! and the block returns to full strength one click after the last.
//!
//! `[[^:EXPR:]]` is not a box: it makes `EXPR` show its type on hover. The
//! type is not written in the block but asked of the compiler, by the
//! `types` module, so it cannot be wrong, and a marker that is not exactly
//! one expression fails the build.
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

use crate::{
  overlay::Problem,
  types::{self, Typer},
};

pub type Replacement = (Range<usize>, String);

/// Words highlight.css paints as keywords. Deliberately not every Rust
/// keyword: the list matches what the deck's snippets actually use, and an
/// unknown word simply renders unstyled.
const KEYWORDS: &[&str] = &[
  "let", "mut", "fn", "struct", "impl", "if", "else", "return", "move", "for",
  "in", "while", "loop", "match", "pub", "use", "as", "ref", "const", "static",
  "enum", "trait", "where", "break", "continue", "unsafe", "async", "await",
  "dyn", "extern", "crate", "mod", "self", "Self", "super", "type", "yield",
];

/// Painted as hljs paints them, apart from the keywords.
const LITERALS: &[&str] = &["true", "false"];

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
  /// For a `[[^:…:]]` marker, the type shown on hover, once it is known.
  data_type: Option<String>,
  /// For a `[[+N:…:]]` marker, the step it is revealed on.
  step: Option<u32>,
  /// For a `[[=N:…:]]` marker, the step it is lit on. A `[[=:…:]]` marker is
  /// lit from the start and has none.
  focus: Option<u32>,
}

impl Span {
  /// Which of two markers over the same code is emitted outside the other: a
  /// step outside everything, so a hidden step hides its boxes too; then a
  /// highlight, so a box it covers is lifted out of the fade with its code;
  /// then the boxes.
  fn nesting(&self) -> u8 {
    if self.step.is_some() {
      0
    } else if self.class.starts_with("ohl") {
      1
    } else {
      2
    }
  }
}

/// A block body as shown and as compiled, the way every other pass wants it:
/// `#N` lines expanded into step markers, hidden lines split out, and lines
/// holding nothing but a marker folded into their neighbours.
fn prepare(body: &str) -> Result<(String, String)> {
  let (shown, all) = split_hidden(&expand_steps(body)?);
  Ok((fold_marker_lines(&shown), fold_marker_lines(&all)))
}

/// Whether a line, with its indentation trimmed, is a hidden one. The code it
/// hides follows.
fn hidden(rest: &str) -> Option<&str> {
  rest
    .strip_prefix("# ")
    .or(rest.strip_prefix("#").filter(|r| r.is_empty()))
}

/// `#N code`, with its indentation trimmed: the step, and the code with the
/// one space after the number dropped. `#N` alone reveals an empty line.
fn step_line(rest: &str) -> Option<(u32, &str)> {
  let after = rest.strip_prefix('#')?;
  let n = after.bytes().take_while(u8::is_ascii_digit).count();
  if n == 0 {
    return None;
  }
  let code = &after[n ..];
  let code = if code.is_empty() {
    code
  } else {
    code.strip_prefix(' ')?
  };
  Some((after[.. n].parse().ok()?, code))
}

/// Rewrites every `#N code` line as `[[+N:code:]]`, keeping the indentation
/// in front of the marker, so that a step line is only sugar and nothing past
/// this point needs to know about it.
///
/// The marker goes after the indentation rather than in the first column,
/// which is the same rule as `# ` for a hidden line: the characters of the
/// marker never count as indentation, so a line at the top level can be a
/// step too. `#` then a digit is never Rust at the start of a line -- an
/// attribute is `#[` or `#![` -- and `##1` is still the escaped `#1`.
fn expand_steps(body: &str) -> Result<String> {
  let mut out = Vec::new();
  for line in body.split('\n') {
    let (line, cr) = match line.strip_suffix('\r') {
      Some(line) => (line, "\r"),
      None => (line, ""),
    };
    let indent = &line[.. line.len() - line.trim_start().len()];
    let rest = line.trim_start();

    if let Some(code) = hidden(rest) {
      if step_line(code.trim_start()).is_some() || has_timed_marker(code) {
        bail!("a step or highlight marker on a hidden line would show nothing");
      }
      out.push(format!("{line}{cr}"));
      continue;
    }

    match step_line(rest) {
      Some((step, code)) => {
        if !balanced(code) {
          bail!(
            "the `#{step}` line opens or closes a marker it does not also \
             close or open. Wrap several lines in `[[+{step}:` … `:]]` \
             instead"
          );
        }
        out.push(format!("{indent}[[+{step}:{code}:]]{cr}"));
      }
      None => out.push(format!("{line}{cr}")),
    }
  }
  Ok(out.join("\n"))
}

/// Whether `text` holds a `[[+N:` or a `[[=…:` marker, neither of which
/// means anything on a line that is not shown.
fn has_timed_marker(text: &str) -> bool {
  (0 .. text.len()).any(|i| {
    matches!(
      opener(&text.as_bytes()[i ..]),
      Some((Opener::Step(_) | Opener::Focus(_), _))
    )
  })
}

/// Whether every marker `text` opens it also closes, and the other way round.
fn balanced(text: &str) -> bool {
  let bytes = text.as_bytes();
  let (mut depth, mut i) = (0usize, 0);
  while i < bytes.len() {
    if let Some((_, len)) = opener(&bytes[i ..]) {
      depth += 1;
      i += len;
    } else if bytes[i ..].starts_with(b":]]") {
      let Some(d) = depth.checked_sub(1) else {
        return false;
      };
      depth = d;
      i += 3;
    } else {
      i += 1;
    }
  }
  depth == 0
}

/// Folds a line holding nothing but opening markers into the start of the
/// line after it, and one holding nothing but `:]]` into the end of the line
/// before it.
///
/// This is what lets a marker wrap whole lines -- a function revealed on one
/// step, an origin box around a struct -- without the lines the markers sit
/// on turning into blank lines inside it. An opening marker lands after the
/// next line's indentation, so a box starts at the code rather than at the
/// margin.
fn fold_marker_lines(text: &str) -> String {
  fn only_openers(mut t: &str) -> bool {
    while let Some((_, len)) = opener(t.as_bytes()) {
      t = &t[len ..];
    }
    t.is_empty()
  }
  fn only_closers(t: &str) -> bool {
    t.split(":]]").all(str::is_empty)
  }

  let all: Vec<&str> = text.split('\n').collect();
  let mut out: Vec<String> = Vec::new();
  let mut pending = String::new();

  for (n, line) in all.iter().enumerate() {
    let t = line.trim();
    if !t.is_empty() && only_openers(t) && n + 1 < all.len() {
      pending.push_str(t);
      continue;
    }
    if !t.is_empty() && only_closers(t) {
      if !pending.is_empty() {
        pending.push_str(t);
        continue;
      }
      if let Some(prev) = out.last_mut() {
        let at = prev.strip_suffix('\r').unwrap_or(prev).len();
        prev.insert_str(at, t);
        continue;
      }
    }
    let at = line.len() - line.trim_start().len();
    out.push(format!(
      "{}{}{}",
      &line[.. at],
      std::mem::take(&mut pending),
      &line[at ..]
    ));
  }
  if !pending.is_empty() {
    out.push(pending);
  }
  out.join("\n")
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
    if let Some(code) = hidden(rest) {
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
  program_and_types(all).0
}

/// [`program`], along with the byte range in it of every `[[^:…:]]` marker,
/// in the order [`strip`] reports markers -- by where they close -- so the two
/// can be paired up one to one.
fn program_and_types(all: &str) -> (String, Vec<Range<usize>>) {
  let mut out = String::new();
  let mut open: Vec<(Opener, usize)> = Vec::new();
  let mut types = Vec::new();
  let bytes = all.as_bytes();
  let mut i = 0;
  while i < bytes.len() {
    if let Some((kind, len)) = opener(&bytes[i ..]) {
      open.push((kind, out.len()));
      i += len;
      continue;
    }
    if bytes[i ..].starts_with(b":]]") {
      if let Some((Opener::Frame(b'^'), start)) = open.pop() {
        types.push(start .. out.len());
      }
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
  (out, types)
}

/// The characters that can follow `[[` to open a frame: an origin letter,
/// `*` for the dashed box, `?` for the neutral one, `^` for a type on hover.
fn is_frame_key(key: u8) -> bool {
  key.is_ascii_lowercase() || matches!(key, b'*' | b'?' | b'^')
}

/// What a `[[…:` marker opens.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Opener {
  /// A frame, by its key: see [`is_frame_key`].
  Frame(u8),
  /// `[[+N:`, code revealed on step N.
  Step(u32),
  /// `[[=:`, code highlighted from the start, or `[[=N:`, highlighted on
  /// step N.
  Focus(Option<u32>),
}

/// The marker `bytes` starts with, and its length. Anything else starting with
/// `[[` is ordinary code, such as a nested index.
fn opener(bytes: &[u8]) -> Option<(Opener, usize)> {
  let rest = bytes.strip_prefix(b"[[")?;
  if let Some(digits) = rest.strip_prefix(b"+") {
    let n = digits.iter().take_while(|c| c.is_ascii_digit()).count();
    if n == 0 || digits.get(n) != Some(&b':') {
      return None;
    }
    let step = std::str::from_utf8(&digits[.. n]).ok()?.parse().ok()?;
    return Some((Opener::Step(step), n + 4));
  }
  if let Some(digits) = rest.strip_prefix(b"=") {
    let n = digits.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits.get(n) != Some(&b':') {
      return None;
    }
    let step = match n {
      0 => None,
      _ => Some(std::str::from_utf8(&digits[.. n]).ok()?.parse().ok()?),
    };
    return Some((Opener::Focus(step), n + 4));
  }
  match rest {
    [key, b':', ..] if is_frame_key(*key) => Some((Opener::Frame(*key), 4)),
    _ => None,
  }
}

/// The class a frame key asks for. `*` is the dashed box, `?` a box with no
/// origin colour, `^` a type shown on hover, and a letter that origin's
/// colour.
fn frame_class(key: char) -> String {
  match key {
    '*' => "oexact".to_string(),
    '^' => "ty".to_string(),
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
    // `[[x:` opens a frame, `[[+N:` a step, `[[=:` a highlight.
    if let Some((kind, len)) = opener(&bytes[i ..]) {
      let step = match kind {
        Opener::Frame(key) => {
          open.push((code.len(), frame_class(key as char), None, None));
          i += len;
          continue;
        }
        Opener::Focus(focus) => {
          if focus == Some(0) {
            bail!(
              "steps count from 1, the first click: `[[=0:` is never lit. \
               `[[=:` is lit from the start"
            );
          }
          // Lit on a step before its code is shown, a highlight would light
          // nothing.
          let outer = open.iter().filter_map(|o| o.2).max();
          if let (Some(n), Some(outer)) = (focus, outer) {
            if n < outer {
              bail!(
                "the highlight on step {n} is inside step {outer}, so its \
                 code is not shown until after it is lit"
              );
            }
          }
          // A timed highlight is a reveal fragment, which is what gives it a
          // click of its own -- `custom`, so reveal does not also hide it.
          let class = match focus {
            Some(_) => "ohl fragment custom",
            None => "ohl on",
          };
          open.push((code.len(), class.to_string(), None, focus));
          i += len;
          continue;
        }
        Opener::Step(step) => step,
      };
      if step == 0 {
        bail!("steps count from 1, the first click: `[[+0:` is never shown");
      }
      // A step inside another is hidden until the outer one shows, so one
      // numbered lower would appear on a click where nothing does.
      if let Some(outer) = open.iter().filter_map(|o| o.2).max() {
        if step < outer {
          bail!(
            "step {step} is inside step {outer}, so it could not show before \
             step {outer} does"
          );
        }
      }
      open.push((code.len(), "fragment".to_string(), Some(step), None));
      i += len;
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
        data_type: None,
        step: None,
        focus: None,
      });
      continue;
    }

    if bytes[i ..].starts_with(b":]]") {
      let (start, class, step, focus) = open
        .pop()
        .context("`:]]` with no matching `[[` origin marker")?;
      spans.push(Span {
        range: start .. code.len(),
        class,
        is_marker: true,
        data_type: None,
        step,
        focus,
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
            data_type: None,
            step: None,
            focus: None,
          });
          continue;
        }
        let after_fn = prev.is_some_and(|(r, k)| {
          *k == TokenKind::Ident && &code[r.clone()] == "fn"
        });
        // `auto` is a keyword only in `auto trait`; anywhere else it is a
        // name like any other.
        let auto_trait = text == "auto"
          && toks[n + 1 ..]
            .iter()
            .find(significant)
            .is_some_and(|(r, k)| *k == TokenKind::Ident && &code[r.clone()] == "trait");
        if KEYWORDS.contains(&text) || auto_trait {
          "hljs-keyword".to_string()
        } else if LITERALS.contains(&text) {
          "hljs-literal".to_string()
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
      data_type: None,
      step: None,
      focus: None,
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
          data_type: None,
          step: None,
          focus: None,
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

/// A callout for a block's highlights, written after the fence as
/// `[=N]: text` for the highlights lit on step N, or `[=]: text` for the
/// `[[=:…:]]` ones lit from the start.
#[derive(Debug)]
pub struct Note {
  step: Option<u32>,
  /// Markdown, inline only.
  text: String,
  /// Line in the file, for diagnostics.
  line: usize,
}

/// `[=N]: text`, if `line` is one. `None` for any other line, which ends the
/// notes after a fence; an error for one that is a note but a malformed one.
fn note(line: &str) -> Option<Result<(Option<u32>, String)>> {
  let rest = line.trim().strip_prefix("[=")?;
  let (key, text) = rest.split_once("]:")?;
  if !key.bytes().all(|c| c.is_ascii_digit()) {
    return None;
  }
  Some((|| {
    let step = match key {
      "" => None,
      n => Some(n.parse::<u32>()?),
    };
    if step == Some(0) {
      bail!("steps count from 1, the first click: `[=0]` is never shown");
    }
    let text = text.trim();
    if text.is_empty() {
      bail!("`[={key}]` has no text");
    }
    Ok((step, text.to_string()))
  })())
}

/// A note's markdown as inline HTML: the paragraph pulldown-cmark wraps it
/// in comes off, and so does any newline, which inside the raw-HTML block
/// the block is spliced in as could end it.
fn note_html(text: &str) -> String {
  let mut html = String::new();
  pulldown_cmark::html::push_html(&mut html, pulldown_cmark::Parser::new(text));
  let html = html.trim();
  let html = html.strip_prefix("<p>").unwrap_or(html);
  let html = html.strip_suffix("</p>").unwrap_or(html);
  html.replace('\n', " ")
}

/// The caption strip under a block: every note, stacked in one grid cell so
/// the strip is as tall as the tallest of them from the first frame, and
/// showing a step's note changes nothing below the block.
///
/// A step's note need not have a highlight on that step: it is then shown on
/// its own, a sentence about the code with nothing in it singled out. Only
/// `[=]` is checked against the highlights, since it means "the ones lit from
/// the start" and is shown with nothing else.
fn notes_html(notes: &[Note], markers: &[Span]) -> Result<String> {
  if notes.is_empty() {
    return Ok(String::new());
  }
  let lit_from_start = markers.iter().any(|m| m.class == "ohl on");

  let mut out = String::from(r#"<div class="ohl-notes">"#);
  for (i, note) in notes.iter().enumerate() {
    let at = |message: String| anyhow::anyhow!("{}: {message}", note.line);
    if notes[.. i].iter().any(|n| n.step == note.step) {
      let key = note.step.map(|n| n.to_string()).unwrap_or_default();
      return Err(at(format!("`[={key}]` is defined twice")));
    }
    match note.step {
      None if !lit_from_start => {
        return Err(at(
          "`[=]` explains the highlights lit from the start, but the block \
           has no `[[=:` highlight"
            .to_string(),
        ))
      }
      _ => {}
    }
    // A step's note is a reveal fragment on that step, as its highlights
    // are, so the two are renumbered together; `custom` keeps reveal from
    // hiding it, which aquascope-reveal.js does instead.
    match note.step {
      Some(n) => out.push_str(&format!(
        r#"<div class="ohl-note fragment custom" data-fragment-index="{n}">{}</div>"#,
        note_html(&note.text)
      )),
      None => out.push_str(&format!(
        r#"<div class="ohl-note">{}</div>"#,
        note_html(&note.text)
      )),
    }
  }
  out.push_str("</div>");
  Ok(out)
}

/// Renders one block's worth of marked-up Rust. `typer` is asked for the
/// block's types only if it has a `[[^:…:]]` marker.
pub fn render(
  src: &str,
  opts: &Options,
  notes: &[Note],
  typer: &dyn Typer,
) -> Result<String> {
  // The block's last line ends in a newline that belongs to the closing fence,
  // not to the code. Exactly that one comes off, so the rendered block has the
  // lines the author wrote -- blank ones at either end included.
  let src = src.strip_suffix('\n').unwrap_or(src);
  let src = src.strip_suffix('\r').unwrap_or(src);
  let (shown, all) = prepare(src)?;
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

  fill_types(&all, opts, &mut markers, typer)?;

  // `.oname` carries the colour and the weight, so a boxed lifetime must not
  // also carry its token class: the inner span's colour would win over the
  // origin's.
  let tokens = highlight(&code)
    .into_iter()
    .filter(|span| span.class != "hljs-symbol" || !boxed.contains(&span.range))
    .collect();

  // The step after the last timed highlight or note, on which the block goes
  // back to full strength and its strip empties. Taken before the markers are
  // moved into `spans`.
  let unlit = markers
    .iter()
    .filter_map(|m| m.focus)
    .chain(notes.iter().filter_map(|n| n.step))
    .max()
    .map(|n| n + 1);
  let notes = notes_html(notes, &markers)?;

  let mut spans = split_at_markers(tokens, &markers);
  spans.extend(markers);
  // Opened outermost-first at each offset, a marker outside a token span
  // covering the same range, and a step outside a box covering the same range
  // -- otherwise the box would stay on the slide around the hidden code. See
  // `Span::nesting` for the order among markers.
  spans.sort_by_key(|s| {
    (s.range.start, Reverse(s.range.end), !s.is_marker, s.nesting())
  });

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
    match &span.data_type {
      Some(ty) => out.push_str(&format!(
        r#"<span class="{}" data-type="{}""#,
        span.class,
        attribute(ty)
      )),
      None => out.push_str(&format!(r#"<span class="{}""#, span.class)),
    }
    if let Some(step) = span.step.or(span.focus) {
      out.push_str(&format!(r#" data-fragment-index="{step}""#));
    }
    out.push('>');
    stack.push(span.range.end);
  }
  while let Some(end) = stack.pop() {
    push_escaped(&mut out, &code[at .. end]);
    at = end;
    out.push_str("</span>");
  }
  push_escaped(&mut out, &code[at ..]);

  // Nothing on the slide may have a click after the last highlight, so the
  // block makes one: an empty fragment that only marks the step.
  if let Some(step) = unlit {
    out.push_str(&format!(
      r#"<span class="ohl-end fragment custom" data-fragment-index="{step}"></span>"#
    ));
  }

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
    format!(" data-run-code=\"{}\"", attribute(&program(&all)))
  } else {
    String::new()
  };

  // The wrapper is what `.aquascope` is to an editor: it carries the frame,
  // it is the positioned element the crab and the Run button hang off, and it
  // is what the run output is appended to -- so the output lands inside the
  // block's border rather than under it.
  Ok(format!(
    r#"<div class="origins-block"{run}>{crab}<pre class="code hljs">{body}</pre>{notes}</div>"#
  ))
}

/// Asks `typer` for the type of every `[[^:…:]]` marker among `markers`, and
/// records it on the marker.
///
/// The markers were found in the code as shown, the types are reported
/// against the program as compiled -- hidden lines put back, notation
/// translated -- so each marker is located again in the program, and the two
/// lists paired in order.
fn fill_types(
  all: &str,
  opts: &Options,
  markers: &mut [Span],
  typer: &dyn Typer,
) -> Result<()> {
  let mut typed: Vec<&mut Span> =
    markers.iter_mut().filter(|m| m.class == "ty").collect();
  if typed.is_empty() {
    return Ok(());
  }
  if opts.notation {
    bail!(
      "a `[[^:…:]]` marker asks the compiler for a type, and a `notation` \
       block is not compiled"
    );
  }

  let (program, ranges) = program_and_types(all);
  if ranges.len() != typed.len() {
    bail!("a `[[^:…:]]` marker on a hidden line would show nothing");
  }
  let found = typer.expr_types(&program, opts.should_fail)?;
  for (marker, range) in typed.iter_mut().zip(ranges) {
    marker.data_type = Some(types::type_at(&program, &found, range)?);
  }
  Ok(())
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
  typer: &dyn Typer,
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
    let Some(mut end) = end else {
      bail!("{}: unclosed ```origins fence", first_line + n);
    };

    // The block's notes follow the fence, blank lines allowed between them,
    // and are spliced away with it. Scanning ahead on a copy is what lets a
    // blank line after the last note stay where it was.
    let mut notes = Vec::new();
    let mut scan = lines.clone();
    let mut scanned = offset;
    while let Some((m, next)) = scan.next() {
      scanned += next.len();
      if next.trim().is_empty() {
        continue;
      }
      let Some(parsed) = note(next) else {
        break;
      };
      let (step, text) =
        parsed.with_context(|| format!("{}: in a callout", first_line + m))?;
      notes.push(Note {
        step,
        text,
        line: first_line + m,
      });
      lines = scan.clone();
      offset = scanned;
      end = scanned - (next.len() - next.trim_end_matches(['\r', '\n']).len());
    }

    let line = first_line + n;
    let opts = Options::parse(spec)
      .with_context(|| format!("in the ```origins block at line {line}"))?;

    let html = render(&body, &opts, &notes, typer)
      .with_context(|| format!("in the ```origins block at line {line}"))?;
    out.push((start .. end, html));

    if opts.checked() {
      let (_, all) = prepare(body.strip_suffix('\n').unwrap_or(&body))?;
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
pub fn check(programs: &[Program], file: &str) -> Vec<Problem> {
  let mut problems = Vec::new();
  for p in programs {
    let problem = |message: &str, diagnostic| Problem {
      location: Some(format!("{file}:{}", p.line)),
      message: message.to_string(),
      diagnostic,
      details: None,
    };
    match (crate::run::check(&p.code), p.should_fail) {
      (Ok(()), false) | (Err(_), true) => {}
      (Ok(()), true) => problems.push(problem(
        "the ```origins block is marked shouldFail but compiles",
        None,
      )),
      (Err(e), false) => problems.push(problem(
        "the ```origins block does not compile. Mark it shouldFail if that \
         is the point of the slide, or notation if it is not a program.",
        Some(e),
      )),
    }
  }
  problems
}

#[cfg(test)]
mod test {
  use super::*;
  use crate::types::ExprType;

  /// For blocks with no `[[^:…:]]` marker, which never ask.
  struct NoTypes;
  impl Typer for NoTypes {
    fn expr_types(&self, _: &str, _: bool) -> Result<Vec<ExprType>> {
      panic!("a block with no type marker asked for types")
    }
  }

  /// Types by expression text: every occurrence of each text in the program
  /// is reported with its type, the way the driver reports every expression.
  struct Fixed(&'static [(&'static str, &'static str)]);
  impl Typer for Fixed {
    fn expr_types(&self, program: &str, _: bool) -> Result<Vec<ExprType>> {
      let mut out = Vec::new();
      for (text, ty) in self.0 {
        for (start, _) in program.match_indices(text) {
          out.push(ExprType {
            start,
            end: start + text.len(),
            ty: ty.to_string(),
            decl: None,
          });
        }
      }
      Ok(out)
    }
  }

  fn typed(md: &str, types: &'static [(&'static str, &'static str)]) -> String {
    let (reps, _) = replacements(md, 1, &Fixed(types)).unwrap();
    reps.into_iter().next().unwrap().1
  }

  #[test]
  fn type_markers_carry_the_compilers_type() {
    let html = typed("```origins\nlet x = [[^:*[[^:b:]]:]] + 1;\n```\n", &[
      ("*b", "i32"),
      ("b", "MyBox<i32>"),
    ]);
    assert!(
      html.contains(
        r#"<span class="ty" data-type="i32">*<span class="ty" data-type="MyBox&lt;i32&gt;">b</span></span>"#
      ),
      "{html}"
    );
  }

  #[test]
  fn type_markers_are_found_past_hidden_lines_and_notation() {
    // The hidden line and the `'!a`, which compiles as the shorter `'_`, both
    // move the expression in the program away from where it is on the slide.
    let html = typed(
      "```origins\n# let v = vec![1];\nlet r: &'!a Vec<i32> = [[^:&v:]];\n```\n",
      &[("&v", "&Vec<i32>")],
    );
    assert!(
      html.contains(r#"<span class="ty" data-type="&amp;Vec&lt;i32&gt;">"#),
      "{html}"
    );
  }

  #[test]
  fn type_markers_are_erased_from_the_program() {
    let md = "```origins\nfn main() { let x = [[^:1:]]; }\n```\n";
    let (_, programs) = replacements(md, 1, &Fixed(&[("1", "i32")])).unwrap();
    assert_eq!(programs[0].code, "fn main() { let x = 1; }");
  }

  #[test]
  fn a_type_marker_needs_a_compiled_block() {
    let md = "```origins,notation\nfn f() -> [[^:T:]];\n```\n";
    let err = format!("{:#}", replacements(md, 1, &Fixed(&[])).unwrap_err());
    assert!(err.contains("`notation` block is not compiled"), "{err}");
  }

  #[test]
  fn a_type_marker_must_cover_an_expression() {
    let md = "```origins\nlet y = [[^:b.deref:]]();\n```\n";
    let err = format!(
      "{:#}",
      replacements(md, 1, &Fixed(&[("b.deref()", "&i32")])).unwrap_err()
    );
    assert!(err.contains("`b.deref` is not an expression"), "{err}");
  }

  /// The single rendered block in `md`.
  fn one(md: &str) -> String {
    let (reps, _) = replacements(md, 1, &NoTypes).unwrap();
    assert_eq!(reps.len(), 1, "{reps:?}");
    reps.into_iter().next().unwrap().1
  }

  /// The programs `md`'s blocks would be checked as.
  fn programs(md: &str) -> Vec<Program> {
    replacements(md, 1, &NoTypes).unwrap().1
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
    let (reps, _) = replacements(md, 1, &NoTypes).unwrap();
    assert_eq!(reps.len(), 2);
    assert_eq!(&md[reps[0].0.clone()], "```origins\nlet a = 1;\n```");
    assert_eq!(&md[reps[1].0.clone()], "```origins\nfn f<'?a>() {}\n```");
  }

  #[test]
  fn names_the_line_in_the_file_rather_than_in_the_body() {
    let err = replacements("```origins\nlet a = [[a:1;\n```\n", 4, &NoTypes)
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
    let (reps, _) = replacements(md, 1, &NoTypes).unwrap();
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
      let err = format!("{:#}", replacements(&md, 1, &NoTypes).unwrap_err());
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

    assert!(check(&[good, expected], "d.md").is_empty());
    let problems = check(&[bad, surprise], "d.md");
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert_eq!(problems[0].location.as_deref(), Some("d.md:2"));
    assert!(problems[0].message.contains("does not compile"));
    assert!(problems[0]
      .diagnostic
      .as_deref()
      .is_some_and(|d| d.contains("mismatched types")));
    assert_eq!(problems[1].location.as_deref(), Some("d.md:4"));
    assert!(problems[1].message.contains("compiles"));
  }

  #[test]
  fn an_unused_name_is_not_a_failure() {
    // A slide shows what makes its point and nothing else.
    let problems = check(&[Program {
      line: 1,
      code: "fn helper() {}\nfn main() { let x = 1; }".into(),
      should_fail: false,
    }], "d.md");
    assert!(problems.is_empty(), "{problems:?}");
  }

  #[test]
  fn ignores_other_fences() {
    assert!(replacements("```rust\nlet a = 1;\n```\n", 1, &NoTypes)
      .unwrap()
      .0
      .is_empty());
    assert!(replacements(
      "```aquascope,permissions\nfn main() {}\n```\n",
      1,
      &NoTypes
    )
    .unwrap()
    .0
    .is_empty());
  }

  #[test]
  fn rejects_a_bad_specifier_and_an_unclosed_marker() {
    assert!(
      replacements("```origins\nlet a = [[a:1;\n```\n", 1, &NoTypes).is_err()
    );
    assert!(replacements("```origins\nlet a = 1;\n", 1, &NoTypes).is_err());
    // The notation modes are gone; only block metadata remains.
    // `{:#}` for the whole chain: the outer layer is only the line number.
    let err = format!(
      "{:#}",
      replacements("```origins,generic\nfn f() {}\n```\n", 1, &NoTypes)
        .unwrap_err()
    );
    assert!(
      err.contains("unknown ```origins specifier `generic`"),
      "{err}"
    );
  }

  /// The `<pre>`'s contents, with the highlighting spans taken out so a test
  /// can read the code and the markers it cares about.
  fn pre(html: &str) -> String {
    let body = html.split("<pre class=\"code hljs\">").nth(1).unwrap();
    let body = body.split("</pre>").next().unwrap();
    let mut out = String::new();
    let mut rest = body;
    while let Some(at) = rest.find("<span class=\"hljs-") {
      out.push_str(&rest[.. at]);
      let after = &rest[at ..];
      rest = &after[after.find('>').unwrap() + 1 ..];
      // Token spans hold no markup, so the next close is their own.
      let close = rest.find("</span>").unwrap();
      out.push_str(&rest[.. close]);
      rest = &rest[close + "</span>".len() ..];
    }
    out.push_str(rest);
    out
  }

  fn err(md: &str) -> String {
    format!("{:#}", replacements(md, 1, &NoTypes).unwrap_err())
  }

  #[test]
  fn a_step_line_keeps_its_indentation() {
    let html = one(
      "```origins\nfn main() {\n    let v = 1;\n    #1 let r = &v;\n}\n```\n",
    );
    assert!(
      pre(&html).contains(
        "    <span class=\"fragment\" data-fragment-index=\"1\">let r = &amp;v;</span>\n"
      ),
      "{html}"
    );
  }

  #[test]
  fn a_step_line_at_the_top_level_is_not_indented() {
    let html = one("```origins\n#2 struct S;\n```\n");
    assert!(
      pre(&html).starts_with(
        "<span class=\"fragment\" data-fragment-index=\"2\">struct S;</span>"
      ),
      "{html}"
    );
  }

  #[test]
  fn a_step_is_compiled_and_run_as_ordinary_code() {
    let md = "```origins,run\nfn main() {\n    #1 let x = 1;\n    [[+2:\n    let y = x;\n    :]]\n}\n```\n";
    let code = &programs(md)[0].code;
    assert_eq!(code, "fn main() {\n    let x = 1;\n    let y = x;\n}");
    assert!(
      one(md).contains(&attribute(code)),
      "Run gets the full program"
    );
  }

  #[test]
  fn marker_lines_fold_into_the_lines_they_wrap() {
    let html =
      one("```origins\n[[+2:\nfn f() {\n    g();\n}\n:]]\nfn g() {}\n```\n");
    let pre = pre(&html);
    assert!(
      pre.starts_with("<span class=\"fragment\" data-fragment-index=\"2\">fn f() {\n    g();\n}</span>\nfn g() {}"),
      "{pre}"
    );
  }

  #[test]
  fn marker_lines_fold_for_origin_boxes_too() {
    let html = one(
      "```origins\nfn main() {\n    [[a:\n    let v = 1;\n    :]]\n}\n```\n",
    );
    let pre = pre(&html);
    assert!(
      pre
        .contains("\n    <span class=\"oframe origin-a\">let v = 1;</span>\n}"),
      "{pre}"
    );
  }

  #[test]
  fn an_inline_step_reveals_part_of_a_line() {
    let html = one(
      "```origins,notation\nfn f<[[+1:'?a:]]>(x: &[[+1:'?a :]]str);\n```\n",
    );
    assert!(
      html.contains("<span class=\"fragment\" data-fragment-index=\"1\"><span class=\"oname\">'a</span></span>"),
      "{html}"
    );
  }

  #[test]
  fn an_escaped_hash_digit_is_not_a_step() {
    let html = one("```origins,notation\n##1 x\n```\n");
    assert!(!html.contains("fragment"), "{html}");
    assert!(pre(&html).starts_with("#1 x"), "{html}");
  }

  #[test]
  fn rejects_steps_that_could_not_show() {
    assert!(err("```origins\n#0 let a = 1;\n```\n").contains("count from 1"));
    assert!(err("```origins\n# #1 fn f() {}\n```\n").contains("hidden line"));
    assert!(
      err("```origins\n# let a = [[+1:1:]];\n```\n").contains("hidden line")
    );
    assert!(
      err("```origins\n[[+2:\nfn f() {\n    #1 g();\n}\n:]]\n```\n")
        .contains("inside step 2")
    );
    assert!(err("```origins\n#1 let a = [[a:vec![\n1]:]];\n```\n")
      .contains("does not also"));
  }

  #[test]
  fn a_static_highlight_is_lit_from_the_start() {
    let html = one("```origins,notation\nlet a = [[=:x + 1:]];\n```\n");
    assert!(pre(&html).contains("<span class=\"ohl on\">x + 1</span>"), "{html}");
    // Nothing timed, so no step to clear it on.
    assert!(!html.contains("ohl-end"), "{html}");
  }

  #[test]
  fn a_timed_highlight_is_a_fragment_reveal_does_not_hide() {
    let html =
      one("```origins,notation\nlet a = [[=1:x:]];\nlet b = [[=3:y:]];\n```\n");
    assert!(
      html.contains(
        "<span class=\"ohl fragment custom\" data-fragment-index=\"1\">x</span>"
      ),
      "{html}"
    );
    // The block clears on the step after its last highlight.
    assert!(
      html.contains(
        "<span class=\"ohl-end fragment custom\" data-fragment-index=\"4\"></span>"
      ),
      "{html}"
    );
  }

  #[test]
  fn a_highlight_lifts_a_box_over_the_same_code_and_stays_inside_a_step() {
    let html =
      one("```origins,notation\nlet a = [[+1:[[=2:[[a:vec![]:]]:]]:]];\n```\n");
    assert!(
      html.contains(
        "<span class=\"fragment\" data-fragment-index=\"1\"><span class=\"ohl \
         fragment custom\" data-fragment-index=\"2\"><span class=\"oframe \
         origin-a\">"
      ),
      "{html}"
    );
  }

  #[test]
  fn a_highlight_wraps_whole_lines_and_is_erased_from_the_program() {
    let md = "```origins\nfn main() {\n    [[=1:\n    let a = 1;\n    let b = a;\n    :]]\n}\n```\n";
    let html = one(md);
    assert!(
      pre(&html).contains(
        "\n    <span class=\"ohl fragment custom\" data-fragment-index=\"1\">\
         let a = 1;\n    let b = a;</span>\n}"
      ),
      "{html}"
    );
    assert_eq!(
      programs(md)[0].code,
      "fn main() {\n    let a = 1;\n    let b = a;\n}"
    );
  }

  #[test]
  fn an_index_with_brackets_is_not_a_highlight() {
    // `x[[=` is not Rust, but `[[` followed by anything else must stay code.
    let html = one("```origins,notation\nlet a = m[[0][0]];\n```\n");
    assert!(!html.contains("ohl"), "{html}");
  }

  #[test]
  fn rejects_highlights_that_could_not_show() {
    assert!(err("```origins\nlet a = [[=0:1:]];\n```\n").contains("count from 1"));
    assert!(
      err("```origins\n# let a = [[=:1:]];\n```\n").contains("hidden line")
    );
    assert!(
      err("```origins\nlet a = [[+3:[[=2:1:]]:]];\n```\n")
        .contains("inside step 3")
    );
  }

  #[test]
  fn notes_after_a_fence_become_its_caption_strip() {
    let md = "```origins,notation\nlet a = [[=1:x:]] + [[=2:y:]];\n```\n[=1]: the `x`\n\n[=2]: the **y**\n\nAfter.\n";
    let (reps, _) = replacements(md, 1, &NoTypes).unwrap();
    let (range, html) = &reps[0];
    assert!(
      html.contains(
        "<div class=\"ohl-notes\"><div class=\"ohl-note fragment custom\" \
         data-fragment-index=\"1\">the <code>x</code></div>"
      ),
      "{html}"
    );
    assert!(
      html.contains("data-fragment-index=\"2\">the <strong>y</strong></div>"),
      "{html}"
    );
    // The notes are spliced away with the fence; the blank line after the
    // last one, and what follows, stay.
    assert_eq!(&md[range.end ..], "\n\nAfter.\n");
  }

  #[test]
  fn a_note_for_the_highlights_lit_from_the_start() {
    let html = one("```origins,notation\nlet a = [[=:x:]];\n```\n[=]: always\n");
    assert!(html.contains("<div class=\"ohl-note\">always</div>"), "{html}");
  }

  #[test]
  fn notes_end_at_the_first_line_that_is_not_one() {
    // A blank line between the fence and the notes is allowed...
    let html = one("```origins,notation\nlet a = [[=1:x:]];\n```\n\n[=1]: a note\n");
    assert!(html.contains("ohl-notes"), "{html}");
    let html = one("```origins,notation\nlet a = [[=1:x:]];\n```\nText.\n[=1]: too late\n");
    // ...but anything else ends them.
    assert!(!html.contains("ohl-notes"), "{html}");
  }

  #[test]
  fn highlights_unsafe_and_auto_trait_but_not_a_variable_named_auto() {
    let html = one(
      "```origins,notation\npub unsafe auto trait Send { }\nlet auto = true;\n```\n",
    );
    assert!(
      html.contains(
        "<span class=\"hljs-keyword\">unsafe</span> \
         <span class=\"hljs-keyword\">auto</span> \
         <span class=\"hljs-keyword\">trait</span>"
      ),
      "{html}"
    );
    assert!(html.contains("<span class=\"hljs-keyword\">let</span> auto = "), "{html}");
    assert!(html.contains("<span class=\"hljs-literal\">true</span>"), "{html}");
  }

  #[test]
  fn a_note_may_stand_on_a_step_without_a_highlight() {
    let html =
      one("```origins,notation\nlet a = [[=1:x:]];\n```\n[=1]: x\n[=3]: all of it\n");
    assert!(
      html.contains("data-fragment-index=\"3\">all of it</div>"),
      "{html}"
    );
    // The block's last click comes after its last note, not its last
    // highlight, so that note is cleared too.
    assert!(
      html.contains(
        "<span class=\"ohl-end fragment custom\" data-fragment-index=\"4\">"
      ),
      "{html}"
    );
  }

  #[test]
  fn rejects_notes_that_explain_nothing() {
    assert!(err("```origins,notation\nlet a = [[=1:x:]];\n```\n[=]: hm\n")
      .contains("no `[[=:` highlight"));
    assert!(err(
      "```origins,notation\nlet a = [[=1:x:]];\n```\n[=1]: a\n[=1]: b\n"
    )
    .contains("defined twice"));
    assert!(err("```origins,notation\nlet a = [[=1:x:]];\n```\n[=0]: a\n")
      .contains("count from 1"));
    assert!(err("```origins,notation\nlet a = [[=1:x:]];\n```\n[=1]:\n")
      .contains("has no text"));
  }
}
