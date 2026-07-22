//! `token-tracker save` — append a usage snapshot to `history.jsonl`.
//!
//! Faithful port of `scripts/save-token-usage.js`.
//!
//! Notes on deliberate differences from the JS source:
//! - JS builds `DEFAULT_HISTORY`/`DEFAULT_PRICES` once at module-load time via a
//!   top-level, zero-argument `paths()` call (both `save-token-usage.js` and
//!   `report-token-usage.js` only ever call `paths()` with no arguments — the
//!   optional `dataDir` parameter in `paths.js` is exercised directly only by
//!   `migrateLegacyDataDir` in tests, never through `paths()` itself). Rust has
//!   no module-load side effects, so [`run_inner`] instead calls
//!   `crate::paths::paths()` on demand wherever the JS closure captured
//!   `DEFAULT_HISTORY`/`DEFAULT_PRICES`. This matches the zero-arg,
//!   non-`Result` `pub fn paths() -> Paths` shape already assumed by
//!   `report.rs`, so a single future `paths.rs` implementation satisfies both
//!   callers. `paths()` is idempotent (directory-ensure + a migration check
//!   that no-ops after the first run), so recomputing it is functionally
//!   equivalent to the JS module-level cache, just not memoized.
//! - JS's `fail(message)` prints `token-tracker: <message>` to stderr and calls
//!   `process.exit(2)` immediately, so nothing after a `fail()` call ever
//!   executes. Here that is modeled as `Result<_, SaveFail>` with an early
//!   `return Err(...)`, which is behaviorally identical (JS never falls through
//!   either) and lets [`run`] be the single place that prints and exits.
//! - `process.stdin.isTTY` is checked via `std::io::IsTerminal` (stable since
//!   Rust 1.70), the closest std-only equivalent; no extra crate required.
//! - JS has no int/float distinction, so `JSON.stringify` renders any
//!   whole-valued number (e.g. `15`, or a cost that happens to land on `1`)
//!   without a trailing `.0`. `serde_json`'s default `f64` serialization always
//!   includes a decimal point for float-typed `Number`s. [`js_json_number`]
//!   picks an integer-typed `serde_json::Number` for whole values so snapshot
//!   output matches `JSON.stringify` byte-for-byte instead of drifting to
//!   `"total_tokens":15.0`.
//! - `lockSnapshotCost`/`appendSnapshot`/`main` can throw on unexpected I/O
//!   failure in JS (uncaught -> Node's default crash, exit code 1, full stack
//!   trace). Those are modeled here with `anyhow::Result` rather than
//!   `SaveFail`, and [`run`] exits `1` (not `2`) for that error class so the
//!   exit-code split mirrors JS's real behavior: `2` only for `fail()`-style
//!   validation errors, `1` for anything else gone wrong.
//!
//! ## Assumed signatures of sibling modules
//!
//! Neither `paths.rs` nor `pricing.rs` existed on disk at port time.
//!
//! `crate::paths` (identical to what `report.rs` already assumes):
//! ```ignore
//! pub struct Paths {
//!     pub data_dir: PathBuf,
//!     pub legacy_data_dir: PathBuf,
//!     pub config_path: PathBuf,
//!     pub history_path: PathBuf,
//!     pub prices_path: PathBuf,
//! }
//! pub fn paths() -> Paths;
//! /// Expands a leading "~/" to the home dir; other paths pass through.
//! pub fn expand(path: &str) -> PathBuf;
//! ```
//!
//! `crate::pricing` (extends the `Rates`/`Prices`/`load_prices` shape already
//! assumed by `report.rs` with `compute_cost_delta`, needed only here):
//! ```ignore
//! pub struct Rates { pub input_per_million_usd: f64, pub output_per_million_usd: f64 } // Serialize, snake_case field names
//! pub struct Prices { pub updated_at: Option<String>, pub source: Option<String>,
//!     pub source_url: Option<String>, pub default: Option<Rates>,
//!     pub models: std::collections::HashMap<String, Rates> }
//! pub fn load_prices(path: &std::path::Path) -> Prices;
//!
//! /// Mirrors pricing.js's `computeCostDelta(previous, current, prices)`.
//! /// `previous`/`current` are loose JSON snapshot objects (project, feature,
//! /// model, prompt_tokens, completion_tokens, total_tokens,
//! /// estimated_cost_usd); only the pricing-relevant fields are read.
//! pub struct CostDelta {
//!     pub cost_delta_usd: Option<f64>,
//!     pub estimated_cost_usd: Option<f64>,
//!     pub rates: Option<Rates>,
//! }
//! pub fn compute_cost_delta(
//!     previous: Option<&serde_json::Value>,
//!     current: &serde_json::Value,
//!     prices: &Prices,
//! ) -> CostDelta;
//! ```

