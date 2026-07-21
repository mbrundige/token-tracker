#!/usr/bin/env node
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");
const { spawnSync } = require("child_process");
const { resolveDataDir, ensureDataDir } = require("../scripts/paths.js");

const ROOT = path.resolve(__dirname, "..");
const DATA_DIR = resolveDataDir();
const TEMPLATE_SKILL = path.join(ROOT, "templates", "SKILL.md");
const TEMPLATE_GEMINI_CMD = path.join(ROOT, "templates", "gemini-command.toml");
const TEMPLATE_GEMINI_SET_FEATURE = path.join(ROOT, "templates", "gemini-set-feature.toml");
const TEMPLATE_SET_FEATURE = path.join(ROOT, "templates", "set-feature.md");

const DEFAULT_CONFIG = {
  default_project: null,
  default_feature: null,
  projects: {},
  features: {},
  statusline: {
    enabled: true,
    show_label: true,
    show_project: true,
    show_feature: true,
    show_model: true,
    show_context: true,
    show_tokens: true,
    show_cost: true,
  },
  prices: {
    auto_pull: true,
    auto_pull_interval_hours: 1,
    source: "openrouter",
  },
};

/**
 * @type {Record<string, {
 *   flag: string,
 *   skillDir: string,
 *   label: string,
 *   statusline?: boolean,
 *   geminiCommand?: boolean,
 *   slashCommandDir?: string,
 * }>}
 */
const TARGETS = {
  cursor: {
    flag: "--cursor",
    skillDir: ".cursor/skills",
    label: "Cursor",
    statusline: true,
    slashCommandDir: ".cursor/commands",
  },
  claude: {
    flag: "--claude",
    skillDir: ".claude/skills",
    label: "Claude Code",
    slashCommandDir: ".claude/commands",
  },
  gemini: {
    flag: "--gemini",
    skillDir: ".gemini/skills",
    label: "Gemini CLI",
    geminiCommand: true,
  },
  codex: {
    flag: "--codex",
    skillDir: ".codex/skills",
    label: "Codex CLI",
  },
  agents: {
    flag: "--agents",
    skillDir: ".agents/skills",
    label: "Agent Skills (~/.agents)",
  },
  continue: {
    flag: "--continue",
    skillDir: ".continue/skills",
    label: "Continue CLI",
  },
};

const SCRIPT_FILES = [
  "save-token-usage.js",
  "set-token-context.js",
  "statusline-token-usage.js",
  "report-token-usage.js",
  "pricing.js",
  "pull-prices.js",
  "paths.js",
  "ansi.js",
];

function usage() {
  console.log(`Usage:
  npx @mbrundige/token-tracker install [targets...] [--statusline|--no-statusline]
  npx @mbrundige/token-tracker report
  npx @mbrundige/token-tracker save --summary "..." [--project NAME] [--feature NAME]
  npx @mbrundige/token-tracker set-context --project NAME --feature NAME [--workspace PATH]
  npx @mbrundige/token-tracker set-feature NAME [--workspace PATH]
  npx @mbrundige/token-tracker set-feature --clear [--workspace PATH]
  npx @mbrundige/token-tracker statusline   # reads status JSON from stdin
  npx @mbrundige/token-tracker prices pull [--source openrouter|llmcosthub|benchgecko]
  npx @mbrundige/token-tracker prices show

Install targets:
  --all                 Cursor, Claude, Gemini, Codex, Agent Skills, Continue
  --cursor              Cursor skill + /set-feature slash command (default when no target flags)
  --claude              Claude Code skill + /set-feature slash command
  --gemini              Gemini CLI skill + /token-tracker and /set-feature commands
  --codex               Codex CLI skill
  --agents              Shared Agent Skills path (~/.agents/skills)
  --continue            Continue CLI skill
  --statusline          Wire Cursor CLI statusLine (default with --cursor/--all)
  --no-statusline       Skip Cursor CLI statusLine changes

Defaults for install: --cursor and --statusline
`);
}

function ensureConfig() {
  const { dataDir, migration } = ensureDataDir(DATA_DIR);
  const configPath = path.join(dataDir, "config.json");
  if (!fs.existsSync(configPath)) {
    fs.writeFileSync(configPath, `${JSON.stringify(DEFAULT_CONFIG, null, 2)}\n`, "utf8");
    return { created: true, configPath, migration };
  }
  return { created: false, configPath, migration };
}

function ensurePrices() {
  const { dataDir, migration } = ensureDataDir(DATA_DIR);
  const pricesPath = path.join(dataDir, "prices.json");
  const template = path.join(ROOT, "templates", "prices.json");
  if (!fs.existsSync(pricesPath)) {
    fs.copyFileSync(template, pricesPath);
    return { created: true, pricesPath, migration };
  }
  return { created: false, pricesPath, migration };
}

