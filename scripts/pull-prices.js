#!/usr/bin/env node
"use strict";

/**
 * Fetch latest model rates and write ~/.token-tracker/prices.json
 *
 * Sources:
 *   openrouter  — https://openrouter.ai/api/v1/models (default)
 *   llmcosthub  — https://llmcosthub.com/api/v1/pricing.json
 *   benchgecko  — https://raw.githubusercontent.com/BenchGecko/llm-pricing/main/pricing.json
 */

const fs = require("fs");
const os = require("os");
const path = require("path");
const https = require("https");
const http = require("http");
const { spawn } = require("child_process");
const { pricesUpdatedAtMs } = require("./pricing.js");
const { paths, expand } = require("./paths.js");

const DEFAULT_PRICES_PATH = paths().pricesPath;
const DEFAULT_MAX_AGE_MS = 60 * 60 * 1000;

const SOURCES = {
  openrouter: {
    url: "https://openrouter.ai/api/v1/models",
    parse: parseOpenRouter,
  },
  llmcosthub: {
    url: "https://llmcosthub.com/api/v1/pricing.json",
    parse: parseLlmCostHub,
  },
  benchgecko: {
    url: "https://raw.githubusercontent.com/BenchGecko/llm-pricing/main/pricing.json",
    parse: parseBenchGecko,
  },
};

/** Prefer these providers when building the local table. */
const PROVIDER_ALLOW = new Set([
  "openai",
  "anthropic",
  "google",
  "google-ai-studio",
  "google-vertex",
]);

/**
 * Convenience aliases for Cursor/Claude display names.
 * Each entry picks the newest matching remote model id.
 */
const FAMILY_ALIASES = [
  { alias: "gpt-5.5 pro", test: (id) => /^openai\/gpt-5\.5-pro/.test(id) },
  { alias: "gpt-5.5", test: (id) => /^openai\/gpt-5\.5(?!-pro)/.test(id) },
  { alias: "gpt-5.4 pro", test: (id) => /^openai\/gpt-5\.4-pro/.test(id) },
  { alias: "gpt-5.4 mini", test: (id) => /^openai\/gpt-5\.4-mini/.test(id) },
  { alias: "gpt-5.4 nano", test: (id) => /^openai\/gpt-5\.4-nano/.test(id) },
  { alias: "gpt-5.4", test: (id) => /^openai\/gpt-5\.4(?!-|pro)/.test(id) || id === "openai/gpt-5.4" },
  { alias: "gpt-5.3", test: (id) => /^openai\/gpt-5\.3/.test(id) },
  { alias: "gpt-5", test: (id) => /^openai\/gpt-5(?![\d.-])/.test(id) || id === "openai/gpt-5" },
  { alias: "gpt-4o", test: (id) => id === "openai/gpt-4o" },
  { alias: "o3", test: (id) => /^openai\/o3(?!-)/.test(id) || id === "openai/o3" },
  { alias: "o4-mini", test: (id) => /^openai\/o4-mini/.test(id) },
  { alias: "claude opus", test: (id) => /^anthropic\/claude-opus-/.test(id) && !id.includes("fast") },
  { alias: "claude sonnet", test: (id) => /^anthropic\/claude-sonnet-/.test(id) },
  { alias: "claude haiku", test: (id) => /^anthropic\/claude-haiku-/.test(id) || id.includes("claude-3-haiku") },
  { alias: "claude", test: (id) => /^anthropic\/claude-sonnet-/.test(id) },
  { alias: "gemini", test: (id) => /gemini-.*pro/.test(id) },
  { alias: "flash", test: (id) => /gemini-.*flash(?!.*lite)/.test(id) },
  { alias: "composer", test: (id) => id.includes("composer") },
];

function usage() {
  console.log(`Usage:
  npx @mbrundige/token-tracker prices pull [--source openrouter|llmcosthub|benchgecko] [--out PATH] [--dry-run]
  npx @mbrundige/token-tracker prices show [--out PATH]

Defaults:
  --source openrouter
  --out ~/.token-tracker/prices.json
`);
}

