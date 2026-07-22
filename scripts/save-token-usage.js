#!/usr/bin/env node
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const { loadPrices, computeCostDelta } = require("./pricing.js");
const { paths, expand: expandHome } = require("./paths.js");
const { 
  resolveIndexPath, 
  ensureIndex, 
  applyAppend, 
  lastFromIndex,
  isIndexFresh,
  loadIndex
} = require("./ledger-index.js");

const { historyPath: DEFAULT_HISTORY, pricesPath: DEFAULT_PRICES } = paths();
const INT_FIELDS = ["prompt_tokens", "completion_tokens", "total_tokens"];

function fail(message) {
  console.error(`token-tracker: ${message}`);
  process.exit(2);
}

function usage() {
  console.log(`Usage: token-tracker save --summary "..." [options]

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
`);
}

function parseArgs(argv) {
  const args = {
    json: null,
    history: process.env.TOKEN_TRACKER_HISTORY || null,
    project: null,
    feature: null,
    summary: null,
    model: null,
    source: "manual",
    prompt_tokens: null,
    completion_tokens: null,
    total_tokens: null,
    metadata_json: null,
    help: false,
  };
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    const next = () => {
      i += 1;
      return argv[i];
    };
    if (a === "--help" || a === "-h" || a === "help") {
      args.help = true;
      continue;
    }
    if (a === "--json") args.json = next();
    else if (a === "--history") args.history = next();
    else if (a === "--project") args.project = next();
    else if (a === "--feature") args.feature = next();
    else if (a === "--summary") args.summary = next();
    else if (a === "--model") args.model = next();
    else if (a === "--source") args.source = next();
    else if (a === "--prompt-tokens" || a === "--prompt_tokens") args.prompt_tokens = Number(next());
    else if (a === "--completion-tokens" || a === "--completion_tokens") args.completion_tokens = Number(next());
    else if (a === "--total-tokens" || a === "--total_tokens") args.total_tokens = Number(next());
    else if (a === "--metadata-json" || a === "--metadata_json") args.metadata_json = next();
    else fail(`unknown argument: ${a}`);
  }
  return args;
}

function readStdinSync() {
  try {
    return fs.readFileSync(0, "utf8");
  } catch {
    return "";
  }
}

function loadPayload(args) {
  let raw = args.json;
  if (raw == null && !process.stdin.isTTY) {
    raw = readStdinSync();
  }
  if (raw) {
    let payload;
    try {
      payload = JSON.parse(raw);
    } catch (err) {
      fail(`invalid JSON payload: ${err.message}`);
    }
    if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
      fail("JSON payload must be an object");
    }
    return payload;
  }

  const payload = {
    project: args.project,
    feature: args.feature,
    summary: args.summary,
    model: args.model,
    source: args.source,
  };
  for (const field of INT_FIELDS) {
    if (args[field] != null && !Number.isNaN(args[field])) payload[field] = args[field];
  }
  if (args.metadata_json) {
    let metadata;
    try {
      metadata = JSON.parse(args.metadata_json);
    } catch (err) {
      fail(`invalid metadata JSON: ${err.message}`);
    }
    if (!metadata || typeof metadata !== "object" || Array.isArray(metadata)) {
      fail("metadata must be a JSON object");
    }
    payload.metadata = metadata;
  }
  return Object.fromEntries(Object.entries(payload).filter(([, v]) => v != null));
}

function cleanSnapshot(payload) {
  const summary = String(payload.summary || "").trim();
  if (!summary) fail("summary is required");

  const snapshot = {
    timestamp: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
    project: String(payload.project || path.basename(process.cwd())),
    source: String(payload.source || "manual"),
    summary,
  };
  if (payload.model) snapshot.model = String(payload.model);
  if (payload.feature) snapshot.feature = String(payload.feature);

  for (const field of INT_FIELDS) {
    if (payload[field] == null) continue;
    const parsed = Number(payload[field]);
    if (!Number.isInteger(parsed)) fail(`${field} must be an integer`);
    if (parsed < 0) fail(`${field} must be non-negative`);
    snapshot[field] = parsed;
  }

  if (
    Number.isInteger(snapshot.prompt_tokens) &&
    Number.isInteger(snapshot.completion_tokens) &&
    Number.isInteger(snapshot.total_tokens) &&
    snapshot.total_tokens !== snapshot.prompt_tokens + snapshot.completion_tokens
  ) {
    fail(
      `total_tokens (${snapshot.total_tokens}) does not match prompt_tokens + completion_tokens (${snapshot.prompt_tokens + snapshot.completion_tokens})`
    );
  }

  if (snapshot.total_tokens == null) {
    if (Number.isInteger(snapshot.prompt_tokens) && Number.isInteger(snapshot.completion_tokens)) {
      snapshot.total_tokens = snapshot.prompt_tokens + snapshot.completion_tokens;
    }
  }

  if (payload.metadata != null) {
    if (typeof payload.metadata !== "object" || Array.isArray(payload.metadata)) {
      fail("metadata must be an object");
    }
    snapshot.metadata = payload.metadata;
  }
  return snapshot;
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
      // skip
    }
  }
  return rows;
}

