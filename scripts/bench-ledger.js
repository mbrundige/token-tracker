#!/usr/bin/env node
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const { performance } = require("perf_hooks");
const { featureBreakdown, loadRowsFrom } = require("./report-token-usage.js");
const { loadPrices, featureOngoingCost, computeCostDelta } = require("./pricing.js");
const { 
  resolveIndexPath, 
  ensureIndex, 
  reportFromIndex,
  lastFromIndex
} = require("./ledger-index.js");

function parseArgs(argv) {
  const args = {
    rows: 1_000_000,
    budgetMs: 10,
    allowSlow: false,
    index: false,
  };
  
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--rows") {
      args.rows = Number(argv[++i]);
    } else if (a === "--budget-ms") {
      args.budgetMs = Number(argv[++i]);
    } else if (a === "--allow-slow") {
      args.allowSlow = true;
    } else if (a === "--index") {
      args.index = true;
    } else {
      console.error(`bench-ledger: unknown argument: ${a}`);
      process.exit(2);
    }
  }
  return args;
}

function generateSyntheticData(rows, tmpPath) {
  const features = [
    { project: "webapp", feature: "auth" },
    { project: "webapp", feature: "dashboard" },
    { project: "api", feature: "users" },
    { project: "api", feature: "orders" },
    { project: "mobile", feature: "checkout" },
  ];
  
  const models = ["GPT-5.5", "Claude Opus", "GPT-4o"];
  
  let lines = [];
  let baseTime = new Date("2024-01-01T00:00:00Z").getTime();
  
  for (let i = 0; i < rows; i++) {
    const feature = features[i % features.length];
    const model = models[i % models.length];
    const timestamp = new Date(baseTime + i * 60000).toISOString(); // 1 minute apart
    
    // Growing token totals with some resets
    const resetEvery = Math.floor(rows / 10);
    const baseTokens = (i % resetEvery < resetEvery - 1) ? 
      Math.floor((i % resetEvery) * 1000 + Math.random() * 500) : 
      Math.floor(Math.random() * 2000);
    
    const promptTokens = Math.floor(baseTokens * 0.7);
    const completionTokens = baseTokens - promptTokens;
    
    const row = {
      timestamp,
      project: feature.project,
      feature: feature.feature,
      source: "bench",
      summary: `Synthetic row ${i}`,
      model,
      prompt_tokens: promptTokens,
      completion_tokens: completionTokens,
      total_tokens: baseTokens,
    };
    
    // Add some locked costs
    if (i % 3 === 0) {
      row.cost_delta_usd = Math.random() * 0.01;
      row.estimated_cost_usd = row.cost_delta_usd * (i + 1);
    }
    
    lines.push(JSON.stringify(row));
    
    // Write in chunks to avoid memory issues
    if (i % 10000 === 9999 || i === rows - 1) {
      fs.appendFileSync(tmpPath, lines.join("\n") + "\n");
      lines = [];
    }
  }
}

function benchStatuslinePath(rowsOrIndex, prices, useIndex = false) {
  // Simulate statusline path: find last scope + ongoing cost for one feature
  const project = "webapp";
  const feature = "auth";
  const inputTokens = 5000;
  const outputTokens = 2000;
  const totalTokens = inputTokens + outputTokens;
  const model = "GPT-5.5";
  
  const start = performance.now();
  let result;
  
  if (useIndex) {
    // Use index path
    const index = rowsOrIndex;
    const lastSnapshot = lastFromIndex(index, project, feature);
    const current = {
      project,
      feature,
      model,
      prompt_tokens: inputTokens,
      completion_tokens: outputTokens,
      total_tokens: totalTokens,
    };
    const tip = computeCostDelta(lastSnapshot, current, prices);
    
    const key = `${project}\t${feature || "(none)"}`;
    const entry = index.features[key];
    const lockedUsd = entry ? (entry.cost_usd || 0) : 0;
    const tipUsd = tip.costDeltaUsd || 0;
    const totalUsd = lockedUsd + tipUsd;
    
    result = {
      costUsd: (entry && entry.cost_usd != null) || tip.costDeltaUsd != null ? totalUsd : null,
    };
  } else {
    // Use full scan path
    const rows = rowsOrIndex;
    result = featureOngoingCost(rows, { 
      project, 
      feature, 
      inputTokens, 
      outputTokens, 
      totalTokens, 
      model 
    }, prices);
  }
  
  const end = performance.now();
  
  return {
    ms: end - start,
    result: result.costUsd,
  };
}

