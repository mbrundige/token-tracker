//! Port of `scripts/report-token-usage.js` — the `token-tracker report` command.
//!
//! Reads `history.jsonl`, aggregates usage/cost by project/feature, and prints
//! a formatted terminal report (feature table + a GitHub-style token heat map).
//!
//! ## Assumed signatures of sibling modules (none existed on disk at port time)
//!
//! `crate::paths`:
//! ```ignore
//! pub struct Paths {
//!     pub data_dir: PathBuf,
//!     pub legacy_data_dir: PathBuf,
//!     pub config_path: PathBuf,
//!     pub history_path: PathBuf,
//!     pub prices_path: PathBuf,
//! }
//! pub fn paths() -> Paths
//! ```
//!
//! `crate::ansi`:
//! ```ignore
//! pub struct Ansi { pub enabled: bool }
//! impl Ansi {
//!     pub fn bold(&self, text: &str) -> String;
//!     pub fn dim(&self, text: &str) -> String;
//!     pub fn cyan(&self, text: &str) -> String;
//!     pub fn green(&self, text: &str) -> String;
//!     pub fn yellow(&self, text: &str) -> String;
//!     pub fn heat(&self, level: usize, ch: &str) -> String;
//! }
//! pub fn create_ansi() -> Ansi
//! ```
//!
//! `crate::pricing`:
//! ```ignore
//! pub struct Rates { pub input_per_million_usd: f64, pub output_per_million_usd: f64 }
//! pub struct Prices {
//!     pub updated_at: Option<String>,
//!     pub source: Option<String>,
//!     pub source_url: Option<String>,
//!     pub default: Option<Rates>,
//!     pub models: std::collections::HashMap<String, Rates>,
//! }
//! pub fn load_prices(path: &std::path::Path) -> Prices;
//! pub fn format_cost(usd: Option<f64>, prefix: &str, digits: Option<u8>, unpriced: &str) -> String;
//!
//! pub struct CostItem {
//!     pub total: f64,
//!     pub prompt_tokens: Option<f64>,
//!     pub completion_tokens: Option<f64>,
//!     pub total_tokens: Option<f64>,
//!     pub model: Option<String>,
//!     pub cost_delta_usd: Option<f64>,
//! }
//! pub struct EpochFeatureCost {
//!     pub cost_usd: Option<f64>,
//!     pub approximate: bool,
//!     pub unpriced_deltas: u32,
//!     pub locked_deltas: u32,
//!     pub live_deltas: u32,
//! }
//! pub fn epoch_feature_cost(items: &[CostItem], prices: &Prices, prefer_locked: bool) -> EpochFeatureCost;
//! ```
//!
//! `crate::pull_prices`:
//! ```ignore
//! pub struct PriceRefreshOptions { pub auto_pull: bool, pub max_age_ms: i64, pub source: String }
//! pub fn price_refresh_options(config: &serde_json::Value) -> PriceRefreshOptions;
//!
//! pub struct EnsureFreshResult {
//!     pub pulled: bool,
//!     pub reason: Option<String>,
//!     pub error: Option<String>,
//!     pub scheduled: bool,
//! }
//! pub fn ensure_fresh_prices(
//!     prices_path: &std::path::Path,
//!     source: &str,
//!     max_age_ms: i64,
//!     on_status: impl FnMut(&str),
//! ) -> EnsureFreshResult;
//! ```
//!
//! `FeatureRow` mirrors JS's per-feature breakdown object shape in full
//! (`snapshots`/`last_seen` aren't rendered by `render_feature_table` today
//! but are kept for fidelity and future callers).
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{Datelike, Duration, NaiveDate, Utc};
use serde_json::Value;

use crate::ansi::{self, Ansi};
use crate::pricing::{self, EpochFeatureCostOptions, FormatCostOptions};
use crate::pull_prices;

const HEAT: [&str; 5] = ["·", "░", "▒", "▓", "█"];
const DAY_LABELS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

// ---------------------------------------------------------------------------
// Small formatting helpers
// ---------------------------------------------------------------------------

fn compact(n: f64) -> String {
    if n >= 1_000_000.0 {
        format!("{:.1}M", n / 1_000_000.0)
    } else if n >= 10_000.0 {
        format!("{}k", (n / 1000.0).round() as i64)
    } else if n >= 1000.0 {
        format!("{:.1}k", n / 1000.0)
    } else {
        format_js_like_number(n)
    }
}

/// Best-effort match for JS's `String(number)` on the "plain" branch of `compact`.
fn format_js_like_number(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

/// JS `Number(x)` coercion approximation for values pulled out of a loosely
/// typed history row (JSON numbers, numeric strings, bools, null).
fn js_number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                Some(0.0)
            } else {
                t.parse::<f64>().ok()
            }
        }
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Null => Some(0.0),
        _ => None,
    }
}

fn row_str(row: &Value, key: &str) -> Option<String> {
    row.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn row_num(row: &Value, key: &str) -> Option<f64> {
    row.get(key).and_then(js_number)
}

// ---------------------------------------------------------------------------
// Data loading
// ---------------------------------------------------------------------------

fn load_rows(history_path: &Path) -> Result<Vec<Value>> {
    if !history_path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(history_path)
        .with_context(|| format!("failed to read history file: {}", history_path.display()))?;
    let mut rows = Vec::new();
    for line in content.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            if value.is_object() {
                rows.push(value);
            }
        }
    }
    Ok(rows)
}

fn load_config(config_path: &Path) -> Result<Value> {
    if !config_path.exists() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let content = match fs::read_to_string(config_path) {
        Ok(c) => c,
        Err(_) => return Ok(Value::Object(serde_json::Map::new())),
    };
    match serde_json::from_str::<Value>(&content) {
        Ok(v) if v.is_object() => Ok(v),
        _ => Ok(Value::Object(serde_json::Map::new())),
    }
}

fn day_key(iso: &str) -> Option<String> {
    // Mirrors `new Date(iso)` (Invalid Date -> null) + `.toISOString().slice(0, 10)`.
    // Only handles RFC3339-ish strings (what this tool actually writes); JS's
    // `Date` parser accepts a broader grab-bag of formats that we don't attempt here.
    chrono::DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).format("%Y-%m-%d").to_string())
}

/// Sum epoch peaks so feature resets do not double-count growing snapshots.
pub fn epoch_total(sorted_totals: &[f64]) -> f64 {
    if sorted_totals.is_empty() {
        return 0.0;
    }
    let mut peak = 0.0_f64;
    let mut sum = 0.0_f64;
    let mut last = 0.0_f64;
    for &total in sorted_totals {
        if total < last {
            sum += peak;
            peak = total;
        } else {
            peak = peak.max(total);
        }
        last = total;
    }
    sum + peak
}

// ---------------------------------------------------------------------------
// Feature breakdown
// ---------------------------------------------------------------------------

