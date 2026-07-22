//! Shared pricing helpers for status line + report.
//!
//! Faithful port of `scripts/pricing.js`.
//!
//! prices.json shape:
//! ```json
//! {
//!   "updated_at": "ISO-8601",
//!   "default": { "input_per_million_usd": 2.5, "output_per_million_usd": 15 },
//!   "models": {
//!     "gpt-5.5": { "input_per_million_usd": 5, "output_per_million_usd": 30 }
//!   }
//! }
//! ```
//!
//! Snapshots may lock costs at save time:
//!   `cost_delta_usd` — price of this snapshot's token growth (epoch-aware)
//!   `estimated_cost_usd` — cumulative locked cost through this snapshot in the current epoch
//!
//! Rows/snapshots/prices tables are represented as [`serde_json::Value`] throughout, mirroring
//! the loosely-typed plain-object handling the original JS does directly on parsed JSON.
//!
//! This module ports pricing.js's complete public function/field surface
//! (including pieces no current call site in this binary exercises, e.g.
//! `prices_updated_at_ms*`, some `CostDelta`/`EpochFeatureCost` fields) for
//! fidelity and future callers; `dead_code` is allowed at module scope
//! rather than pruning otherwise-correct ported API surface.
#![allow(dead_code)]

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ---------------------------------------------------------------------------
// JS-coercion helpers (internal)
// ---------------------------------------------------------------------------

/// Mirrors JS `Number(x)`: missing (`undefined`) -> NaN, `null` -> 0, bool -> 1/0,
/// numeric JSON values pass through, strings are parsed (empty string -> 0, otherwise
/// NaN on failure), arrays/objects -> NaN (practical simplification; real prices/history
/// data never puts objects in numeric fields).
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

/// Mirrors JS `n || fallback` for a value already coerced through `Number()`:
/// NaN and 0 are "falsy" and fall through to `fallback`; anything else (including
/// negative numbers and Infinity) is returned as-is.
fn js_or(n: f64, fallback: f64) -> f64 {
    if n.is_nan() || n == 0.0 {
        fallback
    } else {
        n
    }
}

/// Mirrors JS truthiness for a JSON value (used for `prices.updated_at` checks).
fn is_js_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// Mirrors JS `String(x)` for the handful of JSON value kinds pricing.js actually
/// stringifies (strings, numbers, bools, null).
fn value_to_js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// Mirrors `String(obj[key] || default)`: a non-empty string or a non-zero number or
/// `true` overrides; everything else (missing, null, "", 0, false) falls back to `default`.
fn field_or_default(obj: &Value, key: &str, default: &str) -> String {
    match obj.get(key) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(Value::Number(n)) if n.as_f64().unwrap_or(0.0) != 0.0 => n.to_string(),
        Some(Value::Bool(true)) => "true".to_string(),
        _ => default.to_string(),
    }
}

/// Mirrors `String(opt || default)` where `opt` is already a plain string param
/// (as opposed to a field pulled out of a JSON object; see [`field_or_default`]).
fn str_or_default(opt: Option<&str>, default: &str) -> String {
    match opt {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => default.to_string(),
    }
}

/// Best-effort `Date.parse` equivalent: full RFC3339/ISO-8601 timestamps, plus bare
/// `YYYY-MM-DD` dates treated as UTC midnight. Returns `None` on anything else
/// (equivalent to `Number.isFinite(Date.parse(s))` being false).
fn parse_date_to_ms(s: &str) -> Option<f64> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_millis() as f64);
    }
    if let Ok(date) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let dt = date.and_hms_opt(0, 0, 0)?;
        return Some(Utc.from_utc_datetime(&dt).timestamp_millis() as f64);
    }
    None
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A resolved input/output $-per-million-tokens rate pair. Analogous to the plain
/// `{ input, output }` object `parseRates`/`ratesForModel` return in JS.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
}

/// JSON-shaped rate pair, matching the `cost_rates` field written into history
/// snapshots (`{ input_per_million_usd, output_per_million_usd }`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CostRates {
    pub input_per_million_usd: f64,
    pub output_per_million_usd: f64,
}

