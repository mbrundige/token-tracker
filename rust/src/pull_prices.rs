//! Port of `scripts/pull-prices.js` — the `token-tracker prices pull|show` commands.
//!
//! Fetches live model pricing from one of three remote sources (openrouter,
//! llmcosthub, benchgecko), normalizes each source's response shape into a
//! common `{ "key": { input_per_million_usd, output_per_million_usd } }`
//! table, and writes it to `prices.json`. Also carries the "auto-refresh if
//! stale" scheduling logic shared by `report` and `save`.
//!
//! ## Assumed signature of `crate::paths` (did not exist on disk at port time)
//!
//! `report.rs` (already on disk) documents and calls `crate::paths::paths()`
//! with no arguments, returning a `Paths` struct directly (no `Result`):
//! ```ignore
//! pub struct Paths {
//!     pub data_dir: PathBuf,
//!     pub legacy_data_dir: PathBuf,
//!     pub config_path: PathBuf,
//!     pub history_path: PathBuf,
//!     pub prices_path: PathBuf,
//! }
//! pub fn paths() -> Paths
//! pub fn expand(p: &str) -> PathBuf
//! ```
//! This file follows that contract (`paths::paths().prices_path`) since it's
//! the fuller, already-documented contract and this module is a direct
//! dependency of `report.rs`. Note `save.rs` assumes a *different* signature
//! (`paths::paths(dataDir: Option<&Path>) -> Result<Paths>`) — that's a
//! pre-existing conflict between the two already-written files, not
//! introduced here; the integration step needs to reconcile `paths.rs` to
//! satisfy both call sites (e.g. a zero-arg `paths()` convenience wrapper
//! around a `Result`-returning `paths_in(dir: Option<&Path>)`).
//!
//! ## No `regex` crate available
//!
//! The allowed dependency list has no regex crate. Every JS regex in the
//! source (`cleanKey`, `idKey`, `versionScore`, and the `FAMILY_ALIASES`
//! matchers) is hand-ported to manual `char`/`str` scanning below. Two of the
//! `FAMILY_ALIASES` tests (`gemini` "pro", `flash` "not lite") rely on JS's
//! greedy-backtracking `.test()` semantics for unanchored patterns with a
//! trailing negative lookahead; the ported `t_gemini`/`t_flash` implement the
//! practically-equivalent "does `pro`/`flash` appear after `gemini-`, and for
//! `flash`, does `lite` NOT appear after the last `flash`" — correct for all
//! realistic OpenRouter/BenchGecko model ids, called out as an approximation
//! rather than a byte-for-byte regex-engine port.
//!
//! ## `serde_json` map ordering
//!
//! `buildPricesDocument` in JS relies on `JSON.stringify` preserving object
//! key insertion order (it explicitly sorts model keys, then relies on that
//! order surviving serialization). This file always *inserts* into
//! `serde_json::Map` in the exact order JS would serialize, but that only
//! survives serialization if `serde_json` is built with the `preserve_order`
//! feature (an `IndexMap`-backed `Map`); without it, `serde_json::Map`
//! defaults to a `BTreeMap` and silently re-sorts keys alphabetically
//! (including `_comment`/`updated_at`/`source`/... at the top level, which
//! would then NOT match JS's field order). The integration step should add
//! `features = ["preserve_order"]` to the `serde_json` dependency for exact
//! fidelity.
//!
//! ## Other deliberate differences from JS
//! - No async: `ureq` is used as a blocking HTTP client (per task
//!   instructions), so `runPull`/`ensureFreshPrices` are synchronous here.
//! - `ensureFreshPrices`'s JS `onStatus` callback is optional (`null`
//!   allowed); the Rust port makes it a required `impl FnMut(&str)` per
//!   `report.rs`'s already-established call site, which always passes one.
//! - `pricesUpdatedAtMs` (from `pricing.js`) is not re-exposed via
//!   `crate::pricing` here (that module doesn't exist yet either, and
//!   `report.rs`'s documented `Prices` contract doesn't include it). This
//!   file inlines a narrow, object-only port (`parse_updated_at_ms`) of just
//!   the piece `pricesRefreshStatus` needs, to avoid a speculative dependency
//!   on an unwritten function's signature.
//! - `schedulePricePullIfStale` re-spawns `pull-prices.js` as a detached Node
//!   child process in JS. Here it re-execs the current binary
//!   (`std::env::current_exe()`) with `prices pull --source ... --out ...`,
//!   which is the faithful equivalent for a single compiled CLI. The 30s
//!   best-effort lock-file cleanup (`setTimeout(...).unref()` in JS) is a
//!   detached background thread here.
//!
//! Result structs (`ScheduleResult`, `EnsureFreshResult`) mirror JS's full
//! return object shape; some fields aren't read by any current call site in
//! this binary (`install.rs` only checks `.scheduled`) but are kept for
//! fidelity and future callers.
#![allow(dead_code)]

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value};

use crate::paths::{self, expand};

/// JS: `DEFAULT_MAX_AGE_MS` (`60 * 60 * 1000`).
pub const DEFAULT_MAX_AGE_MS: i64 = 60 * 60 * 1000;

/// Default timeout for `fetchJson`'s un-overridden call in `runPull` (JS:
/// `{ timeoutMs = 20000 } = {}`).
const DEFAULT_FETCH_TIMEOUT_MS: u64 = 20_000;

/// JS: `PROVIDER_ALLOW`.
const PROVIDER_ALLOW: [&str; 5] = ["openai", "anthropic", "google", "google-ai-studio", "google-vertex"];

