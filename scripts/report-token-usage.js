#!/usr/bin/env node
"use strict";

const fs = require("fs");
const path = require("path");
const { loadPrices, formatCost, epochFeatureCost } = require("./pricing.js");
const { schedulePricePullIfStale, priceRefreshOptions } = require("./pull-prices.js");
const { paths } = require("./paths.js");
const { createAnsi } = require("./ansi.js");

const { historyPath: HISTORY_PATH, configPath: CONFIG_PATH, pricesPath: PRICES_PATH } = paths();

const HEAT = ["·", "░", "▒", "▓", "█"];
const DAY_LABELS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const ansi = createAnsi();

function compact(n) {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 10_000) return `${Math.round(n / 1000)}k`;
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

function loadRows() {
  if (!fs.existsSync(HISTORY_PATH)) return [];
  const rows = [];
  for (const line of fs.readFileSync(HISTORY_PATH, "utf8").split("\n")) {
    if (!line.trim()) continue;
    try {
      const row = JSON.parse(line);
      if (row && typeof row === "object" && !Array.isArray(row)) rows.push(row);
    } catch {
      // skip
    }
  }
  return rows;
}

function loadConfig() {
  if (!fs.existsSync(CONFIG_PATH)) return {};
  try {
    return JSON.parse(fs.readFileSync(CONFIG_PATH, "utf8")) || {};
  } catch {
    return {};
  }
}

function dayKey(iso) {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return null;
  return d.toISOString().slice(0, 10);
}

/** Sum epoch peaks so feature resets do not double-count growing snapshots. */
function epochTotal(sortedTotals) {
  if (!sortedTotals.length) return 0;
  let peak = 0;
  let sum = 0;
  let last = 0;
  for (const total of sortedTotals) {
    if (total < last) {
      sum += peak;
      peak = total;
    } else {
      peak = Math.max(peak, total);
    }
    last = total;
  }
  return sum + peak;
}

function featureBreakdown(rows, prices) {
  const byFeature = new Map();
  for (const row of rows) {
    const feature = row.feature || "(none)";
    const project = row.project || "unknown";
    const key = `${project}\t${feature}`;
    if (!byFeature.has(key)) byFeature.set(key, []);
    byFeature.get(key).push({
      ts: row.timestamp || "",
      total: Number(row.total_tokens || 0),
      prompt_tokens: row.prompt_tokens,
      completion_tokens: row.completion_tokens,
      total_tokens: row.total_tokens,
      model: row.model || null,
      cost_delta_usd: row.cost_delta_usd,
      estimated_cost_usd: row.estimated_cost_usd,
    });
  }

  const out = [];
  for (const [key, items] of byFeature) {
    const [project, feature] = key.split("\t");
    items.sort((a, b) => String(a.ts).localeCompare(String(b.ts)));
    const total = epochTotal(items.map((i) => i.total));
    const costInfo = epochFeatureCost(items, prices, { preferLocked: true });
    const last = items[items.length - 1];
    out.push({
      project,
      feature,
      total,
      costUsd: costInfo.costUsd,
      costApproximate: costInfo.approximate,
      costLockedDeltas: costInfo.lockedDeltas,
      costLiveDeltas: costInfo.liveDeltas,
      snapshots: items.length,
      last_seen: last ? last.ts : null,
    });
  }
  out.sort((a, b) => b.total - a.total);
  return out;
}

function dailyTotals(rows) {
  // Per day+feature: track sorted totals, then epoch-sum; then sum features for the day.
  const dayFeature = new Map();
  for (const row of rows) {
    const day = dayKey(row.timestamp);
    if (!day) continue;
    const feature = `${row.project || "unknown"}\t${row.feature || "(none)"}`;
    const mapKey = `${day}\t${feature}`;
    if (!dayFeature.has(mapKey)) dayFeature.set(mapKey, []);
    dayFeature.get(mapKey).push({
      ts: row.timestamp || "",
      total: Number(row.total_tokens || 0),
    });
  }

  const byDay = new Map();
  for (const [mapKey, items] of dayFeature) {
    const day = mapKey.split("\t")[0];
    items.sort((a, b) => String(a.ts).localeCompare(String(b.ts)));
    const total = epochTotal(items.map((i) => i.total));
    byDay.set(day, (byDay.get(day) || 0) + total);
  }
  return byDay;
}

function heatLevel(value, max) {
  if (!value || max <= 0) return 0;
  const ratio = value / max;
  if (ratio <= 0.15) return 1;
  if (ratio <= 0.4) return 2;
  if (ratio <= 0.7) return 3;
  return 4;
}

