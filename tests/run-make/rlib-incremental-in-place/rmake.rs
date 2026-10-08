//@ needs-target-std
//@ only-unix (the in-place update relies on unix file APIs)
//
// In incremental mode rustc updates an existing rlib in place, patching only the bytes that
// differ, instead of writing a whole new archive (see `update_archive_in_place` in
// `rustc_codegen_ssa`). Check that the result is the archive a fresh build produces, that the
// fast path is actually taken for a comment-only edit, and that it is not taken for a file with
// other hard links.
//
// Incremental object file names contain a random per-invocation suffix, so two builds never
// produce byte-identical rlibs; archives are compared member by member with that suffix removed
// from member names and from member data (`lib.rmeta-link` lists the object file names).
// The reference for each step is a build from a copy of the same incremental state into a fresh
// output directory (which always writes a new archive): even without this optimization, an
// rlib built from scratch is not byte-identical to one built from a reused incremental session.

use std::os::unix::fs::MetadataExt;

use run_make_support::object::read::archive::ArchiveFile;
use run_make_support::regex::Regex;
use run_make_support::regex::bytes::Regex as BytesRegex;
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

/// The archive's length and its members (name and data without invocation suffixes).
fn members(path: &str) -> (usize, Vec<(String, Vec<u8>)>) {
    let suffix = Regex::new(r"\.[0-9a-z]+\.rcgu\.o$").unwrap();
    let data_suffix = BytesRegex::new(r"\.[0-9a-z]+\.rcgu\.o").unwrap();
    let data = rfs::read(path);
    let archive = ArchiveFile::parse(&*data).unwrap();
    let members = archive
        .members()
        .map(|member| {
            let member = member.unwrap();
            let name = String::from_utf8(member.name().to_vec()).unwrap();
            let name = suffix.replace(&name, ".rcgu.o").into_owned();
            let member_data = member.data(&*data).unwrap();
            (name, data_suffix.replace_all(member_data, &b".rcgu.o"[..]).into_owned())
        })
        .collect();
    (data.len(), members)
}

/// Rebuild `out` from the incremental session `incr`, after first building a reference from a
/// copy of that session into a fresh directory. Returns the reference rlib's path.
fn rebuild_with_reference(step: &str) -> String {
    let incr_ref = format!("incr-{step}");
    let out_ref = format!("ref-{step}");
    rfs::copy_dir_all("incr", &incr_ref);
    rfs::create_dir(&out_ref);
    build(&out_ref, &incr_ref);
    build("out", "incr");
    format!("{out_ref}/libfoo.rlib")
}

fn main() {
    let rlib = "out/libfoo.rlib";
    rfs::create_dir("out");
    build("out", "incr");
    let first = ino(rlib);

    // A comment-only edit leaves every codegen unit reused and changes only a few bytes of the
    // metadata, so the existing archive is patched rather than replaced, and the patched archive
    // is identical to a newly written one.
    let src = rfs::read_to_string("lib.rs");
    rfs::write("lib.rs", format!("{src}// a comment\n"));
    let reference = rebuild_with_reference("comment");
    assert_eq!(ino(rlib), first, "rlib was not updated in place");
    assert_eq!(members(rlib), members(&reference), "patched rlib differs");

    // Unchanged sources: still updated in place, still identical.
    let reference = rebuild_with_reference("noop");
    assert_eq!(ino(rlib), first);
    assert_eq!(members(rlib), members(&reference), "patched rlib differs after no-op rebuild");

    // A real change: the result must again equal a newly written archive, however it was written.
    rfs::write("lib.rs", src.replace("wrapping_add", "wrapping_sub"));
    let reference = rebuild_with_reference("change");
    assert_eq!(members(rlib), members(&reference), "rebuilt rlib differs");

    // An rlib with another hard link is never modified in place, so that the other name
    // keeps the old contents.
    let before = rfs::read(rlib);
    std::fs::hard_link(rlib, "linked.rlib").unwrap();
    rfs::write("lib.rs", src.clone());
    build("out", "incr");
    assert_eq!(rfs::read("linked.rlib"), before, "hard-linked copy was modified");
    assert_ne!(ino(rlib), ino("linked.rlib"));
}
