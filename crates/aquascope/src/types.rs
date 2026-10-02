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
//!
//! A function or method *name* is reported too -- `channel` in
//! `mpsc::channel::<i32>()`, `send` in `tx.send(1)` -- although a name is not
//! an expression: what it has is a signature, given twice, as instantiated at
//! that call and as declared.

use rustc_hir::{
  Expr, ExprKind, HirId, Pat, PatKind, QPath,
  intravisit::{self, Visitor},
};
use rustc_middle::{
  hir::nested_filter::OnlyBodies,
  ty::{
    self, GenericArgsRef, GenericParamDefKind, Ty, TyCtxt,
    print::with_forced_trimmed_paths,
  },
};
use rustc_span::{Span, def_id::DefId};
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
  /// segment: `String`, not `std::string::String`. For a function, its
  /// signature as instantiated here: `fn channel() -> (Sender<i32>,
  /// Receiver<i32>)`.
  pub ty: String,
  /// For a function, its signature as declared, where that says more than
  /// `ty` does -- the generic `fn channel<T>() -> (Sender<T>, Receiver<T>)`.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub decl: Option<String>,
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
    let typeck = self.typeck(expr.hir_id);
    let ty = typeck.expr_ty_opt(expr);

    // A path naming a function has the function's own type, which rustc
    // prints as `fn() -> T {name}`. It is reported as a signature instead,
    // over the whole path and over the name on its own -- with and without
    // the module in front -- so `[[^:channel:]]` and `[[^:mpsc::channel:]]`
    // both find it.
    if let (Some(&ty::FnDef(def_id, args)), ExprKind::Path(qpath)) =
      (ty.map(|ty| ty.kind()), &expr.kind)
    {
      self.record_fn(expr.span, def_id, args);
      let (prefix, name) = match qpath {
        QPath::Resolved(_, path) => {
          let name = path.segments.last().map(|s| s.ident.span);
          (name.map(|n| path.span.with_hi(n.hi())), name)
        }
        QPath::TypeRelative(qself, segment) => {
          (Some(qself.span.to(segment.ident.span)), Some(segment.ident.span))
        }
      };
      for span in [prefix, name].into_iter().flatten() {
        self.record_fn(span, def_id, args);
      }
      intravisit::walk_expr(self, expr);
      return;
    }

    self.record(expr.span, ty);

    // A method's name is not an expression at all, only a segment of the
    // call; the method it resolved to is in typeck's results.
    if let ExprKind::MethodCall(segment, ..) = expr.kind {
      if let Some(def_id) = typeck.type_dependent_def_id(expr.hir_id) {
        self.record_fn(segment.ident.span, def_id, typeck.node_args(expr.hir_id));
      }
    }
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
    let Some(ty) = ty else {
      return;
    };
    self.push(span, tidy(&with_forced_trimmed_paths!(ty.to_string())), None);
  }

  /// A function at `span`, called with `args`: its signature as instantiated
  /// here, and as declared when that differs -- for a function with nothing
  /// generic about it the two say the same.
  fn record_fn(&mut self, span: Span, def_id: DefId, args: GenericArgsRef<'tcx>) {
    let here = self.signature(def_id, args);
    let decl = self.declaration(def_id).filter(|decl| *decl != here);
    self.push(span, here, decl);
  }

  /// `fn name(params) -> ret`, with the arguments of one call substituted.
  /// Lifetimes are erased: the declaration, shown with it, has them.
  fn signature(&self, def_id: DefId, args: GenericArgsRef<'tcx>) -> String {
    let tcx = self.tcx;
    let sig = tcx.fn_sig(def_id).instantiate(tcx, args).skip_normalization();
    let sig = tcx.instantiate_bound_regions_with_erased(sig);
    let names = tcx.fn_arg_idents(def_id);
    let is_method = tcx.opt_associated_item(def_id).is_some_and(|i| i.is_method());
    let print = |ty: Ty<'tcx>| tidy(&with_forced_trimmed_paths!(ty.to_string()));

    let params: Vec<String> = sig
      .inputs()
      .iter()
      .enumerate()
      .map(|(i, &input)| {
        if i == 0 && is_method {
          // Written the way a method declares it, which is how it is read.
          return match input.kind() {
            ty::Ref(_, _, m) if m.is_mut() => "&mut self".to_string(),
            ty::Ref(..) => "&self".to_string(),
            ty::Adt(adt, _)
              if input.is_box()
                || ["Pin", "Rc", "Arc"]
                  .contains(&tcx.item_name(adt.did()).as_str()) =>
            {
              format!("self: {}", print(input))
            }
            _ => "self".to_string(),
          };
        }
        let name = names
          .get(i)
          .copied()
          .flatten()
          .map_or_else(|| "_".to_string(), |ident| ident.to_string());
        format!("{name}: {}", print(input))
      })
      .collect();

    let output = sig.output();
    let ret = if output.is_unit() {
      String::new()
    } else {
      format!(" -> {}", print(output))
    };
    format!("fn {}({}){ret}", tcx.item_name(def_id), params.join(", "))
  }

  /// The function's signature as its source declares it -- bounds, `where`
  /// clause and lifetimes as written -- on one line and without its `pub`.
  /// Read from the standard library's own source for a `std` function,
  /// which the toolchain's `rust-src` component provides. Without it, the
  /// generic signature rebuilt from the types.
  fn declaration(&self, def_id: DefId) -> Option<String> {
    self.declaration_text(def_id).or_else(|| {
      let tcx = self.tcx;
      let identity = ty::GenericArgs::identity_for_item(tcx, def_id);
      let sig = self.signature(def_id, identity);
      let own: Vec<String> = tcx
        .generics_of(def_id)
        .own_params
        .iter()
        .filter(|p| match p.kind {
          GenericParamDefKind::Lifetime => false,
          GenericParamDefKind::Type { synthetic, .. } => !synthetic,
          GenericParamDefKind::Const { .. } => true,
        })
        .map(|p| p.name.to_string())
        .collect();
      if own.is_empty() {
        return Some(sig);
      }
      let name = format!("fn {}", tcx.item_name(def_id));
      Some(sig.replacen(&name, &format!("{name}<{}>", own.join(", ")), 1))
    })
  }

  fn declaration_text(&self, def_id: DefId) -> Option<String> {
    let source_map = self.tcx.sess.source_map();
    let span = self.tcx.def_span(def_id);
    if span.is_dummy() {
      return None;
    }
    // The span is the signature up to the return type; the `where` clause
    // follows it, up to the body or the `;` of a declaration without one.
    let file = source_map.lookup_source_file(span.lo());
    let rest = source_map
      .span_to_snippet(span.with_hi(file.end_position()))
      .ok()?;
    let end = rest.find(['{', ';']).unwrap_or(rest.len());
    let text = rest[.. end].split_whitespace().collect::<Vec<_>>().join(" ");
    let text = text.trim_end_matches(',').trim();
    let text = match text.strip_prefix("pub") {
      Some(after) if after.starts_with('(') => after[after.find(')')? + 1 ..].trim_start(),
      Some(after) if after.starts_with(' ') => after.trim_start(),
      _ => text,
    };
    // The bounds on a line of their own, as rustfmt puts them, so the line
    // with the parameters stays the length of a signature.
    text
      .contains("fn ")
      .then(|| text.replacen(" where ", "\nwhere ", 1))
  }

  fn push(&mut self, span: Span, ty: String, decl: Option<String>) {
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

    let file = self.tcx.sess.source_map().lookup_source_file(span.lo());
    let start = file.relative_position(span.lo()).0 as usize;
    let end = file.relative_position(span.hi()).0 as usize;
    self.types.push(ExprType {
      start,
      end,
      ty,
      decl,
    });
  }
}

/// `{closure@src/main.rs:5:19: 5:21}` as `{closure}`: where a closure was
/// written is noise in a type shown on the slide it was written on. The same
/// for async blocks and coroutines.
fn tidy(ty: &str) -> String {
  let mut out = String::with_capacity(ty.len());
  let mut rest = ty;
  while let Some(open) = rest.find('{') {
    out.push_str(&rest[.. open]);
    let after = &rest[open + 1 ..];
    match (after.find('@'), after.find('}')) {
      (Some(at), Some(close))
        if at < close
          && after[.. at].chars().all(|c| c.is_alphanumeric() || c == ' ') =>
      {
        out.push('{');
        out.push_str(&after[.. at]);
        out.push('}');
        rest = &after[close + 1 ..];
      }
      _ => {
        out.push('{');
        rest = after;
      }
    }
  }
  out.push_str(rest);
  out
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
