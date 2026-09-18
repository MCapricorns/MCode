//! Product gates for the fail-closed `mcode` command skeleton.
//!
//! These tests clear the child environment before every invocation. They do
//! not inspect or forward ambient credentials, and the only product sentinel
//! supplied is a dummy `MCODE_FAKE` value.

use std::process::Output;

use assert_cmd::Command;
use tempfile::TempDir;

const SETUP_GUIDANCE: &str =
    "product commands are unavailable: first-party features are not yet delivered in this build";

/// Creates the isolated `mcode` process under test.
fn mcode() -> Command {
    let mut command = assert_cmd::cargo::cargo_bin_cmd!("mcode");
    command.env_clear();
    command
}

fn output(command: &mut Command) -> Output {
    command.output().expect("mcode process must start")
}

fn assert_setup_failure(output: &Output) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(SETUP_GUIDANCE), "{stderr}");
}

#[test]
fn run_and_resume_fail_closed_with_both_pack_instructions() {
    let invocations: &[&[&str]] = &[&["run", "hello"], &["resume", "latest", "continue"]];
    for (index, arguments) in invocations.iter().enumerate() {
        let sandbox = TempDir::new().expect("temporary test root");
        let home = sandbox.path().join(format!("home-{index}"));
        let cwd = sandbox.path().join(format!("cwd-{index}"));
        let mut command = mcode();
        command
            .env("MCODE_HOME", &home)
            .arg("--cwd")
            .arg(&cwd)
            .args(*arguments);
        assert_setup_failure(&output(&mut command));
        assert!(!home.exists(), "MCODE_HOME must not be created");
        assert!(!cwd.exists(), "--cwd must not be accessed or created");
    }
}

#[test]
fn fake_environment_and_missing_cwd_do_not_change_setup_failure_or_create_state() {
    let sandbox = TempDir::new().expect("temporary test root");
    let home = sandbox.path().join("mcode-home-must-stay-absent");
    let cwd = sandbox.path().join("cwd-must-stay-absent");

    let mut baseline = mcode();
    baseline
        .env("MCODE_HOME", &home)
        .arg("--cwd")
        .arg(&cwd)
        .args(["run", "hello"]);
    let baseline = output(&mut baseline);
    assert_setup_failure(&baseline);

    let mut fake = mcode();
    fake.env("MCODE_HOME", &home)
        .env("MCODE_FAKE", "dummy-sentinel-that-must-not-be-read")
        .arg("--cwd")
        .arg(&cwd)
        .args(["run", "hello"]);
    let fake = output(&mut fake);
    assert_setup_failure(&fake);

    assert_eq!(fake.stderr, baseline.stderr);
    assert!(!home.exists(), "MCODE_HOME must not be created");
    assert!(!cwd.exists(), "--cwd must not be accessed or created");
}

#[test]
fn legacy_product_flags_are_clap_usage_errors() {
    for flag in ["--provider", "--profile", "--model", "--fake", "--yolo"] {
        let mut command = mcode();
        command.args([flag, "dummy", "run", "hello"]);
        let output = output(&mut command);
        assert_eq!(output.status.code(), Some(2), "{flag}: {output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("unexpected argument '{flag}' found")),
            "{flag}: {stderr}"
        );
    }
}

#[test]
fn run_without_prompt_prints_usage() {
    let mut command = mcode();
    command.arg("run");
    let output = output(&mut command);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage:"), "{stderr}");
    assert!(stderr.contains("run <PROMPT>"), "{stderr}");
}

#[test]
fn resume_without_prompt_prints_usage() {
    let mut command = mcode();
    command.args(["resume", "latest"]);
    let output = output(&mut command);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Usage:"), "{stderr}");
    assert!(stderr.contains("resume <SESSION> <PROMPT>"), "{stderr}");
}
