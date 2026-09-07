//! `cargo build --release` must leave `loci` on PATH. The rustc-wrapper copies
//! the release `loci` binary to `$HOME/.local/bin/loci` after a successful link.
//!
//! Cargo does not pass `-o target/release/loci`. It compiles `--crate-name loci`
//! `--crate-type bin` into `--out-dir …/release/deps` with `-C extra-filename`.
//! HOME is a temp dir so the test never writes the real install.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

fn wrapper() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/rustc-wrapper.sh")
}

fn write_fake_rustc(work: &std::path::Path, body: &str) -> PathBuf {
    let fake_rustc = work.join("fake-rustc");
    std::fs::write(&fake_rustc, body).unwrap();
    std::fs::set_permissions(&fake_rustc, std::fs::Permissions::from_mode(0o755)).unwrap();
    fake_rustc
}

fn write_file_at_dash_o() -> &'static str {
    "#!/bin/sh\npath=\"\"\nprev=\"\"\nfor a in \"$@\"; do\n  if [ \"$prev\" = \"-o\" ]; then path=\"$a\"; fi\n  prev=\"$a\"\ndone\nprintf 'new-loci\\n' > \"$path\"\n"
}

fn write_file_at_out_dir() -> &'static str {
    "#!/bin/sh\nout_dir=\"\"\nname=\"\"\nextra=\"\"\nprev=\"\"\nfor a in \"$@\"; do\n  if [ \"$prev\" = \"--out-dir\" ]; then out_dir=\"$a\"; fi\n  if [ \"$prev\" = \"--crate-name\" ]; then name=\"$a\"; fi\n  if [ \"$prev\" = \"-C\" ]; then\n    case \"$a\" in extra-filename=*) extra=\"${a#extra-filename=}\" ;; esac\n  fi\n  prev=\"$a\"\ndone\nprintf 'new-loci\\n' > \"$out_dir/${name}${extra}\"\n"
}

#[test]
fn a_release_loci_output_is_copied_to_home_local_bin() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let out = work.path().join("release/loci");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();

    let status = Command::new(wrapper())
        .arg(write_fake_rustc(work.path(), write_file_at_dash_o()))
        .arg("-o")
        .arg(&out)
        .env("HOME", home.path())
        .env_remove("LOCI_SKIP_RELEASE_INSTALL")
        .status()
        .unwrap();

    assert!(status.success(), "wrapper must succeed when rustc succeeds");
    let installed = home.path().join(".local/bin/loci");
    assert_eq!(
        std::fs::read_to_string(&installed).unwrap(),
        "new-loci\n",
        "the file `loci` on PATH must be the binary cargo just linked"
    );
    let mode = std::fs::metadata(&installed).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111, "the installed copy must be executable");
}

#[test]
fn cargo_release_deps_loci_bin_is_copied_to_home_local_bin() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let out_dir = work.path().join("release/deps");
    std::fs::create_dir_all(&out_dir).unwrap();

    let status = Command::new(wrapper())
        .arg(write_fake_rustc(work.path(), write_file_at_out_dir()))
        .arg("--crate-name")
        .arg("loci")
        .arg("--crate-type")
        .arg("bin")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg("-C")
        .arg("extra-filename=-70df160e80adddc0")
        .env("HOME", home.path())
        .env_remove("LOCI_SKIP_RELEASE_INSTALL")
        .status()
        .unwrap();

    assert!(status.success());
    assert_eq!(
        std::fs::read_to_string(home.path().join(".local/bin/loci")).unwrap(),
        "new-loci\n",
        "cargo writes the bin under release/deps; that file is what PATH must get"
    );
}

#[test]
fn a_debug_loci_bin_is_not_installed() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let out_dir = work.path().join("debug/deps");
    std::fs::create_dir_all(&out_dir).unwrap();

    let status = Command::new(wrapper())
        .arg(write_fake_rustc(work.path(), write_file_at_out_dir()))
        .arg("--crate-name")
        .arg("loci")
        .arg("--crate-type")
        .arg("bin")
        .arg("--out-dir")
        .arg(&out_dir)
        .arg("-C")
        .arg("extra-filename=-abc")
        .env("HOME", home.path())
        .status()
        .unwrap();

    assert!(status.success());
    assert!(
        !home.path().join(".local/bin/loci").exists(),
        "cargo build (debug) must not touch ~/.local/bin/loci"
    );
}

#[test]
fn a_dep_crate_in_release_deps_is_not_installed() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let out = work.path().join("release/deps/libloci_parse.rlib");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();

    let status = Command::new(wrapper())
        .arg(write_fake_rustc(work.path(), write_file_at_dash_o()))
        .arg("-o")
        .arg(&out)
        .env("HOME", home.path())
        .status()
        .unwrap();

    assert!(status.success());
    assert!(
        !home.path().join(".local/bin/loci").exists(),
        "linking a dependency must not touch ~/.local/bin/loci"
    );
}

#[test]
fn a_failed_rustc_does_not_install_and_keeps_the_exit_code() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let out = work.path().join("release/loci");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();

    let status = Command::new(wrapper())
        .arg(write_fake_rustc(work.path(), "#!/bin/sh\nexit 1\n"))
        .arg("-o")
        .arg(&out)
        .env("HOME", home.path())
        .status()
        .unwrap();

    assert_eq!(status.code(), Some(1));
    assert!(!home.path().join(".local/bin/loci").exists());
}