function renderTemplate(template, vars) {
  return template.replace(/\{\{(\w+)\}\}/g, (match, key) => {
    // Leave unknown placeholders alone (e.g. Gemini CLI {{args}}).
    if (!(key in vars)) return match;
    return vars[key];
  });
}

function installSkill(targetKey) {
  const target = TARGETS[targetKey];
  const dest = path.join(os.homedir(), target.skillDir, "token-tracker");
  const scriptsDest = path.join(dest, "scripts");
  const skillBin = path.join("~", target.skillDir, "token-tracker", "scripts");

  fs.mkdirSync(dest, { recursive: true });
  const skillBody = renderTemplate(fs.readFileSync(TEMPLATE_SKILL, "utf8"), {
    SKILL_BIN: skillBin,
  });
  fs.writeFileSync(path.join(dest, "SKILL.md"), skillBody, "utf8");

  fs.rmSync(scriptsDest, { recursive: true, force: true });
  fs.mkdirSync(scriptsDest, { recursive: true });
  for (const file of SCRIPT_FILES) {
    const to = path.join(scriptsDest, file);
    fs.copyFileSync(path.join(ROOT, "scripts", file), to);
    fs.chmodSync(to, 0o755);
  }

  const extras = {};
  if (target.geminiCommand) {
    extras.gemini_commands = installGeminiCommands(dest);
  }
  if (target.slashCommandDir) {
    extras.set_feature_command = installMarkdownSetFeatureCommand(
      target.slashCommandDir,
      skillBin,
    );
  }
  return { dest, label: target.label, ...extras };
}

function installMarkdownSetFeatureCommand(commandsDirRel, skillBin) {
  const commandsDir = path.join(os.homedir(), commandsDirRel);
  fs.mkdirSync(commandsDir, { recursive: true });
  const body = renderTemplate(fs.readFileSync(TEMPLATE_SET_FEATURE, "utf8"), {
    SKILL_BIN: skillBin,
  });
  const dest = path.join(commandsDir, "set-feature.md");
  fs.writeFileSync(dest, body, "utf8");
  return dest;
}

function installGeminiCommands(skillRoot) {
  const commandsDir = path.join(os.homedir(), ".gemini", "commands");
  fs.mkdirSync(commandsDir, { recursive: true });
  const reportScript = path.join(skillRoot, "scripts", "report-token-usage.js");
  const setScript = path.join(skillRoot, "scripts", "set-token-context.js");
  const reportBody = renderTemplate(fs.readFileSync(TEMPLATE_GEMINI_CMD, "utf8"), {
    SKILL_ROOT: skillRoot,
    REPORT_SCRIPT: reportScript,
  });
  const setBody = renderTemplate(fs.readFileSync(TEMPLATE_GEMINI_SET_FEATURE, "utf8"), {
    SKILL_ROOT: skillRoot,
    SET_SCRIPT: setScript,
  });
  const reportDest = path.join(commandsDir, "token-tracker.toml");
  const setDest = path.join(commandsDir, "set-feature.toml");
  fs.writeFileSync(reportDest, reportBody, "utf8");
  fs.writeFileSync(setDest, setBody, "utf8");
  return [reportDest, setDest];
}

function patchCliStatusLine(scriptPath) {
  const cliConfig = path.join(os.homedir(), ".cursor", "cli-config.json");
  if (!fs.existsSync(cliConfig)) {
    console.log(`Skipped statusLine: ${cliConfig} not found`);
    return false;
  }
  const config = JSON.parse(fs.readFileSync(cliConfig, "utf8"));
  config.statusLine = {
    type: "command",
    command: scriptPath,
    padding: 2,
    timeoutMs: 1000,
  };
  fs.writeFileSync(cliConfig, `${JSON.stringify(config, null, 2)}\n`, "utf8");
  return true;
}

function selectedTargets(argv) {
  const wantAll = argv.includes("--all");
  const explicit = Object.entries(TARGETS)
    .filter(([, t]) => argv.includes(t.flag))
    .map(([key]) => key);

  if (wantAll) return Object.keys(TARGETS);
  if (explicit.length) return explicit;
  return ["cursor"];
}

