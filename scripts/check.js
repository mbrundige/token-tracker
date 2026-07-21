#!/usr/bin/env node
"use strict";

const assert = require("assert");
const fs = require("fs");
const path = require("path");
const { cleanSnapshot } = require("./save-token-usage.js");
const { TARGETS, selectedTargets, renderTemplate } = require("../bin/token-tracker.js");
const {
  ratesForModel,
  estimateCostUsdForModel,
  formatCost,
  epochFeatureCost,
} = require("./pricing.js");

const snap = cleanSnapshot({
  summary: "check",
  project: "token-tracker",
  prompt_tokens: 10,
  completion_tokens: 5,
});
assert.strictEqual(snap.total_tokens, 15);
assert.strictEqual(snap.source, "manual");

assert.deepStrictEqual(selectedTargets([]), ["cursor"]);
assert.deepStrictEqual(selectedTargets(["--claude", "--gemini"]), ["claude", "gemini"]);
assert.deepStrictEqual(selectedTargets(["--all"]), Object.keys(TARGETS));
assert.ok(TARGETS.gemini.geminiCommand);
assert.ok(TARGETS.cursor.statusline);

assert.strictEqual(
  renderTemplate("hi {{NAME}}", { NAME: "world" }),
  "hi world",
);

const template = fs.readFileSync(path.join(__dirname, "..", "templates", "SKILL.md"), "utf8");
assert.ok(template.includes("{{SKILL_BIN}}"));
assert.ok(fs.existsSync(path.join(__dirname, "..", "templates", "gemini-command.toml")));
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

console.log("ok");
