//! Port of `scripts/set-token-context.js` — implements the `token-tracker
//! set-context` command (`--project NAME --feature NAME [--workspace PATH]
//! [--clear-feature|--clear]`).
//!
//! This module is also reused, via CLI-level flag remapping done elsewhere
//! (main.rs / the command dispatcher), to implement `token-tracker
//! set-feature`. That remapping is out of scope for this file — callers
//! just need to build an appropriate argv slice (or `Args` value) and call
//! [`run`].
//!
//! Notes on deliberate differences from the JS source:
//! - The JS script has no `module.exports`; it is a bare script guarded by
//!   `if (require.main === module) main();`. There is nothing to "export"
//!   in the JS sense. Every top-level function is ported here as a `pub`
//!   item instead, so `main.rs` (and any `set-feature` shim) can call into
//!   it directly rather than shelling out to a script.
//! - `path.resolve()` has no direct std equivalent that (a) doesn't require
//!   the path to exist and (b) doesn't touch the filesystem. `resolve_workspace`
//!   reimplements its lexical-normalization behavior (join against cwd if
//!   relative, then collapse `.`/`..` components) without calling
//!   `fs::canonicalize`, matching Node's `path.resolve` semantics for
//!   non-existent paths.
//! - JS's `next()` helper can leave a flag's value as `undefined` if the
//!   flag is the last argv token (e.g. a trailing bare `--workspace`), which
//!   later causes `path.resolve(undefined)` to throw a `TypeError` deep in
//!   `main()`. Rust has no `undefined`; a trailing flag with no value here
//!   yields an empty string for that flag instead of crashing later. This is
//!   a deliberate graceful-degradation deviation — replicating a JS
//!   crash-on-`undefined` code path was judged not worth chasing.
//! - `ensure_object_field` resets a config field to `{}` whenever it isn't
//!   a JSON object. JS's equivalent guard (`!config.x || typeof config.x
//!   !== "object"`) technically *keeps* a JSON array in place (arrays are
//!   truthy and `typeof [] === "object"`), which would then go on to break
//!   downstream `object[workspace] = ...` assignments in confusing ways.
//!   Treating a non-object (array included) as "needs reset" is simpler,
//!   still self-consistent, and only differs from JS on already-malformed
//!   config files.
//! - **Key ordering**: `serde_json::Map` sorts keys alphabetically unless
//!   the crate's `preserve_order` feature is enabled (that decision belongs
//!   to the integration step's `Cargo.toml`, which this file is not allowed
//!   to write). JS objects preserve insertion order, so `config.json`'s key
//!   order will differ from the original JS output unless
//!   `serde_json/preserve_order` is turned on. Values and field names are
//!   unaffected — only the on-disk key order changes.

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::paths;

/// Parsed CLI arguments for `set-context` (and, via remapping, `set-feature`).
#[derive(Debug, Clone)]
pub struct Args {
    pub workspace: String,
    pub project: Option<String>,
    pub feature: Option<String>,
    pub clear_feature: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            workspace: env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".to_string()),
            project: None,
            feature: None,
            clear_feature: false,
        }
    }
}

/// JSON payload printed to stdout on success, mirroring the JS
/// `console.log(JSON.stringify({ workspace, project, feature, tokens_reset }))`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SetContextResult {
    pub workspace: String,
    pub project: Option<String>,
    pub feature: Option<String>,
    pub tokens_reset: bool,
}

/// A parse failure: the message to print to stderr and the process exit
/// code to use (always 2, matching JS's `process.exit(2)` on an unknown
/// argument).
pub type ParseError = (String, i32);

