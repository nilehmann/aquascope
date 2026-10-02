//! Types for the `[[^:TEXT:]]` markers in ```origins blocks, asked of the
//! compiler.
//!
//! This is the one part of the ```origins pipeline that needs rustc's
//! internals, so it is kept out of `origins.rs`: that module finds the
//! markers and renders them, and hands this one the program and the byte
//! ranges it wants typed. The answer comes from aquascope-driver's `types`
//! subcommand, run through `mdbook-aquascope`'s preprocessor -- the same
//! temporary crate, nightly sysroot and cache the ```aquascope blocks use --
//! which reports the type of every expression and binding in the program.
//! Matching those against the markers happens here, so a marker that is not
//! exactly one expression fails the build with the text the author wrote.

use std::ops::Range;

use anyhow::{bail, Context, Result};
use mdbook_aquascope::AquascopePreprocessor;
use serde::Deserialize;

/// One expression's type, as aquascope-driver's `types` subcommand reports it.
/// For a function or method name, `ty` is its signature at this call and
/// `decl` the signature it was declared with, where the two differ.
#[derive(Debug, Deserialize)]
pub struct ExprType {
  pub start: usize,
  pub end: usize,
  pub ty: String,
  #[serde(default)]
  pub decl: Option<String>,
}

/// Answers "what are the types in this program?" for `origins.rs`. A trait
/// object rather than the preprocessor itself, so the renderer's tests can
/// supply types without a nightly toolchain.
pub trait Typer {
  fn expr_types(
    &self,
    program: &str,
    should_fail: bool,
  ) -> Result<Vec<ExprType>>;
}

impl Typer for AquascopePreprocessor {
  fn expr_types(
    &self,
    program: &str,
    should_fail: bool,
  ) -> Result<Vec<ExprType>> {
    let json = self
      .query(program, "types", should_fail)
      .context("asking aquascope-driver for the block's types")?;
    Ok(serde_json::from_value(json)?)
  }
}

/// The type of the expression at `range` in `program`.
///
/// A marker has to cover exactly one expression, the name in a binding such
/// as the `b` of `let mut b`, or a function or method name, which shows two
/// lines: the signature as declared, then as instantiated at this call. rustc gives an expression
/// in parentheses the parentheses' span, so `(b.deref())` is where the method
/// call is reported; a marker around just `b.deref()` is still that
/// expression, and is found by looking through the parentheses.
pub fn type_at(
  program: &str,
  types: &[ExprType],
  range: Range<usize>,
) -> Result<String> {
  // Outermost first where two share a span, which is the order the driver
  // reports them in.
  let found = types.iter().find(|t| {
    t.start == range.start && t.end == range.end
      || unparenthesized(program, t.start .. t.end) == Some(range.clone())
  });
  let Some(found) = found else {
    bail!(
      "`{}` is not an expression, a variable binding or a function name, so \
       there is no type to show for it. A `[[^:…:]]` marker has to cover \
       exactly one.",
      &program[range]
    );
  };
  if found.ty.contains("{type error}") {
    bail!(
      "`{}` has no type, because the block does not type-check",
      &program[range]
    );
  }
  Ok(match &found.decl {
    Some(decl) => format!("{decl}\n{}", found.ty),
    None => found.ty.clone(),
  })
}

/// `range` with one pair of enclosing parentheses taken off, if it is
/// `( … )` with the two parentheses matching each other, and the whitespace
/// just inside them trimmed.
fn unparenthesized(program: &str, range: Range<usize>) -> Option<Range<usize>> {
  let text = program.get(range.clone())?;
  let inner = text.strip_prefix('(')?.strip_suffix(')')?;
  // `(a) + (b)` starts and ends with a parenthesis without being one
  // parenthesised expression: the opening one has to close at the very end.
  let mut depth = 0usize;
  for (i, ch) in text.char_indices() {
    match ch {
      '(' => depth += 1,
      ')' => {
        depth = depth.checked_sub(1)?;
        if depth == 0 && i != text.len() - 1 {
          return None;
        }
      }
      _ => {}
    }
  }
  let lead = inner.len() - inner.trim_start().len();
  let start = range.start + 1 + lead;
  Some(start .. start + inner.trim().len())
}

#[cfg(test)]
mod test {
  use super::*;

  fn ty(start: usize, end: usize, ty: &str) -> ExprType {
    ExprType {
      start,
      end,
      ty: ty.to_string(),
      decl: None,
    }
  }

  #[test]
  fn matches_exact_ranges() {
    let program = "let x = *b + 1;";
    let types = [ty(8, 14, "i32"), ty(8, 10, "i32"), ty(9, 10, "MyBox<i32>")];
    assert_eq!(type_at(program, &types, 9 .. 10).unwrap(), "MyBox<i32>");
    assert_eq!(type_at(program, &types, 8 .. 10).unwrap(), "i32");
  }

  #[test]
  fn looks_through_parentheses() {
    let program = "*(b.deref())";
    let types = [ty(1, 12, "&i32")];
    assert_eq!(type_at(program, &types, 2 .. 11).unwrap(), "&i32");
  }

  #[test]
  fn a_function_shows_its_declaration_above_this_call() {
    let program = "mpsc::channel::<i32>()";
    let types = [ExprType {
      start: 6,
      end: 13,
      ty: "fn channel() -> (Sender<i32>, Receiver<i32>)".into(),
      decl: Some("fn channel<T>() -> (Sender<T>, Receiver<T>)".into()),
    }];
    assert_eq!(
      type_at(program, &types, 6 .. 13).unwrap(),
      "fn channel<T>() -> (Sender<T>, Receiver<T>)\n\
       fn channel() -> (Sender<i32>, Receiver<i32>)"
    );
  }

  #[test]
  fn does_not_unwrap_two_groups() {
    assert_eq!(unparenthesized("(a) + (b)", 0 .. 9), None);
  }

  #[test]
  fn rejects_a_range_that_is_not_an_expression() {
    let program = "b.deref()";
    let types = [ty(0, 9, "&i32"), ty(0, 1, "MyBox<i32>")];
    let err = type_at(program, &types, 0 .. 7).unwrap_err().to_string();
    assert!(err.contains("`b.deref` is not an expression"), "{err}");
  }
}