struct FeatureItem {
    ts: String,
    total: f64,
    prompt_tokens: Option<f64>,
    completion_tokens: Option<f64>,
    total_tokens: Option<f64>,
    model: Option<String>,
    cost_delta_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct FeatureRow {
    pub project: String,
    pub feature: String,
    pub total: f64,
    pub cost_usd: Option<f64>,
    pub cost_approximate: bool,
    pub cost_locked_deltas: u32,
    pub cost_live_deltas: u32,
    pub snapshots: usize,
    pub last_seen: Option<String>,
}

pub fn feature_breakdown(rows: &[Value], prices: &Value) -> Vec<FeatureRow> {
    // Preserve first-seen order (like a JS Map) so the final stable sort by
    // total ties break the same way the JS `Map` iteration order would.
    let mut order: Vec<(String, String)> = Vec::new();
    let mut by_feature: HashMap<(String, String), Vec<FeatureItem>> = HashMap::new();

    for row in rows {
        let feature = row_str(row, "feature")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(none)".to_string());
        let project = row_str(row, "project")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let key = (project, feature);

        if !by_feature.contains_key(&key) {
            order.push(key.clone());
            by_feature.insert(key.clone(), Vec::new());
        }
        by_feature.get_mut(&key).unwrap().push(FeatureItem {
            ts: row_str(row, "timestamp").unwrap_or_default(),
            total: row_num(row, "total_tokens").unwrap_or(0.0),
            prompt_tokens: row_num(row, "prompt_tokens"),
            completion_tokens: row_num(row, "completion_tokens"),
            total_tokens: row_num(row, "total_tokens"),
            model: row_str(row, "model"),
            cost_delta_usd: row_num(row, "cost_delta_usd"),
        });
    }

    let mut out = Vec::with_capacity(order.len());
    for key in order {
        let items = by_feature.get_mut(&key).unwrap();
        items.sort_by(|a, b| a.ts.cmp(&b.ts));

        let totals: Vec<f64> = items.iter().map(|i| i.total).collect();
        let total = epoch_total(&totals);

        // Build each item as a sparse JSON object rather than via `json!`
        // with `Option<f64>` fields: `pricing::token_split`/`epoch_feature_cost`
        // read these through `js_number`, which deliberately distinguishes a
        // truly *missing* key (mirrors JS `undefined` -> NaN, e.g. no
        // prompt/completion split recorded, or no locked cost yet) from a
        // key explicitly present with JSON `null` (mirrors JS `null` -> 0).
        // `serde_json::json!` would serialize `None` as an explicit `null`,
        // collapsing that distinction and silently breaking both the
        // approximate-split fallback and the locked-vs-live-priced cost
        // accounting.
        let cost_items: Vec<Value> = items
            .iter()
            .map(|i| {
                let mut m = serde_json::Map::new();
                m.insert("total".to_string(), serde_json::json!(i.total));
                if let Some(v) = i.prompt_tokens {
                    m.insert("prompt_tokens".to_string(), serde_json::json!(v));
                }
                if let Some(v) = i.completion_tokens {
                    m.insert("completion_tokens".to_string(), serde_json::json!(v));
                }
                if let Some(v) = i.total_tokens {
                    m.insert("total_tokens".to_string(), serde_json::json!(v));
                }
                m.insert(
                    "model".to_string(),
                    i.model.clone().map(Value::String).unwrap_or(Value::Null),
                );
                if let Some(v) = i.cost_delta_usd {
                    m.insert("cost_delta_usd".to_string(), serde_json::json!(v));
                }
                Value::Object(m)
            })
            .collect();
        let cost_info = pricing::epoch_feature_cost(
            cost_items.iter(),
            prices,
            EpochFeatureCostOptions { prefer_locked: true },
        );

        let last_seen = items.last().map(|i| i.ts.clone());

        out.push(FeatureRow {
            project: key.0,
            feature: key.1,
            total,
            cost_usd: cost_info.cost_usd,
            cost_approximate: cost_info.approximate,
            cost_locked_deltas: cost_info.locked_deltas,
            cost_live_deltas: cost_info.live_deltas,
            snapshots: items.len(),
            last_seen,
        });
    }

    out.sort_by(|a, b| b.total.partial_cmp(&a.total).unwrap_or(std::cmp::Ordering::Equal));
    out
}

// ---------------------------------------------------------------------------
// Daily totals / heat map
// ---------------------------------------------------------------------------

/// Per day+feature: track sorted totals, epoch-sum, then sum features for the day.
fn daily_totals(rows: &[Value]) -> HashMap<String, f64> {
    let mut day_feature: HashMap<String, Vec<(String, f64)>> = HashMap::new();

    for row in rows {
        let day = match row_str(row, "timestamp").as_deref().and_then(day_key) {
            Some(d) => d,
            None => continue,
        };
        let feature = row_str(row, "feature")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(none)".to_string());
        let project = row_str(row, "project")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        let map_key = format!("{}\t{}\t{}", day, project, feature);
        let ts = row_str(row, "timestamp").unwrap_or_default();
        let total = row_num(row, "total_tokens").unwrap_or(0.0);
        day_feature.entry(map_key).or_default().push((ts, total));
    }

    let mut by_day: HashMap<String, f64> = HashMap::new();
    for (map_key, mut items) in day_feature {
        let day = map_key.split('\t').next().unwrap_or_default().to_string();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        let totals: Vec<f64> = items.iter().map(|(_, t)| *t).collect();
        let total = epoch_total(&totals);
        *by_day.entry(day).or_insert(0.0) += total;
    }
    by_day
}

fn heat_level(value: f64, max: f64) -> usize {
    if value == 0.0 || value.is_nan() || max <= 0.0 {
        return 0;
    }
    let ratio = value / max;
    if ratio <= 0.15 {
        1
    } else if ratio <= 0.4 {
        2
    } else if ratio <= 0.7 {
        3
    } else {
        4
    }
}

fn render_heatmap(by_day: &HashMap<String, f64>, weeks: usize, ansi: &Ansi) -> String {
    let end: NaiveDate = Utc::now().date_naive();
    let end_dow = end.weekday().num_days_from_sunday() as i64;
    // End on today; start enough days back to fill `weeks` columns ending this week.
    let offset_days = (weeks as i64) * 7 - 1 + end_dow;
    let start = end - Duration::days(offset_days);

    let mut days: Vec<(String, f64, usize)> = Vec::new();
    let mut d = start;
    while d <= end {
        let key = d.format("%Y-%m-%d").to_string();
        let value = *by_day.get(&key).unwrap_or(&0.0);
        let dow = d.weekday().num_days_from_sunday() as usize;
        days.push((key, value, dow));
        d = d + Duration::days(1);
    }

    let max = days.iter().fold(0.0_f64, |acc, (_, v, _)| acc.max(*v));

    let columns: Vec<&[(String, f64, usize)]> = days.chunks(7).collect();

    let mut lines: Vec<String> = Vec::new();
    lines.push(ansi.bold(&format!("Token heat map (last {} weeks, UTC)", columns.len())));

    let mut label_line = String::from("     ");
    for (i, _) in columns.iter().enumerate() {
        if i % 4 == 0 {
            label_line.push_str(&format!("{:>2}", i + 1));
        } else {
            label_line.push_str("  ");
        }
    }
    lines.push(ansi.dim(&label_line));

    for dow in 0..7usize {
        let label = format!("{:<4}", DAY_LABELS[dow]);
        let mut row = format!("{} ", ansi.dim(&label));
        for col in &columns {
            match col.iter().find(|(_, _, d)| *d == dow) {
                None => row.push_str("  "),
                Some((_, value, _)) => {
                    let level = heat_level(*value, max);
                    row.push_str(&ansi.heat(level as i64, HEAT[level]));
                    row.push(' ');
                }
            }
        }
        lines.push(row.trim_end().to_string());
    }

    lines.push(String::new());
    let legend: Vec<String> = HEAT.iter().enumerate().map(|(i, ch)| ansi.heat(i as i64, ch)).collect();
    lines.push(format!(
        "{} {} {}   {}",
        ansi.dim("less"),
        legend.join(" "),
        ansi.dim("more"),
        ansi.dim(&format!("max/day {} toks", compact(max)))
    ));
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Feature table
// ---------------------------------------------------------------------------

pub fn render_feature_table(features: &[FeatureRow], ansi: &Ansi) -> String {
    if features.is_empty() {
        return "No token history yet.".to_string();
    }

    let mut lines = vec![ansi.bold("By feature"), ansi.dim("----------")];
    let grand: f64 = features.iter().map(|f| f.total).sum();
    let grand_cost: f64 = features.iter().map(|f| f.cost_usd.unwrap_or(0.0)).sum();
    let any_cost = features.iter().any(|f| f.cost_usd.is_some());
    let any_approx = features
        .iter()
        .any(|f| f.cost_approximate && f.cost_usd.is_some());

    for f in features {
        let label = format!("{}/{}", f.project, f.feature);
        let pct: i64 = if grand > 0.0 {
            ((f.total / grand) * 100.0).round() as i64
        } else {
            0
        };
        let bar_width = 20usize;
        let filled: usize = if grand > 0.0 {
            ((f.total / grand) * bar_width as f64).round() as usize
        } else {
            0
        };
        let filled = filled.min(bar_width);
        let bar = format!(
            "[{}{}]",
            ansi.cyan(&"#".repeat(filled)),
            ansi.dim(&".".repeat(bar_width - filled))
        );

        let cost_raw: Option<String> = if any_cost {
            Some(format!(
                "{:>8}",
                pricing::format_cost(
                    f.cost_usd,
                    FormatCostOptions { prefix: "$", digits: None, unpriced: "  n/a" }
                )
            ))
        } else {
            None
        };
        let approx = if f.cost_approximate && f.cost_usd.is_some() {
            "~"
        } else {
            " "
        };
        let cost_aligned = match &cost_raw {
            None => String::new(),
            Some(raw) => format!("  {}{}", approx, ansi.green(raw)),
        };

        let label_padded = format!("{:<36}", label);
        let compact_padded = format!("{:>7}", compact(f.total));
        let pct_padded = format!("{:>3}", pct);

        lines.push(format!(
            "{} {}  {}%{}  {}",
            label_padded,
            ansi.bold(&compact_padded),
            pct_padded,
            cost_aligned,
            bar
        ));
    }

    lines.push(String::new());
    if any_cost {
        let locked: u32 = features.iter().map(|f| f.cost_locked_deltas).sum();
        let live: u32 = features.iter().map(|f| f.cost_live_deltas).sum();
        let mut bits: Vec<String> = Vec::new();
        if locked > 0 {
            bits.push(format!("{} locked", locked));
        }
        if live > 0 {
            bits.push(format!("{} live-priced", live));
        }
        if any_approx {
            bits.push("some rows lack prompt/completion split".to_string());
        }
        let note = if bits.is_empty() {
            String::new()
        } else {
            format!(" ({})", bits.join(", "))
        };
        lines.push(format!(
            "Total tracked: {} toks / {} est across {} feature(s){}",
            ansi.bold(&compact(grand)),
            ansi.green(&pricing::format_cost(
                Some(grand_cost),
                FormatCostOptions { prefix: "$", digits: None, unpriced: "n/a" }
            )),
            features.len(),
            ansi.dim(&note)
        ));
    } else {
        lines.push(format!(
            "Total tracked: {} toks across {} feature(s)",
            ansi.bold(&compact(grand)),
            features.len()
        ));
        lines.push(ansi.dim(
            "Cost: n/a (add ~/.token-tracker/prices.json or run: npx @mbrundige/token-tracker prices pull)",
        ));
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Current scope
// ---------------------------------------------------------------------------

struct Scope {
    project: String,
    feature: Option<String>,
}

fn current_scope(config: &Value) -> Scope {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let cwd_str = cwd.to_string_lossy().to_string();

    let project = config
        .get("projects")
        .and_then(|p| p.get(&cwd_str))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            cwd.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        });

    let feature = config
        .get("features")
        .and_then(|f| f.get(&cwd_str))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Scope { project, feature }
}

// ---------------------------------------------------------------------------
// CLI entry point (`token-tracker report`)
// ---------------------------------------------------------------------------

/// Equivalent of report-token-usage.js's `main()` / `require.main === module` block.
pub fn run() -> Result<()> {
    let paths = crate::paths::paths(None)?;
    let ansi = ansi::create_ansi();

    let rows = load_rows(&paths.history_path)?;
    let config = load_config(&paths.config_path)?;

    let refresh = pull_prices::price_refresh_options(&config);
    if refresh.auto_pull {
        let ansi_status = &ansi;
        let ensured = pull_prices::ensure_fresh_prices(
            &paths.prices_path,
            &refresh.source,
            refresh.max_age_ms,
            |msg: &str| eprintln!("{}", ansi_status.dim(msg)),
        );
        if ensured.pulled {
            eprintln!(
                "{}",
                ansi.dim(&format!("Prices updated from {}.", refresh.source))
            );
        } else if ensured.reason.as_deref() == Some("pull_failed") {
            let err_msg = ensured.error.clone().unwrap_or_else(|| "error".to_string());
            let suffix = if ensured.scheduled {
                " and retrying in background"
            } else {
                ""
            };
            eprintln!(
                "{}",
                ansi.yellow(&format!(
                    "Price refresh failed ({}); using cached rates{}.",
                    err_msg, suffix
                ))
            );
        }
    }

    let prices = pricing::load_prices(&paths.prices_path);
    let scope = current_scope(&config);
    let features = feature_breakdown(&rows, &prices);
    let by_day = daily_totals(&rows);

    println!("{}", ansi.bold(&ansi.cyan("Token Tracker Report")));
    println!("{}", ansi.dim("===================="));
    println!("{} {}", ansi.dim("History:"), paths.history_path.display());
    let missing_note = if paths.prices_path.exists() {
        String::new()
    } else {
        ansi.yellow(" (missing)")
    };
    println!(
        "{}  {}{}",
        ansi.dim("Prices:"),
        paths.prices_path.display(),
        missing_note
    );
    if let Some(updated_at) = prices.get("updated_at").and_then(|v| v.as_str()) {
        println!("{} {}", ansi.dim("Price as of:"), updated_at);
    }
    if let Some(source) = prices.get("source").and_then(|v| v.as_str()) {
        println!("{} {}", ansi.dim("Price source:"), source);
    }
    let scope_label = match &scope.feature {
        Some(f) => format!("{}/{}", scope.project, f),
        None => scope.project.clone(),
    };
    println!("{} {}", ansi.dim("Current scope:"), ansi.cyan(&scope_label));
    println!();
    println!("{}", render_feature_table(&features, &ansi));
    println!();
    println!("{}", render_heatmap(&by_day, 16, &ansi));

    Ok(())
}