// ---------------------------------------------------------------------------
// JS `Number(x)` / `String(x)` / truthiness helpers
// ---------------------------------------------------------------------------

fn js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) => true,
        Value::Object(_) => true,
    }
}

/// JS: `String(x)` for a concrete (non-missing) value.
fn js_display(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if f.is_finite() && f == f.trunc() && f.abs() < 1e15 {
                    format!("{}", f as i64)
                } else {
                    format!("{}", f)
                }
            } else {
                n.to_string()
            }
        }
        other => other.to_string(),
    }
}

/// JS: `String(x || fallback)` — falls back when `x` is missing or falsy.
fn js_string_or_default(v: Option<&Value>, fallback: &str) -> String {
    match v {
        Some(val) if js_truthy(val) => js_display(val),
        _ => fallback.to_string(),
    }
}

/// `Some(String(x))` when `x` is present and truthy, else `None`.
fn opt_str_from(v: Option<&Value>) -> Option<String> {
    match v {
        Some(val) if js_truthy(val) => Some(js_display(val)),
        _ => None,
    }
}

fn opt_str(doc: &Value, key: &str) -> Option<String> {
    opt_str_from(doc.get(key))
}

/// JS: `Number(x)`.
fn js_number_value(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Some(Value::Bool(b)) => {
            if *b {
                1.0
            } else {
                0.0
            }
        }
        _ => f64::NAN,
    }
}

/// Renders `x` the way `JSON.stringify` would: whole numbers with no
/// trailing `.0`, and non-finite values (NaN/Infinity) as JSON `null` (JS
/// `JSON.stringify(NaN) === "null"`).
fn js_number(x: f64) -> Value {
    if x.is_finite() && x == x.trunc() && x.abs() < 9_007_199_254_740_992.0 {
        Value::Number((x as i64).into())
    } else {
        serde_json::Number::from_f64(x).map(Value::Number).unwrap_or(Value::Null)
    }
}

fn pad_end(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - len))
    }
}

fn pad_start(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{}{}", " ".repeat(width - len), s)
    }
}

