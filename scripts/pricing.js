#!/usr/bin/env node
"use strict";

/**
 * Shared pricing helpers for status line + report.
 *
 * prices.json shape:
 * {
 *   "default": { "input_per_million_usd": 2.5, "output_per_million_usd": 15 },
 *   "models": {
 *     "gpt-5.5": { "input_per_million_usd": 5, "output_per_million_usd": 30 },
 *     "claude opus": { "input_per_million_usd": 5, "output_per_million_usd": 25 }
 *   }
 * }
 *
 * Model keys are matched as case-insensitive substrings against the model name.
 * Longer / more specific keys should be listed; first match wins (Object key order).
 */

const fs = require("fs");

function parseRates(rates) {
  if (!rates || typeof rates !== "object") return null;
  const input = Number(rates.input_per_million_usd);
  const output = Number(rates.output_per_million_usd);
  if (!Number.isFinite(input) || !Number.isFinite(output) || input < 0 || output < 0) return null;
  return { input, output };
}

function loadPrices(filePath) {
  if (!filePath || !fs.existsSync(filePath)) return {};
  try {
    const payload = JSON.parse(fs.readFileSync(filePath, "utf8"));
    return payload && typeof payload === "object" && !Array.isArray(payload) ? payload : {};
  } catch {
    return {};
  }
}

function ratesForModel(prices, model) {
  const models = prices && prices.models;
  if (models && typeof models === "object") {
    const modelLower = String(model || "").toLowerCase();
    // Prefer longer pattern matches so "gpt-5.5 pro" wins over "gpt-5.5".
    const entries = Object.entries(models).sort((a, b) => String(b[0]).length - String(a[0]).length);
    for (const [pattern, rates] of entries) {
      if (!pattern) continue;
      if (modelLower.includes(String(pattern).toLowerCase())) {
        const parsed = parseRates(rates);
        if (parsed) return parsed;
      }
    }
  }
  return parseRates(prices && prices.default);
}

function estimateCostUsd(inputTokens, outputTokens, rates) {
  if (!rates) return null;
  const input = Math.max(0, Number(inputTokens) || 0);
  const output = Math.max(0, Number(outputTokens) || 0);
  return (input / 1_000_000) * rates.input + (output / 1_000_000) * rates.output;
}

function estimateCostUsdForModel(prices, model, inputTokens, outputTokens) {
  return estimateCostUsd(inputTokens, outputTokens, ratesForModel(prices, model));
}

/** Compact money for status line / report cells. */
function formatCost(usd, { prefix = "$", digits = null, unpriced = "n/a" } = {}) {
  if (usd == null || !Number.isFinite(usd)) return unpriced;
  let d = digits;
  if (d == null) {
    if (usd >= 10) d = 2;
    else if (usd >= 1) d = 3;
    else d = 4;
  }
  return `${prefix}${usd.toFixed(d)}`;
}

/**
 * Split a total-only snapshot into prompt/completion for rough costing.
 * Prefer real prompt/completion fields when present.
 */
function tokenSplit(row) {
  const prompt = Number(row.prompt_tokens);
  const completion = Number(row.completion_tokens);
  if (Number.isFinite(prompt) && Number.isFinite(completion) && prompt >= 0 && completion >= 0) {
    return { prompt, completion };
  }
  const total = Math.max(0, Number(row.total_tokens) || 0);
  // Heuristic when history only has totals (manual saves without split).
  const approxPrompt = Math.round(total * 0.7);
  return { prompt: approxPrompt, completion: total - approxPrompt, approximate: true };
}

/**
 * Epoch-aware feature cost: price positive token deltas between snapshots,
 * resetting when the cumulative total drops (feature / baseline reset).
 */
function epochFeatureCost(sortedItems, prices) {
  let lastPrompt = 0;
  let lastCompletion = 0;
  let lastTotal = 0;
  let cost = 0;
  let priced = false;
  let approximate = false;
  let unpricedDeltas = 0;

  for (const item of sortedItems) {
    const total = Math.max(0, Number(item.total) || Number(item.total_tokens) || 0);
    const split = tokenSplit(item);
    if (split.approximate) approximate = true;

    if (total < lastTotal) {
      lastPrompt = 0;
      lastCompletion = 0;
    }

    const deltaIn = Math.max(0, split.prompt - lastPrompt);
    const deltaOut = Math.max(0, split.completion - lastCompletion);
    if (deltaIn > 0 || deltaOut > 0) {
      const usd = estimateCostUsdForModel(prices, item.model, deltaIn, deltaOut);
      if (usd == null) unpricedDeltas += 1;
      else {
        cost += usd;
        priced = true;
      }
    }

    lastPrompt = split.prompt;
    lastCompletion = split.completion;
    lastTotal = total;
  }

  return {
    costUsd: priced ? cost : null,
    approximate,
    unpricedDeltas,
  };
}

module.exports = {
  parseRates,
  loadPrices,
  ratesForModel,
  estimateCostUsd,
  estimateCostUsdForModel,
  formatCost,
  tokenSplit,
  epochFeatureCost,
};
