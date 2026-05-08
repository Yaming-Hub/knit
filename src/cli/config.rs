//! Configuration file and environment variable support for knit.
//!
//! Resolution order (highest priority first):
//! 1. CLI flags
//! 2. Environment variables (`KNIT_SEED`, `KNIT_PARALLEL`, etc.)
//! 3. Config file (`knit.toml` in cwd, then `~/.config/knit/config.toml`)

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;
use tracing::debug;

/// Top-level config file structure.
#[derive(Debug, Default, Deserialize)]
pub struct KnitConfig {
    /// Default settings that can be overridden by CLI flags.
    #[serde(default)]
    pub defaults: ConfigDefaults,
}

/// Default values from `[defaults]` section of config.
#[derive(Debug, Default, Deserialize)]
pub struct ConfigDefaults {
    /// Default random seed.
    pub seed: Option<u64>,
    /// Default output format.
    pub format: Option<String>,
    /// Default parallelism.
    pub parallel: Option<usize>,
    /// Default batch size.
    pub batch_size: Option<usize>,
}

/// Resolved configuration after merging config file + env vars.
#[derive(Debug, Default)]
pub struct ResolvedConfig {
    /// Random seed override.
    pub seed: Option<u64>,
    /// Output format override.
    pub format: Option<String>,
    /// Parallelism override.
    pub parallel: Option<usize>,
    /// Batch size override.
    pub batch_size: Option<usize>,
}

/// Locate the config file by searching cwd then the global config directory.
///
/// Checks `knit.toml` in the current directory first, then
/// `~/.config/knit/config.toml` (or platform equivalent).
pub fn find_config_file() -> Option<PathBuf> {
    // Check cwd
    let local = Path::new("knit.toml");
    if local.is_file() {
        debug!("found local config: {}", local.display());
        return Some(local.to_path_buf());
    }

    // Check global config dir
    if let Some(config_dir) = dirs::config_dir() {
        let global = config_dir.join("knit").join("config.toml");
        if global.is_file() {
            debug!("found global config: {}", global.display());
            return Some(global);
        }
    }

    None
}

/// Load and parse a config file from the given path.
fn load_config_file(path: &Path) -> Result<KnitConfig> {
    let content = fs::read_to_string(path)?;
    let config: KnitConfig = toml::from_str(&content)?;
    Ok(config)
}

/// Read environment variables for knit configuration.
fn read_env_vars() -> ResolvedConfig {
    ResolvedConfig {
        seed: env::var("KNIT_SEED").ok().and_then(|v| v.parse().ok()),
        format: env::var("KNIT_FORMAT").ok(),
        parallel: env::var("KNIT_PARALLEL").ok().and_then(|v| v.parse().ok()),
        batch_size: env::var("KNIT_BATCH_SIZE")
            .ok()
            .and_then(|v| v.parse().ok()),
    }
}

/// Resolve the full configuration by loading the config file and overlaying
/// environment variables. CLI flags are applied by the caller.
pub fn resolve_config() -> ResolvedConfig {
    let file_cfg = find_config_file()
        .and_then(|p| {
            load_config_file(&p)
                .map_err(|e| {
                    debug!("failed to load config file: {}", e);
                    e
                })
                .ok()
        })
        .unwrap_or_default();

    let env_cfg = read_env_vars();

    // Env vars override config file values
    ResolvedConfig {
        seed: env_cfg.seed.or(file_cfg.defaults.seed),
        format: env_cfg.format.or(file_cfg.defaults.format),
        parallel: env_cfg.parallel.or(file_cfg.defaults.parallel),
        batch_size: env_cfg.batch_size.or(file_cfg.defaults.batch_size),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_config_toml() {
        let toml_str = r#"
[defaults]
seed = 42
format = "parquet"
parallel = 8
batch_size = 10000
"#;
        let cfg: KnitConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.defaults.seed, Some(42));
        assert_eq!(cfg.defaults.format.as_deref(), Some("parquet"));
        assert_eq!(cfg.defaults.parallel, Some(8));
        assert_eq!(cfg.defaults.batch_size, Some(10000));
    }

    #[test]
    fn parse_empty_config() {
        let cfg: KnitConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.defaults.seed, None);
        assert_eq!(cfg.defaults.format, None);
    }

    #[test]
    fn env_vars_override_config() {
        // Set env vars temporarily
        env::set_var("KNIT_SEED", "99");
        let env_cfg = read_env_vars();
        assert_eq!(env_cfg.seed, Some(99));
        env::remove_var("KNIT_SEED");
    }

    #[test]
    fn load_config_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("knit.toml");
        let mut f = fs::File::create(&path).unwrap();
        writeln!(f, "[defaults]\nseed = 123").unwrap();

        let cfg = load_config_file(&path).unwrap();
        assert_eq!(cfg.defaults.seed, Some(123));
    }
}
