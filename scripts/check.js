#!/usr/bin/env node
"use strict";

const assert = require("assert");
const fs = require("fs");
const os = require("os");
const path = require("path");
const { cleanSnapshot, parseArgs } = require("./save-token-usage.js");
const { TARGETS, selectedTargets, renderTemplate } = require("../bin/token-tracker.js");
const { createAnsi, wantsColor } = require("./ansi.js");
const {
  ratesForModel,
  estimateCostUsdForModel,
  formatCost,
  epochFeatureCost,
  computeCostDelta,
} = require("./pricing.js");
const {
  parseOpenRouter,
  buildPricesDocument,
  cleanKey,
  schedulePricePullIfStale,
  priceRefreshOptions,
  pricesRefreshStatus,
} = require("./pull-prices.js");

const snap = cleanSnapshot({
  summary: "check",
  project: "token-tracker",
  prompt_tokens: 10,
  completion_tokens: 5,
});
assert.strictEqual(snap.total_tokens, 15);
assert.strictEqual(snap.source, "manual");

// Test underscore aliases
const underscoreArgs = parseArgs(["--prompt_tokens", "10", "--completion_tokens", "5", "--summary", "x"]);
assert.strictEqual(underscoreArgs.prompt_tokens, 10);
assert.strictEqual(underscoreArgs.completion_tokens, 5);
assert.strictEqual(underscoreArgs.summary, "x");

// Test kebab forms still work
const kebabArgs = parseArgs(["--prompt-tokens", "20", "--completion-tokens", "10", "--summary", "y"]);
assert.strictEqual(kebabArgs.prompt_tokens, 20);
assert.strictEqual(kebabArgs.completion_tokens, 10);
assert.strictEqual(kebabArgs.summary, "y");

// Test --help flag
const helpArgs = parseArgs(["--help"]);
assert.strictEqual(helpArgs.help, true);

const hArgs = parseArgs(["-h"]);
assert.strictEqual(hArgs.help, true);

const helpWordArgs = parseArgs(["help"]);
assert.strictEqual(helpWordArgs.help, true);

// Test mixed underscore aliases
const mixedArgs = parseArgs(["--total_tokens", "100", "--metadata_json", '{"foo":"bar"}', "--summary", "z"]);
assert.strictEqual(mixedArgs.total_tokens, 100);
assert.strictEqual(mixedArgs.metadata_json, '{"foo":"bar"}');
assert.strictEqual(mixedArgs.summary, "z");

assert.deepStrictEqual(selectedTargets([]), ["cursor"]);
assert.deepStrictEqual(selectedTargets(["--claude", "--gemini"]), ["claude", "gemini"]);
assert.deepStrictEqual(selectedTargets(["--all"]), Object.keys(TARGETS));
assert.ok(TARGETS.gemini.geminiCommand);
assert.ok(TARGETS.cursor.statusline);
assert.ok(TARGETS.cursor.slashCommandDir);
assert.ok(TARGETS.claude.slashCommandDir);

const off = createAnsi(false);
assert.strictEqual(off.green("$1.23"), "$1.23");
assert.strictEqual(off.heat(4, "█"), "█");
const on = createAnsi(true);
assert.ok(on.green("$1.23").includes("\x1b["));
assert.ok(on.heat(4, "█").includes("\x1b["));
assert.strictEqual(typeof wantsColor(), "boolean");

assert.strictEqual(
  renderTemplate("hi {{NAME}}", { NAME: "world" }),
  "hi world",
);
assert.strictEqual(
  renderTemplate("keep {{args}} and {{SET}}", { SET: "ok" }),
  "keep {{args}} and ok",
);

const template = fs.readFileSync(path.join(__dirname, "..", "templates", "SKILL.md"), "utf8");
assert.ok(template.includes("{{SKILL_BIN}}"));
assert.ok(template.includes("set-feature"));
assert.ok(fs.existsSync(path.join(__dirname, "..", "templates", "gemini-command.toml")));
assert.ok(fs.existsSync(path.join(__dirname, "..", "templates", "gemini-set-feature.toml")));
assert.ok(fs.existsSync(path.join(__dirname, "..", "templates", "set-feature.md")));
assert.ok(fs.existsSync(path.join(__dirname, "..", "templates", "prices.json")));

for (const key of Object.keys(TARGETS)) {
  assert.ok(fs.existsSync(path.join(__dirname, "..", key, "SKILL.md")), `missing ${key}/SKILL.md`);
}

const prices = JSON.parse(
  fs.readFileSync(path.join(__dirname, "..", "templates", "prices.json"), "utf8"),
);
const gpt55 = ratesForModel(prices, "GPT-5.5");
assert.ok(gpt55);
assert.strictEqual(gpt55.input, 5);
assert.strictEqual(gpt55.output, 30);

// Longer pattern should win over shorter "gpt-5.5"
const gpt55pro = ratesForModel(prices, "GPT-5.5 Pro");
assert.strictEqual(gpt55pro.input, 30);

const cost = estimateCostUsdForModel(prices, "GPT-5.5", 1_000_000, 1_000_000);
assert.ok(Math.abs(cost - 35) < 1e-9);
assert.strictEqual(formatCost(0.01234), "$0.0123");

const epoch = epochFeatureCost(
  [
    { ts: "1", total: 1000, prompt_tokens: 700, completion_tokens: 300, model: "GPT-5.5" },
    { ts: "2", total: 2000, prompt_tokens: 1400, completion_tokens: 600, model: "GPT-5.5" },
    // reset
    { ts: "3", total: 500, prompt_tokens: 400, completion_tokens: 100, model: "Claude Opus" },
  ],
  prices,
);
assert.ok(epoch.costUsd != null);
// GPT-5.5 deltas: 1400in/600out at $5/$30 per M = 0.007+0.018=0.025
// Claude Opus: 400in/100out at $5/$25 = 0.002+0.0025=0.0045
// total ~0.0295
assert.ok(Math.abs(epoch.costUsd - 0.0295) < 1e-9);

// Locked deltas win even if live prices would differ.
const lockedEpoch = epochFeatureCost(
  [
    {
      ts: "1",
      total: 1000,
      prompt_tokens: 700,
      completion_tokens: 300,
      model: "GPT-5.5",
      cost_delta_usd: 0.01,
    },
    {
      ts: "2",
      total: 2000,
      prompt_tokens: 1400,
      completion_tokens: 600,
      model: "GPT-5.5",
      cost_delta_usd: 0.02,
    },
  ],
  prices,
  { preferLocked: true },
);
assert.ok(Math.abs(lockedEpoch.costUsd - 0.03) < 1e-9);
assert.strictEqual(lockedEpoch.lockedDeltas, 2);
assert.strictEqual(lockedEpoch.liveDeltas, 0);

const delta = computeCostDelta(
  { project: "p", feature: "f", prompt_tokens: 700, completion_tokens: 300, total_tokens: 1000, estimated_cost_usd: 0.01 },
  { project: "p", feature: "f", model: "GPT-5.5", prompt_tokens: 1400, completion_tokens: 600, total_tokens: 2000 },
  prices,
);
assert.ok(Math.abs(delta.costDeltaUsd - 0.0125) < 1e-9);
assert.ok(Math.abs(delta.estimatedCostUsd - 0.0225) < 1e-9);

const freshPricesPath = path.join("/tmp", `tt-prices-fresh-${process.pid}.json`);
fs.writeFileSync(
  freshPricesPath,
  `${JSON.stringify({
    updated_at: new Date().toISOString(),
    source: "openrouter",
    default: prices.default,
    models: {},
  }, null, 2)}\n`,
);
const sched = schedulePricePullIfStale({
  pricesPath: freshPricesPath,
  maxAgeMs: 60 * 60 * 1000,
});
assert.strictEqual(sched.scheduled, false);
assert.strictEqual(sched.reason, "fresh");

// Seed / missing source must refresh even when file mtime is new
const seedPath = path.join("/tmp", `tt-prices-seed-${process.pid}.json`);
fs.writeFileSync(
  seedPath,
  `${JSON.stringify({ source: "seed", default: prices.default, models: {} }, null, 2)}\n`,
);
const seedStatus = pricesRefreshStatus(seedPath, 60 * 60 * 1000);
assert.strictEqual(seedStatus.needed, true);
assert.strictEqual(seedStatus.reason, "seed");
const seedSched = schedulePricePullIfStale({
  pricesPath: seedPath,
  maxAgeMs: 60 * 60 * 1000,
});
assert.strictEqual(seedSched.scheduled, true);
assert.ok(["seed", "in_flight"].includes(seedSched.reason) || seedSched.scheduled);
fs.rmSync(freshPricesPath, { force: true });
fs.rmSync(seedPath, { force: true });
fs.rmSync(`${seedPath}.pulling`, { force: true });

