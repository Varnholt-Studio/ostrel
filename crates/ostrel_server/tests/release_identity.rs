//! AC-12: the development identity cannot reach a release build (D127).
//!
//! A plain `cargo test` builds this file in the debug profile. To check a real release build,
//! each test runs itself again through Cargo in two further builds, in a target directory of
//! its own (the outer `cargo test` keeps its own directory locked):
//!
//! * the release profile, without features: a supplied development identity is anonymous and
//!   the test executable, which links the server crate, does not contain the marker
//!   `ostrel-dev-identity`;
//! * the debug profile with the feature `dev-identity`, as control: the development identity
//!   applies and the marker is present, so the marker scan can fail.
//!
//! Each nested run sets [`NESTED`], so it checks its own build and never starts another one.
//! Finally `cargo check --release --features dev-identity` must fail with the `compile_error!`
//! of the crate root: the feature cannot be combined with a release profile at all.

use std::path::PathBuf;
use std::process::{Command, Output};

use ostrel_core::ids::RowId;
use ostrel_server::identity::{Credentials, DEV_IDENTITY_ENABLED, NoTokens, resolve_principal};

/// Set in the environment of a nested run.
const NESTED: &str = "OSTREL_AC12_NESTED";

/// The marker of D127, built at run time so that this file does not put it into the
/// executable it scans.
fn marker() -> Vec<u8> {
    ["ostrel", "dev", "identity"].join("-").into_bytes()
}

fn user() -> RowId {
    RowId::from_hex(&format!("{:032x}", 1)).unwrap()
}

/// The principal of a request that carries only a development identity.
fn principal_of_dev_identity() -> Option<RowId> {
    let creds = Credentials::anonymous().with_dev_identity(user());
    resolve_principal(&creds, &NoTokens).unwrap()
}

/// Whether the running test executable contains the marker.
fn executable_has_marker() -> bool {
    let exe = std::env::current_exe().unwrap();
    let bytes = std::fs::read(&exe).unwrap();
    let needle = marker();
    bytes.windows(needle.len()).any(|w| w == needle.as_slice())
}

/// Runs `cargo <command> <args>` on this package in the target directory of this file.
fn cargo(command: &str, args: &[&str]) -> Output {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let outer = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    Command::new(cargo)
        .arg(command)
        .arg("--manifest-path")
        .arg(manifest)
        .args(["--offline", "--locked"])
        .args(args)
        .env("CARGO_TARGET_DIR", outer.join("ac_12"))
        .env(NESTED, "1")
        .output()
        .unwrap()
}

/// Runs the test `name` of this file in a release build and in a debug build with the
/// feature; both must pass.
fn run_nested(name: &str) {
    let builds: [&[&str]; 2] = [&["--release"], &["--features", "dev-identity"]];
    for build in builds {
        let mut args = vec!["--test", "release_identity"];
        args.extend_from_slice(build);
        args.extend(["--", "--exact", name]);
        let out = cargo("test", &args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "nested run {build:?} of {name} failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// True in the outer run of a debug build: it must start the nested runs.
fn is_outer_debug_run() -> bool {
    cfg!(debug_assertions) && std::env::var_os(NESTED).is_none()
}

#[test]
fn ac_12_release_ignores_dev_identity() {
    if is_outer_debug_run() {
        run_nested("ac_12_release_ignores_dev_identity");
    }
    // The feature exists only in debug builds, so a release build always takes the second arm.
    let expected = if DEV_IDENTITY_ENABLED {
        Some(user())
    } else {
        None
    };
    assert_eq!(principal_of_dev_identity(), expected);
}

#[test]
fn ac_12_release_binary_has_no_dev_marker() {
    if is_outer_debug_run() {
        run_nested("ac_12_release_binary_has_no_dev_marker");
    }
    // Keeps the server code that would carry the marker linked into this executable.
    let _ = principal_of_dev_identity();
    assert_eq!(executable_has_marker(), DEV_IDENTITY_ENABLED);
}

#[test]
fn ac_12_release_build_rejects_dev_identity_feature() {
    if !is_outer_debug_run() {
        return;
    }
    let out = cargo("check", &["--release", "--features", "dev-identity"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "release build with dev-identity compiled"
    );
    assert!(
        stderr.contains("only allowed in debug builds"),
        "release build failed for another reason:\n{stderr}"
    );
}