function fetchJson(url, { timeoutMs = 20000 } = {}) {
  return new Promise((resolve, reject) => {
    const lib = url.startsWith("https:") ? https : http;
    const req = lib.get(
      url,
      {
        headers: {
          Accept: "application/json",
          "User-Agent": "token-tracker-prices/1.0",
        },
        timeout: timeoutMs,
      },
      (res) => {
        if (res.statusCode && res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          res.resume();
          fetchJson(res.headers.location, { timeoutMs }).then(resolve, reject);
          return;
        }
        if (res.statusCode !== 200) {
          res.resume();
          reject(new Error(`HTTP ${res.statusCode} from ${url}`));
          return;
        }
        const chunks = [];
        res.on("data", (c) => chunks.push(c));
        res.on("end", () => {
          try {
            resolve(JSON.parse(Buffer.concat(chunks).toString("utf8")));
          } catch (err) {
            reject(err);
          }
        });
      },
    );
    req.on("timeout", () => {
      req.destroy();
      reject(new Error(`timeout fetching ${url}`));
    });
    req.on("error", reject);
  });
}

function cleanKey(raw) {
  return String(raw || "")
    .toLowerCase()
    .replace(/^[^:]+:\s*/, "") // "OpenAI: GPT-5.5" -> "gpt-5.5"
    .replace(/[_\s]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function idKey(id) {
  const slug = String(id || "").split("/").pop() || "";
  return slug.toLowerCase().replace(/_/g, "-");
}

function providerOf(id) {
  return String(id || "").split("/")[0] || "";
}

function versionScore(id) {
  // Prefer higher dotted versions; "4.8" > "4.6" > "4".
  const m = String(id).match(/(\d+(?:\.\d+)*)/g);
  if (!m) return 0;
  return m.reduce((score, part) => {
    const bits = part.split(".").map((n) => Number(n) || 0);
    let s = 0;
    for (let i = 0; i < bits.length; i += 1) s += bits[i] * 1000 ** (3 - i);
    return Math.max(score, s);
  }, 0);
}

function parseOpenRouter(payload) {
  const rows = Array.isArray(payload?.data) ? payload.data : [];
  const models = {};
  const catalog = []; // { id, rates } for alias resolution

  for (const row of rows) {
    const id = String(row.id || "");
    if (!id || id.includes(":free")) continue;
    const provider = providerOf(id);
    if (PROVIDER_ALLOW.size && !PROVIDER_ALLOW.has(provider)) continue;
    const pricing = row.pricing || {};
    const input = Number(pricing.prompt) * 1_000_000;
    const output = Number(pricing.completion) * 1_000_000;
    if (!Number.isFinite(input) || !Number.isFinite(output) || (input <= 0 && output <= 0)) continue;
    // Skip exotic image-only / absurd outliers lightly: keep chat-ish
    const rates = {
      input_per_million_usd: roundRate(input),
      output_per_million_usd: roundRate(output),
    };
    catalog.push({ id, rates, score: versionScore(id) });

    const keys = new Set([idKey(id), cleanKey(row.name)]);
    for (const key of keys) {
      if (!key || key.length < 2) continue;
      // Prefer higher version when colliding on the same key.
      const prev = models[key];
      if (!prev || versionScore(id) >= versionScore(prev._id || "")) {
        models[key] = { ...rates, _id: id };
      }
    }
  }

  applyFamilyAliases(models, catalog);
  stripInternal(models);
  return models;
}

function parseLlmCostHub(payload) {
  const rows = Array.isArray(payload?.models) ? payload.models : [];
  const models = {};
  const catalog = [];
  for (const row of rows) {
    const provider = String(row.provider_slug || row.provider || "").toLowerCase();
    if (PROVIDER_ALLOW.size && provider && !PROVIDER_ALLOW.has(provider)) continue;
    const input = Number(row.input_price_per_1m);
    const output = Number(row.output_price_per_1m);
    if (!Number.isFinite(input) || !Number.isFinite(output)) continue;
    const rates = {
      input_per_million_usd: roundRate(input),
      output_per_million_usd: roundRate(output),
    };
    const id = `${provider}/${row.model_slug || row.model || ""}`;
    catalog.push({ id, rates, score: versionScore(id) });
    for (const key of [cleanKey(row.model), cleanKey(row.model_slug)]) {
      if (!key) continue;
      models[key] = { ...rates, _id: id };
    }
  }
  applyFamilyAliases(models, catalog);
  stripInternal(models);
  return models;
}

function parseBenchGecko(payload) {
  const rows = Array.isArray(payload?.models) ? payload.models : [];
  const models = {};
  const catalog = [];
  for (const row of rows) {
    const id = String(row.id || "").replace(/^~/, "");
    const provider = providerOf(id);
    if (PROVIDER_ALLOW.size && !PROVIDER_ALLOW.has(provider)) continue;
    if (row.type && row.type !== "chat") continue;
    const input = Number(row.input_per_million);
    const output = Number(row.output_per_million);
    if (!Number.isFinite(input) || !Number.isFinite(output)) continue;
    const rates = {
      input_per_million_usd: roundRate(input),
      output_per_million_usd: roundRate(output),
    };
    catalog.push({ id, rates, score: versionScore(id) });
    for (const key of [idKey(id), cleanKey(row.name)]) {
      if (!key) continue;
      models[key] = { ...rates, _id: id };
    }
  }
  applyFamilyAliases(models, catalog);
  stripInternal(models);
  return models;
}

function applyFamilyAliases(models, catalog) {
  for (const { alias, test } of FAMILY_ALIASES) {
    const matches = catalog.filter((c) => test(c.id)).sort((a, b) => b.score - a.score);
    if (!matches.length) continue;
    models[alias] = { ...matches[0].rates, _id: matches[0].id };
  }
}

function stripInternal(models) {
  for (const key of Object.keys(models)) {
    if (models[key] && typeof models[key] === "object") delete models[key]._id;
  }
}

function roundRate(n) {
  // Keep enough precision for cheap flash/nano models.
  const x = Number(n);
  if (!Number.isFinite(x)) return x;
  if (x >= 10) return Math.round(x * 100) / 100;
  if (x >= 1) return Math.round(x * 1000) / 1000;
  return Math.round(x * 10000) / 10000;
}

function loadLocal(filePath) {
  if (!fs.existsSync(filePath)) return {};
  try {
    const payload = JSON.parse(fs.readFileSync(filePath, "utf8"));
    return payload && typeof payload === "object" && !Array.isArray(payload) ? payload : {};
  } catch {
    return {};
  }
}

function buildPricesDocument({ source, url, models, previous }) {
  const defaultRates =
    (previous.default && typeof previous.default === "object" && previous.default) ||
    { input_per_million_usd: 2.5, output_per_million_usd: 15 };

  // Preserve locally locked model overrides.
  const locked = {};
  if (previous.models && typeof previous.models === "object") {
    for (const [key, rates] of Object.entries(previous.models)) {
      if (rates && typeof rates === "object" && rates.locked === true) {
        locked[key] = {
          input_per_million_usd: rates.input_per_million_usd,
          output_per_million_usd: rates.output_per_million_usd,
          locked: true,
        };
      }
    }
  }

  const merged = { ...models, ...locked };
  // Stable key order: aliases / shorter family names first-ish by sorting longer keys later is for matcher;
  // store alphabetically for diffs.
  const ordered = {};
  for (const key of Object.keys(merged).sort((a, b) => a.localeCompare(b))) {
    ordered[key] = merged[key];
  }

  return {
    _comment:
      "Estimated API list prices in USD per 1M tokens. Matched as case-insensitive substrings against the model display name (longest match wins). Edit freely; mark a model with \"locked\": true to keep it across `prices pull`.",
    updated_at: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
    source,
    source_url: url,
    default: {
      input_per_million_usd: Number(defaultRates.input_per_million_usd),
      output_per_million_usd: Number(defaultRates.output_per_million_usd),
    },
    models: ordered,
  };
}

async function runPull({ source = "openrouter", outPath = DEFAULT_PRICES_PATH, dryRun = false } = {}) {
  const src = SOURCES[source];
  if (!src) throw new Error(`unknown source "${source}". Use: ${Object.keys(SOURCES).join(", ")}`);

  const remote = await fetchJson(src.url);
  const models = src.parse(remote);
  const previous = loadLocal(outPath);
  const doc = buildPricesDocument({ source, url: src.url, models, previous });
  const count = Object.keys(doc.models).length;
  if (!count) throw new Error("pull produced 0 model rates; refusing to write");

  if (dryRun) {
    return {
      dry_run: true,
      source,
      source_url: src.url,
      out: outPath,
      model_count: count,
      sample: Object.fromEntries(Object.entries(doc.models).slice(0, 8)),
    };
  }

  fs.mkdirSync(path.dirname(outPath), { recursive: true });
  if (fs.existsSync(outPath)) {
    fs.copyFileSync(outPath, `${outPath}.bak`);
  }
  fs.writeFileSync(outPath, `${JSON.stringify(doc, null, 2)}\n`, "utf8");
  return {
    updated: outPath,
    source,
    source_url: src.url,
    model_count: count,
    backup: fs.existsSync(`${outPath}.bak`) ? `${outPath}.bak` : undefined,
    note: 'Mark any local override with "locked": true to keep it on the next pull.',
  };
}

/**
 * Decide whether prices.json should be refreshed from the network.
 * Seed files (no feed `source` / no `updated_at`) always need a pull — do not
 * treat file mtime as freshness (that blocked auto-pull for ~1h after install).
 */
function pricesRefreshStatus(pricesPath, maxAgeMs = DEFAULT_MAX_AGE_MS) {
  const ageGate = Number(maxAgeMs);
  if (!Number.isFinite(ageGate) || ageGate <= 0) {
    return { needed: false, reason: "disabled" };
  }
  if (!pricesPath || !fs.existsSync(pricesPath)) {
    return { needed: true, reason: "missing", ageMs: Infinity };
  }
  const doc = loadPricesDoc(pricesPath);
  const source = doc.source ? String(doc.source) : "";
  if (!source || source === "seed" || !doc.updated_at) {
    return { needed: true, reason: "seed", ageMs: Infinity, source: source || "seed" };
  }
  const updatedMs = pricesUpdatedAtMs(doc);
  const ageMs = updatedMs ? Date.now() - updatedMs : Infinity;
  if (ageMs >= ageGate) {
    return { needed: true, reason: "stale", ageMs, updatedMs, source };
  }
  return { needed: false, reason: "fresh", ageMs, updatedMs, source };
}

function loadPricesDoc(pricesPath) {
  try {
    const payload = JSON.parse(fs.readFileSync(pricesPath, "utf8"));
    return payload && typeof payload === "object" && !Array.isArray(payload) ? payload : {};
  } catch {
    return {};
  }
}

/**
 * If prices are older than maxAgeMs (or still seed), spawn a detached `prices pull`.
 * Never blocks the caller (safe for Cursor statusLine's ~1s budget).
 */
function schedulePricePullIfStale({
  pricesPath = DEFAULT_PRICES_PATH,
  source = "openrouter",
  maxAgeMs = DEFAULT_MAX_AGE_MS,
  pullScript = path.join(__dirname, "pull-prices.js"),
} = {}) {
  const status = pricesRefreshStatus(pricesPath, maxAgeMs);
  if (!status.needed) {
    return { scheduled: false, reason: status.reason, ageMs: status.ageMs, updatedMs: status.updatedMs };
  }

  const lockPath = `${pricesPath}.pulling`;
  try {
    if (fs.existsSync(lockPath)) {
      const lockAge = Date.now() - fs.statSync(lockPath).mtimeMs;
      if (lockAge < 5 * 60 * 1000) {
        return { scheduled: false, reason: "in_flight", lockAge };
      }
    }
    fs.mkdirSync(path.dirname(pricesPath), { recursive: true });
    fs.writeFileSync(lockPath, `${Date.now()}\n`, "utf8");
  } catch {
    return { scheduled: false, reason: "lock_failed" };
  }

  try {
    const child = spawn(
      process.execPath,
      [pullScript, "pull", "--source", source, "--out", pricesPath],
      {
        detached: true,
        stdio: "ignore",
        env: process.env,
      },
    );
    child.unref();
    // Best-effort lock cleanup shortly after start; pull also overwrites prices.
    setTimeout(() => {
      try {
        fs.rmSync(lockPath, { force: true });
      } catch {
        // ignore
      }
    }, 30_000).unref?.();
    return {
      scheduled: true,
      reason: status.reason,
      ageMs: status.ageMs,
      updatedMs: status.updatedMs,
      pid: child.pid,
    };
  } catch (err) {
    try {
      fs.rmSync(lockPath, { force: true });
    } catch {
      // ignore
    }
    return { scheduled: false, reason: "spawn_failed", error: String(err.message || err) };
  }
}

/**
 * For report / interactive CLI: pull synchronously when seed or stale so the
 * user does not need to run `prices pull` by hand. Falls back to background
 * schedule if the foreground pull fails.
 */
async function ensureFreshPrices({
  pricesPath = DEFAULT_PRICES_PATH,
  source = "openrouter",
  maxAgeMs = DEFAULT_MAX_AGE_MS,
  timeoutMs = 20000,
  onStatus = null,
} = {}) {
  const status = pricesRefreshStatus(pricesPath, maxAgeMs);
  if (!status.needed) {
    return { pulled: false, ...status };
  }
  if (typeof onStatus === "function") {
    onStatus(
      status.reason === "seed" || status.reason === "missing"
        ? "Fetching latest model prices…"
        : "Refreshing model prices…",
    );
  }
  try {
    const result = await runPull({ source, outPath: pricesPath, dryRun: false });
    try {
      fs.rmSync(`${pricesPath}.pulling`, { force: true });
    } catch {
      // ignore
    }
    return { pulled: true, reason: status.reason, result };
  } catch (err) {
    const scheduled = schedulePricePullIfStale({ pricesPath, source, maxAgeMs });
    return {
      pulled: false,
      reason: "pull_failed",
      error: String(err.message || err),
      scheduled: scheduled.scheduled,
    };
  }
}

function priceRefreshOptions(config) {
  const prices = config && typeof config.prices === "object" ? config.prices : {};
  const hours = prices.auto_pull_interval_hours;
  let maxAgeMs = DEFAULT_MAX_AGE_MS;
  if (hours === false || prices.auto_pull === false) maxAgeMs = 0;
  else if (hours != null && Number.isFinite(Number(hours))) {
    maxAgeMs = Math.max(0, Number(hours) * 60 * 60 * 1000);
  }
  return {
    autoPull: maxAgeMs > 0,
    maxAgeMs,
    source: prices.source || "openrouter",
  };
}

async function pullPrices(argv) {
  let source = "openrouter";
  let outPath = DEFAULT_PRICES_PATH;
  let dryRun = false;
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--source") source = String(argv[++i] || "");
    else if (a === "--out") outPath = expand(String(argv[++i] || ""));
    else if (a === "--dry-run") dryRun = true;
    else if (a === "-h" || a === "--help") {
      usage();
      return;
    } else {
      console.error(`token-tracker: unknown prices pull option: ${a}`);
      usage();
      process.exit(2);
    }
  }

  const result = await runPull({ source, outPath, dryRun });
  console.log(JSON.stringify(result, null, 2));
  // Clear any pull lock left by schedulePricePullIfStale.
  try {
    fs.rmSync(`${outPath}.pulling`, { force: true });
  } catch {
    // ignore
  }
}

function showPrices(argv) {
  let outPath = DEFAULT_PRICES_PATH;
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--out") outPath = expand(String(argv[++i] || ""));
    else if (a === "-h" || a === "--help") {
      usage();
      return;
    } else {
      console.error(`token-tracker: unknown prices show option: ${a}`);
      usage();
      process.exit(2);
    }
  }
  if (!fs.existsSync(outPath)) {
    console.error(`token-tracker: no prices file at ${outPath}. Run: npx @mbrundige/token-tracker prices pull`);
    process.exit(1);
  }
  const doc = loadLocal(outPath);
  const models = doc.models && typeof doc.models === "object" ? doc.models : {};
  const entries = Object.entries(models).sort((a, b) => a[0].localeCompare(b[0]));
  console.log(`Prices: ${outPath}`);
  if (doc.updated_at) console.log(`Updated: ${doc.updated_at}`);
  if (doc.source) console.log(`Source:  ${doc.source}${doc.source_url ? ` (${doc.source_url})` : ""}`);
  console.log(
    `Default: $${doc.default?.input_per_million_usd ?? "?"}/M in · $${doc.default?.output_per_million_usd ?? "?"}/M out`,
  );
  console.log("");
  console.log(`${"model".padEnd(28)} ${"input/M".padStart(10)} ${"output/M".padStart(10)}`);
  console.log(`${"-".repeat(28)} ${"-".repeat(10)} ${"-".repeat(10)}`);
  for (const [key, rates] of entries) {
    const lock = rates.locked ? "*" : " ";
    console.log(
      `${(lock + key).padEnd(28)} ${String(rates.input_per_million_usd).padStart(10)} ${String(rates.output_per_million_usd).padStart(10)}`,
    );
  }
  console.log("");
  console.log(`${entries.length} model rate(s). * = locked local override.`);
}

async function main(argv = process.argv.slice(2)) {
  const [cmd, ...rest] = argv;
  if (!cmd || cmd === "-h" || cmd === "--help") {
    usage();
    return;
  }
  if (cmd === "pull") {
    await pullPrices(rest);
    return;
  }
  if (cmd === "show") {
    showPrices(rest);
    return;
  }
  console.error(`token-tracker: unknown prices command: ${cmd}`);
  usage();
  process.exit(2);
}

module.exports = {
  SOURCES,
  DEFAULT_MAX_AGE_MS,
  parseOpenRouter,
  parseLlmCostHub,
  parseBenchGecko,
  buildPricesDocument,
  cleanKey,
  idKey,
  runPull,
  pricesRefreshStatus,
  schedulePricePullIfStale,
  ensureFreshPrices,
  priceRefreshOptions,
  main,
};

if (require.main === module) {
  main().catch((err) => {
    console.error(`token-tracker: ${err.message || err}`);
    process.exit(1);
  });
}
