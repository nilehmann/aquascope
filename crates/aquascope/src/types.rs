//! The type of every expression and variable binding in a program, for
//! aquascope-reveal's type-on-hover markers.
//!
//! Deliberately apart from [`crate::analysis`] and [`crate::interpreter`]:
//! those produce the permissions and runtime diagrams behind ```aquascope
//! blocks, and share none of their machinery with this. Here nothing is
//! borrow-checked or run -- typeck's own results are read back, one entry per
//! expression or binding written in the source, and the caller picks out the
//! ones its markers ask for.
//!
//! Callers match on byte ranges, so a range is reported relative to the start
//! of the file it is in, which for a one-file crate is the program exactly as
//! it was written to disk.

use rustc_hir::{
  Expr, HirId, Pat, PatKind,
  intravisit::{self, Visitor},
};
use rustc_middle::{
  hir::nested_filter::OnlyBodies,
  ty::{Ty, TyCtxt, print::with_forced_trimmed_paths},
};
use rustc_span::Span;
use serde::Serialize;

use crate::errors::silent::silent_session;

/// One expression's or binding's type.
#[derive(Debug, Serialize)]
pub struct ExprType {
  /// Byte offset of the start, from the start of its file.
  pub start: usize,
  /// Byte offset one past the end.
  pub end: usize,
  /// The type as rustc prints it in a diagnostic, paths trimmed to the last
  /// segment: `String`, not `std::string::String`.
  pub ty: String,
}

struct TypeCollector<'tcx> {
  tcx: TyCtxt<'tcx>,
  types: Vec<ExprType>,
}

impl<'tcx> Visitor<'tcx> for TypeCollector<'tcx> {
  type NestedFilter = OnlyBodies;

  fn maybe_tcx(&mut self) -> Self::MaybeTyCtxt {
    self.tcx
  }

  fn visit_expr(&mut self, expr: &'tcx Expr<'tcx>) {
    // The type as written, before the adjustments typeck inserts: `b` in
    // `b.deref()` is a `MyBox<i32>`, not the `&MyBox<i32>` it is autoref'd
    // to.
    let ty = self.typeck(expr.hir_id).expr_ty_opt(expr);
    self.record(expr.span, ty);
    intravisit::walk_expr(self, expr);
  }

  /// A binding is reported at its name as well as over the whole pattern, so
  /// the `b` of `let mut b` can be marked on its own. The two only differ
  /// when there is a `mut`, a `ref` or an `@` subpattern to leave out.
  fn visit_pat(&mut self, pat: &'tcx Pat<'tcx>) {
    let ty = self.typeck(pat.hir_id).node_type_opt(pat.hir_id);
    self.record(pat.span, ty);
    if let PatKind::Binding(_, _, ident, _) = pat.kind {
      if ident.span != pat.span {
        self.record(ident.span, ty);
      }
    }
    intravisit::walk_pat(self, pat);
  }
}

impl<'tcx> TypeCollector<'tcx> {
  /// The typeck results `id` is in. A closure is type-checked with the item
  /// around it, and the owner of everything in it is that item -- so this is
  /// the right table for closure bodies too, without asking for them
  /// separately.
  fn typeck(&self, id: HirId) -> &'tcx rustc_middle::ty::TypeckResults<'tcx> {
    self.tcx.typeck(id.owner.def_id)
  }

  fn record(&mut self, span: Span, ty: Option<Ty<'tcx>>) {
    // Code a macro or a desugaring wrote has no source text of its own, so it
    // is reported at the invocation it came from: `vec![1, 2]`, `x?` and
    // `a..b` are each one expression in the source. Everything the expansion
    // wrote maps to that same range, and the outermost -- visited first --
    // is the one with the invocation's type. The arguments of `println!` are
    // still found on their own: tokens passed into a macro keep the spans
    // they had where they were written.
    let span = if span.from_expansion() {
      span.source_callsite()
    } else {
      span
    };
    if span.from_expansion() || span.is_dummy() {
      return;
    }
    let Some(ty) = ty else {
      return;
    };

    let file = self.tcx.sess.source_map().lookup_source_file(span.lo());
    let start = file.relative_position(span.lo()).0 as usize;
    let end = file.relative_position(span.hi()).0 as usize;
    self.types.push(ExprType {
      start,
      end,
      ty: with_forced_trimmed_paths!(ty.to_string()),
    });
  }
}

/// Every expression and binding written in the local crate, with its type,
/// outermost first where two share a span.
pub fn expr_types(tcx: TyCtxt) -> Vec<ExprType> {
  let mut collector = TypeCollector {
    tcx,
    types: Vec::new(),
  };
  tcx.hir_visit_all_item_likes_in_crate(&mut collector);
  collector.types
}

/// Runs [`expr_types`] once rustc has expanded the crate, and stops it there.
///
/// After expansion rather than after analysis, because a block whose borrow
/// check fails -- which is what a ```origins,shouldFail block usually is --
/// never reaches the end of analysis. Its types are known all the same:
/// typeck runs before borrowck, and is asked for on demand.
#[derive(Default)]
pub struct TypesCallbacks {
  pub result: Vec<ExprType>,
}

impl rustc_driver::Callbacks for TypesCallbacks {
  fn config(&mut self, config: &mut rustc_interface::Config) {
    // The caller compiles the block itself and reports its errors; rustc's
    // diagnostics here would only be noise in front of the JSON.
    config.psess_created = Some(silent_session());
  }

  fn after_expansion(
    &mut self,
    _compiler: &rustc_interface::interface::Compiler,
    tcx: TyCtxt<'_>,
  ) -> rustc_driver::Compilation {
    self.result = expr_types(tcx);
    rustc_driver::Compilation::Stop
  }
}
