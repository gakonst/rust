// tidy-alphabetical-start
#![feature(file_buffered)]
#![feature(impl_trait_in_assoc_type)]
#![feature(once_cell_get_mut)]
// tidy-alphabetical-end

use rustc_attr_ir::lang_items::LangItem;
use rustc_middle::query::TyCtxtAt;
use rustc_middle::traits;
use rustc_middle::ty::adjustment::CustomCoerceUnsized;
use rustc_middle::ty::{self, Ty};
use rustc_middle::util::Providers;
use rustc_span::{ErrorGuaranteed, bug};

mod collector;
mod diagnostics;
mod graph_checks;
mod mono_checks;
mod offload;
mod partitioning;
mod util;

// Exposed so `rustc_codegen_ssa::base::codegen_crate` can trigger the
// host-metadata manifest write.
pub use offload::manifest::write_host_metadata_offload_manifest;

fn custom_coerce_unsize_info<'tcx>(
    tcx: TyCtxtAt<'tcx>,
    source_ty: Ty<'tcx>,
    target_ty: Ty<'tcx>,
) -> Result<CustomCoerceUnsized, ErrorGuaranteed> {
    let coerce_unsized_trait = tcx.require_lang_item(LangItem::CoerceUnsized, tcx.span);

    // Fast path: the MIR we are monomorphizing contains this unsizing cast, so it was already
    // proven well-formed, and `CoerceUnsized` has no builtin impls for ADTs. If exactly one
    // impl in the whole crate graph can apply to this ADT, selection can only pick that impl,
    // and all we need from it is which field it coerces. This skips re-proving the nested
    // `Unsize` obligations (e.g. auto traits of huge async-fn futures for
    // `Pin<Box<Fut>> -> Pin<Box<dyn Future + Send>>`) after monomorphization.
    if let ty::Adt(adt_def, _) = source_ty.kind() {
        let impls = tcx.trait_impls_of(coerce_unsized_trait);
        if impls.blanket_impls().is_empty()
            && let Some(&[impl_def_id]) = impls
                .non_blanket_impls()
                .get(&ty::fast_reject::SimplifiedType::Adt(adt_def.did()))
                .map(|v| v.as_slice())
            && let Some(custom_kind) = tcx.coerce_unsized_info(impl_def_id)?.custom_kind
        {
            return Ok(custom_kind);
        }
    }

    let trait_ref = ty::TraitRef::new(tcx.tcx, coerce_unsized_trait, [source_ty, target_ty]);

    match tcx
        .codegen_select_candidate(ty::TypingEnv::fully_monomorphized().as_query_input(trait_ref))
    {
        Ok(traits::ImplSource::UserDefined(traits::ImplSourceUserDefinedData {
            impl_def_id,
            ..
        })) => Ok(tcx.coerce_unsized_info(*impl_def_id)?.custom_kind.unwrap()),
        impl_source => {
            bug!(
                "invalid `CoerceUnsized` from {source_ty} to {target_ty}: impl_source: {:?}",
                impl_source
            );
        }
    }
}

pub fn provide(providers: &mut Providers) {
    partitioning::provide(providers);
    mono_checks::provide(&mut providers.queries);
}