/// `${path}${suffix}` — literal string concatenation, matching JS's
/// `` `${outPath}.bak` `` (not a "swap extension" operation).
fn path_with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// JS: `loadLocal` / `loadPricesDoc` (identical bodies in the source; ported
/// once here and reused for both call sites).
fn load_json_object(path: &Path) -> Value {
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

/// Narrow, object-only port of `pricing.js`'s `pricesUpdatedAtMs` — see the
/// module doc comment for why this isn't `crate::pricing::prices_updated_at_ms`.
fn parse_updated_at_ms(doc: &Value) -> i64 {
    let raw = match opt_str(doc, "updated_at") {
        Some(s) => s,
        None => return 0,
    };
    match chrono::DateTime::parse_from_rfc3339(&raw) {
        Ok(dt) => dt.timestamp_millis(),
        Err(_) => 0,
    }
}

// ---------------------------------------------------------------------------
// JS: `cleanKey`, `idKey`, `providerOf`, `versionScore`
// ---------------------------------------------------------------------------

/// JS: `cleanKey(raw)`.
pub fn clean_key(raw: &str) -> String {
    let mut s = raw.to_lowercase();
    if let Some(idx) = s.find(':') {
        if idx > 0 {
            s = s[idx + 1..].trim_start().to_string();
        }
    }
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch == '_' || ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

/// JS: `idKey(id)`.
pub fn id_key(id: &str) -> String {
    let slug = id.rsplit('/').next().unwrap_or("");
    slug.to_lowercase().replace('_', "-")
}

/// JS: `providerOf(id)`.
fn provider_of(id: &str) -> String {
    id.split('/').next().unwrap_or("").to_string()
}

/// Manual port of JS's `id.match(/(\d+(?:\.\d+)*)/g)` — every maximal
/// digit-run (optionally dot-separated) substring, in order of appearance.
fn extract_version_parts(id: &str) -> Vec<String> {
    let chars: Vec<char> = id.chars().collect();
    let mut result = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            loop {
                if i < chars.len() && chars[i] == '.' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit() {
                    i += 1;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            result.push(chars[start..i].iter().collect());
        } else {
            i += 1;
        }
    }
    result
}

/// JS: `versionScore(id)`.
fn version_score(id: &str) -> f64 {
    let mut score = 0.0_f64;
    for part in extract_version_parts(id) {
        let bits: Vec<f64> = part.split('.').map(|n| n.parse::<f64>().unwrap_or(0.0)).collect();
        let mut s = 0.0_f64;
        for (i, b) in bits.iter().enumerate() {
            s += b * 1000f64.powi(3 - i as i32);
        }
        if s > score {
            score = s;
        }
    }
    score
}

fn round_rate(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    if x >= 10.0 {
        (x * 100.0).round() / 100.0
    } else if x >= 1.0 {
        (x * 1000.0).round() / 1000.0
    } else {
        (x * 10000.0).round() / 10000.0
    }
}

fn rates_value(input: f64, output: f64) -> Value {
    serde_json::json!({
        "input_per_million_usd": js_number(round_rate(input)),
        "output_per_million_usd": js_number(round_rate(output)),
    })
}

// ---------------------------------------------------------------------------
// JS: `FAMILY_ALIASES`
// ---------------------------------------------------------------------------

fn t_gpt55pro(id: &str) -> bool {
    id.starts_with("openai/gpt-5.5-pro")
}

fn t_gpt55(id: &str) -> bool {
    let prefix = "openai/gpt-5.5";
    id.starts_with(prefix) && !id[prefix.len()..].starts_with("-pro")
}

fn t_gpt54pro(id: &str) -> bool {
    id.starts_with("openai/gpt-5.4-pro")
}

fn t_gpt54mini(id: &str) -> bool {
    id.starts_with("openai/gpt-5.4-mini")
}

fn t_gpt54nano(id: &str) -> bool {
    id.starts_with("openai/gpt-5.4-nano")
}

fn t_gpt54(id: &str) -> bool {
    let prefix = "openai/gpt-5.4";
    let re_ok = id.starts_with(prefix) && {
        let rest = &id[prefix.len()..];
        !rest.starts_with('-') && !rest.starts_with("pro")
    };
    re_ok || id == "openai/gpt-5.4"
}

fn t_gpt53(id: &str) -> bool {
    id.starts_with("openai/gpt-5.3")
}

fn t_gpt5(id: &str) -> bool {
    let prefix = "openai/gpt-5";
    let re_ok = id.starts_with(prefix) && {
        let rest = &id[prefix.len()..];
        match rest.chars().next() {
            None => true,
            Some(c) => !(c.is_ascii_digit() || c == '.' || c == '-'),
        }
    };
    re_ok || id == "openai/gpt-5"
}

fn t_gpt4o(id: &str) -> bool {
    id.starts_with("openai/gpt-4o")
}

fn t_o3(id: &str) -> bool {
    let prefix = "openai/o3";
    let re_ok = id.starts_with(prefix) && !id[prefix.len()..].starts_with('-');
    re_ok || id == "openai/o3"
}

fn t_o4mini(id: &str) -> bool {
    id.starts_with("openai/o4-mini")
}

fn t_claude_opus(id: &str) -> bool {
    id.starts_with("anthropic/claude-opus-") && !id.contains("fast")
}

fn t_claude_sonnet(id: &str) -> bool {
    id.starts_with("anthropic/claude-sonnet-")
}

fn t_claude_haiku(id: &str) -> bool {
    id.starts_with("anthropic/claude-haiku-") || id.contains("claude-3-haiku")
}

fn t_claude(id: &str) -> bool {
    id.starts_with("anthropic/claude-sonnet-")
}

/// JS: `/gemini-.*pro/`. Unanchored, no lookahead — a plain "does `pro`
/// appear anywhere after `gemini-`" is exact for this one.
fn t_gemini(id: &str) -> bool {
    match id.find("gemini-") {
        Some(gi) => id[gi..].contains("pro"),
        None => false,
    }
}

/// JS: `/gemini-.*flash(?!.*lite)/`. See the module doc comment for why this
/// is an approximation of full regex backtracking semantics.
fn t_flash(id: &str) -> bool {
    match id.find("gemini-") {
        Some(gi) => {
            let after = &id[gi..];
            match after.rfind("flash") {
                Some(fi) => {
                    let after_flash = &after[fi + "flash".len()..];
                    !after_flash.contains("lite")
                }
                None => false,
            }
        }
        None => false,
    }
}

fn t_composer(id: &str) -> bool {
    id.contains("composer")
}

struct FamilyAlias {
    alias: &'static str,
    test: fn(&str) -> bool,
}

/// JS: `FAMILY_ALIASES`.
const FAMILY_ALIASES: &[FamilyAlias] = &[
    FamilyAlias { alias: "gpt-5.5 pro", test: t_gpt55pro },
    FamilyAlias { alias: "gpt-5.5", test: t_gpt55 },
    FamilyAlias { alias: "gpt-5.4 pro", test: t_gpt54pro },
    FamilyAlias { alias: "gpt-5.4 mini", test: t_gpt54mini },
    FamilyAlias { alias: "gpt-5.4 nano", test: t_gpt54nano },
    FamilyAlias { alias: "gpt-5.4", test: t_gpt54 },
    FamilyAlias { alias: "gpt-5.3", test: t_gpt53 },
    FamilyAlias { alias: "gpt-5", test: t_gpt5 },
    FamilyAlias { alias: "gpt-4o", test: t_gpt4o },
    FamilyAlias { alias: "o3", test: t_o3 },
    FamilyAlias { alias: "o4-mini", test: t_o4mini },
    FamilyAlias { alias: "claude opus", test: t_claude_opus },
    FamilyAlias { alias: "claude sonnet", test: t_claude_sonnet },
    FamilyAlias { alias: "claude haiku", test: t_claude_haiku },
    FamilyAlias { alias: "claude", test: t_claude },
    FamilyAlias { alias: "gemini", test: t_gemini },
    FamilyAlias { alias: "flash", test: t_flash },
    FamilyAlias { alias: "composer", test: t_composer },
];

#[derive(Clone)]
struct CatalogEntry {
    id: String,
    rates: Value,
    score: f64,
}

/// JS: `applyFamilyAliases(models, catalog)`. (JS's `stripInternal` has no
/// equivalent here: the `_id` bookkeeping it strips was only ever needed
/// internally to compare version scores on key collisions, which this port
/// tracks in a separate `best_id` map instead of stashing on the rates
/// object — so there is nothing to strip.)
fn apply_family_aliases(models: &mut Map<String, Value>, catalog: &[CatalogEntry]) {
    for fa in FAMILY_ALIASES {
        let mut matches: Vec<&CatalogEntry> = catalog.iter().filter(|c| (fa.test)(&c.id)).collect();
        matches.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        if let Some(best) = matches.first() {
            models.insert(fa.alias.to_string(), best.rates.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// JS: `parseOpenRouter`, `parseLlmCostHub`, `parseBenchGecko`
// ---------------------------------------------------------------------------

/// JS: `parseOpenRouter(payload)`.
pub fn parse_open_router(payload: &Value) -> Map<String, Value> {
    let empty = Vec::new();
    let rows = payload.get("data").and_then(|d| d.as_array()).unwrap_or(&empty);

    let mut models: Map<String, Value> = Map::new();
    let mut best_id: HashMap<String, String> = HashMap::new();
    let mut catalog: Vec<CatalogEntry> = Vec::new();

    for row in rows {
        let id = js_string_or_default(row.get("id"), "");
        if id.is_empty() || id.contains(":free") {
            continue;
        }
        let provider = provider_of(&id);
        if !PROVIDER_ALLOW.contains(&provider.as_str()) {
            continue;
        }
        let pricing = row.get("pricing");
        let input = js_number_value(pricing.and_then(|p| p.get("prompt"))) * 1_000_000.0;
        let output = js_number_value(pricing.and_then(|p| p.get("completion"))) * 1_000_000.0;
        if !input.is_finite() || !output.is_finite() || (input <= 0.0 && output <= 0.0) {
            continue;
        }
        let rates = rates_value(input, output);
        let score = version_score(&id);
        catalog.push(CatalogEntry { id: id.clone(), rates: rates.clone(), score });

        let name = js_string_or_default(row.get("name"), "");
        // JS wraps [idKey(id), cleanKey(row.name)] in a Set to dedupe; skipped
        // here since re-processing the same (key, id) pair is idempotent.
        for key in [id_key(&id), clean_key(&name)] {
            if key.len() < 2 {
                continue;
            }
            let should_replace = match best_id.get(&key) {
                None => true,
                Some(prev_id) => score >= version_score(prev_id),
            };
            if should_replace {
                models.insert(key.clone(), rates.clone());
                best_id.insert(key, id.clone());
            }
        }
    }

    apply_family_aliases(&mut models, &catalog);
    models
}

/// JS: `parseLlmCostHub(payload)`.
pub fn parse_llm_cost_hub(payload: &Value) -> Map<String, Value> {
    let empty = Vec::new();
    let rows = payload.get("models").and_then(|d| d.as_array()).unwrap_or(&empty);

    let mut models: Map<String, Value> = Map::new();
    let mut catalog: Vec<CatalogEntry> = Vec::new();

    for row in rows {
        let provider_slug = js_string_or_default(row.get("provider_slug"), "");
        let provider_raw = if !provider_slug.is_empty() {
            provider_slug
        } else {
            js_string_or_default(row.get("provider"), "")
        };
        let provider = provider_raw.to_lowercase();
        if !provider.is_empty() && !PROVIDER_ALLOW.contains(&provider.as_str()) && provider != "google" {
            continue;
        }
        let input = js_number_value(row.get("input_price_per_1m"));
        let output = js_number_value(row.get("output_price_per_1m"));
        if !input.is_finite() || !output.is_finite() {
            continue;
        }
        let rates = rates_value(input, output);

        let model_slug = js_string_or_default(row.get("model_slug"), "");
        let model = js_string_or_default(row.get("model"), "");
        let tail = if !model_slug.is_empty() {
            model_slug.clone()
        } else if !model.is_empty() {
            model.clone()
        } else {
            String::new()
        };
        let id = format!("{}/{}", provider, tail);
        catalog.push(CatalogEntry { id: id.clone(), rates: rates.clone(), score: version_score(&id) });

        for key in [clean_key(&model), clean_key(&model_slug)] {
            if key.is_empty() {
                continue;
            }
            models.insert(key, rates.clone());
        }
    }

    apply_family_aliases(&mut models, &catalog);
    models
}

/// JS: `parseBenchGecko(payload)`.
pub fn parse_bench_gecko(payload: &Value) -> Map<String, Value> {
    let empty = Vec::new();
    let rows = payload.get("models").and_then(|d| d.as_array()).unwrap_or(&empty);

    let mut models: Map<String, Value> = Map::new();
    let mut catalog: Vec<CatalogEntry> = Vec::new();

    for row in rows {
        let raw_id = js_string_or_default(row.get("id"), "");
        let id = raw_id.strip_prefix('~').unwrap_or(&raw_id).to_string();
        let provider = provider_of(&id);
        if !PROVIDER_ALLOW.contains(&provider.as_str()) && provider != "google" {
            continue;
        }
        if let Some(t) = row.get("type") {
            if js_truthy(t) && js_display(t) != "chat" {
                continue;
            }
        }
        let input = js_number_value(row.get("input_per_million"));
        let output = js_number_value(row.get("output_per_million"));
        if !input.is_finite() || !output.is_finite() {
            continue;
        }
        let rates = rates_value(input, output);
        let score = version_score(&id);
        catalog.push(CatalogEntry { id: id.clone(), rates: rates.clone(), score });

        let name = js_string_or_default(row.get("name"), "");
        for key in [id_key(&id), clean_key(&name)] {
            if key.is_empty() {
                continue;
            }
            models.insert(key, rates.clone());
        }
    }

    apply_family_aliases(&mut models, &catalog);
    models
}

// ---------------------------------------------------------------------------
// JS: `buildPricesDocument`
// ---------------------------------------------------------------------------

/// JS: `buildPricesDocument({ source, url, models, previous })`.
pub fn build_prices_document(source: &str, url: &str, models: Map<String, Value>, previous: &Value) -> Value {
    let default_rates = match previous.get("default") {
        Some(v) if v.is_object() => v.clone(),
        _ => serde_json::json!({"input_per_million_usd": 2.5, "output_per_million_usd": 15}),
    };

    let mut locked: Map<String, Value> = Map::new();
    if let Some(prev_models) = previous.get("models").and_then(|v| v.as_object()) {
        for (key, rates) in prev_models {
            let is_locked = rates.is_object() && rates.get("locked").and_then(|v| v.as_bool()) == Some(true);
            if is_locked {
                let input = rates.get("input_per_million_usd").cloned().unwrap_or(Value::Null);
                let output = rates.get("output_per_million_usd").cloned().unwrap_or(Value::Null);
                locked.insert(
                    key.clone(),
                    serde_json::json!({
                        "input_per_million_usd": input,
                        "output_per_million_usd": output,
                        "locked": true,
                    }),
                );
            }
        }
    }

    // JS: `{ ...models, ...locked }` — locked overrides win on key collision.
    let mut merged = models;
    for (k, v) in locked {
        merged.insert(k, v);
    }

    let mut keys: Vec<String> = merged.keys().cloned().collect();
    keys.sort();
    let mut ordered = Map::new();
    for key in keys {
        if let Some(v) = merged.get(&key) {
            ordered.insert(key, v.clone());
        }
    }

    // JS: `new Date().toISOString().replace(/\.\d{3}Z$/, "Z")` — RFC3339 at
    // seconds precision.
    let updated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

    let default_input = js_number_value(default_rates.get("input_per_million_usd"));
    let default_output = js_number_value(default_rates.get("output_per_million_usd"));

    let mut doc = Map::new();
    doc.insert(
        "_comment".to_string(),
        Value::String(
            "Estimated API list prices in USD per 1M tokens. Matched as case-insensitive substrings against the model display name (longest match wins). Edit freely; mark a model with \"locked\": true to keep it across `prices pull`.".to_string(),
        ),
    );
    doc.insert("updated_at".to_string(), Value::String(updated_at));
    doc.insert("source".to_string(), Value::String(source.to_string()));
    doc.insert("source_url".to_string(), Value::String(url.to_string()));
    doc.insert(
        "default".to_string(),
        serde_json::json!({
            "input_per_million_usd": js_number(default_input),
            "output_per_million_usd": js_number(default_output),
        }),
    );
    doc.insert("models".to_string(), Value::Object(ordered));

    Value::Object(doc)
}

// ---------------------------------------------------------------------------
// JS: `SOURCES` / `fetchJson` / `runPull`
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct SourceSpec {
    pub url: &'static str,
    pub parse: fn(&Value) -> Map<String, Value>,
}

/// JS: `Object.keys(SOURCES)` (insertion order, used for the "unknown
/// source" error message).
pub const SOURCE_NAMES: [&str; 3] = ["openrouter", "llmcosthub", "benchgecko"];

/// JS: `SOURCES[source]`.
pub fn source_spec(name: &str) -> Option<SourceSpec> {
    match name {
        "openrouter" => Some(SourceSpec {
            url: "https://openrouter.ai/api/v1/models",
            parse: parse_open_router,
        }),
        "llmcosthub" => Some(SourceSpec {
            url: "https://llmcosthub.com/api/v1/pricing.json",
            parse: parse_llm_cost_hub,
        }),
        "benchgecko" => Some(SourceSpec {
            url: "https://raw.githubusercontent.com/BenchGecko/llm-pricing/main/pricing.json",
            parse: parse_bench_gecko,
        }),
        _ => None,
    }
}

/// JS: `fetchJson(url, { timeoutMs })`, using blocking `ureq` instead of
/// `http`/`https` + manual redirect-following (ureq follows redirects
/// itself).
fn fetch_json(url: &str, timeout_ms: u64) -> Result<Value> {
    let response = ureq::get(url)
        .set("Accept", "application/json")
        .set("User-Agent", "token-tracker-prices/1.0")
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .call();

    let response = match response {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _resp)) => {
            anyhow::bail!("HTTP {} from {}", code, url);
        }
        Err(ureq::Error::Transport(t)) => {
            let msg = t.to_string();
            if msg.to_lowercase().contains("timed out") {
                anyhow::bail!("timeout fetching {}", url);
            }
            anyhow::bail!("{} fetching {}", msg, url);
        }
    };

    response.into_json::<Value>().with_context(|| format!("failed to parse JSON from {}", url))
}

/// JS: `loadLocal(filePath)` — reused as `load_json_object`.
///
/// JS: `runPull({ source, outPath, dryRun })`.
pub fn run_pull(source: &str, out_path: &Path, dry_run: bool) -> Result<Value> {
    let spec = source_spec(source)
        .ok_or_else(|| anyhow::anyhow!("unknown source \"{}\". Use: {}", source, SOURCE_NAMES.join(", ")))?;

    let remote = fetch_json(spec.url, DEFAULT_FETCH_TIMEOUT_MS)?;
    let models = (spec.parse)(&remote);
    let previous = load_json_object(out_path);
    let doc = build_prices_document(source, spec.url, models, &previous);
    let count = doc.get("models").and_then(|m| m.as_object()).map(|m| m.len()).unwrap_or(0);
    if count == 0 {
        anyhow::bail!("pull produced 0 model rates; refusing to write");
    }

    if dry_run {
        let sample: Map<String, Value> = doc
            .get("models")
            .and_then(|m| m.as_object())
            .map(|m| m.iter().take(8).map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        return Ok(serde_json::json!({
            "dry_run": true,
            "source": source,
            "source_url": spec.url,
            "out": out_path.display().to_string(),
            "model_count": count,
            "sample": Value::Object(sample),
        }));
    }

    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("failed to create directory {}", parent.display()))?;
    }
    let backup_path = path_with_suffix(out_path, ".bak");
    if out_path.exists() {
        fs::copy(out_path, &backup_path)
            .with_context(|| format!("failed to back up {} to {}", out_path.display(), backup_path.display()))?;
    }
    let pretty = serde_json::to_string_pretty(&doc)?;
    fs::write(out_path, format!("{}\n", pretty)).with_context(|| format!("failed to write {}", out_path.display()))?;

    let mut result = Map::new();
    result.insert("updated".to_string(), Value::String(out_path.display().to_string()));
    result.insert("source".to_string(), Value::String(source.to_string()));
    result.insert("source_url".to_string(), Value::String(spec.url.to_string()));
    result.insert("model_count".to_string(), Value::from(count));
    if backup_path.exists() {
        result.insert("backup".to_string(), Value::String(backup_path.display().to_string()));
    }
    result.insert(
        "note".to_string(),
        Value::String("Mark any local override with \"locked\": true to keep it on the next pull.".to_string()),
    );
    Ok(Value::Object(result))
}

// ---------------------------------------------------------------------------
// JS: `pricesRefreshStatus`, `schedulePricePullIfStale`, `ensureFreshPrices`,
// `priceRefreshOptions`
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RefreshStatus {
    pub needed: bool,
    pub reason: String,
    /// `Some(f64::INFINITY)` mirrors JS's `Infinity`; `None` when JS omits
    /// the `ageMs` key entirely (the "disabled" case).
    pub age_ms: Option<f64>,
    pub updated_ms: Option<i64>,
    pub source: Option<String>,
}

/// JS: `pricesRefreshStatus(pricesPath, maxAgeMs = DEFAULT_MAX_AGE_MS)`.
pub fn prices_refresh_status(prices_path: Option<&Path>, max_age_ms: i64) -> RefreshStatus {
    if max_age_ms <= 0 {
        return RefreshStatus {
            needed: false,
            reason: "disabled".to_string(),
            age_ms: None,
            updated_ms: None,
            source: None,
        };
    }

    let path = match prices_path {
        Some(p) if !p.as_os_str().is_empty() && p.exists() => p,
        _ => {
            return RefreshStatus {
                needed: true,
                reason: "missing".to_string(),
                age_ms: Some(f64::INFINITY),
                updated_ms: None,
                source: None,
            };
        }
    };

    let doc = load_json_object(path);
    let source_val = opt_str(&doc, "source");
    let has_updated_at = opt_str(&doc, "updated_at").is_some();
    if source_val.is_none() || source_val.as_deref() == Some("seed") || !has_updated_at {
        return RefreshStatus {
            needed: true,
            reason: "seed".to_string(),
            age_ms: Some(f64::INFINITY),
            updated_ms: None,
            source: Some(source_val.unwrap_or_else(|| "seed".to_string())),
        };
    }
    let source_val = source_val.unwrap();

    let updated_ms = parse_updated_at_ms(&doc);
    let age_ms = if updated_ms != 0 {
        (Utc::now().timestamp_millis() - updated_ms) as f64
    } else {
        f64::INFINITY
    };

    if age_ms >= max_age_ms as f64 {
        return RefreshStatus {
            needed: true,
            reason: "stale".to_string(),
            age_ms: Some(age_ms),
            updated_ms: Some(updated_ms),
            source: Some(source_val),
        };
    }
    RefreshStatus {
        needed: false,
        reason: "fresh".to_string(),
        age_ms: Some(age_ms),
        updated_ms: Some(updated_ms),
        source: Some(source_val),
    }
}

#[derive(Debug, Clone)]
pub struct ScheduleResult {
    pub scheduled: bool,
    pub reason: String,
    pub age_ms: Option<f64>,
    pub updated_ms: Option<i64>,
    pub lock_age_ms: Option<f64>,
    pub pid: Option<u32>,
    pub error: Option<String>,
}

/// JS: `schedulePricePullIfStale({ pricesPath, source, maxAgeMs, pullScript })`.
/// Re-execs the current binary as `prices pull --source <source> --out
/// <pricesPath>` (detached, ignored stdio) instead of spawning a sibling
/// `pull-prices.js` with node — see module doc comment.
pub fn schedule_price_pull_if_stale(prices_path: &Path, source: &str, max_age_ms: i64) -> ScheduleResult {
    let status = prices_refresh_status(Some(prices_path), max_age_ms);
    if !status.needed {
        return ScheduleResult {
            scheduled: false,
            reason: status.reason,
            age_ms: status.age_ms,
            updated_ms: status.updated_ms,
            lock_age_ms: None,
            pid: None,
            error: None,
        };
    }

    let lock_path = path_with_suffix(prices_path, ".pulling");
    if lock_path.exists() {
        if let Ok(meta) = fs::metadata(&lock_path) {
            if let Ok(modified) = meta.modified() {
                if let Ok(age) = std::time::SystemTime::now().duration_since(modified) {
                    let age_ms = age.as_millis() as f64;
                    if age_ms < 5.0 * 60.0 * 1000.0 {
                        return ScheduleResult {
                            scheduled: false,
                            reason: "in_flight".to_string(),
                            age_ms: None,
                            updated_ms: None,
                            lock_age_ms: Some(age_ms),
                            pid: None,
                            error: None,
                        };
                    }
                }
            }
        }
    }

    if let Some(parent) = prices_path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return ScheduleResult {
                scheduled: false,
                reason: "lock_failed".to_string(),
                age_ms: None,
                updated_ms: None,
                lock_age_ms: None,
                pid: None,
                error: None,
            };
        }
    }
    if fs::write(&lock_path, format!("{}\n", Utc::now().timestamp_millis())).is_err() {
        return ScheduleResult {
            scheduled: false,
            reason: "lock_failed".to_string(),
            age_ms: None,
            updated_ms: None,
            lock_age_ms: None,
            pid: None,
            error: None,
        };
    }

    let exe = match env::current_exe() {
        Ok(p) => p,
        Err(err) => {
            let _ = fs::remove_file(&lock_path);
            return ScheduleResult {
                scheduled: false,
                reason: "spawn_failed".to_string(),
                age_ms: None,
                updated_ms: None,
                lock_age_ms: None,
                pid: None,
                error: Some(err.to_string()),
            };
        }
    };

    let spawned = Command::new(&exe)
        .args(["prices", "pull", "--source", source, "--out"])
        .arg(prices_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match spawned {
        Ok(child) => {
            let pid = child.id();
            let cleanup_lock = lock_path.clone();
            // JS: `setTimeout(() => fs.rmSync(lockPath, { force: true }), 30_000).unref()`.
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_secs(30));
                let _ = fs::remove_file(&cleanup_lock);
            });
            ScheduleResult {
                scheduled: true,
                reason: status.reason,
                age_ms: status.age_ms,
                updated_ms: status.updated_ms,
                lock_age_ms: None,
                pid: Some(pid),
                error: None,
            }
        }
        Err(err) => {
            let _ = fs::remove_file(&lock_path);
            ScheduleResult {
                scheduled: false,
                reason: "spawn_failed".to_string(),
                age_ms: None,
                updated_ms: None,
                lock_age_ms: None,
                pid: None,
                error: Some(err.to_string()),
            }
        }
    }
}

/// JS: `ensureFreshPrices({ ... })`'s return shape. `report.rs` (already on
/// disk) documents and depends on exactly the first four fields
/// (`pulled`/`reason`/`error`/`scheduled`); the rest are additive fidelity
/// with JS's fuller return object and don't affect that contract.
#[derive(Debug, Clone)]
pub struct EnsureFreshResult {
    pub pulled: bool,
    pub reason: Option<String>,
    pub error: Option<String>,
    pub scheduled: bool,
    pub age_ms: Option<f64>,
    pub updated_ms: Option<i64>,
    pub source: Option<String>,
    pub result: Option<Value>,
}

/// JS: `ensureFreshPrices({ pricesPath, source, maxAgeMs, timeoutMs, onStatus })`.
/// `timeoutMs` (JS default 20000, only relevant to the underlying
/// `fetchJson` call) is not a separate parameter here: `run_pull` always
/// uses [`DEFAULT_FETCH_TIMEOUT_MS`], matching the JS call site (`runPull`
/// is invoked without a custom `timeoutMs`, so it always used the default
/// too).
pub fn ensure_fresh_prices(
    prices_path: &Path,
    source: &str,
    max_age_ms: i64,
    mut on_status: impl FnMut(&str),
) -> EnsureFreshResult {
    let status = prices_refresh_status(Some(prices_path), max_age_ms);
    if !status.needed {
        return EnsureFreshResult {
            pulled: false,
            reason: Some(status.reason),
            error: None,
            scheduled: false,
            age_ms: status.age_ms,
            updated_ms: status.updated_ms,
            source: status.source,
            result: None,
        };
    }

    if status.reason == "seed" || status.reason == "missing" {
        on_status("Fetching latest model prices…");
    } else {
        on_status("Refreshing model prices…");
    }

    match run_pull(source, prices_path, false) {
        Ok(result) => {
            let _ = fs::remove_file(path_with_suffix(prices_path, ".pulling"));
            EnsureFreshResult {
                pulled: true,
                reason: Some(status.reason),
                error: None,
                scheduled: false,
                age_ms: status.age_ms,
                updated_ms: status.updated_ms,
                source: status.source,
                result: Some(result),
            }
        }
        Err(err) => {
            let scheduled = schedule_price_pull_if_stale(prices_path, source, max_age_ms);
            EnsureFreshResult {
                pulled: false,
                reason: Some("pull_failed".to_string()),
                error: Some(err.to_string()),
                scheduled: scheduled.scheduled,
                age_ms: None,
                updated_ms: None,
                source: None,
                result: None,
            }
        }
    }
}

