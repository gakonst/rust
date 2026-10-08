// Verify that function linkage names are only emitted in line-tables-only debuginfo when they
// can end up in the object file (name tables of DWARF 5) or are needed to match a sample
// profile, like clang's `-gline-tables-only` (LLVM never emits `DW_AT_linkage_name` for
// line-tables-only compile units).
//
//@ revisions: LINES DWARF5 PROFILING FULL
//@ only-linux
//@ compile-flags: -Copt-level=0
//@ [LINES] compile-flags: -Cdebuginfo=line-tables-only -Cdwarf-version=4
//@ [DWARF5] compile-flags: -Cdebuginfo=line-tables-only -Cdwarf-version=5
//@ [PROFILING] compile-flags: -Cdebuginfo=line-tables-only -Cdwarf-version=4 -Zdebuginfo-for-profiling
//@ [FULL] compile-flags: -Cdebuginfo=full -Cdwarf-version=4

#![crate_type = "lib"]

#[inline(never)]
pub fn generic<T: Copy>(x: T) -> T {
    x
}

pub fn caller() -> u32 {
    generic(1u32)
}

// LINES-NOT: linkageName:
// DWARF5: !DISubprogram(name: "generic<u32>", linkageName:
// PROFILING: !DISubprogram(name: "generic<u32>", linkageName:
// FULL: !DISubprogram(name: "generic<u32>", linkageName:
