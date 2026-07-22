#!/usr/bin/env node
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawnSync } = require("child_process");
const {
  loadPrices,
  formatCost,
  computeCostDelta,
  featureOngoingCost,
} = require("./pricing.js");
const { schedulePricePullIfStale, priceRefreshOptions } = require("./pull-prices.js");
const { paths } = require("./paths.js");
const { 
  resolveIndexPath, 
  ensureIndex, 
  lastFromIndex,
  isIndexFresh,
  loadIndex
} = require("./ledger-index.js");

const { historyPath: HISTORY_PATH, configPath: CONFIG_PATH, pricesPath: PRICES_PATH } = paths();

function loadJsonFile(filePath) {
  if (!fs.existsSync(filePath)) return {};
  try {
    const payload = JSON.parse(fs.readFileSync(filePath, "utf8"));
    return payload && typeof payload === "object" && !Array.isArray(payload) ? payload : {};
  } catch {
    return {};
  }
}

function saveConfig(config) {
  fs.mkdirSync(path.dirname(CONFIG_PATH), { recursive: true });
  fs.writeFileSync(CONFIG_PATH, `${JSON.stringify(config, null, 2)}\n`, "utf8");
}

function resolveFeatureTokens(config, workspace, project, feature, inputTokens, outputTokens) {
  if (!workspace) {
    return { inputTokens, outputTokens, totalTokens: inputTokens + outputTokens };
  }
  if (!config.token_baselines || typeof config.token_baselines !== "object") {
    config.token_baselines = {};
  }
  const baseline = config.token_baselines[workspace];
  const scopeChanged =
    !baseline ||
    baseline.project !== project ||
    baseline.feature !== (feature || null) ||
    baseline.pending_reset === true;

  if (scopeChanged) {
    config.token_baselines[workspace] = {
      project,
      feature: feature || null,
      prompt_tokens: inputTokens,
      completion_tokens: outputTokens,
      pending_reset: false,
    };
    saveConfig(config);
    return { inputTokens: 0, outputTokens: 0, totalTokens: 0 };
  }

  const baseIn = Number(baseline.prompt_tokens || 0);
  const baseOut = Number(baseline.completion_tokens || 0);
  // Context window can shrink (compaction); keep non-negative feature totals.
  const featureIn = Math.max(0, inputTokens - baseIn);
  const featureOut = Math.max(0, outputTokens - baseOut);
  return { inputTokens: featureIn, outputTokens: featureOut, totalTokens: featureIn + featureOut };
}

function loadJsonStdin() {
  let raw = "";
  try {
    raw = fs.readFileSync(0, "utf8");
  } catch {
    return {};
  }
  if (!raw.trim()) return {};
  try {
    const payload = JSON.parse(raw);
    return payload && typeof payload === "object" && !Array.isArray(payload) ? payload : {};
  } catch {
    return {};
  }
}

function workspaceDirFromPayload(payload) {
  const workspace = payload.workspace;
  let currentDir = null;
  if (workspace && typeof workspace === "object") currentDir = workspace.current_dir;
  return currentDir || payload.cwd || null;
}

function projectFromPayload(config, currentDir) {
  if (process.env.TOKEN_TRACKER_PROJECT) return process.env.TOKEN_TRACKER_PROJECT;
  const projects = config.projects;
  if (currentDir && projects && typeof projects === "object" && projects[currentDir]) {
    return String(projects[currentDir]);
  }
  if (config.default_project) return String(config.default_project);
  return currentDir ? path.basename(String(currentDir)) : "unknown";
}

function gitBranchFromPayload(payload, currentDir) {
  const worktree = payload.worktree;
  if (worktree && typeof worktree === "object" && worktree.name) return String(worktree.name);
  if (!currentDir) return null;
  try {
    const result = spawnSync("git", ["-C", currentDir, "branch", "--show-current"], {
      encoding: "utf8",
      timeout: 200,
    });
    const branch = (result.stdout || "").trim();
    return branch || null;
  } catch {
    return null;
  }
}

