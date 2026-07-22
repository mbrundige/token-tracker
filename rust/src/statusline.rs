//! Port of `scripts/statusline-token-usage.js` — the `token-tracker statusline`
//! command. Reads a JSON payload from stdin (Cursor CLI's statusLine
//! protocol), resolves project/feature/model/context info from it, updates
//! the feature-scoped token baseline, opportunistically auto-saves a
//! "statusline" usage snapshot, and prints a single compact status line.
//!
//! Faithful port; local helpers duplicate small JS-coercion functions
//! (`is_js_truthy`, `value_to_js_string`, `js_number`) rather than reaching
//! into `crate::pricing`'s private helpers of the same shape, matching the
//! pattern already established in `save.rs` and `pull_prices.rs`.
//!
//! Deliberate differences from the JS source:
//! - `gitBranchFromPayload` uses `spawnSync(..., { timeout: 200 })` in JS.
//!   Rust's `std::process::Command` has no built-in timeout, so
//!   [`git_branch_show_current`] polls `try_wait()` against a 200ms deadline
//!   and kills the child if it's still running, which is the practical
//!   equivalent.
//! - JS's `main()` can throw synchronously on unexpected I/O failure
//!   (uncaught -> Node's default crash). Modeled here as `anyhow::Result`;
//!   the caller (`main.rs`) is expected to print the error and exit
//!   non-zero, matching that crash behavior.

use std::env;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value};

use crate::paths;
use crate::pricing::{self, FormatCostOptions};
use crate::pull_prices;

// ---------------------------------------------------------------------------
// JS-coercion helpers (internal)
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

fn value_to_js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => {
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

/// Mirrors JS `Number(x)`.
fn js_number(v: Option<&Value>) -> f64 {
    match v {
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
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Some(Value::Array(_)) | Some(Value::Object(_)) => f64::NAN,
    }
}

/// Mirrors `Number(x || 0)`.
fn js_number_or_zero(v: Option<&Value>) -> f64 {
    match v {
        Some(x) if is_js_truthy(x) => js_number(Some(x)),
        _ => 0.0,
    }
}

/// Renders an f64 the way `JSON.stringify`/template-literal interpolation
/// would: no trailing `.0` for whole values.
fn js_json_number(v: f64) -> Value {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        Value::Number(serde_json::Number::from(v as i64))
    } else {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

/// Mirrors JS's default `Number.prototype.toString()` for the values that
/// appear inside template-literal interpolation (`${inputTokens}` etc).
fn js_num_to_string(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{}", v)
    }
}

/// Mirrors `Number(x.toFixed(6))`.
fn round6(v: f64) -> f64 {
    format!("{:.6}", v).parse::<f64>().unwrap_or(v)
}

// ---------------------------------------------------------------------------
// JSON I/O
// ---------------------------------------------------------------------------

fn load_json_file(path: &Path) -> Value {
    if !path.exists() {
        return Value::Object(Map::new());
    }
    match fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) if v.is_object() => v,
            _ => Value::Object(Map::new()),
        },
        Err(_) => Value::Object(Map::new()),
    }
}

fn load_json_stdin() -> Value {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return Value::Object(Map::new());
    }
    if buf.trim().is_empty() {
        return Value::Object(Map::new());
    }
    match serde_json::from_str::<Value>(&buf) {
        Ok(v) if v.is_object() => v,
        _ => Value::Object(Map::new()),
    }
}

fn save_config(config_path: &Path, config: &Value) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(config).context("failed to serialize config")?;
    fs::write(config_path, format!("{}\n", body))
        .with_context(|| format!("failed to write {}", config_path.display()))?;
    Ok(())
}

fn iter_history(history_path: &Path) -> Vec<Value> {
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
            if row.is_object() {
                rows.push(row);
            }
        }
    }
    rows
}