/// JS: `priceRefreshOptions(config)`'s return shape — matches `report.rs`'s
/// documented `crate::pull_prices::PriceRefreshOptions` exactly.
pub struct PriceRefreshOptions {
    pub auto_pull: bool,
    pub max_age_ms: i64,
    pub source: String,
}

/// JS: `priceRefreshOptions(config)`.
pub fn price_refresh_options(config: &Value) -> PriceRefreshOptions {
    let prices = match config.get("prices") {
        Some(v) if v.is_object() => Some(v),
        _ => None,
    };

    let hours_val = prices.and_then(|p| p.get("auto_pull_interval_hours"));
    let hours_is_false = matches!(hours_val, Some(Value::Bool(false)));
    let auto_pull_is_false = matches!(prices.and_then(|p| p.get("auto_pull")), Some(Value::Bool(false)));

    let mut max_age_ms = DEFAULT_MAX_AGE_MS;
    if hours_is_false || auto_pull_is_false {
        max_age_ms = 0;
    } else if let Some(h) = hours_val {
        if !h.is_null() {
            let n = js_number_value(Some(h));
            if n.is_finite() {
                max_age_ms = (n * 60.0 * 60.0 * 1000.0).max(0.0) as i64;
            }
        }
    }

    let source = opt_str_from(prices.and_then(|p| p.get("source"))).unwrap_or_else(|| "openrouter".to_string());

    PriceRefreshOptions { auto_pull: max_age_ms > 0, max_age_ms, source }
}