impl From<Rates> for CostRates {
    fn from(r: Rates) -> Self {
        CostRates {
            input_per_million_usd: r.input,
            output_per_million_usd: r.output,
        }
    }
}

/// Result of [`format_cost`] options — mirrors the JS `{ prefix, digits, unpriced }`
/// options object. `digits = None` reproduces the JS `digits: null` "auto" behavior.
#[derive(Debug, Clone)]
pub struct FormatCostOptions<'a> {
    pub prefix: &'a str,
    pub digits: Option<usize>,
    pub unpriced: &'a str,
}

impl<'a> Default for FormatCostOptions<'a> {
    fn default() -> Self {
        FormatCostOptions {
            prefix: "$",
            digits: None,
            unpriced: "n/a",
        }
    }
}

/// Result of [`token_split`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenSplit {
    pub prompt: f64,
    pub completion: f64,
    pub approximate: bool,
}

/// Result of [`compute_cost_delta`] — mirrors the JS return object
/// `{ deltaIn, deltaOut, reset, approximate, costDeltaUsd, estimatedCostUsd, rates }`.
#[derive(Debug, Clone)]
pub struct CostDelta {
    pub delta_in: f64,
    pub delta_out: f64,
    pub reset: bool,
    pub approximate: bool,
    pub cost_delta_usd: Option<f64>,
    pub estimated_cost_usd: Option<f64>,
    pub rates: Option<CostRates>,
}

/// Options for [`epoch_feature_cost`] — mirrors JS `{ preferLocked = true }`.
#[derive(Debug, Clone, Copy)]
pub struct EpochFeatureCostOptions {
    pub prefer_locked: bool,
}

impl Default for EpochFeatureCostOptions {
    fn default() -> Self {
        EpochFeatureCostOptions {
            prefer_locked: true,
        }
    }
}

/// Result of [`epoch_feature_cost`] — mirrors JS
/// `{ costUsd, approximate, unpricedDeltas, lockedDeltas, liveDeltas }`.
#[derive(Debug, Clone, Copy)]
pub struct EpochFeatureCost {
    pub cost_usd: Option<f64>,
    pub approximate: bool,
    pub unpriced_deltas: u32,
    pub locked_deltas: u32,
    pub live_deltas: u32,
}

/// Input params for [`feature_ongoing_cost`] — mirrors the JS destructured second
/// argument `{ project, feature, inputTokens, outputTokens, totalTokens, model }`.
#[derive(Debug, Clone, Copy)]
pub struct FeatureOngoingCostParams<'a> {
    pub project: Option<&'a str>,
    pub feature: Option<&'a str>,
    pub input_tokens: f64,
    pub output_tokens: f64,
    pub total_tokens: f64,
    pub model: Option<&'a str>,
}

/// Result of [`feature_ongoing_cost`] — mirrors JS `{ lockedUsd, tipUsd, costUsd, tip }`.
#[derive(Debug, Clone)]
pub struct FeatureOngoingCost {
    pub locked_usd: Option<f64>,
    pub tip_usd: Option<f64>,
    pub cost_usd: Option<f64>,
    pub tip: CostDelta,
}

// ---------------------------------------------------------------------------
// Public functions
// ---------------------------------------------------------------------------

/// JS: `parseRates(rates)`.
///
/// `rates` must be a JSON object with finite, non-negative
/// `input_per_million_usd` / `output_per_million_usd` fields.
pub fn parse_rates(rates: &Value) -> Option<Rates> {
    let obj = rates.as_object()?;
    let input = js_number(obj.get("input_per_million_usd"));
    let output = js_number(obj.get("output_per_million_usd"));
    if !input.is_finite() || !output.is_finite() || input < 0.0 || output < 0.0 {
        return None;
    }
    Some(Rates { input, output })
}

/// JS: `loadPrices(filePath)`.
///
/// Returns an empty JSON object (never an error) when the path is missing, unreadable,
/// unparsable, or does not parse to a JSON object.
pub fn load_prices(file_path: &Path) -> Value {
    if file_path.as_os_str().is_empty() || !file_path.exists() {
        return Value::Object(Map::new());
    }
    match fs::read_to_string(file_path) {
        Ok(contents) => match serde_json::from_str::<Value>(&contents) {
            Ok(Value::Object(map)) => Value::Object(map),
            _ => Value::Object(Map::new()),
        },
        Err(_) => Value::Object(Map::new()),
    }
}

/// JS: `pricesUpdatedAtMs(pricesOrPath)` when called with an already-loaded prices
/// object. No mtime fallback (JS only falls back when a path string was passed —
/// see [`prices_updated_at_ms_from_path`] for that variant).
pub fn prices_updated_at_ms(prices: &Value) -> f64 {
    match prices.get("updated_at") {
        Some(v) if is_js_truthy(v) => {
            let s = value_to_js_string(v);
            parse_date_to_ms(&s).unwrap_or(0.0)
        }
        _ => 0.0,
    }
}

/// JS: `pricesUpdatedAtMs(pricesOrPath)` when called with a path string — loads the
/// prices file, reads `updated_at`, and falls back to the file's mtime when
/// `updated_at` is missing/falsy.
pub fn prices_updated_at_ms_from_path(path: &Path) -> f64 {
    let prices = load_prices(path);
    if let Some(v) = prices.get("updated_at") {
        if is_js_truthy(v) {
            let s = value_to_js_string(v);
            return parse_date_to_ms(&s).unwrap_or(0.0);
        }
    }
    if path.exists() {
        if let Ok(meta) = fs::metadata(path) {
            if let Ok(modified) = meta.modified() {
                if let Ok(dur) = modified.duration_since(UNIX_EPOCH) {
                    return dur.as_secs_f64() * 1000.0;
                }
            }
        }
    }
    0.0
}

/// JS: `ratesForModel(prices, model)`.
///
/// Looks up `prices.models` for the longest pattern whose lowercased text is a
/// substring of the lowercased `model` name, falling back to `prices.default`.
pub fn rates_for_model(prices: &Value, model: Option<&str>) -> Option<Rates> {
    let model_lower = model.unwrap_or("").to_lowercase();
    if let Some(models) = prices.get("models").and_then(|v| v.as_object()) {
        let mut entries: Vec<(&String, &Value)> = models.iter().collect();
        // Longest pattern wins; ties keep the (stable-sort-preserved) iteration order.
        // Note: serde_json's default `Map` is a BTreeMap (alphabetical key order)
        // unless the `preserve_order` feature is enabled, so tie-breaking among
        // same-length patterns may differ from JS's insertion-order tie-break.
        entries.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        for (pattern, rates) in entries {
            if pattern.is_empty() {
                continue;
            }
            if model_lower.contains(&pattern.to_lowercase()) {
                if let Some(parsed) = parse_rates(rates) {
                    return Some(parsed);
                }
            }
        }
    }
    prices.get("default").and_then(parse_rates)
}

/// JS: `estimateCostUsd(inputTokens, outputTokens, rates)`.
pub fn estimate_cost_usd(input_tokens: f64, output_tokens: f64, rates: Option<Rates>) -> Option<f64> {
    let rates = rates?;
    let input = js_or(input_tokens, 0.0).max(0.0);
    let output = js_or(output_tokens, 0.0).max(0.0);
    Some((input / 1_000_000.0) * rates.input + (output / 1_000_000.0) * rates.output)
}

/// JS: `estimateCostUsdForModel(prices, model, inputTokens, outputTokens)`.
pub fn estimate_cost_usd_for_model(
    prices: &Value,
    model: Option<&str>,
    input_tokens: f64,
    output_tokens: f64,
) -> Option<f64> {
    estimate_cost_usd(input_tokens, output_tokens, rates_for_model(prices, model))
}

/// JS: `formatCost(usd, { prefix, digits, unpriced })`.
///
/// `usd = None` or non-finite returns `options.unpriced`. Digit count auto-scales
/// with magnitude when `options.digits` is `None`: 2 digits at >= $10, 3 at >= $1,
/// else 4.
pub fn format_cost(usd: Option<f64>, options: FormatCostOptions) -> String {
    let usd = match usd {
        Some(v) if v.is_finite() => v,
        _ => return options.unpriced.to_string(),
    };
    let digits = options.digits.unwrap_or_else(|| {
        if usd >= 10.0 {
            2
        } else if usd >= 1.0 {
            3
        } else {
            4
        }
    });
    format!("{}{:.*}", options.prefix, digits, usd)
}

/// JS: `tokenSplit(row)`.
///
/// Prefers explicit `prompt_tokens`/`completion_tokens` fields when both are finite
/// and non-negative; otherwise approximates a 70/30 prompt/completion split from
/// `total_tokens` and marks the result `approximate`.
pub fn token_split(row: &Value) -> TokenSplit {
    let prompt = js_number(row.get("prompt_tokens"));
    let completion = js_number(row.get("completion_tokens"));
    if prompt.is_finite() && completion.is_finite() && prompt >= 0.0 && completion >= 0.0 {
        return TokenSplit {
            prompt,
            completion,
            approximate: false,
        };
    }
    let total = js_or(js_number(row.get("total_tokens")), 0.0).max(0.0);
    let approx_prompt = (total * 0.7).round();
    TokenSplit {
        prompt: approx_prompt,
        completion: total - approx_prompt,
        approximate: true,
    }
}

/// JS: `sameScope(a, b)`.
///
/// True when `a`/`b` share the same `project` (default `"unknown"`) and `feature`
/// (default `"(none)"`).
pub fn same_scope(a: &Value, b: &Value) -> bool {
    field_or_default(a, "project", "unknown") == field_or_default(b, "project", "unknown")
        && field_or_default(a, "feature", "(none)") == field_or_default(b, "feature", "(none)")
}

/// JS: `computeCostDelta(previous, current, prices)`.
///
/// Prices the token growth from `previous` -> `current` at `prices`. On feature
/// reset (total token count dropping, or scope mismatch), the full current snapshot
/// is priced as the delta.
pub fn compute_cost_delta(previous: Option<&Value>, current: &Value, prices: &Value) -> CostDelta {
    let curr_split = token_split(current);
    let curr_total_raw = js_number(current.get("total_tokens"));
    let curr_total = js_or(curr_total_raw, curr_split.prompt + curr_split.completion).max(0.0);

    let mut base_in = 0.0;
    let mut base_out = 0.0;
    let mut reset = previous.is_none();

    if let Some(prev) = previous {
        if same_scope(prev, current) {
            let prev_total = js_or(js_number(prev.get("total_tokens")), 0.0).max(0.0);
            if curr_total < prev_total {
                reset = true;
            } else {
                let prev_split = token_split(prev);
                base_in = prev_split.prompt;
                base_out = prev_split.completion;
                reset = false;
            }
        } else {
            reset = true;
        }
    }

    let delta_in = (curr_split.prompt - base_in).max(0.0);
    let delta_out = (curr_split.completion - base_out).max(0.0);
    let model = current.get("model").and_then(|v| v.as_str());
    let rates = rates_for_model(prices, model);
    let cost_delta_usd = estimate_cost_usd(delta_in, delta_out, rates);

    let mut estimated_cost_usd = cost_delta_usd;
    if !reset {
        if let Some(prev) = previous {
            let prev_est = js_number(prev.get("estimated_cost_usd"));
            if prev_est.is_finite() {
                estimated_cost_usd = Some(match cost_delta_usd {
                    None => prev_est,
                    Some(d) => prev_est + d,
                });
            }
        }
    }

    CostDelta {
        delta_in,
        delta_out,
        reset,
        approximate: curr_split.approximate,
        cost_delta_usd,
        estimated_cost_usd,
        rates: rates.map(CostRates::from),
    }
}

/// JS: `epochFeatureCost(sortedItems, prices, { preferLocked = true })`.
///
/// Epoch-aware feature cost: prefers each item's locked `cost_delta_usd` when
/// present and `prefer_locked` is set, otherwise re-prices the token delta live
/// against `prices`. `sorted_items` must already be sorted oldest-first.
pub fn epoch_feature_cost<'a, I>(
    sorted_items: I,
    prices: &Value,
    options: EpochFeatureCostOptions,
) -> EpochFeatureCost
where
    I: IntoIterator<Item = &'a Value>,
{
    let mut last_prompt = 0.0_f64;
    let mut last_completion = 0.0_f64;
    let mut last_total = 0.0_f64;
    let mut cost = 0.0_f64;
    let mut priced = false;
    let mut approximate = false;
    let mut unpriced_deltas: u32 = 0;
    let mut locked_deltas: u32 = 0;
    let mut live_deltas: u32 = 0;

    for item in sorted_items {
        let total = js_or(
            js_number(item.get("total")),
            js_or(js_number(item.get("total_tokens")), 0.0),
        )
        .max(0.0);
        let split = token_split(item);
        if split.approximate {
            approximate = true;
        }

        if total < last_total {
            last_prompt = 0.0;
            last_completion = 0.0;
        }

        let delta_in = (split.prompt - last_prompt).max(0.0);
        let delta_out = (split.completion - last_completion).max(0.0);
        let locked = if options.prefer_locked {
            js_number(item.get("cost_delta_usd"))
        } else {
            f64::NAN
        };

        if delta_in > 0.0 || delta_out > 0.0 || locked.is_finite() {
            if options.prefer_locked && locked.is_finite() {
                cost += locked;
                priced = true;
                locked_deltas += 1;
            } else {
                let model = item.get("model").and_then(|v| v.as_str());
                let usd = estimate_cost_usd_for_model(prices, model, delta_in, delta_out);
                match usd {
                    None => unpriced_deltas += 1,
                    Some(u) => {
                        cost += u;
                        priced = true;
                        live_deltas += 1;
                    }
                }
            }
        }

        last_prompt = split.prompt;
        last_completion = split.completion;
        last_total = total;
    }

    EpochFeatureCost {
        cost_usd: if priced { Some(cost) } else { None },
        approximate,
        unpriced_deltas,
        locked_deltas,
        live_deltas,
    }
}

/// JS: `featureOngoingCost(rows, { project, feature, inputTokens, outputTokens, totalTokens, model }, prices)`.
///
/// Status-line ongoing cost for a feature: locked historical deltas in the current
/// epoch plus a live "tip" price for tokens beyond the last snapshot.
pub fn feature_ongoing_cost(
    rows: &[Value],
    params: FeatureOngoingCostParams,
    prices: &Value,
) -> FeatureOngoingCost {
    let scope_feature = str_or_default(params.feature, "(none)");
    let project_key = str_or_default(params.project, "unknown");

    let mut scope_rows: Vec<&Value> = Vec::new();
    for row in rows {
        if field_or_default(row, "project", "unknown") != project_key {
            continue;
        }
        if field_or_default(row, "feature", "(none)") != scope_feature {
            continue;
        }
        scope_rows.push(row);
    }
    scope_rows.sort_by(|a, b| {
        let ta = field_or_default(*a, "timestamp", "");
        let tb = field_or_default(*b, "timestamp", "");
        ta.cmp(&tb)
    });

    let locked_info = epoch_feature_cost(
        scope_rows.iter().copied(),
        prices,
        EpochFeatureCostOptions { prefer_locked: true },
    );
    let last = scope_rows.last().copied();

    let current_value = serde_json::json!({
        "project": params.project,
        "feature": params.feature,
        "model": params.model,
        "prompt_tokens": params.input_tokens,
        "completion_tokens": params.output_tokens,
        "total_tokens": params.total_tokens,
    });

    let tip = compute_cost_delta(last, &current_value, prices);
    let locked_usd = locked_info.cost_usd.unwrap_or(0.0);
    let tip_usd = tip.cost_delta_usd.unwrap_or(0.0);
    let total_usd = locked_usd + tip_usd;

    FeatureOngoingCost {
        locked_usd: locked_info.cost_usd,
        tip_usd: tip.cost_delta_usd,
        cost_usd: if locked_info.cost_usd.is_some() || tip.cost_delta_usd.is_some() {
            Some(total_usd)
        } else {
            None
        },
        tip,
    }
}
