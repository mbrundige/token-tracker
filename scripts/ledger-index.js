#!/usr/bin/env node
"use strict";

const fs = require("fs");
const path = require("path");
const { paths } = require("./paths.js");
const { loadPrices, computeCostDelta, epochFeatureCost } = require("./pricing.js");

const SCHEMA_VERSION = 1;

function resolveIndexPath(historyPath) {
  const indexOverride = process.env.TOKEN_TRACKER_INDEX;
  if (indexOverride) {
    return indexOverride.startsWith("~/") ? 
      path.join(require("os").homedir(), indexOverride.slice(2)) : 
      indexOverride;
  }
  
  const dir = path.dirname(historyPath);
  return path.join(dir, "ledger-index.json");
}

function loadIndex(indexPath) {
  if (!fs.existsSync(indexPath)) return null;
  try {
    const data = JSON.parse(fs.readFileSync(indexPath, "utf8"));
    if (data && typeof data === "object" && data.version === SCHEMA_VERSION) {
      return data;
    }
  } catch {
    // ignore parse errors
  }
  return null;
}

function isIndexFresh(index, historyPath) {
  if (!index || !fs.existsSync(historyPath)) return false;
  
  const stat = fs.statSync(historyPath);
  return index.history_bytes === stat.size && 
         index.history_mtime_ms === stat.mtimeMs;
}

function loadHistoryRows(historyPath) {
  if (!fs.existsSync(historyPath)) return [];
  const rows = [];
  for (const line of fs.readFileSync(historyPath, "utf8").split("\n")) {
    if (!line.trim()) continue;
    try {
      const row = JSON.parse(line);
      if (row && typeof row === "object" && !Array.isArray(row)) rows.push(row);
    } catch {
      // skip bad lines
    }
  }
  return rows;
}

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

function rebuildIndex({ historyPath, pricesPath, indexPath }) {
  const rows = loadHistoryRows(historyPath);
  const prices = loadPrices(pricesPath);
  
  // Group by project\tfeature
  const byFeature = new Map();
  for (const row of rows) {
    const project = row.project || "unknown";
    const feature = row.feature || "(none)";
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
      raw: row, // Keep for computeCostDelta
    });
  }
  
  const features = {};
  for (const [key, items] of byFeature) {
    const [project, feature] = key.split("\t");
    items.sort((a, b) => String(a.ts).localeCompare(String(b.ts)));
    
    const total = epochTotal(items.map(i => i.total));
    const costInfo = epochFeatureCost(items, prices, { preferLocked: true });
    const last = items[items.length - 1];
    
    features[key] = {
      project,
      feature,
      total_tokens: total,
      cost_usd: costInfo.costUsd,
      snapshots: items.length,
      last_seen: last ? last.ts : null,
      last: last ? last.raw : null, // Store last raw snapshot for computeCostDelta
    };
  }
  
  const stat = fs.existsSync(historyPath) ? fs.statSync(historyPath) : { size: 0, mtimeMs: 0 };
  
  const index = {
    version: SCHEMA_VERSION,
    history_bytes: stat.size,
    history_mtime_ms: stat.mtimeMs,
    features,
  };
  
  fs.mkdirSync(path.dirname(indexPath), { recursive: true });
  fs.writeFileSync(indexPath, JSON.stringify(index, null, 2), "utf8");
  
  return index;
}

function ensureIndex({ historyPath, pricesPath, indexPath }) {
  const index = loadIndex(indexPath);
  if (index && isIndexFresh(index, historyPath)) {
    return index;
  }
  return rebuildIndex({ historyPath, pricesPath, indexPath });
}

function applyAppend(index, snapshot, prices) {
  const project = snapshot.project || "unknown";
  const feature = snapshot.feature || "(none)";
  const key = `${project}\t${feature}`;
  
  let entry = index.features[key];
  if (!entry) {
    // First snapshot for this feature
    entry = {
      project,
      feature,
      total_tokens: 0,
      cost_usd: null,
      snapshots: 0,
      last_seen: null,
      last: null,
    };
    index.features[key] = entry;
  }
  
  const current = {
    project,
    feature,
    model: snapshot.model,
    prompt_tokens: snapshot.prompt_tokens,
    completion_tokens: snapshot.completion_tokens,
    total_tokens: snapshot.total_tokens,
  };
  
  const previous = entry.last;
  const priced = computeCostDelta(previous, current, prices);
  
  // Update total tokens with epoch logic
  const currentTotal = Number(snapshot.total_tokens || 0);
  const previousTotal = previous ? Number(previous.total_tokens || 0) : 0;
  
  if (!previous || currentTotal < previousTotal) {
    // New feature or epoch reset
    entry.total_tokens = currentTotal;
    entry.cost_usd = priced.costDeltaUsd;
  } else {
    // Growth within epoch
    const deltaTokens = currentTotal - previousTotal;
    entry.total_tokens += deltaTokens;
    
    if (entry.cost_usd != null && priced.costDeltaUsd != null) {
      entry.cost_usd += priced.costDeltaUsd;
    } else if (priced.costDeltaUsd != null) {
      entry.cost_usd = priced.costDeltaUsd;
    }
  }
  
  entry.snapshots += 1;
  entry.last_seen = snapshot.timestamp;
  entry.last = snapshot;
  
  return index;
}

function reportFromIndex(index) {
  const features = [];
  for (const entry of Object.values(index.features)) {
    features.push({
      project: entry.project,
      feature: entry.feature,
      total_tokens: entry.total_tokens,
      cost_usd: entry.cost_usd,
      snapshots: entry.snapshots,
      last_seen: entry.last_seen,
    });
  }
  features.sort((a, b) => b.total_tokens - a.total_tokens);
  return features;
}

function lastFromIndex(index, project, feature) {
  const key = `${project}\t${feature || "(none)"}`;
  const entry = index.features[key];
  return entry ? entry.last : null;
}

function main() {
  // Demo/test the index functionality
  const { historyPath, pricesPath } = paths();
  const indexPath = resolveIndexPath(historyPath);
  
  console.log("Index paths:");
  console.log(`  History: ${historyPath}`);
  console.log(`  Prices:  ${pricesPath}`);
  console.log(`  Index:   ${indexPath}`);
  
  const index = ensureIndex({ historyPath, pricesPath, indexPath });
  console.log(`\nIndex status: ${Object.keys(index.features).length} features`);
  console.log(`History: ${index.history_bytes} bytes, mtime: ${new Date(index.history_mtime_ms).toISOString()}`);
  
  const report = reportFromIndex(index);
  if (report.length > 0) {
    console.log("\nTop features from index:");
    for (const f of report.slice(0, 3)) {
      console.log(`  ${f.project}/${f.feature}: ${f.total_tokens} tokens, ${f.snapshots} snapshots`);
    }
  }
}

module.exports = {
  resolveIndexPath,
  loadIndex,
  isIndexFresh,
  rebuildIndex,
  ensureIndex,
  applyAppend,
  reportFromIndex,
  lastFromIndex,
};

if (require.main === module) main();