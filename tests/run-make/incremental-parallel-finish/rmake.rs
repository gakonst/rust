//@ ignore-cross-compile
//@ needs-threads
//
// With multiple threads, rustc saves the incremental state of a crate that is only an rlib while
// it writes the rlib (see `Linker::finish_and_write_rlib` in `rustc_interface`). Check that the
// session directory is still finalized and reused by the next session, and that the rlib works.

use std::path::{Path, PathBuf};

use run_make_support::{rfs, run, rustc};

fn build_lib(assert_incr_state: Option<&str>) {
    let mut cmd = rustc();
    cmd.input("lib.rs").crate_type("rlib").incremental("incr").arg("-Zthreads=2");
    if let Some(state) = assert_incr_state {
        cmd.arg(format!("-Zassert-incr-state={state}"));
    }
    cmd.run();
}

fn build_and_run_main() {
    rustc().input("main.rs").extern_("lib", "liblib.rlib").output("main").run();
    run("main").assert_stdout_contains("hello, world hello, world");
}

fn dir_entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries = vec![];
    rfs::read_dir_entries(dir, |path| entries.push(path.to_owned()));
    entries
}

/// The session directories in the incremental directory of the crate, which must be finalized.
fn sessions() -> Vec<String> {
    let crate_dirs = dir_entries(Path::new("incr"));
    assert_eq!(crate_dirs.len(), 1, "{crate_dirs:?}");
    let mut sessions: Vec<String> = dir_entries(&crate_dirs[0])
        .into_iter()
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_owned())
        .filter(|name| !name.ends_with(".lock"))
        .collect();
    sessions.sort();
    for session in &sessions {
        assert!(!session.ends_with("-working"), "session not finalized: {sessions:?}");
    }
    sessions
}

fn main() {
    build_lib(Some("not-loaded"));
    assert_eq!(sessions().len(), 1);
    build_and_run_main();

    // A rebuild without changes, and one after a comment-only change, load the previous session.
    build_lib(Some("loaded"));
    assert_eq!(sessions().len(), 1);
    let src = rfs::read_to_string("lib.rs");
    rfs::write("lib.rs", format!("{src}// a comment\n"));
    build_lib(Some("loaded"));
    assert_eq!(sessions().len(), 1);
    build_and_run_main();

    // A real change.
    rfs::write("lib.rs", src.replace("hello", "hi"));
    build_lib(Some("loaded"));
    rustc().input("main.rs").extern_("lib", "liblib.rlib").output("main").run();
    run("main").assert_stdout_contains("hi, world hi, world");
}
