//! Build script to embed git commit hash and build date in the binary.

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

    // Build date (YYYY-MM-DD)
    let build_date = chrono_free_date();

    println!("cargo:rustc-env=KNIT_GIT_HASH={git_hash}");
    println!("cargo:rustc-env=KNIT_BUILD_DATE={build_date}");

    // Re-run if HEAD changes (new commits)
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/");
}

/// Get current date as YYYY-MM-DD without pulling in chrono.
fn chrono_free_date() -> String {
    // Use git log as a portable date source
    Command::new("git")
        .args(["log", "-1", "--format=%cd", "--date=short"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}