fn append_history(row: &Value, history_path: &Path) -> Result<()> {
    if let Some(parent) = history_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    let line = format!("{}\n", serde_json::to_string(row)?);
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(history_path)
        .with_context(|| format!("failed to open {} for append", history_path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("failed to append to {}", history_path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Feature-scoped token baseline
// ---------------------------------------------------------------------------

struct FeatureTokens {
    input_tokens: f64,
    output_tokens: f64,
    total_tokens: f64,
}

/// JS: `resolveFeatureTokens(config, workspace, project, feature, inputTokens, outputTokens)`.
fn resolve_feature_tokens(
    config: &mut Value,
    config_path: &Path,
    workspace: Option<&str>,
    project: &str,
    feature: Option<&str>,
    input_tokens: f64,
    output_tokens: f64,
) -> FeatureTokens {
    let workspace = match workspace {
        Some(w) if !w.is_empty() => w,
        _ => {
            return FeatureTokens {
                input_tokens,
                output_tokens,
                total_tokens: input_tokens + output_tokens,
            }
        }
    };

    if !matches!(config.get("token_baselines"), Some(Value::Object(_))) {
        if let Some(obj) = config.as_object_mut() {
            obj.insert("token_baselines".to_string(), Value::Object(Map::new()));
        }
    }

    let baseline = config
        .get("token_baselines")
        .and_then(|tb| tb.get(workspace))
        .cloned();

    let baseline_project = baseline
        .as_ref()
        .and_then(|b| b.get("project"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let baseline_feature = baseline
        .as_ref()
        .and_then(|b| b.get("feature"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let pending_reset = baseline
        .as_ref()
        .and_then(|b| b.get("pending_reset"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let scope_changed = baseline.is_none()
        || baseline_project.as_deref() != Some(project)
        || baseline_feature.as_deref() != feature
        || pending_reset;

    if scope_changed {
        let mut new_baseline = Map::new();
        new_baseline.insert("project".to_string(), Value::String(project.to_string()));
        new_baseline.insert(
            "feature".to_string(),
            feature
                .map(|f| Value::String(f.to_string()))
                .unwrap_or(Value::Null),
        );
        new_baseline.insert("prompt_tokens".to_string(), js_json_number(input_tokens));
        new_baseline.insert(
            "completion_tokens".to_string(),
            js_json_number(output_tokens),
        );
        new_baseline.insert("pending_reset".to_string(), Value::Bool(false));

        if let Some(Value::Object(tb)) = config.get_mut("token_baselines") {
            tb.insert(workspace.to_string(), Value::Object(new_baseline));
        }

        let _ = save_config(config_path, config);

        return FeatureTokens {
            input_tokens: 0.0,
            output_tokens: 0.0,
            total_tokens: 0.0,
        };
    }

    let base_in = baseline
        .as_ref()
        .and_then(|b| b.get("prompt_tokens"))
        .map(|v| js_number_or_zero(Some(v)))
        .unwrap_or(0.0);
    let base_out = baseline
        .as_ref()
        .and_then(|b| b.get("completion_tokens"))
        .map(|v| js_number_or_zero(Some(v)))
        .unwrap_or(0.0);
    let feature_in = (input_tokens - base_in).max(0.0);
    let feature_out = (output_tokens - base_out).max(0.0);
    FeatureTokens {
        input_tokens: feature_in,
        output_tokens: feature_out,
        total_tokens: feature_in + feature_out,
    }
}

// ---------------------------------------------------------------------------
// Payload field extraction
// ---------------------------------------------------------------------------

fn workspace_dir_from_payload(payload: &Value) -> Option<String> {
    let mut current_dir = None;
    if let Some(w) = payload.get("workspace") {
        if w.is_object() {
            current_dir = w
                .get("current_dir")
                .filter(|v| is_js_truthy(v))
                .map(value_to_js_string);
        }
    }
    current_dir.or_else(|| {
        payload
            .get("cwd")
            .filter(|v| is_js_truthy(v))
            .map(value_to_js_string)
    })
}

fn project_from_payload(config: &Value, current_dir: Option<&str>) -> String {
    if let Ok(v) = env::var("TOKEN_TRACKER_PROJECT") {
        if !v.is_empty() {
            return v;
        }
    }
    if let Some(dir) = current_dir {
        if let Some(val) = config.get("projects").and_then(|p| p.get(dir)) {
            if is_js_truthy(val) {
                return value_to_js_string(val);
            }
        }
    }
    if let Some(dp) = config.get("default_project") {
        if is_js_truthy(dp) {
            return value_to_js_string(dp);
        }
    }
    match current_dir {
        Some(dir) => Path::new(dir)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        None => "unknown".to_string(),
    }
}

/// `git -C <dir> branch --show-current`, matching JS's `spawnSync(..., {
/// timeout: 200 })` via a manual poll-and-kill deadline (see module doc).
fn git_branch_show_current(dir: &str) -> Option<String> {
    let mut child = Command::new("git")
        .args(["-C", dir, "branch", "--show-current"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => return None,
        }
    }

    let mut stdout = child.stdout.take()?;
    let mut buf = String::new();
    stdout.read_to_string(&mut buf).ok()?;
    let branch = buf.trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

fn git_branch_from_payload(payload: &Value, current_dir: Option<&str>) -> Option<String> {
    if let Some(worktree) = payload.get("worktree") {
        if worktree.is_object() {
            if let Some(name) = worktree.get("name") {
                if is_js_truthy(name) {
                    return Some(value_to_js_string(name));
                }
            }
        }
    }
    git_branch_show_current(current_dir?)
}

fn feature_from_payload(payload: &Value, config: &Value, current_dir: Option<&str>) -> Option<String> {
    if let Ok(v) = env::var("TOKEN_TRACKER_FEATURE") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if let Some(dir) = current_dir {
        if let Some(val) = config.get("features").and_then(|f| f.get(dir)) {
            if is_js_truthy(val) {
                return Some(value_to_js_string(val));
            }
        }
    }
    if let Some(df) = config.get("default_feature") {
        if is_js_truthy(df) {
            return Some(value_to_js_string(df));
        }
    }
    git_branch_from_payload(payload, current_dir)
}

fn model_from_payload(payload: &Value) -> String {
    match payload.get("model") {
        Some(m) if m.is_object() => {
            let display = m.get("display_name").filter(|v| is_js_truthy(v));
            let id = m.get("id").filter(|v| is_js_truthy(v));
            match display.or(id) {
                Some(v) => value_to_js_string(v),
                None => "unknown-model".to_string(),
            }
        }
        _ => "unknown-model".to_string(),
    }
}

fn context_tokens(payload: &Value) -> (f64, f64, Option<i64>) {
    let context = match payload.get("context_window") {
        Some(c) if c.is_object() => c,
        _ => return (0.0, 0.0, None),
    };
    let input_tokens = js_number_or_zero(context.get("total_input_tokens"));
    let output_tokens = js_number_or_zero(context.get("total_output_tokens"));
    let mut used: Option<i64> = None;
    if let Some(v) = context.get("used_percentage") {
        if !v.is_null() {
            let n = js_number(Some(v));
            if !n.is_nan() {
                used = Some(n.trunc() as i64);
            }
        }
    }
    (input_tokens, output_tokens, used)
}

// ---------------------------------------------------------------------------
// Auto-save snapshot
// ---------------------------------------------------------------------------

fn field_or_empty(row: &Value, key: &str) -> String {
    match row.get(key) {
        Some(v) if is_js_truthy(v) => value_to_js_string(v),
        _ => String::new(),
    }
}

fn last_scope_snapshot<'a>(rows: &'a [Value], project: &str, feature: Option<&str>) -> Option<&'a Value> {
    let mut last: Option<&Value> = None;
    for row in rows {
        if field_or_empty(row, "project") != project {
            continue;
        }
        let row_feature = match row.get("feature") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.is_empty() => None,
            Some(other) => Some(value_to_js_string(other)),
        };
        if row_feature.as_deref() != feature {
            continue;
        }
        last = Some(row);
    }
    last
}

#[allow(clippy::too_many_arguments)]
fn auto_save_snapshot(
    payload: &Value,
    rows: &[Value],
    project: &str,
    feature: Option<&str>,
    model: &str,
    input_tokens: f64,
    output_tokens: f64,
    used_pct: Option<i64>,
    prices: &Value,
    history_path: &Path,
) -> Result<bool> {
    let total_tokens = input_tokens + output_tokens;
    if total_tokens <= 0.0 {
        return Ok(false);
    }

    let session_key = payload
        .get("session_id")
        .filter(|v| is_js_truthy(v))
        .or_else(|| payload.get("transcript_path").filter(|v| is_js_truthy(v)))
        .map(value_to_js_string)
        .unwrap_or_else(|| "unknown-session".to_string());
    let auto_key = format!(
        "{}:{}:{}:{}:{}:{}",
        session_key,
        project,
        feature.unwrap_or(""),
        model,
        js_num_to_string(input_tokens),
        js_num_to_string(output_tokens)
    );

    for row in rows {
        if let Some(meta) = row.get("metadata") {
            if let Some(k) = meta.get("auto_key") {
                if is_js_truthy(k) && value_to_js_string(k) == auto_key {
                    return Ok(false);
                }
            }
        }
    }

    let current = serde_json::json!({
        "project": project,
        "feature": feature,
        "model": model,
        "prompt_tokens": input_tokens,
        "completion_tokens": output_tokens,
        "total_tokens": total_tokens,
    });
    let previous = last_scope_snapshot(rows, project, feature);
    let priced = pricing::compute_cost_delta(previous, &current, prices);

    let mut snapshot = Map::new();
    snapshot.insert(
        "timestamp".to_string(),
        Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)),
    );
    snapshot.insert("project".to_string(), Value::String(project.to_string()));
    snapshot.insert("source".to_string(), Value::String("statusline".to_string()));
    snapshot.insert(
        "summary".to_string(),
        Value::String("Status line usage snapshot.".to_string()),
    );
    snapshot.insert("model".to_string(), Value::String(model.to_string()));
    snapshot.insert("prompt_tokens".to_string(), js_json_number(input_tokens));
    snapshot.insert(
        "completion_tokens".to_string(),
        js_json_number(output_tokens),
    );
    snapshot.insert("total_tokens".to_string(), js_json_number(total_tokens));

    let mut metadata = Map::new();
    metadata.insert("auto_key".to_string(), Value::String(auto_key));
    metadata.insert(
        "session_id".to_string(),
        payload
            .get("session_id")
            .cloned()
            .filter(is_js_truthy)
            .unwrap_or(Value::Null),
    );
    metadata.insert(
        "transcript_path".to_string(),
        payload
            .get("transcript_path")
            .cloned()
            .filter(is_js_truthy)
            .unwrap_or(Value::Null),
    );
    metadata.insert(
        "used_percentage".to_string(),
        used_pct.map(Value::from).unwrap_or(Value::Null),
    );
    metadata.insert(
        "cost_locked".to_string(),
        Value::Bool(priced.cost_delta_usd.is_some()),
    );
    snapshot.insert("metadata".to_string(), Value::Object(metadata));

    if let Some(f) = feature {
        snapshot.insert("feature".to_string(), Value::String(f.to_string()));
    }
    if let Some(delta) = priced.cost_delta_usd {
        if delta.is_finite() {
            snapshot.insert("cost_delta_usd".to_string(), js_json_number(round6(delta)));
        }
    }
    if let Some(est) = priced.estimated_cost_usd {
        if est.is_finite() {
            snapshot.insert(
                "estimated_cost_usd".to_string(),
                js_json_number(round6(est)),
            );
        }
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
        snapshot.insert("cost_rates".to_string(), Value::Object(rates_obj));
    }

    append_history(&Value::Object(snapshot), history_path)?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn compact_tokens(value: f64) -> String {
    if value >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value >= 10_000.0 {
        format!("{}k", (value / 1000.0).round() as i64)
    } else if value >= 1000.0 {
        format!("{:.1}k", value / 1000.0)
    } else {
        js_num_to_string(value)
    }
}

fn context_bar(used_pct: Option<i64>) -> String {
    match used_pct {
        None => "ctx [??????????]".to_string(),
        Some(pct) => {
            let clamped = pct.max(0).min(100);
            let width = 10i64;
            let filled = ((clamped * width) as f64 / 100.0).round() as i64;
            format!(
                "ctx [{}{}] {}%",
                "#".repeat(filled.max(0) as usize),
                ".".repeat((width - filled).max(0) as usize),
                clamped
            )
        }
    }
}

struct StatuslineOptions {
    enabled: bool,
    show_label: bool,
    show_project: bool,
    show_feature: bool,
    show_model: bool,
    show_context: bool,
    show_tokens: bool,
    show_cost: bool,
}

fn statusline_options(config: &Value) -> StatuslineOptions {
    let options = config.get("statusline").filter(|v| v.is_object());
    let flag = |key: &str| -> bool { !matches!(options.and_then(|o| o.get(key)), Some(Value::Bool(false))) };
    StatuslineOptions {
        enabled: flag("enabled"),
        show_label: flag("show_label"),
        show_project: flag("show_project"),
        show_feature: flag("show_feature"),
        show_model: flag("show_model"),
        show_context: flag("show_context"),
        show_tokens: flag("show_tokens"),
        show_cost: flag("show_cost"),
    }
}

// ---------------------------------------------------------------------------
// CLI entry point (`token-tracker statusline`)
// ---------------------------------------------------------------------------

/// Equivalent of statusline-token-usage.js's `main()` / `require.main ===
/// module` block. Reads the status JSON payload from stdin and prints a
/// single status line to stdout. `argv` is accepted (and ignored) for
/// dispatch-site symmetry with the other subcommands — the JS script itself
/// never reads `process.argv` either.
pub fn run() -> Result<()> {
    let resolved = paths::paths(None)?;
    let history_path: PathBuf = resolved.history_path;
    let config_path: PathBuf = resolved.config_path;
    let prices_path: PathBuf = resolved.prices_path;

    let payload = load_json_stdin();
    let mut config = load_json_file(&config_path);
    let options = statusline_options(&config);
    if !options.enabled {
        return Ok(());
    }

    let refresh = pull_prices::price_refresh_options(&config);
    if refresh.auto_pull {
        let _ = pull_prices::schedule_price_pull_if_stale(&prices_path, &refresh.source, refresh.max_age_ms);
    }

    let current_dir = workspace_dir_from_payload(&payload);
    let project = project_from_payload(&config, current_dir.as_deref());
    let feature = feature_from_payload(&payload, &config, current_dir.as_deref());
    let model = model_from_payload(&payload);
    let (session_in, session_out, used_pct) = context_tokens(&payload);
    let feature_tokens = resolve_feature_tokens(
        &mut config,
        &config_path,
        current_dir.as_deref(),
        &project,
        feature.as_deref(),
        session_in,
        session_out,
    );

    let prices = pricing::load_prices(&prices_path);
    let rows = iter_history(&history_path);
    let _ = auto_save_snapshot(
        &payload,
        &rows,
        &project,
        feature.as_deref(),
        &model,
        feature_tokens.input_tokens,
        feature_tokens.output_tokens,
        used_pct,
        &prices,
        &history_path,
    )?;

    // Re-read after possible append so ongoing cost includes the just-locked
    // delta tip correctly.
    let rows_after = iter_history(&history_path);
    let ongoing = pricing::feature_ongoing_cost(
        &rows_after,
        pricing::FeatureOngoingCostParams {
            project: Some(&project),
            feature: feature.as_deref(),
            input_tokens: feature_tokens.input_tokens,
            output_tokens: feature_tokens.output_tokens,
            total_tokens: feature_tokens.total_tokens,
            model: Some(&model),
        },
        &prices,
    );

    let ctx = context_bar(used_pct);
    let toks = format!("toks {}", compact_tokens(feature_tokens.total_tokens));
    let scope = if feature.is_some() && options.show_feature {
        format!("{}/{}", project, feature.as_deref().unwrap_or(""))
    } else {
        project.clone()
    };

    let mut parts: Vec<String> = Vec::new();
    if options.show_label {
        parts.push("token-tracker".to_string());
    }
    if options.show_project {
        parts.push(scope);
    }
    if options.show_model {
        parts.push(model);
    }
    if options.show_context {
        parts.push(ctx);
    }
    if options.show_tokens {
        parts.push(toks);
    }
    if options.show_cost {
        parts.push(pricing::format_cost(
            ongoing.cost_usd,
            FormatCostOptions {
                prefix: "$",
                digits: None,
                unpriced: "$?",
            },
        ));
    }
    println!("{}", parts.join(" | "));
    Ok(())
}
