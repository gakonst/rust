//@ compile-flags: -Copt-level=2 -Zshare-inline-never-generics=no

#![crate_type = "rlib"]

#[inline(never)]
pub fn never_fn<T: Copy>(x: T) -> T {
    x
}

pub fn use_never_fn_f32() -> f32 {
    never_fn(1.0f32)
}
