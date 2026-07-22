//! Agent-neutral data home for token-tracker.
//!
//! Primary:  ~/.token-tracker/
//! Legacy:   ~/.cursor/token-tracker/  (migrated once on first use)
//!
//! Override the whole tree with TOKEN_TRACKER_HOME, or individual files with
//! TOKEN_TRACKER_CONFIG / TOKEN_TRACKER_HISTORY / TOKEN_TRACKER_PRICES.
//!
//! Faithful port of scripts/paths.js. Notes on deliberate differences from
//! the JS source:
//! - JS exports a `LEGACY_DATA_DIR` constant computed once at module load.
//!   Rust has no ambient module-load side effects here, so `legacy_data_dir()`
//!   (a cheap, deterministic function) stands in for both the JS constant and
//!   the JS `legacyDataDir()` function.
//! - JS's `migrateLegacyDataDir` and `ensureDataDir` can throw synchronously
//!   (e.g. `fs.mkdirSync`/`fs.copyFileSync` failures) despite otherwise
//!   returning a plain result object; here that is modeled as
//!   `anyhow::Result<...>` so callers can propagate the error with `?`
//!   instead of panicking.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};

/// The three files that make up a token-tracker data directory.
pub const DATA_FILES: [&str; 3] = ["config.json", "history.jsonl", "prices.json"];

/// Result of `migrate_legacy_data_dir`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MigrationResult {
    pub migrated: bool,
    pub reason: String,
    #[serde(rename = "dataDir")]
    pub data_dir: PathBuf,
    pub legacy: PathBuf,
    /// `Some(names)` only when `migrated` is true; mirrors JS where `copied`
    /// is simply absent from the returned object on the non-migrated paths.
    pub copied: Option<Vec<String>>,
}

/// Result of `ensure_data_dir`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EnsureResult {
    #[serde(rename = "dataDir")]
    pub data_dir: PathBuf,
    pub migration: MigrationResult,
}

/// Result of `paths()` — the resolved set of file locations token-tracker
/// reads/writes, mirroring the JS `paths()` return object field-for-field.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Paths {
    #[serde(rename = "dataDir")]
    pub data_dir: PathBuf,
    #[serde(rename = "legacyDataDir")]
    pub legacy_data_dir: PathBuf,
    #[serde(rename = "configPath")]
    pub config_path: PathBuf,
    #[serde(rename = "historyPath")]
    pub history_path: PathBuf,
    #[serde(rename = "pricesPath")]
    pub prices_path: PathBuf,
}

