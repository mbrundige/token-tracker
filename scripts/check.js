#!/usr/bin/env node
"use strict";

const assert = require("assert");
const fs = require("fs");
const path = require("path");
const { cleanSnapshot } = require("./save-token-usage.js");
const { TARGETS, selectedTargets, renderTemplate } = require("../bin/token-tracker.js");

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

for (const key of Object.keys(TARGETS)) {
  assert.ok(fs.existsSync(path.join(__dirname, "..", key, "SKILL.md")), `missing ${key}/SKILL.md`);
}

console.log("ok");
