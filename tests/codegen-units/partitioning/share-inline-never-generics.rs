//@ compile-flags: -Copt-level=2 -Ccodegen-units=1

#![crate_type = "rlib"]

//@ aux-build:inline_never_generics_aux.rs
extern crate inline_never_generics_aux;

// Without share-generics (the default with optimizations), instantiations of `#[inline(never)]`
// generic functions are still exported by the crate instantiating them and reused downstream:
// `never_fn::<f32>` comes from the dependency, `never_fn::<u16>` is instantiated here and
// exported again.

//~ MONO_ITEM fn foo @@ share_inline_never_generics-cgu.0[External]
pub fn foo() -> (f32, u16) {
    //~ MONO_ITEM fn inline_never_generics_aux::never_fn::<u16> @@ share_inline_never_generics-cgu.0[External]
    (inline_never_generics_aux::never_fn(2.0f32), inline_never_generics_aux::never_fn(3u16))
}
