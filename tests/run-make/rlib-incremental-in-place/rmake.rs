//@ needs-target-std
//@ only-unix (the in-place update relies on unix file APIs)
//
// In incremental mode rustc updates an existing rlib in place, patching only the bytes that
// differ, instead of writing a whole new archive (see `update_archive_in_place` in
// `rustc_codegen_ssa`). Check that the result is byte-for-byte the archive a fresh build
// produces, that the fast path is actually taken for a comment-only edit, and that it is not
// taken for a file with other hard links.

use std::os::unix::fs::MetadataExt;

use run_make_support::{rfs, rustc};

fn build(out_dir: &str, incremental: &str) {
    rustc()
        .input("lib.rs")
        .crate_name("foo")
        .crate_type("lib")
        .emit("dep-info,metadata,link")
        .incremental(incremental)
        .out_dir(out_dir)
        .run();
}

fn ino(path: &str) -> u64 {
    rfs::metadata(path).ino()
}

fn main() {
    let rlib = "out/libfoo.rlib";
    rfs::create_dir("out");
    build("out", "incr");
    let first = ino(rlib);

    // A comment-only edit leaves every codegen unit reused and changes only a few bytes of the
    // metadata, so the existing archive is patched rather than replaced.
    let src = rfs::read_to_string("lib.rs");
    rfs::write("lib.rs", format!("{src}// a comment\n"));
    build("out", "incr");
    assert_eq!(ino(rlib), first, "rlib was not updated in place");

    // The patched archive is identical to the one a clean build writes.
    rfs::create_dir("fresh");
    build("fresh", "incr-fresh");
    assert_eq!(rfs::read(rlib), rfs::read("fresh/libfoo.rlib"), "patched rlib differs");

    // Unchanged sources: still updated in place, still identical.
    build("out", "incr");
    assert_eq!(ino(rlib), first);
    assert_eq!(rfs::read(rlib), rfs::read("fresh/libfoo.rlib"));

    // A real change: the result must again equal a fresh build, however it was written.
    rfs::write("lib.rs", src.replace("wrapping_add", "wrapping_sub"));
    build("out", "incr");
    rfs::remove_file("fresh/libfoo.rlib");
    build("fresh", "incr-fresh");
    assert_eq!(rfs::read(rlib), rfs::read("fresh/libfoo.rlib"), "rebuilt rlib differs");

    // An rlib with another hard link is never modified in place, so that the other name
    // keeps the old contents.
    let before = rfs::read(rlib);
    std::fs::hard_link(rlib, "linked.rlib").unwrap();
    rfs::write("lib.rs", src.clone());
    build("out", "incr");
    assert_eq!(rfs::read("linked.rlib"), before, "hard-linked copy was modified");
    assert_ne!(ino(rlib), ino("linked.rlib"));
}
