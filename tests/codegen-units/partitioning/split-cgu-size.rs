//@ compile-flags: -Copt-level=0 -Ccodegen-units=2 -Zsplit-cgu-size=1

#![crate_type = "rlib"]

// In non-incremental builds, a source module that is much bigger than an even share of the crate
// is split before merging, so that it does not become one oversized CGU. Its root items are
// distributed in source order. `-Zsplit-cgu-size=1` lowers the minimum size for splitting (by
// default only modules with a size estimate above 100000 are split).

//~ MONO_ITEM fn foo1 @@ split_cgu_size-cgu.0[External]
pub fn foo1(a: u64) -> u64 {
    a + 1
}

//~ MONO_ITEM fn foo2 @@ split_cgu_size-cgu.0[External]
pub fn foo2(a: u64) -> u64 {
    a + 2
}

//~ MONO_ITEM fn foo3 @@ split_cgu_size-cgu.1[External]
pub fn foo3(a: u64) -> u64 {
    a + 3
}

//~ MONO_ITEM fn foo4 @@ split_cgu_size-cgu.1[External]
pub fn foo4(a: u64) -> u64 {
    a + 4
}
