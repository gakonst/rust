// When `x86_64-unknown-linux-gnu` defaults to the self-contained `rust-lld` and `mold` is
// installed as `ld.mold` in `PATH`, rustc links with `mold` instead, unless the user asked for a
// specific linker setup or set `RUSTC_NO_DEFAULT_MOLD`. The program must work the same.

//@ needs-rust-lld
//@ only-x86_64-unknown-linux-gnu

use run_make_support::{Rustc, env_var_os, run, rustc};

fn linker_messages(rustc: &mut Rustc) -> String {
    let output = rustc.arg("-Wlinker-messages").link_arg("-Wl,-v").run();
    output.stderr_utf8()
}

fn uses_mold(stderr: &str) -> bool {
    stderr.lines().any(|line| line.contains("mold") && line.contains("compatible with GNU ld"))
}

fn uses_lld(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        let line = line.trim().trim_start_matches("warning: linker stdout:").trim();
        let line = line.trim_start_matches("warning: linker stderr:").trim();
        line.starts_with("LLD ")
    })
}

fn main() {
    let path = env_var_os("PATH");
    let has_mold = std::env::split_paths(&path).any(|dir| dir.join("ld.mold").is_file());

    let default = linker_messages(rustc().input("main.rs").env_remove("RUSTC_NO_DEFAULT_MOLD"));
    if has_mold {
        assert!(uses_mold(&default), "mold should be used by default:\n{default}");
        assert!(!uses_lld(&default), "lld should not be used by default:\n{default}");
    } else {
        assert!(uses_lld(&default), "lld should be used without mold:\n{default}");
    }
    run("main");

    // Opting out, or asking for lld explicitly, keeps rust-lld.
    let opt_out = linker_messages(rustc().input("main.rs").env("RUSTC_NO_DEFAULT_MOLD", "1"));
    assert!(uses_lld(&opt_out), "lld should be used with RUSTC_NO_DEFAULT_MOLD:\n{opt_out}");
    run("main");

    let explicit = linker_messages(
        rustc()
            .input("main.rs")
            .env_remove("RUSTC_NO_DEFAULT_MOLD")
            .arg("-Zunstable-options")
            .arg("-Clinker-features=+lld"),
    );
    assert!(uses_lld(&explicit), "lld should be used with -Clinker-features=+lld:\n{explicit}");
    assert!(!uses_mold(&explicit), "mold should not be used with explicit lld:\n{explicit}");

    // Turning lld off uses the system linker driver's default, not mold.
    let off = linker_messages(
        rustc().input("main.rs").env_remove("RUSTC_NO_DEFAULT_MOLD").arg("-Clinker-features=-lld"),
    );
    assert!(!uses_lld(&off) && !uses_mold(&off), "-Clinker-features=-lld:\n{off}");
    run("main");
}