function install(argv) {
  const unknown = argv.filter(
    (a) =>
      ![
        "--all",
        "--statusline",
        "--no-statusline",
        ...Object.values(TARGETS).map((t) => t.flag),
      ].includes(a),
  );
  if (unknown.length) {
    console.error(`token-tracker: unknown install option(s): ${unknown.join(", ")}`);
    usage();
    process.exit(2);
  }

  const noStatusline = argv.includes("--no-statusline");
  const forceStatusline = argv.includes("--statusline");
  const keys = selectedTargets(argv);
  const installed = keys.map((key) => installSkill(key));
  const { created, configPath, migration } = ensureConfig();
  const { created: pricesCreated, pricesPath } = ensurePrices();

  const cursorInstall = installed.find((item) => item.dest.includes(`${path.sep}.cursor${path.sep}`));
  const wantsStatusline =
    forceStatusline || (!noStatusline && keys.includes("cursor"));

  let statuslinePath = null;
  let statuslineWired = false;
  if (wantsStatusline && cursorInstall) {
    statuslinePath = path.join(cursorInstall.dest, "scripts", "statusline-token-usage.js");
    statuslineWired = patchCliStatusLine(statuslinePath);
  } else if (wantsStatusline && !cursorInstall) {
    console.log("Skipped statusLine: install --cursor (or --all) to wire Cursor CLI statusLine.");
  }

  const notes = [];
  if (migration && migration.migrated) {
    notes.push(
      `Migrated shared data from ${migration.legacy} to ${migration.dataDir} (${(migration.copied || []).join(", ") || "files"}).`,
    );
  }
  if (statuslineWired) notes.push("Restart Cursor CLI to pick up statusLine changes.");
  if (pricesCreated) notes.push(`Seeded default price table at ${pricesPath}.`);
  if (keys.includes("gemini")) notes.push("In Gemini CLI run /commands reload and /skills reload.");
  if (keys.includes("cursor") || keys.includes("claude")) {
    notes.push("Cursor/Claude: /set-feature is available after install (reload chat if needed).");
  }
  if (keys.includes("codex") || keys.includes("agents") || keys.includes("continue")) {
    notes.push("Restart Codex/Continue (or reload skills) if the new skill does not appear.");
  }

  console.log(
    JSON.stringify(
      {
        installed: installed.map((item) => ({
          label: item.label,
          path: item.dest,
          ...(item.gemini_commands ? { gemini_commands: item.gemini_commands } : {}),
          ...(item.set_feature_command
            ? { set_feature_command: item.set_feature_command }
            : {}),
        })),
        data_dir: DATA_DIR,
        config: configPath,
        config_created: created,
        prices: pricesPath,
        prices_created: pricesCreated,
        migrated_from_cursor: Boolean(migration && migration.migrated),
        statusline: statuslinePath,
        statusline_wired: statuslineWired,
        note: notes.length ? notes.join(" ") : undefined,
      },
      null,
      2,
    ),
  );
}

function normalizeSetFeatureArgs(argv) {
  const out = [];
  let sawFeature = false;
  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--clear" || a === "--clear-feature") {
      out.push("--clear-feature");
      continue;
    }
    if (a === "--feature") {
      out.push("--feature", argv[++i]);
      sawFeature = true;
      continue;
    }
    if (a === "--workspace") {
      out.push("--workspace", argv[++i]);
      continue;
    }
    if (a === "--project") {
      out.push("--project", argv[++i]);
      continue;
    }
    if (a.startsWith("-")) {
      console.error(`token-tracker: unknown set-feature option: ${a}`);
      usage();
      process.exit(2);
    }
    if (sawFeature) {
      console.error(`token-tracker: unexpected argument: ${a}`);
      usage();
      process.exit(2);
    }
    out.push("--feature", a);
    sawFeature = true;
  }
  if (!sawFeature && !out.includes("--clear-feature")) {
    console.error("token-tracker: set-feature requires a feature name (or --clear)");
    usage();
    process.exit(2);
  }
  return out;
}

function delegate(scriptName, argv) {
  const script = path.join(ROOT, "scripts", scriptName);
  const result = spawnSync(process.execPath, [script, ...argv], { stdio: "inherit" });
  process.exit(result.status == null ? 1 : result.status);
}

function main() {
  const [cmd, ...rest] = process.argv.slice(2);
  if (!cmd || cmd === "-h" || cmd === "--help") {
    usage();
    return;
  }
  if (cmd === "install") return install(rest);
  if (cmd === "report") return delegate("report-token-usage.js", rest);
  if (cmd === "save") return delegate("save-token-usage.js", rest);
  if (cmd === "set-context") return delegate("set-token-context.js", rest);
  if (cmd === "set-feature") return delegate("set-token-context.js", normalizeSetFeatureArgs(rest));
  if (cmd === "statusline") return delegate("statusline-token-usage.js", rest);
  if (cmd === "prices") {
    const script = path.join(ROOT, "scripts", "pull-prices.js");
    const result = spawnSync(process.execPath, [script, ...rest], { stdio: "inherit" });
    process.exit(result.status == null ? 1 : result.status);
  }
  console.error(`token-tracker: unknown command: ${cmd}`);
  usage();
  process.exit(2);
}

module.exports = { TARGETS, selectedTargets, renderTemplate };

if (require.main === module) main();
