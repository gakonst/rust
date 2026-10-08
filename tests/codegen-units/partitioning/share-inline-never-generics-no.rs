//@ compile-flags: -Copt-level=2 -Ccodegen-units=1 -Zshare-inline-never-generics=no

#![crate_type = "rlib"]

//@ aux-build:inline_never_generics_aux_noshare.rs
extern crate inline_never_generics_aux_noshare;

// With `-Zshare-inline-never-generics=no` (and no share-generics), a crate does not export the
// instantiations of `#[inline(never)]` generic functions, so it never needs its mono items to
// encode its metadata, and downstream crates instantiate their own private copies.

//~ MONO_ITEM fn foo @@ share_inline_never_generics_no-cgu.0[External]
pub fn foo() -> (f32, u16) {
    //~ MONO_ITEM fn inline_never_generics_aux_noshare::never_fn::<f32> @@ share_inline_never_generics_no-cgu.0[Internal]
    //~ MONO_ITEM fn inline_never_generics_aux_noshare::never_fn::<u16> @@ share_inline_never_generics_no-cgu.0[Internal]
    (
        inline_never_generics_aux_noshare::never_fn(2.0f32),
        inline_never_generics_aux_noshare::never_fn(3u16),
    )
}