function renderHeatmap(byDay, weeks = 16) {
  const today = new Date();
  const todayUtc = Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), today.getUTCDate());
  // End on today; start enough days back to fill `weeks` columns ending this week.
  const end = new Date(todayUtc);
  const start = new Date(todayUtc);
  start.setUTCDate(start.getUTCDate() - (weeks * 7 - 1) - end.getUTCDay());

  const days = [];
  for (let d = new Date(start); d <= end; d.setUTCDate(d.getUTCDate() + 1)) {
    const key = d.toISOString().slice(0, 10);
    days.push({ key, value: byDay.get(key) || 0, dow: d.getUTCDay() });
  }

  const max = Math.max(0, ...days.map((d) => d.value));
  const columns = [];
  for (let i = 0; i < days.length; i += 7) {
    columns.push(days.slice(i, i + 7));
  }

  const lines = [];
  lines.push(ansi.bold(`Token heat map (last ${columns.length} weeks, UTC)`));
  lines.push(
    ansi.dim("     " + columns.map((_, i) => (i % 4 === 0 ? String(i + 1).padStart(2, " ") : "  ")).join("")),
  );

  for (let dow = 0; dow < 7; dow += 1) {
    const label = DAY_LABELS[dow].padEnd(4, " ");
    let row = `${ansi.dim(label)} `;
    for (const col of columns) {
      const cell = col.find((d) => d.dow === dow);
      if (!cell) {
        row += "  ";
        continue;
      }
      const level = heatLevel(cell.value, max);
      row += `${ansi.heat(level, HEAT[level])} `;
    }
    lines.push(row.trimEnd());
  }

  lines.push("");
  const legend = HEAT.map((ch, i) => ansi.heat(i, ch)).join(" ");
  lines.push(`${ansi.dim("less")} ${legend} ${ansi.dim("more")}   ${ansi.dim(`max/day ${compact(max)} toks`)}`);
  return lines.join("\n");
}

function renderFeatureTable(features) {
  if (!features.length) return "No token history yet.";
  const lines = [ansi.bold("By feature"), ansi.dim("----------")];
  const grand = features.reduce((s, f) => s + f.total, 0);
  const grandCost = features.reduce((s, f) => s + (f.costUsd || 0), 0);
  const anyCost = features.some((f) => f.costUsd != null);
  const anyApprox = features.some((f) => f.costApproximate && f.costUsd != null);

  for (const f of features) {
    const label = `${f.project}/${f.feature}`;
    const pct = grand ? Math.round((f.total / grand) * 100) : 0;
    const barWidth = 20;
    const filled = grand ? Math.round((f.total / grand) * barWidth) : 0;
    const bar = `[${ansi.cyan("#".repeat(filled))}${ansi.dim(".".repeat(barWidth - filled))}]`;
    const costRaw = anyCost
      ? formatCost(f.costUsd, { prefix: "$", unpriced: "  n/a" }).padStart(8, " ")
      : null;
    const approx = f.costApproximate && f.costUsd != null ? "~" : " ";
    const costAligned = costRaw == null ? "" : `  ${approx}${ansi.green(costRaw)}`;
    lines.push(
      `${label.padEnd(36, " ")} ${ansi.bold(compact(f.total).padStart(7, " "))}  ${String(pct).padStart(3, " ")}%${costAligned}  ${bar}`,
    );
  }
  lines.push("");
  if (anyCost) {
    const locked = features.reduce((s, f) => s + (f.costLockedDeltas || 0), 0);
    const live = features.reduce((s, f) => s + (f.costLiveDeltas || 0), 0);
    const bits = [];
    if (locked) bits.push(`${locked} locked`);
    if (live) bits.push(`${live} live-priced`);
    if (anyApprox) bits.push("some rows lack prompt/completion split");
    const note = bits.length ? ` (${bits.join(", ")})` : "";
    lines.push(
      `Total tracked: ${ansi.bold(compact(grand))} toks / ${ansi.green(formatCost(grandCost))} est across ${features.length} feature(s)${ansi.dim(note)}`,
    );
  } else {
    lines.push(`Total tracked: ${ansi.bold(compact(grand))} toks across ${features.length} feature(s)`);
    lines.push(
      ansi.dim(
        "Cost: n/a (add ~/.token-tracker/prices.json or run: npx @mbrundige/token-tracker prices pull)",
      ),
    );
  }
  return lines.join("\n");
}

function currentScope(config) {
  const cwd = process.cwd();
  const project = (config.projects && config.projects[cwd]) || path.basename(cwd);
  const feature = (config.features && config.features[cwd]) || null;
  return { project, feature, cwd };
}

function main() {
  const rows = loadRows();
  const config = loadConfig();
  const refresh = priceRefreshOptions(config);
  if (refresh.autoPull) {
    schedulePricePullIfStale({
      pricesPath: PRICES_PATH,
      source: refresh.source,
      maxAgeMs: refresh.maxAgeMs,
    });
  }
  const prices = loadPrices(PRICES_PATH);
  const scope = currentScope(config);
  const features = featureBreakdown(rows, prices);
  const byDay = dailyTotals(rows);

  console.log(ansi.bold(ansi.cyan("Token Tracker Report")));
  console.log(ansi.dim("===================="));
  console.log(`${ansi.dim("History:")} ${HISTORY_PATH}`);
  console.log(
    `${ansi.dim("Prices:")}  ${PRICES_PATH}${fs.existsSync(PRICES_PATH) ? "" : ansi.yellow(" (missing)")}`,
  );
  if (prices.updated_at) console.log(`${ansi.dim("Price as of:")} ${prices.updated_at}`);
  console.log(
    `${ansi.dim("Current scope:")} ${ansi.cyan(`${scope.project}${scope.feature ? `/${scope.feature}` : ""}`)}`,
  );
  console.log("");
  console.log(renderFeatureTable(features));
  console.log("");
  console.log(renderHeatmap(byDay));
}

if (require.main === module) main();

module.exports = { featureBreakdown, epochTotal, renderFeatureTable };