/// Parse an argv slice (already excluding the program name / subcommand,
/// analogous to JS's `process.argv.slice(2)`).
///
/// Recognized flags: `--workspace <path>`, `--project <name>`,
/// `--feature <name>`, `--clear-feature` / `--clear`. A single bare
/// positional token is accepted as a feature name (only if `--feature`
/// hasn't already been set and `--clear-feature` wasn't passed) — this
/// mirrors the JS quirk allowing `set-token-context.js maintenance`.
/// Any other unrecognized argument is a parse error.
pub fn parse_args(argv: &[String]) -> Result<Args, ParseError> {
    let mut args = Args::default();
    let mut i = 0usize;
    while i < argv.len() {
        let a = argv[i].as_str();
        match a {
            "--workspace" => {
                i += 1;
                args.workspace = argv.get(i).cloned().unwrap_or_default();
            }
            "--project" => {
                i += 1;
                args.project = argv.get(i).cloned();
            }
            "--feature" => {
                i += 1;
                args.feature = argv.get(i).cloned();
            }
            "--clear-feature" | "--clear" => {
                args.clear_feature = true;
            }
            _ if !a.starts_with('-') && args.feature.is_none() && !args.clear_feature => {
                // Positional feature name: `set-token-context.js maintenance`
                args.feature = Some(a.to_string());
            }
            other => {
                return Err((format!("token-tracker: unknown argument: {}", other), 2));
            }
        }
        i += 1;
    }
    Ok(args)
}

/// True if `opt` holds a JS-truthy string (present and non-empty) — mirrors
/// JS's implicit `if (args.project)` / `x || null` truthiness checks used
/// throughout the original script for these fields.
fn is_truthy(opt: &Option<String>) -> bool {
    matches!(opt, Some(s) if !s.is_empty())
}