function featureFromPayload(payload, config, currentDir) {
  if (process.env.TOKEN_TRACKER_FEATURE) return process.env.TOKEN_TRACKER_FEATURE;
  const features = config.features;
  if (currentDir && features && typeof features === "object" && features[currentDir]) {
    return String(features[currentDir]);
  }
  if (config.default_feature) return String(config.default_feature);
  return gitBranchFromPayload(payload, currentDir);
}

function modelFromPayload(payload) {
  const model = payload.model;
  if (!model || typeof model !== "object") return "unknown-model";
  return String(model.display_name || model.id || "unknown-model");
}

function contextTokens(payload) {
  const context = payload.context_window;
  if (!context || typeof context !== "object") return [0, 0, null];
  const inputTokens = Number(context.total_input_tokens || 0);
  const outputTokens = Number(context.total_output_tokens || 0);
  let used = null;
  if (context.used_percentage != null) {
    const n = Number(context.used_percentage);
    if (!Number.isNaN(n)) used = Math.trunc(n);
  }
  return [inputTokens, outputTokens, used];
}

function iterHistory() {
  // ponytail: O(n) scan is fine for local JSONL; move to an index/MCP when history gets large.
  if (!fs.existsSync(HISTORY_PATH)) return [];
  const rows = [];
  for (const line of fs.readFileSync(HISTORY_PATH, "utf8").split("\n")) {
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

function appendHistory(row) {
  fs.mkdirSync(path.dirname(HISTORY_PATH), { recursive: true });
  fs.appendFileSync(HISTORY_PATH, `${JSON.stringify(row)}\n`, "utf8");
}

function lastScopeSnapshot(rows, project, feature) {
  let last = null;
  const scopeFeature = feature || null;
  for (const row of rows) {
    if (String(row.project || "") !== String(project || "")) continue;
    const rowFeature = row.feature == null || row.feature === "" ? null : String(row.feature);
    if (rowFeature !== scopeFeature) continue;
    last = row;
  }
  return last;
}

function autoSaveSnapshot(payload, rows, project, feature, model, inputTokens, outputTokens, usedPct, prices) {
  const totalTokens = inputTokens + outputTokens;
  if (totalTokens <= 0) return false;
  const sessionKey = String(payload.session_id || payload.transcript_path || "unknown-session");
  const autoKey = `${sessionKey}:${project}:${feature || ""}:${model}:${inputTokens}:${outputTokens}`;
  for (const row of rows) {
    if (row.metadata && row.metadata.auto_key === autoKey) return false;
  }

  const current = {
    project,
    feature,
    model,
    prompt_tokens: inputTokens,
    completion_tokens: outputTokens,
    total_tokens: totalTokens,
  };
  const previous = lastScopeSnapshot(rows, project, feature);
  const priced = computeCostDelta(previous, current, prices);

  const snapshot = {
    timestamp: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
    project,
    source: "statusline",
    summary: "Status line usage snapshot.",
    model,
    prompt_tokens: inputTokens,
    completion_tokens: outputTokens,
    total_tokens: totalTokens,
    metadata: {
      auto_key: autoKey,
      session_id: payload.session_id || null,
      transcript_path: payload.transcript_path || null,
      used_percentage: usedPct,
      cost_locked: priced.costDeltaUsd != null,
    },
  };
  if (feature) snapshot.feature = feature;
  if (priced.costDeltaUsd != null && Number.isFinite(priced.costDeltaUsd)) {
    snapshot.cost_delta_usd = Number(priced.costDeltaUsd.toFixed(6));
  }
  if (priced.estimatedCostUsd != null && Number.isFinite(priced.estimatedCostUsd)) {
    snapshot.estimated_cost_usd = Number(priced.estimatedCostUsd.toFixed(6));
  }
  if (priced.rates) snapshot.cost_rates = priced.rates;
  appendHistory(snapshot);
  return true;
}

function compactTokens(value) {
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 10_000) return `${Math.round(value / 1000)}k`;
  if (value >= 1000) return `${(value / 1000).toFixed(1)}k`;
  return String(value);
}

function contextBar(usedPct) {
  if (usedPct == null) return "ctx [??????????]";
  const clamped = Math.max(0, Math.min(100, usedPct));
  const width = 10;
  const filled = Math.round((clamped * width) / 100);
  return `ctx [${"#".repeat(filled)}${".".repeat(width - filled)}] ${clamped}%`;
}

function statuslineOptions(config) {
  const options = config.statusline && typeof config.statusline === "object" ? config.statusline : {};
  return {
    enabled: options.enabled !== false,
    show_label: options.show_label !== false,
    show_project: options.show_project !== false,
    show_feature: options.show_feature !== false,
    show_model: options.show_model !== false,
    show_context: options.show_context !== false,
    show_tokens: options.show_tokens !== false,
    show_cost: options.show_cost !== false,
  };
}

function main() {
  const payload = loadJsonStdin();
  const config = loadJsonFile(CONFIG_PATH);
  const options = statuslineOptions(config);
  if (!options.enabled) return;

  const refresh = priceRefreshOptions(config);
  if (refresh.autoPull) {
    schedulePricePullIfStale({
      pricesPath: PRICES_PATH,
      source: refresh.source,
      maxAgeMs: refresh.maxAgeMs,
    });
  }

  const currentDir = workspaceDirFromPayload(payload);
  const project = projectFromPayload(config, currentDir);
  const feature = featureFromPayload(payload, config, currentDir);
  const model = modelFromPayload(payload);
  const [sessionIn, sessionOut, usedPct] = contextTokens(payload);
  const featureTokens = resolveFeatureTokens(
    config,
    currentDir,
    project,
    feature,
    sessionIn,
    sessionOut,
  );

  const prices = loadPrices(PRICES_PATH);
  const rows = iterHistory();
  autoSaveSnapshot(
    payload,
    rows,
    project,
    feature,
    model,
    featureTokens.inputTokens,
    featureTokens.outputTokens,
    usedPct,
    prices,
  );

  // Calculate ongoing cost using index if available, otherwise fall back to full scan
  const indexPath = resolveIndexPath(HISTORY_PATH);
  const index = loadIndex(indexPath);
  let ongoing;
  
  if (index && isIndexFresh(index, HISTORY_PATH)) {
    // Use index for fast lookup
    const lastSnapshot = lastFromIndex(index, project, feature);
    const current = {
      project,
      feature,
      model,
      prompt_tokens: featureTokens.inputTokens,
      completion_tokens: featureTokens.outputTokens,
      total_tokens: featureTokens.totalTokens,
    };
    const tip = computeCostDelta(lastSnapshot, current, prices);
    
    // Get base cost from index
    const key = `${project}\t${feature || "(none)"}`;
    const entry = index.features[key];
    const lockedUsd = entry ? (entry.cost_usd || 0) : 0;
    const tipUsd = tip.costDeltaUsd || 0;
    const totalUsd = lockedUsd + tipUsd;
    
    ongoing = {
      lockedUsd: entry ? entry.cost_usd : null,
      tipUsd: tip.costDeltaUsd,
      costUsd: (entry && entry.cost_usd != null) || tip.costDeltaUsd != null ? totalUsd : null,
      tip,
    };
  } else {
    // Fallback to full scan
    const rowsAfter = iterHistory();
    ongoing = featureOngoingCost(
      rowsAfter,
      {
        project,
        feature,
        inputTokens: featureTokens.inputTokens,
        outputTokens: featureTokens.outputTokens,
        totalTokens: featureTokens.totalTokens,
        model,
      },
      prices,
    );
  }

  const ctx = contextBar(usedPct);
  const toks = `toks ${compactTokens(featureTokens.totalTokens)}`;
  const scope = feature && options.show_feature ? `${project}/${feature}` : project;
  const parts = [];
  if (options.show_label) parts.push("token-tracker");
  if (options.show_project) parts.push(scope);
  if (options.show_model) parts.push(model);
  if (options.show_context) parts.push(ctx);
  if (options.show_tokens) parts.push(toks);
  if (options.show_cost) {
    parts.push(formatCost(ongoing.costUsd, { prefix: "$", unpriced: "$?" }));
  }
  console.log(parts.join(" | "));
}

if (require.main === module) main();