// ---------------------------------------------------------------------------
// CLI: `token-tracker prices pull|show`
// ---------------------------------------------------------------------------

/// JS: `usage()`'s template literal, verbatim (including the trailing blank
/// line `console.log` adds).
const USAGE: &str = "Usage:\n  npx @mbrundige/token-tracker prices pull [--source openrouter|llmcosthub|benchgecko] [--out PATH] [--dry-run]\n  npx @mbrundige/token-tracker prices show [--out PATH]\n\nDefaults:\n  --source openrouter\n  --out ~/.token-tracker/prices.json\n";

fn print_usage() {
    println!("{}", USAGE);
}

fn default_prices_path() -> PathBuf {
    paths::paths(None)
        .map(|p| p.prices_path)
        .unwrap_or_else(|_| paths::resolve_data_dir().join("prices.json"))
}

/// JS: `pullPrices(argv)`.
fn pull_cmd(argv: &[String]) -> Result<i32> {
    let mut source = "openrouter".to_string();
    let mut out_path = default_prices_path();
    let mut dry_run = false;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--source" => {
                i += 1;
                source = argv.get(i).cloned().unwrap_or_default();
            }
            "--out" => {
                i += 1;
                let raw = argv.get(i).cloned().unwrap_or_default();
                out_path = expand(&raw);
            }
            "--dry-run" => dry_run = true,
            "-h" | "--help" => {
                print_usage();
                return Ok(0);
            }
            other => {
                eprintln!("token-tracker: unknown prices pull option: {}", other);
                print_usage();
                return Ok(2);
            }
        }
        i += 1;
    }

    let result = run_pull(&source, &out_path, dry_run)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    let _ = fs::remove_file(path_with_suffix(&out_path, ".pulling"));
    Ok(0)
}