/// Home directory, falling back to "." if it cannot be determined (mirrors
/// the practical reliability of Node's `os.homedir()`, which effectively
/// never fails on supported platforms).
fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// Expand a leading `~/` to the user's home directory. Empty input is
/// returned unchanged (mirrors JS `expand`'s `if (!p) return p;` guard).
pub fn expand(p: &str) -> PathBuf {
    if p.is_empty() {
        return PathBuf::from(p);
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(p)
}

/// Resolve the shared data directory: `TOKEN_TRACKER_HOME` (expanded) if set
/// and non-empty, otherwise `~/.token-tracker`.
pub fn resolve_data_dir() -> PathBuf {
    if let Some(val) = env::var("TOKEN_TRACKER_HOME").ok().filter(|v| !v.is_empty()) {
        return expand(&val);
    }
    home_dir().join(".token-tracker")
}

/// The legacy pre-migration data directory: `~/.cursor/token-tracker`.
/// Stands in for both JS's `LEGACY_DATA_DIR` constant and `legacyDataDir()`.
pub fn legacy_data_dir() -> PathBuf {
    home_dir().join(".cursor").join("token-tracker")
}

/// True if `dir` exists and contains at least one of the known data files.
pub fn has_any_data(dir: &Path) -> bool {
    if dir.as_os_str().is_empty() || !dir.exists() {
        return false;
    }
    DATA_FILES.iter().any(|name| dir.join(name).exists())
}

/// One-time copy from `~/.cursor/token-tracker` -> the resolved data dir when
/// the new home is empty and legacy data exists. Never deletes the legacy
/// folder.
pub fn migrate_legacy_data_dir(data_dir: Option<PathBuf>) -> Result<MigrationResult> {
    let data_dir = data_dir.unwrap_or_else(resolve_data_dir);
    let legacy = legacy_data_dir();

    if data_dir == legacy {
        return Ok(MigrationResult {
            migrated: false,
            reason: "same_dir".to_string(),
            data_dir,
            legacy,
            copied: None,
        });
    }
    if has_any_data(&data_dir) {
        return Ok(MigrationResult {
            migrated: false,
            reason: "destination_exists".to_string(),
            data_dir,
            legacy,
            copied: None,
        });
    }
    if !has_any_data(&legacy) {
        return Ok(MigrationResult {
            migrated: false,
            reason: "no_legacy".to_string(),
            data_dir,
            legacy,
            copied: None,
        });
    }

    fs::create_dir_all(&data_dir)
        .with_context(|| format!("failed to create data directory {}", data_dir.display()))?;

    let mut copied: Vec<String> = Vec::new();
    for name in DATA_FILES.iter() {
        let from = legacy.join(name);
        let to = data_dir.join(name);
        if !from.exists() || to.exists() {
            continue;
        }
        fs::copy(&from, &to).with_context(|| {
            format!("failed to copy {} to {}", from.display(), to.display())
        })?;
        copied.push((*name).to_string());
    }

    // Marker so users can see migration happened. JS swallows write errors
    // here (`try { ... } catch { /* ignore */ }`); do the same.
    let marker_path = data_dir.join("MIGRATED_FROM_CURSOR");
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let files_line = if copied.is_empty() {
        "(none)".to_string()
    } else {
        copied.join(", ")
    };
    let contents = format!(
        "Copied from {} at {}\nFiles: {}\n",
        legacy.display(),
        timestamp,
        files_line
    );
    let _ = fs::write(&marker_path, contents);

    Ok(MigrationResult {
        migrated: true,
        reason: "copied".to_string(),
        data_dir,
        legacy,
        copied: Some(copied),
    })
}

/// Ensure the data directory exists, running the legacy migration first.
pub fn ensure_data_dir(data_dir: Option<PathBuf>) -> Result<EnsureResult> {
    let data_dir = data_dir.unwrap_or_else(resolve_data_dir);
    let migration = migrate_legacy_data_dir(Some(data_dir.clone()))?;
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("failed to create data directory {}", data_dir.display()))?;
    Ok(EnsureResult { data_dir, migration })
}

/// Resolve a single file's path: the env override (expanded) if set and
/// non-empty, otherwise `data_dir/file_name`.
pub fn resolve_path(env_key: &str, file_name: &str, data_dir: Option<PathBuf>) -> PathBuf {
    if let Some(val) = env::var(env_key).ok().filter(|v| !v.is_empty()) {
        return expand(&val);
    }
    let data_dir = data_dir.unwrap_or_else(resolve_data_dir);
    data_dir.join(file_name)
}

/// Resolve the full set of token-tracker paths, ensuring the data directory
/// (and any one-time legacy migration) has happened first.
pub fn paths(data_dir: Option<PathBuf>) -> Result<Paths> {
    let data_dir = data_dir.unwrap_or_else(resolve_data_dir);
    ensure_data_dir(Some(data_dir.clone()))?;
    Ok(Paths {
        config_path: resolve_path("TOKEN_TRACKER_CONFIG", "config.json", Some(data_dir.clone())),
        history_path: resolve_path(
            "TOKEN_TRACKER_HISTORY",
            "history.jsonl",
            Some(data_dir.clone()),
        ),
        prices_path: resolve_path("TOKEN_TRACKER_PRICES", "prices.json", Some(data_dir.clone())),
        legacy_data_dir: legacy_data_dir(),
        data_dir,
    })
}
