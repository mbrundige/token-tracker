#!/usr/bin/env node
"use strict";

const fs = require("fs");
const path = require("path");
const { featureBreakdown, loadRowsFrom } = require("./report-token-usage.js");
const { loadPrices } = require("./pricing.js");

function main() {
  const fixtureDir = path.join(__dirname, "..", "testdata", "parity");
  const historyPath = path.join(fixtureDir, "history.jsonl");
  const pricesPath = path.join(fixtureDir, "prices.json");
  const expectedPath = path.join(fixtureDir, "expected.json");

  if (!fs.existsSync(historyPath)) {
    console.error("parity-check: missing fixture history.jsonl");
    process.exit(1);
  }
  if (!fs.existsSync(pricesPath)) {
    console.error("parity-check: missing fixture prices.json");
    process.exit(1);
  }
  if (!fs.existsSync(expectedPath)) {
    console.error("parity-check: missing fixture expected.json");
    process.exit(1);
  }

  const rows = loadRowsFrom(historyPath);
  const prices = loadPrices(pricesPath);
  const features = featureBreakdown(rows, prices);
  
  const actual = features.map(f => ({
    project: f.project,
    feature: f.feature,
    total_tokens: f.total,
    cost_usd: f.costUsd,
    snapshots: f.snapshots,
    last_seen: f.last_seen,
  }));

  let expected;
  try {
    expected = JSON.parse(fs.readFileSync(expectedPath, "utf8"));
  } catch (err) {
    console.error(`parity-check: failed to read expected.json: ${err.message}`);
    process.exit(1);
  }

  // Compare with tolerance for floating point
  const tolerance = 1e-9;
  let passed = true;

  if (actual.length !== expected.length) {
    console.error(`parity-check: length mismatch - actual: ${actual.length}, expected: ${expected.length}`);
    passed = false;
  } else {
    for (let i = 0; i < actual.length; i++) {
      const a = actual[i];
      const e = expected[i];
      
      if (a.project !== e.project) {
        console.error(`parity-check: project mismatch at index ${i} - actual: "${a.project}", expected: "${e.project}"`);
        passed = false;
      }
      if (a.feature !== e.feature) {
        console.error(`parity-check: feature mismatch at index ${i} - actual: "${a.feature}", expected: "${e.feature}"`);
        passed = false;
      }
      if (a.total_tokens !== e.total_tokens) {
        console.error(`parity-check: total_tokens mismatch at index ${i} - actual: ${a.total_tokens}, expected: ${e.total_tokens}`);
        passed = false;
      }
      if (a.snapshots !== e.snapshots) {
        console.error(`parity-check: snapshots mismatch at index ${i} - actual: ${a.snapshots}, expected: ${e.snapshots}`);
        passed = false;
      }
      if (a.last_seen !== e.last_seen) {
        console.error(`parity-check: last_seen mismatch at index ${i} - actual: "${a.last_seen}", expected: "${e.last_seen}"`);
        passed = false;
      }
      
      // Float tolerance for cost_usd
      if (a.cost_usd == null && e.cost_usd != null) {
        console.error(`parity-check: cost_usd mismatch at index ${i} - actual: null, expected: ${e.cost_usd}`);
        passed = false;
      } else if (a.cost_usd != null && e.cost_usd == null) {
        console.error(`parity-check: cost_usd mismatch at index ${i} - actual: ${a.cost_usd}, expected: null`);
        passed = false;
      } else if (a.cost_usd != null && e.cost_usd != null) {
        const diff = Math.abs(a.cost_usd - e.cost_usd);
        if (diff > tolerance) {
          console.error(`parity-check: cost_usd mismatch at index ${i} - actual: ${a.cost_usd}, expected: ${e.cost_usd}, diff: ${diff}`);
          passed = false;
        }
      }
    }
  }

  if (passed) {
    console.log("ok");
    process.exit(0);
  } else {
    console.log("fail");
    process.exit(1);
  }
}

if (require.main === module) main();