const refresh = priceRefreshOptions({ prices: { auto_pull: true, auto_pull_interval_hours: 1 } });
assert.strictEqual(refresh.maxAgeMs, 3600000);
assert.strictEqual(priceRefreshOptions({ prices: { auto_pull: false } }).autoPull, false);

// Shared data home migration: legacy ~/.cursor/token-tracker -> ~/.token-tracker
const home = fs.mkdtempSync(path.join(os.tmpdir(), "tt-home-"));
const prevHome = process.env.HOME;
process.env.HOME = home;
delete require.cache[require.resolve("./paths.js")];
const pathsFresh = require("./paths.js");
fs.mkdirSync(path.join(home, ".cursor", "token-tracker"), { recursive: true });
fs.writeFileSync(path.join(home, ".cursor", "token-tracker", "history.jsonl"), '{"ok":1}\n');
const mig = pathsFresh.migrateLegacyDataDir(path.join(home, ".token-tracker"));
assert.strictEqual(mig.migrated, true);
assert.ok(fs.existsSync(path.join(home, ".token-tracker", "history.jsonl")));
assert.ok(fs.existsSync(path.join(home, ".cursor", "token-tracker", "history.jsonl")), "legacy kept");
assert.strictEqual(pathsFresh.resolveDataDir(), path.join(home, ".token-tracker"));
process.env.HOME = prevHome;
delete require.cache[require.resolve("./paths.js")];
fs.rmSync(home, { recursive: true, force: true });

// Install writes /set-feature slash commands for Cursor, Claude, and Gemini
const installHome = fs.mkdtempSync(path.join(os.tmpdir(), "tt-install-"));
const prevHome2 = process.env.HOME;
process.env.HOME = installHome;
fs.mkdirSync(path.join(installHome, ".cursor"), { recursive: true });
fs.writeFileSync(path.join(installHome, ".cursor", "cli-config.json"), "{}\n", "utf8");
const installResult = require("child_process").spawnSync(
  process.execPath,
  [path.join(__dirname, "..", "bin", "token-tracker.js"), "install", "--cursor", "--claude", "--gemini", "--no-statusline"],
  { encoding: "utf8", env: { ...process.env, HOME: installHome } },
);
assert.strictEqual(installResult.status, 0, installResult.stderr || installResult.stdout);
const installJson = JSON.parse(installResult.stdout.slice(installResult.stdout.indexOf("{")));
assert.ok(installJson.cli_launcher, "install should report cli_launcher");
assert.ok(fs.existsSync(path.join(installHome, ".cursor", "commands", "set-feature.md")));
assert.ok(fs.existsSync(path.join(installHome, ".claude", "commands", "set-feature.md")));
assert.ok(fs.existsSync(path.join(installHome, ".gemini", "commands", "set-feature.toml")));
assert.ok(fs.existsSync(path.join(installHome, ".gemini", "commands", "token-tracker.toml")));
const cursorCmd = fs.readFileSync(path.join(installHome, ".cursor", "commands", "set-feature.md"), "utf8");
assert.ok(cursorCmd.includes("set-token-context.js"));
assert.ok(cursorCmd.includes("node ~/.cursor/skills/token-tracker/scripts"));
assert.ok(cursorCmd.includes("~/.cursor/skills/token-tracker/scripts"));
const geminiSet = fs.readFileSync(path.join(installHome, ".gemini", "commands", "set-feature.toml"), "utf8");
assert.ok(geminiSet.includes("{{args}}"));
assert.ok(geminiSet.includes("set-token-context.js"));
const skillMd = fs.readFileSync(
  path.join(installHome, ".cursor", "skills", "token-tracker", "SKILL.md"),
  "utf8",
);
assert.ok(skillMd.includes("node ~/.cursor/skills/token-tracker/scripts/report-token-usage.js"));

// PATH launcher must make `token-tracker` resolvable (regression for command not found)
const launcher = path.join(installHome, ".local", "bin", "token-tracker");
assert.ok(fs.existsSync(launcher), "missing ~/.local/bin/token-tracker");
assert.ok(fs.statSync(launcher).mode & 0o111, "launcher must be executable");
assert.ok(fs.existsSync(path.join(installHome, ".token-tracker", "cli", "bin", "token-tracker.js")));
const which = require("child_process").spawnSync("bash", ["-lc", "command -v token-tracker"], {
  encoding: "utf8",
  env: {
    ...process.env,
    HOME: installHome,
    PATH: `${path.join(installHome, ".local", "bin")}${path.delimiter}${process.env.PATH || ""}`,
  },
});
assert.strictEqual(which.status, 0, which.stderr || which.stdout);
assert.ok(String(which.stdout).includes("token-tracker"));
const help = require("child_process").spawnSync("token-tracker", ["--help"], {
  encoding: "utf8",
  env: {
    ...process.env,
    HOME: installHome,
    PATH: `${path.join(installHome, ".local", "bin")}${path.delimiter}${process.env.PATH || ""}`,
  },
});
assert.strictEqual(help.status, 0, help.stderr || help.stdout);
assert.ok(String(help.stdout).includes("set-feature"), help.stdout);

