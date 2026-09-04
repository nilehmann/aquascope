use miri::{
  InterpCx, InterpResult, MPlaceTy, Machine, MemPlaceMeta, OpTy, Projectable,
  interp_ok,
};
use rustc_abi::{FieldIdx, FieldsShape, Size};
use rustc_middle::{
  mir::{Local, PlaceElem},
  ty::{AdtKind, FieldDef, Ty, TyKind, layout::TyAndLayout},
};

pub trait OpTyExt<'tcx, M: Machine<'tcx>>: Sized {
  fn field_by_name(
    &self,
    name: &str,
    ecx: &InterpCx<'tcx, M>,
  ) -> InterpResult<'tcx, (&FieldDef, Self)>;
}

impl<'tcx, M> OpTyExt<'tcx, M> for OpTy<'tcx>
where
  M: Machine<'tcx>,
  Self: Projectable<'tcx, M::Provenance>,
{
  fn field_by_name(
    &self,
    name: &str,
    ecx: &InterpCx<'tcx, M>,
  ) -> InterpResult<'tcx, (&FieldDef, Self)> {
    let adt_def = self.layout.ty.ty_adt_def().unwrap();
    let (i, field) = adt_def
      .all_fields()
      .enumerate()
      .find(|(_, field)| field.name.as_str() == name)
      .unwrap_or_else(|| {
        panic!(
          "Could not find field with name `{name}` out of fields: {:?}",
          adt_def
            .all_fields()
            .map(|field| field.name)
            .collect::<Vec<_>>()
        )
      });
    let field_op = ecx.project_field(self, FieldIdx::from_usize(i))?;
    interp_ok((field, field_op))
  }
}

struct AddressLocator<'a, 'tcx> {
  ecx: &'a InterpCx<'tcx, miri::MiriMachine<'tcx>>,
  target: u64,
  target_ty: Ty<'tcx>,
  segments: Vec<PlaceElem<'tcx>>,
}

impl<'tcx> AddressLocator<'_, 'tcx> {
  /// Descends into whichever field of `layout` contains the target address.
  ///
  /// Offsets come from the layout rather than from summing up the sizes of the
  /// preceding fields: rustc is free to reorder fields and to insert padding
  /// between them, so declaration order says nothing about where a field
  /// actually lives.
  fn locate_field(
    &mut self,
    layout: TyAndLayout<'tcx>,
    base: u64,
    n_fields: usize,
  ) {
    for i in 0 .. n_fields {
      let field = layout.field(self.ecx, i);
      let offset = base + layout.layout.fields().offset(i).bytes();
      if offset <= self.target && self.target < offset + field.size.bytes() {
        self
          .segments
          .push(PlaceElem::Field(FieldIdx::from_usize(i), field.ty));
        self.locate(field, offset);
        return;
      }
    }
  }

  fn locate(&mut self, layout: TyAndLayout<'tcx>, offset: u64) {
    // A field at the start of its parent shares the parent's address, so the
    // address alone doesn't say which of them is being pointed at. The pointee
    // type breaks the tie: keep descending until it matches.
    if offset == self.target && layout.ty == self.target_ty {
      return;
    }

    let ty = layout.ty;
    match ty.kind() {
      TyKind::Adt(adt_def, _) => {
        let def_id = adt_def.did();
        let name = self.ecx.tcx.item_name(def_id).to_ident_string();
        match adt_def.adt_kind() {
          AdtKind::Struct => match name.as_str() {
            "String" | "Vec" => {}
            _ => {
              self.locate_field(layout, offset, adt_def.all_fields().count())
            }
          },
          AdtKind::Enum => todo!(),
          _ => {}
        }
      }

      TyKind::Array(_, _) => {
        // dbg!(("array", offset, target));
        let FieldsShape::Array { stride, .. } = layout.layout.fields() else {
          unreachable!()
        };
        let stride = stride.bytes();
        let array_offset = (self.target - offset) / stride * stride;
        let elem = layout.field(self.ecx, 0);
        let index = (array_offset / stride) as usize;
        // dbg!((index, array_offset));
        self
          .segments
          .push(PlaceElem::Index(Local::from_usize(index)));
        self.locate(elem, offset + array_offset);
      }

      TyKind::Tuple(tys) => {
        // dbg!(("tuple", offset, target));
        self.locate_field(layout, offset, tys.len())
      }

      _ if ty.is_primitive() || ty.is_any_ptr() => {
        // A pointee whose type never matched anything along the way (a `&[T]`
        // into an array, say, or a raw pointer cast) bottoms out here, and the
        // path found so far is the closest we can get.
        assert_eq!(
          offset, self.target,
          "offset {offset} != target {}",
          self.target
        );
      }

      ty => unimplemented!("{ty:#?}"),
    }
  }
}

pub fn locate_address_in_type<'tcx>(
  ecx: &InterpCx<'tcx, miri::MiriMachine<'tcx>>,
  alloc_layout: TyAndLayout<'tcx>,
  alloc_size: Size,
  mplace: MPlaceTy<'tcx>,
  target: Size,
) -> Vec<PlaceElem<'tcx>> {
  // dbg!((alloc_layout, alloc_size, mplace, target));
  let mut locator = AddressLocator {
    ecx,
    target: target.bytes(),
    target_ty: mplace.layout.ty,
    segments: Vec::new(),
  };

  let mut offset = 0;
  if alloc_layout.size.bytes() < alloc_size.bytes() {
    let array_elem_size = alloc_layout.size.bytes();
    assert!(
      array_elem_size > 0,
      "Array has zero-sized elements: {alloc_layout:#?}"
    );

    offset = target.bytes() / array_elem_size * array_elem_size;
    let index = offset / array_elem_size;
    // dbg!((array_elem_size, offset, index));

    let segment = match mplace.meta() {
      MemPlaceMeta::Meta(meta) => {
        // Slice metadata is an element count, not a byte size, so it must not
        // be divided by the element size. That division was invisible for
        // `&str`, whose elements are bytes, but for `&[i32]` it truncated a
        // length of 2 to 0 -- yielding an end index of `start - 1`, which
        // underflowed to usize::MAX whenever the slice started at 0.
        let len = meta.to_u64().unwrap();
        let to = index + len.saturating_sub(1);
        PlaceElem::Subslice {
          from: index,
          to,
          from_end: false,
        }
      }
      MemPlaceMeta::None => PlaceElem::Index(Local::from_usize(index as usize)),
    };

    locator.segments.push(segment);
  }

  locator.locate(alloc_layout, offset);
  locator.segments
}