function lastScopeSnapshot(rows, project, feature) {
  let last = null;
  const scopeFeature = feature == null || feature === "" ? null : String(feature);
  for (const row of rows) {
    if (String(row.project || "") !== String(project || "")) continue;
    const rowFeature = row.feature == null || row.feature === "" ? null : String(row.feature);
    if (rowFeature !== scopeFeature) continue;
    last = row;
  }
  return last;
}

function lockSnapshotCost(snapshot, historyPath) {
  if (snapshot.total_tokens == null && snapshot.prompt_tokens == null) return snapshot;
  const pricesPath = process.env.TOKEN_TRACKER_PRICES ? expandHome(process.env.TOKEN_TRACKER_PRICES) : DEFAULT_PRICES;
  const prices = loadPrices(pricesPath);
  
  // Try to use index for faster lookup of previous snapshot
  const indexPath = resolveIndexPath(historyPath);
  const index = loadIndex(indexPath);
  let previous = null;
  
  if (index && isIndexFresh(index, historyPath)) {
    // Use index for fast lookup
    previous = lastFromIndex(index, snapshot.project, snapshot.feature);
  } else {
    // Fallback to full scan
    const rows = loadHistoryRows(historyPath);
    previous = lastScopeSnapshot(rows, snapshot.project, snapshot.feature);
  }
  
  const priced = computeCostDelta(previous, snapshot, prices);
  if (priced.costDeltaUsd != null) snapshot.cost_delta_usd = Number(priced.costDeltaUsd.toFixed(6));
  if (priced.estimatedCostUsd != null) {
    snapshot.estimated_cost_usd = Number(priced.estimatedCostUsd.toFixed(6));
  }
  if (priced.rates) snapshot.cost_rates = priced.rates;
  if (!snapshot.metadata || typeof snapshot.metadata !== "object") snapshot.metadata = {};
  snapshot.metadata.cost_locked = priced.costDeltaUsd != null;
  return snapshot;
}

function appendSnapshot(snapshot, historyPath) {
  fs.mkdirSync(path.dirname(historyPath), { recursive: true });
  fs.appendFileSync(historyPath, `${JSON.stringify(snapshot)}\n`, "utf8");
  
  // Update index after append
  const pricesPath = process.env.TOKEN_TRACKER_PRICES ? expandHome(process.env.TOKEN_TRACKER_PRICES) : DEFAULT_PRICES;
  const prices = loadPrices(pricesPath);
  const indexPath = resolveIndexPath(historyPath);
  
  try {
    // Ensure index exists first (rebuild if missing/stale)
    let index = ensureIndex({ historyPath, pricesPath, indexPath });
    
    // Apply the new snapshot
    index = applyAppend(index, snapshot, prices);
    
    // Update file metadata to reflect new state
    const stat = fs.statSync(historyPath);
    index.history_bytes = stat.size;
    index.history_mtime_ms = stat.mtimeMs;
    
    // Write updated index
    fs.writeFileSync(indexPath, JSON.stringify(index, null, 2), "utf8");
  } catch (err) {
    // If index update fails, continue - the full scan fallback will work
    console.error(`Warning: failed to update index: ${err.message}`);
  }
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    usage();
    process.exit(0);
  }
  const payload = loadPayload(args);
  let snapshot = cleanSnapshot(payload);
  const historyPath = expandHome(args.history) || DEFAULT_HISTORY;
  snapshot = lockSnapshotCost(snapshot, historyPath);
  appendSnapshot(snapshot, historyPath);
  console.log(JSON.stringify({ saved: historyPath, snapshot }));
}

if (require.main === module) main();

module.exports = { cleanSnapshot, loadPayload, parseArgs, lockSnapshotCost, usage };