/// JS: `showPrices(argv)`.
fn show_cmd(argv: &[String]) -> i32 {
    let mut out_path = default_prices_path();

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--out" => {
                i += 1;
                let raw = argv.get(i).cloned().unwrap_or_default();
                out_path = expand(&raw);
            }
            "-h" | "--help" => {
                print_usage();
                return 0;
            }
            other => {
                eprintln!("token-tracker: unknown prices show option: {}", other);
                print_usage();
                return 2;
            }
        }
        i += 1;
    }

    if !out_path.exists() {
        eprintln!(
            "token-tracker: no prices file at {}. Run: npx @mbrundige/token-tracker prices pull",
            out_path.display()
        );
        return 1;
    }

    let doc = load_json_object(&out_path);
    let models = doc.get("models").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let mut entries: Vec<(String, Value)> = models.into_iter().collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    println!("Prices: {}", out_path.display());
    if let Some(updated_at) = opt_str(&doc, "updated_at") {
        println!("Updated: {}", updated_at);
    }
    if let Some(source) = opt_str(&doc, "source") {
        match opt_str(&doc, "source_url") {
            Some(url) => println!("Source:  {} ({})", source, url),
            None => println!("Source:  {}", source),
        }
    }

    let default_val = doc.get("default");
    let default_input = match default_val.and_then(|d| d.get("input_per_million_usd")) {
        Some(v) if !v.is_null() => js_display(v),
        _ => "?".to_string(),
    };
    let default_output = match default_val.and_then(|d| d.get("output_per_million_usd")) {
        Some(v) if !v.is_null() => js_display(v),
        _ => "?".to_string(),
    };
    println!("Default: ${}/M in \u{b7} ${}/M out", default_input, default_output);

    println!();
    println!("{} {} {}", pad_end("model", 28), pad_start("input/M", 10), pad_start("output/M", 10));
    println!("{} {} {}", "-".repeat(28), "-".repeat(10), "-".repeat(10));
    for (key, rates) in &entries {
        let locked = rates.get("locked").and_then(|v| v.as_bool()).unwrap_or(false);
        let lock_ch = if locked { "*" } else { " " };
        let label = format!("{}{}", lock_ch, key);
        let input_str = match rates.get("input_per_million_usd") {
            Some(v) => js_display(v),
            None => "undefined".to_string(),
        };
        let output_str = match rates.get("output_per_million_usd") {
            Some(v) => js_display(v),
            None => "undefined".to_string(),
        };
        println!("{} {} {}", pad_end(&label, 28), pad_start(&input_str, 10), pad_start(&output_str, 10));
    }
    println!();
    println!("{} model rate(s). * = locked local override.", entries.len());
    0
}

/// JS: `main(argv)`, folded together with the top-level `main().catch(...)`
/// guard (`process.exit(1)` on an uncaught error) from `if (require.main
/// === module)`. `argv` is just the subcommand + flags (the JS equivalent of
/// `process.argv.slice(2)`) — no program name.
pub fn run(argv: &[String]) -> i32 {
    match argv.first().map(|s| s.as_str()) {
        None | Some("-h") | Some("--help") => {
            print_usage();
            0
        }
        Some("pull") => match pull_cmd(&argv[1..]) {
            Ok(code) => code,
            Err(err) => {
                eprintln!("token-tracker: {}", err);
                1
            }
        },
        Some("show") => show_cmd(&argv[1..]),
        Some(other) => {
            eprintln!("token-tracker: unknown prices command: {}", other);
            print_usage();
            2
        }
    }
}
