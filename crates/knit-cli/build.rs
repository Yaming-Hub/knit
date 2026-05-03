//! Build script to embed git commit hash and commit date in the binary.

use std::process::Command;

fn main() {
    // Git commit hash (short)
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    // Commit date (YYYY-MM-DD) — intentionally the commit date, not a wall-clock
    // build date, so that rebuilding the same commit produces identical output.
    let commit_date = commit_date();

    println!("cargo:rustc-env=KNIT_GIT_HASH={git_hash}");
    println!("cargo:rustc-env=KNIT_COMMIT_DATE={commit_date}");

    // Resolve the actual git directory (supports worktrees where .git is a file).
    let git_dir = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "../../.git".into());

    // Re-run if HEAD changes (new commits)
    println!("cargo:rerun-if-changed={git_dir}/HEAD");
    println!("cargo:rerun-if-changed={git_dir}/refs/");
}

/// Get HEAD commit date as YYYY-MM-DD without pulling in chrono.
fn commit_date() -> String {
    Command::new("git")
        .args(["log", "-1", "--format=%cd", "--date=short"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}