use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value};

use crate::paths::{self, expand};
use crate::pricing::{compute_cost_delta, load_prices};

/// JS: `INT_FIELDS`.
pub const INT_FIELDS: [&str; 3] = ["prompt_tokens", "completion_tokens", "total_tokens"];

/// JS: `usage()`'s template literal, verbatim.
pub const USAGE: &str = r#"Usage: token-tracker save --summary "..." [options]

Required:
  --summary TEXT             Description of the work (required)

Optional:
  --project NAME             Project name (defaults to current directory name)
  --feature NAME             Feature name
  --model NAME               Model name
  --prompt-tokens N          Input token count
  --completion-tokens N      Output token count
  --total-tokens N           Total token count (computed if prompt+completion given)
  --json '...'               Full snapshot JSON object (overrides other flags)
  --source TEXT              Source label (defaults to "manual")
  --metadata-json '...'      Additional metadata as JSON object

Underscore aliases (accepted for convenience):
  --prompt_tokens            Same as --prompt-tokens
  --completion_tokens        Same as --completion-tokens
  --total_tokens             Same as --total-tokens
  --metadata_json            Same as --metadata-json
"#;

/// A `fail()`-style validation error: token-tracker should print
/// `token-tracker: <0>` to stderr and exit with code 2. Carries just the
/// message (without the `token-tracker: ` prefix), which [`run`] adds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveFail(pub String);

impl std::fmt::Display for SaveFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for SaveFail {}

/// JS: the `args` object built by `parseArgs`.
#[derive(Debug, Clone)]
pub struct SaveArgs {
    pub json: Option<String>,
    pub history: Option<String>,
    pub project: Option<String>,
    pub feature: Option<String>,
    pub summary: Option<String>,
    pub model: Option<String>,
    /// JS default: `"manual"`. Becomes `None` if `--source` is given as the
    /// last argument with no following value (mirrors JS `next()` returning
    /// `undefined`); `cleanSnapshot`'s own `|| "manual"` fallback covers that
    /// case too, so the effective default is preserved either way.
    pub source: Option<String>,
    pub prompt_tokens: Option<f64>,
    pub completion_tokens: Option<f64>,
    pub total_tokens: Option<f64>,
    pub metadata_json: Option<String>,
    pub help: bool,
}

// ---------------------------------------------------------------------------
// JS-coercion helpers (internal) — mirror the semantics pricing.rs uses for
// its own private helpers of the same names/shapes, reimplemented locally
// since payloads here can carry arbitrary JSON (e.g. via `--json`).
// ---------------------------------------------------------------------------

fn is_js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Mirrors JS `String(x)` for the value kinds that can plausibly show up in a
/// `--json`/`--metadata-json` payload.
fn value_to_js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            // JS makes no int/float distinction, so `String(123.0) === "123"`.
            // `serde_json::Number`'s own `Display` keeps a trailing `.0` for
            // float-typed whole values (e.g. one parsed from `"123.0"` in a
            // `--json` payload), so strip it explicitly to match.
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(f) = n.as_f64() {
                if f.is_finite() && f.fract() == 0.0 {
                    format!("{}", f as i64)
                } else {
                    f.to_string()
                }
            } else {
                n.to_string()
            }
        }
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(items) => items
            .iter()
            .map(value_to_js_string)
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

/// Mirrors `String(value || fallback)`.
fn js_or_string(value: Option<&Value>, fallback: &str) -> String {
    match value {
        Some(v) if is_js_truthy(v) => value_to_js_string(v),
        _ => fallback.to_string(),
    }
}