function benchFullReport(rowsOrIndex, prices, useIndex = false) {
  const start = performance.now();
  let features;
  
  if (useIndex) {
    // Use index path
    features = reportFromIndex(rowsOrIndex);
  } else {
    // Use full scan path
    features = featureBreakdown(rowsOrIndex, prices);
  }
  
  const end = performance.now();
  
  return {
    ms: end - start,
    features: features.length,
  };
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  
  const tmpPath = path.join(os.tmpdir(), `bench-ledger-${process.pid}.jsonl`);
  const tmpIndexPath = path.join(os.tmpdir(), `bench-index-${process.pid}.json`);
  
  try {
    console.log(`Generating ${args.rows} synthetic rows...`);
    generateSyntheticData(args.rows, tmpPath);
    
    console.log("Loading data...");
    const pricesPath = path.join(__dirname, "..", "testdata", "parity", "prices.json");
    const prices = loadPrices(pricesPath);
    
    let data, dataType;
    if (args.index) {
      console.log("Building index...");
      const index = ensureIndex({ 
        historyPath: tmpPath, 
        pricesPath, 
        indexPath: tmpIndexPath 
      });
      data = index;
      dataType = "index";
    } else {
      const rows = loadRowsFrom(tmpPath);
      data = rows;
      dataType = "rows";
    }
    
    console.log(`\nBench A: Statusline path (${args.rows} ${dataType})`);
    const statuslineBench = benchStatuslinePath(data, prices, args.index);
    const statuslineRowsPerSec = args.rows / (statuslineBench.ms / 1000);
    console.log(`  Time: ${statuslineBench.ms.toFixed(2)}ms`);
    console.log(`  Rate: ${statuslineRowsPerSec.toFixed(0)} rows/sec`);
    console.log(`  Result: ${statuslineBench.result}`);
    
    console.log(`\nBench B: Full report (${args.rows} ${dataType})`);
    const reportBench = benchFullReport(data, prices, args.index);
    const reportRowsPerSec = args.rows / (reportBench.ms / 1000);
    console.log(`  Time: ${reportBench.ms.toFixed(2)}ms`);
    console.log(`  Rate: ${reportRowsPerSec.toFixed(0)} rows/sec`);
    console.log(`  Features: ${reportBench.features}`);
    
    // Charlie bar is enforced at N>=1e6; smaller N only warns when over budget.
    const overBudget = statuslineBench.ms >= args.budgetMs;
    if (!overBudget) {
      console.log(`\nPASS: Bench A within budget (${statuslineBench.ms.toFixed(2)}ms < ${args.budgetMs}ms)`);
      process.exit(0);
    }
    if (args.rows >= 1e6 && !args.allowSlow) {
      console.log(
        `\nFAIL: Bench A exceeded budget of ${args.budgetMs}ms at ${args.rows} rows (${statuslineBench.ms.toFixed(2)}ms)`,
      );
      process.exit(1);
    }
    if (args.allowSlow) {
      console.log(
        `\nWARN: Bench A exceeded budget of ${args.budgetMs}ms (${statuslineBench.ms.toFixed(2)}ms) but --allow-slow specified`,
      );
    } else {
      console.log(
        `\nWARN: Bench A exceeded budget of ${args.budgetMs}ms (${statuslineBench.ms.toFixed(2)}ms); enforce only at rows>=1e6`,
      );
    }
    process.exit(0);
  } finally {
    try {
      fs.unlinkSync(tmpPath);
      fs.unlinkSync(tmpIndexPath);
    } catch {
      // ignore cleanup errors
    }
  }
}

if (require.main === module) main();