// Status line must be invoked via node (not a bare shebang path alone)
const statusInstall = require("child_process").spawnSync(
  process.execPath,
  [path.join(__dirname, "..", "bin", "token-tracker.js"), "install", "--cursor"],
  { encoding: "utf8", env: { ...process.env, HOME: installHome } },
);
assert.strictEqual(statusInstall.status, 0, statusInstall.stderr || statusInstall.stdout);
const cliConfig = JSON.parse(
  fs.readFileSync(path.join(installHome, ".cursor", "cli-config.json"), "utf8"),
);
assert.ok(cliConfig.statusLine && typeof cliConfig.statusLine.command === "string");
assert.ok(
  cliConfig.statusLine.command.startsWith("node "),
  `statusLine should use node: ${cliConfig.statusLine.command}`,
);

const setFeature = require("child_process").spawnSync(
  process.execPath,
  [
    path.join(__dirname, "..", "bin", "token-tracker.js"),
    "set-feature",
    "slash-demo",
    "--workspace",
    path.join(installHome, "proj"),
  ],
  {
    encoding: "utf8",
    env: {
      ...process.env,
      HOME: installHome,
      TOKEN_TRACKER_HOME: path.join(installHome, ".token-tracker"),
    },
  },
);
assert.strictEqual(setFeature.status, 0, setFeature.stderr || setFeature.stdout);
const setOut = JSON.parse(setFeature.stdout);
assert.strictEqual(setOut.feature, "slash-demo");
assert.strictEqual(setOut.tokens_reset, true);
process.env.HOME = prevHome2;
fs.rmSync(installHome, { recursive: true, force: true });

assert.strictEqual(cleanKey("OpenAI: GPT-5.5"), "gpt-5.5");
const pulled = parseOpenRouter({
  data: [
    {
      id: "openai/gpt-5.5",
      name: "OpenAI: GPT-5.5",
      pricing: { prompt: "0.000005", completion: "0.00003" },
    },
    {
      id: "anthropic/claude-opus-4.8",
      name: "Anthropic: Claude Opus 4.8",
      pricing: { prompt: "0.000005", completion: "0.000025" },
    },
    {
      id: "mistral/skip-me",
      name: "Mistral: Skip",
      pricing: { prompt: "0.000001", completion: "0.000001" },
    },
  ],
});
assert.ok(pulled["gpt-5.5"]);
assert.strictEqual(pulled["gpt-5.5"].input_per_million_usd, 5);
assert.ok(pulled["claude opus"]);
assert.strictEqual(pulled["claude opus"].output_per_million_usd, 25);
assert.ok(!pulled["skip-me"]);

// "o4-mini" alias must resolve to the base model, never the
// "-deep-research" sibling (different price tier), regardless of which
// one the upstream API lists first (both tie on versionScore, so array
// order previously decided the winner).
const o4miniRows = [
  { id: "openai/o4-mini", name: "OpenAI: o4-mini", pricing: { prompt: "0.0000011", completion: "0.0000044" } },
  {
    id: "openai/o4-mini-deep-research",
    name: "OpenAI: o4-mini (deep research)",
    pricing: { prompt: "0.000002", completion: "0.000008" },
  },
];
const o4BaseFirst = parseOpenRouter({ data: o4miniRows });
const o4ResearchFirst = parseOpenRouter({ data: [...o4miniRows].reverse() });
assert.strictEqual(o4BaseFirst["o4-mini"].input_per_million_usd, 1.1);
assert.strictEqual(o4ResearchFirst["o4-mini"].input_per_million_usd, 1.1);

const doc = buildPricesDocument({
  source: "openrouter",
  url: "https://example.test",
  models: pulled,
  previous: {
    default: { input_per_million_usd: 1, output_per_million_usd: 2 },
    models: { "my-local": { input_per_million_usd: 9, output_per_million_usd: 9, locked: true } },
  },
});
assert.strictEqual(doc.default.input_per_million_usd, 1);
assert.ok(doc.models["my-local"].locked);

console.log("ok");
