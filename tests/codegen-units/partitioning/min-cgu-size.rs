//@ compile-flags: -Copt-level=0 -Ccodegen-units=4 -Zmin-cgu-size=1800

#![crate_type = "rlib"]

// An explicit `-Ccodegen-units` normally disables the merging of small CGUs in non-incremental
// builds. `-Zmin-cgu-size` re-enables it with the given threshold, so these four tiny modules
// (one CGU each before merging) end up in a single CGU.

pub mod aaa {
    //~ MONO_ITEM fn aaa::foo @@ min_cgu_size-cgu.0[External]
    pub fn foo(a: u64) -> u64 {
        a + 1
    }
}

pub mod bbb {
    //~ MONO_ITEM fn bbb::foo @@ min_cgu_size-cgu.0[External]
    pub fn foo(a: u64, b: u64) -> u64 {
        a + b + 1
    }
}

pub mod ccc {
    //~ MONO_ITEM fn ccc::foo @@ min_cgu_size-cgu.0[External]
    pub fn foo(a: u64, b: u64, c: u64) -> u64 {
        a + b + c + 1
    }
}

pub mod ddd {
    //~ MONO_ITEM fn ddd::foo @@ min_cgu_size-cgu.0[External]
    pub fn foo(a: u64, b: u64, c: u64, d: u64) -> u64 {
        a + b + c + d + 1
    }
}