/// Mirrors JS `Number(x)` for JSON value kinds.
fn js_number(value: Option<&Value>) -> f64 {
    match value {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                0.0
            } else {
                trimmed.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Some(Value::Array(_)) | Some(Value::Object(_)) => f64::NAN,
    }
}

/// Mirrors JS `Number(x)` applied to a raw CLI token: `None` (no following
/// argument, i.e. JS `undefined`) -> NaN; empty string -> 0; otherwise a
/// best-effort numeric parse (NaN on failure), matching `js_number`'s string
/// branch above.
fn js_number_from_arg(s: Option<&String>) -> f64 {
    match s {
        None => f64::NAN,
        Some(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                0.0
            } else {
                trimmed.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
    }
}

/// Mirrors `Number.isInteger(x)`.
fn is_integer(f: f64) -> bool {
    f.is_finite() && f.fract() == 0.0
}

/// Renders an f64 the way `JSON.stringify` would render the equivalent JS
/// number: no trailing `.0` for whole values. See the module doc comment.
fn js_json_number(v: f64) -> Value {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        Value::Number(serde_json::Number::from(v as i64))
    } else {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

/// Mirrors `Number(x.toFixed(6))`.
fn round6(v: f64) -> f64 {
    format!("{:.6}", v).parse::<f64>().unwrap_or(v)
}

// ---------------------------------------------------------------------------
// Ported functions (JS `module.exports`)
// ---------------------------------------------------------------------------

/// JS: `usage()`. Prints the help text to stdout (matches `console.log`).
pub fn usage() {
    println!("{}", USAGE);
}

/// JS: `parseArgs(argv)`.
pub fn parse_args(argv: &[String]) -> Result<SaveArgs, SaveFail> {
    let mut args = SaveArgs {
        json: None,
        history: env::var("TOKEN_TRACKER_HISTORY")
            .ok()
            .filter(|v| !v.is_empty()),
        project: None,
        feature: None,
        summary: None,
        model: None,
        source: Some("manual".to_string()),
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        metadata_json: None,
        help: false,
    };

    let mut i = 0usize;
    while i < argv.len() {
        let a = argv[i].as_str();
        match a {
            "--help" | "-h" | "help" => {
                args.help = true;
            }
            "--json" => {
                i += 1;
                args.json = argv.get(i).cloned();
            }
            "--history" => {
                i += 1;
                args.history = argv.get(i).cloned();
            }
            "--project" => {
                i += 1;
                args.project = argv.get(i).cloned();
            }
            "--feature" => {
                i += 1;
                args.feature = argv.get(i).cloned();
            }
            "--summary" => {
                i += 1;
                args.summary = argv.get(i).cloned();
            }
            "--model" => {
                i += 1;
                args.model = argv.get(i).cloned();
            }
            "--source" => {
                i += 1;
                args.source = argv.get(i).cloned();
            }
            "--prompt-tokens" | "--prompt_tokens" => {
                i += 1;
                args.prompt_tokens = Some(js_number_from_arg(argv.get(i)));
            }
            "--completion-tokens" | "--completion_tokens" => {
                i += 1;
                args.completion_tokens = Some(js_number_from_arg(argv.get(i)));
            }
            "--total-tokens" | "--total_tokens" => {
                i += 1;
                args.total_tokens = Some(js_number_from_arg(argv.get(i)));
            }
            "--metadata-json" | "--metadata_json" => {
                i += 1;
                args.metadata_json = argv.get(i).cloned();
            }
            other => {
                return Err(SaveFail(format!("unknown argument: {}", other)));
            }
        }
        i += 1;
    }
    Ok(args)
}

fn read_stdin_sync() -> String {
    let mut buf = String::new();
    match io::stdin().read_to_string(&mut buf) {
        Ok(_) => buf,
        Err(_) => String::new(),
    }
}

fn is_stdin_tty() -> bool {
    io::stdin().is_terminal()
}

/// JS: `loadPayload(args)`.
pub fn load_payload(args: &SaveArgs) -> Result<Value, SaveFail> {
    let raw: Option<String> = if let Some(j) = &args.json {
        Some(j.clone())
    } else if !is_stdin_tty() {
        Some(read_stdin_sync())
    } else {
        None
    };

    if let Some(raw) = &raw {
        if !raw.is_empty() {
            let payload: Value = serde_json::from_str(raw)
                .map_err(|err| SaveFail(format!("invalid JSON payload: {}", err)))?;
            if !matches!(payload, Value::Object(_)) {
                return Err(SaveFail("JSON payload must be an object".to_string()));
            }
            return Ok(payload);
        }
    }

    let mut payload = Map::new();
    if let Some(v) = &args.project {
        payload.insert("project".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &args.feature {
        payload.insert("feature".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &args.summary {
        payload.insert("summary".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &args.model {
        payload.insert("model".to_string(), Value::String(v.clone()));
    }
    if let Some(v) = &args.source {
        payload.insert("source".to_string(), Value::String(v.clone()));
    }

    if let Some(v) = args.prompt_tokens {
        if !v.is_nan() {
            payload.insert("prompt_tokens".to_string(), js_json_number(v));
        }
    }
    if let Some(v) = args.completion_tokens {
        if !v.is_nan() {
            payload.insert("completion_tokens".to_string(), js_json_number(v));
        }
    }
    if let Some(v) = args.total_tokens {
        if !v.is_nan() {
            payload.insert("total_tokens".to_string(), js_json_number(v));
        }
    }

    if let Some(metadata_json) = &args.metadata_json {
        if !metadata_json.is_empty() {
            let metadata: Value = serde_json::from_str(metadata_json)
                .map_err(|err| SaveFail(format!("invalid metadata JSON: {}", err)))?;
            if !matches!(metadata, Value::Object(_)) {
                return Err(SaveFail("metadata must be a JSON object".to_string()));
            }
            payload.insert("metadata".to_string(), metadata);
        }
    }

    Ok(Value::Object(payload))
}

/// JS: `cleanSnapshot(payload)`. `payload` is an arbitrary JSON object (it may
/// have come straight from `--json`), so every field is read with JS-style
/// loose coercion rather than assumed to already be the "right" type.
pub fn clean_snapshot(payload: &Value) -> Result<Value, SaveFail> {
    let summary = js_or_string(payload.get("summary"), "").trim().to_string();
    if summary.is_empty() {
        return Err(SaveFail("summary is required".to_string()));
    }

    let cwd_basename = env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_default();

    let mut snapshot = Map::new();
    snapshot.insert(
        "timestamp".to_string(),
        Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)),
    );
    snapshot.insert(
        "project".to_string(),
        Value::String(js_or_string(payload.get("project"), &cwd_basename)),
    );
    snapshot.insert(
        "source".to_string(),
        Value::String(js_or_string(payload.get("source"), "manual")),
    );
    snapshot.insert("summary".to_string(), Value::String(summary));

    if let Some(v) = payload.get("model") {
        if is_js_truthy(v) {
            snapshot.insert("model".to_string(), Value::String(value_to_js_string(v)));
        }
    }
    if let Some(v) = payload.get("feature") {
        if is_js_truthy(v) {
            snapshot.insert("feature".to_string(), Value::String(value_to_js_string(v)));
        }
    }

    for field in INT_FIELDS {
        let v = match payload.get(field) {
            None | Some(Value::Null) => continue,
            Some(v) => v,
        };
        let parsed = js_number(Some(v));
        if !is_integer(parsed) {
            return Err(SaveFail(format!("{} must be an integer", field)));
        }
        if parsed < 0.0 {
            return Err(SaveFail(format!("{} must be non-negative", field)));
        }
        snapshot.insert(field.to_string(), js_json_number(parsed));
    }

    if !snapshot.contains_key("total_tokens") {
        let prompt_v = snapshot.get("prompt_tokens").and_then(|v| v.as_f64());
        let completion_v = snapshot.get("completion_tokens").and_then(|v| v.as_f64());
        if let (Some(p), Some(c)) = (prompt_v, completion_v) {
            snapshot.insert("total_tokens".to_string(), js_json_number(p + c));
        }
    }

    match payload.get("metadata") {
        None | Some(Value::Null) => {}
        Some(v) => {
            if !matches!(v, Value::Object(_)) {
                return Err(SaveFail("metadata must be an object".to_string()));
            }
            snapshot.insert("metadata".to_string(), v.clone());
        }
    }

    Ok(Value::Object(snapshot))
}

fn load_history_rows(history_path: &Path) -> Vec<Value> {
    if !history_path.exists() {
        return Vec::new();
    }
    let contents = match fs::read_to_string(history_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut rows = Vec::new();
    for line in contents.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(row) = serde_json::from_str::<Value>(line) {
            if matches!(row, Value::Object(_)) {
                rows.push(row);
            }
        }
    }
    rows
}

/// Mirrors `feature == null || feature === "" ? null : String(feature)`,
/// treating a missing key (`None`) the same as JSON `null`.
fn scope_key(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(other) => Some(value_to_js_string(other)),
    }
}

fn last_scope_snapshot(
    rows: &[Value],
    project: Option<&Value>,
    feature: Option<&Value>,
) -> Option<Value> {
    let scope_feature = scope_key(feature);
    let project_str = js_or_string(project, "");
    let mut last: Option<Value> = None;
    for row in rows {
        let row_project = js_or_string(row.get("project"), "");
        if row_project != project_str {
            continue;
        }
        let row_feature = scope_key(row.get("feature"));
        if row_feature != scope_feature {
            continue;
        }
        last = Some(row.clone());
    }
    last
}

/// JS: `lockSnapshotCost(snapshot, historyPath)`.
pub fn lock_snapshot_cost(mut snapshot: Value, history_path: &Path) -> Value {
    let has_total = snapshot
        .get("total_tokens")
        .map(|v| !v.is_null())
        .unwrap_or(false);
    let has_prompt = snapshot
        .get("prompt_tokens")
        .map(|v| !v.is_null())
        .unwrap_or(false);
    if !has_total && !has_prompt {
        return snapshot;
    }

    let prices_path = match env::var("TOKEN_TRACKER_PRICES")
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(val) => expand(&val),
        None => paths::paths(None)
            .map(|p| p.prices_path)
            .unwrap_or_else(|_| paths::resolve_data_dir().join("prices.json")),
    };
    let prices = load_prices(&prices_path);
    let rows = load_history_rows(history_path);
    let previous = last_scope_snapshot(&rows, snapshot.get("project"), snapshot.get("feature"));
    let priced = compute_cost_delta(previous.as_ref(), &snapshot, &prices);

    let obj = snapshot
        .as_object_mut()
        .expect("clean_snapshot always returns a JSON object");

    if let Some(delta) = priced.cost_delta_usd {
        obj.insert("cost_delta_usd".to_string(), js_json_number(round6(delta)));
    }
    if let Some(est) = priced.estimated_cost_usd {
        obj.insert(
            "estimated_cost_usd".to_string(),
            js_json_number(round6(est)),
        );
    }
    if let Some(rates) = priced.rates {
        let mut rates_obj = Map::new();
        rates_obj.insert(
            "input_per_million_usd".to_string(),
            js_json_number(rates.input_per_million_usd),
        );
        rates_obj.insert(
            "output_per_million_usd".to_string(),
            js_json_number(rates.output_per_million_usd),
        );
        obj.insert("cost_rates".to_string(), Value::Object(rates_obj));
    }

    let has_metadata_object = matches!(obj.get("metadata"), Some(Value::Object(_)));
    if !has_metadata_object {
        obj.insert("metadata".to_string(), Value::Object(Map::new()));
    }
    if let Some(Value::Object(metadata)) = obj.get_mut("metadata") {
        metadata.insert(
            "cost_locked".to_string(),
            Value::Bool(priced.cost_delta_usd.is_some()),
        );
    }

    snapshot
}

/// JS: `appendSnapshot(snapshot, historyPath)`.
fn append_snapshot(snapshot: &Value, history_path: &Path) -> Result<()> {
    if let Some(parent) = history_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    let line = format!("{}\n", serde_json::to_string(snapshot)?);
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(history_path)
        .with_context(|| format!("failed to open {} for append", history_path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("failed to append to {}", history_path.display()))?;
    Ok(())
}

/// JS: `main()`, minus the process-exit side effects (those live in [`run`]).
fn run_inner(argv: &[String]) -> Result<i32> {
    let args = parse_args(argv)?;
    if args.help {
        usage();
        return Ok(0);
    }
    let payload = load_payload(&args)?;
    let snapshot = clean_snapshot(&payload)?;

    let history_path = match &args.history {
        Some(h) if !h.is_empty() => expand(h),
        _ => paths::paths(None)?.history_path,
    };

    let snapshot = lock_snapshot_cost(snapshot, &history_path);
    append_snapshot(&snapshot, &history_path)?;

    let output = serde_json::json!({
        "saved": history_path,
        "snapshot": snapshot,
    });
    println!("{}", serde_json::to_string(&output)?);
    Ok(0)
}

/// Entry point for the `save` subcommand. `argv` is just the flags (the JS
/// equivalent of `process.argv.slice(2)`) — no program name and no `save`
/// subcommand token. Returns the process exit code; the caller (`main.rs`) is
/// expected to call `std::process::exit(save::run(&args))`.
///
/// Mirrors JS `fail()` (exit code 2, `token-tracker: <message>` on stderr) for
/// validation errors. Any other unexpected error (I/O failure, etc. — in JS,
/// an uncaught exception that crashes the process) is printed the same way
/// but exits `1`, matching Node's default non-`fail()` crash exit code.
pub fn run(argv: &[String]) -> i32 {
    match run_inner(argv) {
        Ok(code) => code,
        Err(err) => {
            if let Some(fail) = err.downcast_ref::<SaveFail>() {
                eprintln!("token-tracker: {}", fail);
                2
            } else {
                eprintln!("token-tracker: {}", err);
                1
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn clean_snapshot_computes_total_and_defaults_source() {
        let payload = json!({
            "summary": "check",
            "project": "token-tracker",
            "prompt_tokens": 10,
            "completion_tokens": 5,
        });
        let snap = clean_snapshot(&payload).unwrap();
        assert_eq!(snap["total_tokens"], json!(15));
        assert_eq!(snap["source"], json!("manual"));
    }

    #[test]
    fn parse_args_accepts_underscore_aliases() {
        let argv: Vec<String> = [
            "--prompt_tokens",
            "10",
            "--completion_tokens",
            "5",
            "--summary",
            "x",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let args = parse_args(&argv).unwrap();
        assert_eq!(args.prompt_tokens, Some(10.0));
        assert_eq!(args.completion_tokens, Some(5.0));
        assert_eq!(args.summary.as_deref(), Some("x"));
    }

    #[test]
    fn parse_args_kebab_forms_still_work() {
        let argv: Vec<String> = [
            "--prompt-tokens",
            "20",
            "--completion-tokens",
            "10",
            "--summary",
            "y",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let args = parse_args(&argv).unwrap();
        assert_eq!(args.prompt_tokens, Some(20.0));
        assert_eq!(args.completion_tokens, Some(10.0));
        assert_eq!(args.summary.as_deref(), Some("y"));
    }

    #[test]
    fn parse_args_help_variants() {
        assert!(parse_args(&["--help".to_string()]).unwrap().help);
        assert!(parse_args(&["-h".to_string()]).unwrap().help);
        assert!(parse_args(&["help".to_string()]).unwrap().help);
    }

    #[test]
    fn parse_args_mixed_underscore_aliases() {
        let argv: Vec<String> = [
            "--total_tokens",
            "100",
            "--metadata_json",
            "{\"foo\":\"bar\"}",
            "--summary",
            "z",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let args = parse_args(&argv).unwrap();
        assert_eq!(args.total_tokens, Some(100.0));
        assert_eq!(args.metadata_json.as_deref(), Some("{\"foo\":\"bar\"}"));
        assert_eq!(args.summary.as_deref(), Some("z"));
    }

    #[test]
    fn parse_args_rejects_unknown_flag() {
        let err = parse_args(&["--nope".to_string()]).unwrap_err();
        assert_eq!(err.0, "unknown argument: --nope");
    }

    #[test]
    fn clean_snapshot_requires_summary() {
        let err = clean_snapshot(&json!({})).unwrap_err();
        assert_eq!(err.0, "summary is required");
    }

    #[test]
    fn clean_snapshot_rejects_non_integer_tokens() {
        let err = clean_snapshot(&json!({"summary": "x", "prompt_tokens": 1.5})).unwrap_err();
        assert_eq!(err.0, "prompt_tokens must be an integer");
    }
}
