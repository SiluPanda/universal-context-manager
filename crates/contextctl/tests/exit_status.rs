use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::tempdir_in;

#[test]
fn doctor_failed_state_exits_nonzero_with_valid_json_stdout() {
    let workspace = tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("workspace");
    let output = run_failed_report(
        workspace.path(),
        &["--json", "doctor"],
        "doctor reported failed state",
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("valid doctor JSON");
    assert_eq!(report["overall"], "failed");
}

#[test]
fn setup_failed_state_exits_nonzero_after_human_and_json_output() {
    let workspace = tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("workspace");
    let project = workspace.path().join("project");
    fs::create_dir_all(&project).expect("project");

    let json_output = run_failed_report(
        workspace.path(),
        &[
            "--json",
            "setup",
            "--project",
            project.to_str().expect("project path"),
            "--adapter",
            "codex",
        ],
        "setup reported failed state",
    );
    let report: Value = serde_json::from_slice(&json_output.stdout).expect("valid setup JSON");
    assert_eq!(report["state"], "failed");

    let human_output = run_failed_report(
        workspace.path(),
        &[
            "setup",
            "--project",
            project.to_str().expect("project path"),
            "--adapter",
            "codex",
        ],
        "setup reported failed state",
    );
    assert!(String::from_utf8_lossy(&human_output.stdout).contains("Setup preflight: failed"));
}

fn run_failed_report(workspace: &Path, args: &[&str], marker: &str) -> Output {
    let home = workspace.join(format!("home-{}", args.contains(&"--json")));
    fs::create_dir_all(&home).expect("home");
    let missing_contextd = workspace.join("missing-contextd");
    let missing_mcp = workspace.join("missing-context-mcp");
    let output = Command::new(contextctl_binary())
        .args(args)
        .env("CONTEXT_MANAGER_HOME", &home)
        .env("CONTEXTD_BIN", &missing_contextd)
        .env("CONTEXT_MCP_BIN", &missing_mcp)
        .env("HOME", &home)
        .output()
        .expect("run contextctl");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(marker),
        "stderr was: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn contextctl_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_contextctl"))
}