/// `map.get(key) || null` for string-valued fields: returns `None` unless
/// the key holds a non-empty string.
fn truthy_string_field(map: &Map<String, Value>, key: &str) -> Option<String> {
    match map.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// Ensure `config[key]` is a JSON object, resetting it to `{}` if it is
/// missing or not an object. See the module-level doc comment for the one
/// documented deviation from JS here (JSON arrays are also reset).
fn ensure_object_field(config: &mut Map<String, Value>, key: &str) {
    let is_object = matches!(config.get(key), Some(Value::Object(_)));
    if !is_object {
        config.insert(key.to_string(), Value::Object(Map::new()));
    }
}

/// Resolve the config.json path via `crate::paths` (mirrors JS `configPath()`,
/// which is `paths().configPath`).
pub fn config_path() -> Result<PathBuf> {
    Ok(paths::paths(None)?.config_path)
}

/// Load `config.json` as a generic JSON object, preserving any fields this
/// module doesn't know about (mirrors JS `loadConfig`, which returns the
/// parsed payload as-is when it's a non-array object).
///
/// - Missing file: returns a fresh config with `default_project`,
///   `default_feature`, `projects`, `features` all present (matching JS's
///   default literal).
/// - Present but not a JSON object (or a JSON array): returns an empty map,
///   matching JS's `payload && typeof payload === "object" &&
///   !Array.isArray(payload) ? payload : {}` fallback.
pub fn load_config() -> Result<Map<String, Value>> {
    let file = config_path()?;
    if !file.exists() {
        let mut m = Map::new();
        m.insert("default_project".to_string(), Value::Null);
        m.insert("default_feature".to_string(), Value::Null);
        m.insert("projects".to_string(), Value::Object(Map::new()));
        m.insert("features".to_string(), Value::Object(Map::new()));
        return Ok(m);
    }
    let raw = fs::read_to_string(&file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let value: Value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {} as JSON", file.display()))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Ok(Map::new()),
    }
}

/// Write `config` back to `config.json` as pretty-printed JSON with a
/// trailing newline, creating the parent directory if needed (mirrors JS
/// `saveConfig`: `fs.mkdirSync(dir, { recursive: true })` +
/// `` `${JSON.stringify(config, null, 2)}\n` ``).
pub fn save_config(config: &Map<String, Value>) -> Result<()> {
    let file = config_path()?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create data directory {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(config).context("failed to serialize config")?;
    fs::write(&file, format!("{}\n", body))
        .with_context(|| format!("failed to write {}", file.display()))?;
    Ok(())
}

/// Lexically normalize a path (collapse `.` and `..` components) without
/// touching the filesystem — the piece of Node's `path.resolve` that
/// `Path`/`PathBuf` don't provide directly (`fs::canonicalize` requires the
/// path to exist, which a workspace path may not).
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !result.pop() {
                    result.push(component.as_os_str());
                }
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    result
}

/// Resolve a `--workspace` value the way JS does:
/// `path.resolve(raw.startsWith("~/") ? join(home, raw.slice(2)) : raw)`.
/// Expands a leading `~/` via `crate::paths::expand`, joins against the
/// current directory if relative, then lexically normalizes.
pub fn resolve_workspace(raw: &str) -> Result<PathBuf> {
    let expanded = paths::expand(raw);
    let combined = if expanded.is_absolute() {
        expanded
    } else {
        env::current_dir()
            .context("failed to determine current directory")?
            .join(expanded)
    };
    Ok(normalize_path(&combined))
}

/// Run `set-context` end to end: parse argv, load config.json, apply the
/// project/feature/clear-feature update, reset the per-workspace token
/// baseline when the effective project or feature changed, save config.json,
/// and print the JSON result line to stdout — mirroring JS `main()`.
///
/// `argv` should already exclude the program name / subcommand (i.e. it's
/// the JS `process.argv.slice(2)` equivalent).
///
/// Returns the process exit code the caller should use (`0` on success,
/// `2` on an unknown-argument parse error — matching JS's `process.exit(2)`).
/// I/O and JSON errors are propagated via `Err` for the caller to report.
pub fn run(argv: &[String]) -> Result<i32> {
    let args = match parse_args(argv) {
        Ok(a) => a,
        Err((msg, code)) => {
            eprintln!("{}", msg);
            return Ok(code);
        }
    };

    let workspace_path = resolve_workspace(&args.workspace)?;
    let workspace = workspace_path.to_string_lossy().into_owned();

    let mut config = load_config()?;

    if !config.contains_key("default_project") {
        config.insert("default_project".to_string(), Value::Null);
    }
    if !config.contains_key("default_feature") {
        config.insert("default_feature".to_string(), Value::Null);
    }
    ensure_object_field(&mut config, "projects");
    ensure_object_field(&mut config, "features");

    let prev_project = {
        let projects = config["projects"].as_object().expect("ensured object");
        truthy_string_field(projects, &workspace)
    };
    let prev_feature = {
        let features = config["features"].as_object().expect("ensured object");
        truthy_string_field(features, &workspace)
    };

    if is_truthy(&args.project) {
        config["projects"]
            .as_object_mut()
            .expect("ensured object")
            .insert(workspace.clone(), Value::String(args.project.clone().unwrap()));
    }
    if args.clear_feature {
        config["features"]
            .as_object_mut()
            .expect("ensured object")
            .remove(&workspace);
    } else if is_truthy(&args.feature) {
        config["features"]
            .as_object_mut()
            .expect("ensured object")
            .insert(workspace.clone(), Value::String(args.feature.clone().unwrap()));
    }

    let next_project = {
        let projects = config["projects"].as_object().expect("ensured object");
        truthy_string_field(projects, &workspace)
    };
    let next_feature = {
        let features = config["features"].as_object().expect("ensured object");
        truthy_string_field(features, &workspace)
    };

    let switched = is_truthy(&args.project) || is_truthy(&args.feature) || args.clear_feature;
    let changed = prev_project != next_project || prev_feature != next_feature;

    // Reset feature-scoped token display on project/feature switch.
    if switched && changed {
        ensure_object_field(&mut config, "token_baselines");
        let mut baseline = Map::new();
        baseline.insert(
            "project".to_string(),
            next_project.clone().map(Value::String).unwrap_or(Value::Null),
        );
        baseline.insert(
            "feature".to_string(),
            next_feature.clone().map(Value::String).unwrap_or(Value::Null),
        );
        baseline.insert("pending_reset".to_string(), Value::Bool(true));
        config["token_baselines"]
            .as_object_mut()
            .expect("ensured object")
            .insert(workspace.clone(), Value::Object(baseline));
    }

    save_config(&config)?;

    let result = SetContextResult {
        workspace,
        project: next_project,
        feature: next_feature,
        tokens_reset: switched && changed,
    };
    println!(
        "{}",
        serde_json::to_string(&result).context("failed to serialize result")?
    );

    Ok(0)
}
