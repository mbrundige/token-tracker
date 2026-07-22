//! Port of `bin/token-tracker.js`'s install-command logic and CLI usage text.
//!
//! Faithful port with two deliberate, unavoidable deviations from the JS
//! source, both stemming from the same root cause: this is a single
//! compiled binary, not a Node script invoked by a `node` interpreter.
//!
//! 1. **`installPathLauncher`**: JS copies `bin/token-tracker.js` (an
//!    interpretable script) into `~/.token-tracker/cli/bin/token-tracker.js`
//!    and writes a `~/.local/bin/token-tracker` bash launcher that runs
//!    `node '<cliJs>' "$@"`. There is no interpretable entry-point file to
//!    copy here — the equivalent artifact is the compiled binary itself.
//!    [`install_path_launcher`] copies the currently-running executable
//!    (`std::env::current_exe()`) to `~/.token-tracker/cli/bin/token-tracker`
//!    (no `.js` extension — it is a native binary) and writes a launcher
//!    that `exec`s it directly (no `node` prefix, since none is needed).
//! 2. **Deposited skill scripts**: the embedded `templates/*.md`/`*.toml`
//!    files (verbatim, unmodified — see `crate::install`'s use of
//!    `include_str!`) literally instruct the invoking AI agent to run
//!    `node {{SKILL_BIN}}/report-token-usage.js` etc. Those strings cannot
//!    be changed (the templates are fixed content this port must preserve
//!    byte-for-byte), so for the resulting skill instructions to keep
//!    working, [`install_skill`]/[`install_path_launcher`] still deposit the
//!    real `scripts/*.js` files (embedded at compile time via
//!    `include_str!`, mirroring how `templates/*` are embedded) alongside
//!    the compiled binary. Running `token-tracker <subcommand>` directly
//!    uses the compiled Rust implementation; the deposited `.js` files exist
//!    only so agent-invoked `node .../*.js` commands (as instructed by the
//!    unmodified skill text) keep working for hosts that have Node
//!    installed.
//!
//! Everything else (config/prices seeding, target selection, gemini command
//! generation, statusLine patching, `{{VAR}}` template substitution,
//! `set-feature` argv normalization) mirrors the JS source field-for-field.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::paths;
use crate::pull_prices;

// ---------------------------------------------------------------------------
// Embedded scripts (JS `SCRIPT_FILES`) and templates (JS `templates/*`)
// ---------------------------------------------------------------------------

/// JS: `SCRIPT_FILES`. Order matches the original array (used only for
/// iteration order in output/copy loops; does not affect correctness).
const SCRIPT_FILES: &[(&str, &str)] = &[
    ("save-token-usage.js", include_str!("../../scripts/save-token-usage.js")),
    ("set-token-context.js", include_str!("../../scripts/set-token-context.js")),
    (
        "statusline-token-usage.js",
        include_str!("../../scripts/statusline-token-usage.js"),
    ),
    ("report-token-usage.js", include_str!("../../scripts/report-token-usage.js")),
    ("pricing.js", include_str!("../../scripts/pricing.js")),
    ("pull-prices.js", include_str!("../../scripts/pull-prices.js")),
    ("paths.js", include_str!("../../scripts/paths.js")),
    ("ansi.js", include_str!("../../scripts/ansi.js")),
];

const TEMPLATE_SKILL: &str = include_str!("../../templates/SKILL.md");
const TEMPLATE_GEMINI_CMD: &str = include_str!("../../templates/gemini-command.toml");
const TEMPLATE_GEMINI_SET_FEATURE: &str = include_str!("../../templates/gemini-set-feature.toml");
const TEMPLATE_SET_FEATURE: &str = include_str!("../../templates/set-feature.md");
const TEMPLATE_PRICES: &str = include_str!("../../templates/prices.json");

/// JS: `fs.readdirSync(path.join(ROOT, "templates"))` — all files under
/// `templates/`, copied verbatim into the PATH-launcher's `templates/` dir.
const TEMPLATE_FILES: &[(&str, &str)] = &[
    ("SKILL.md", TEMPLATE_SKILL),
    ("gemini-command.toml", TEMPLATE_GEMINI_CMD),
    ("gemini-set-feature.toml", TEMPLATE_GEMINI_SET_FEATURE),
    ("set-feature.md", TEMPLATE_SET_FEATURE),
    ("prices.json", TEMPLATE_PRICES),
];

// ---------------------------------------------------------------------------
// JS: `usage()`
// ---------------------------------------------------------------------------

/// JS: `usage()`'s template literal, verbatim.
pub const USAGE: &str = r#"Usage:
  npx @mbrundige/token-tracker install [targets...] [--statusline|--no-statusline]
  npx @mbrundige/token-tracker report
  npx @mbrundige/token-tracker save --summary "..." [--project NAME] [--feature NAME]
      [--model NAME] [--prompt-tokens N] [--completion-tokens N] [--total-tokens N]
      [--json '...'] [--source TEXT] [--metadata-json '...']
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
"#;

pub fn print_usage() {
    println!("{}", USAGE);
}

// ---------------------------------------------------------------------------
// JS: `DEFAULT_CONFIG`
// ---------------------------------------------------------------------------

fn default_config() -> Value {
    serde_json::json!({
        "default_project": null,
        "default_feature": null,
        "projects": {},
        "features": {},
        "statusline": {
            "enabled": true,
            "show_label": true,
            "show_project": true,
            "show_feature": true,
            "show_model": true,
            "show_context": true,
            "show_tokens": true,
            "show_cost": true,
        },
        "prices": {
            "auto_pull": true,
            "auto_pull_interval_hours": 1,
            "source": "openrouter",
        },
    })
}

// ---------------------------------------------------------------------------
// JS: `TARGETS`
// ---------------------------------------------------------------------------

pub struct Target {
    pub flag: &'static str,
    pub skill_dir: &'static str,
    pub label: &'static str,
    /// Mirrors JS `TARGETS.cursor.statusline` — present in the JS source
    /// object literal but, there as here, never actually read (JS
    /// determines statusLine wiring via `keys.includes("cursor")`, not this
    /// field). Kept for field-for-field fidelity with the JS `TARGETS` shape.
    #[allow(dead_code)]
    pub statusline: bool,
    pub gemini_command: bool,
    pub slash_command_dir: Option<&'static str>,
}

/// JS: `TARGETS`. Order matches `Object.keys(TARGETS)` insertion order
/// (relied on by `--all`'s iteration order).
pub const TARGETS: &[(&str, Target)] = &[
    (
        "cursor",
        Target {
            flag: "--cursor",
            skill_dir: ".cursor/skills",
            label: "Cursor",
            statusline: true,
            gemini_command: false,
            slash_command_dir: Some(".cursor/commands"),
        },
    ),
    (
        "claude",
        Target {
            flag: "--claude",
            skill_dir: ".claude/skills",
            label: "Claude Code",
            statusline: false,
            gemini_command: false,
            slash_command_dir: Some(".claude/commands"),
        },
    ),
    (
        "gemini",
        Target {
            flag: "--gemini",
            skill_dir: ".gemini/skills",
            label: "Gemini CLI",
            statusline: false,
            gemini_command: true,
            slash_command_dir: None,
        },
    ),
    (
        "codex",
        Target {
            flag: "--codex",
            skill_dir: ".codex/skills",
            label: "Codex CLI",
            statusline: false,
            gemini_command: false,
            slash_command_dir: None,
        },
    ),
    (
        "agents",
        Target {
            flag: "--agents",
            skill_dir: ".agents/skills",
            label: "Agent Skills (~/.agents)",
            statusline: false,
            gemini_command: false,
            slash_command_dir: None,
        },
    ),
    (
        "continue",
        Target {
            flag: "--continue",
            skill_dir: ".continue/skills",
            label: "Continue CLI",
            statusline: false,
            gemini_command: false,
            slash_command_dir: None,
        },
    ),
];

fn target_for(key: &str) -> Option<&'static Target> {
    TARGETS.iter().find(|(k, _)| *k == key).map(|(_, t)| t)
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// JS: `shellSingleQuote(value)`.
pub fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
        .with_context(|| format!("failed to chmod {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// JS: `renderTemplate(template, vars)`. Substitutes `{{key}}` tokens using
/// the same `\{\{(\w+)\}\}` matching the JS regex performs (word chars
/// only), leaving unrecognized placeholders untouched.
pub fn render_template(template: &str, vars: &[(&str, &str)]) -> String {
    let chars: Vec<char> = template.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(template.len());
    let mut i = 0usize;
    while i < n {
        if chars[i] == '{' && i + 1 < n && chars[i + 1] == '{' {
            let key_start = i + 2;
            let mut j = key_start;
            while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if j > key_start && j + 1 < n && chars[j] == '}' && chars[j + 1] == '}' {
                let key: String = chars[key_start..j].iter().collect();
                match vars.iter().find(|(k, _)| *k == key) {
                    Some((_, v)) => out.push_str(v),
                    None => {
                        out.push_str("{{");
                        out.push_str(&key);
                        out.push_str("}}");
                    }
                }
                i = j + 2;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Lexically resolve a path against cwd (mirrors `path.resolve` for the
/// non-existent-path case), without touching the filesystem.
fn resolve_path_lexical(p: &Path) -> PathBuf {
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    };
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::ParentDir => {
                if !result.pop() {
                    result.push(component.as_os_str());
                }
            }
            std::path::Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    result
}

/// JS: `pathHasLocalBin(localBin)`.
pub fn path_has_local_bin(local_bin: &Path) -> bool {
    let path_env = env::var("PATH").unwrap_or_default();
    let resolved = resolve_path_lexical(local_bin);
    env::split_paths(&path_env).any(|p| resolve_path_lexical(&p) == resolved)
}

// ---------------------------------------------------------------------------
// JS: `ensureConfig`, `ensurePrices`
// ---------------------------------------------------------------------------

pub struct EnsureConfigResult {
    pub created: bool,
    pub config_path: PathBuf,
    pub migration: paths::MigrationResult,
}

/// JS: `ensureConfig()`.
pub fn ensure_config() -> Result<EnsureConfigResult> {
    let ensured = paths::ensure_data_dir(Some(paths::resolve_data_dir()))?;
    let config_path = ensured.data_dir.join("config.json");

    if !config_path.exists() {
        let body = serde_json::to_string_pretty(&default_config())
            .context("failed to serialize default config")?;
        fs::write(&config_path, format!("{}\n", body))
            .with_context(|| format!("failed to write {}", config_path.display()))?;
        return Ok(EnsureConfigResult {
            created: true,
            config_path,
            migration: ensured.migration,
        });
    }

    // Upgrade older configs so auto price pull stays on by default. JS
    // silently leaves the file alone if it's unreadable/unparsable/not an
    // object; mirrored here by simply skipping the upgrade block on any of
    // those conditions.
    if let Ok(raw) = fs::read_to_string(&config_path) {
        if let Ok(Value::Object(mut existing)) = serde_json::from_str::<Value>(&raw) {
            let mut changed = false;
            let needs_prices_obj = !matches!(existing.get("prices"), Some(Value::Object(_)));
            if needs_prices_obj {
                existing.insert("prices".to_string(), default_config()["prices"].clone());
                changed = true;
            } else if let Some(Value::Object(prices)) = existing.get_mut("prices") {
                if !prices.contains_key("auto_pull") {
                    prices.insert("auto_pull".to_string(), Value::Bool(true));
                    changed = true;
                }
                if !prices.contains_key("auto_pull_interval_hours") {
                    prices.insert("auto_pull_interval_hours".to_string(), Value::from(1));
                    changed = true;
                }
                let has_source = matches!(prices.get("source"), Some(Value::String(s)) if !s.is_empty());
                if !has_source {
                    prices.insert("source".to_string(), Value::String("openrouter".to_string()));
                    changed = true;
                }
            }
            if changed {
                if let Ok(body) = serde_json::to_string_pretty(&Value::Object(existing)) {
                    let _ = fs::write(&config_path, format!("{}\n", body));
                }
            }
        }
    }

    Ok(EnsureConfigResult {
        created: false,
        config_path,
        migration: ensured.migration,
    })
}

pub struct EnsurePricesResult {
    pub created: bool,
    pub prices_path: PathBuf,
    /// Mirrors JS `ensurePrices()`'s returned `migration` field; `install()`
    /// only ever reads the migration info from `ensureConfig()`'s result, so
    /// this one goes unread there too — kept for fidelity with the JS
    /// return shape.
    #[allow(dead_code)]
    pub migration: paths::MigrationResult,
}

/// JS: `ensurePrices()`.
pub fn ensure_prices() -> Result<EnsurePricesResult> {
    let ensured = paths::ensure_data_dir(Some(paths::resolve_data_dir()))?;
    let prices_path = ensured.data_dir.join("prices.json");
    if !prices_path.exists() {
        fs::write(&prices_path, TEMPLATE_PRICES)
            .with_context(|| format!("failed to write {}", prices_path.display()))?;
        return Ok(EnsurePricesResult {
            created: true,
            prices_path,
            migration: ensured.migration,
        });
    }
    Ok(EnsurePricesResult {
        created: false,
        prices_path,
        migration: ensured.migration,
    })
}

// ---------------------------------------------------------------------------
// JS: `installPathLauncher`
// ---------------------------------------------------------------------------

pub struct PathLauncherResult {
    pub launcher: PathBuf,
    pub cli_bin: PathBuf,
    #[allow(dead_code)]
    pub cli_root: PathBuf,
    pub local_bin: PathBuf,
}

/// JS: `installPathLauncher()`. See the module doc comment for the
/// compiled-binary-vs-interpreted-script deviation.
pub fn install_path_launcher() -> Result<PathLauncherResult> {
    let data_dir = paths::resolve_data_dir();
    let cli_root = data_dir.join("cli");
    let cli_bin_dir = cli_root.join("bin");
    let cli_scripts_dir = cli_root.join("scripts");
    let cli_templates_dir = cli_root.join("templates");
    fs::create_dir_all(&cli_bin_dir)
        .with_context(|| format!("failed to create {}", cli_bin_dir.display()))?;
    fs::create_dir_all(&cli_scripts_dir)
        .with_context(|| format!("failed to create {}", cli_scripts_dir.display()))?;
    fs::create_dir_all(&cli_templates_dir)
        .with_context(|| format!("failed to create {}", cli_templates_dir.display()))?;

    let cli_bin = cli_bin_dir.join("token-tracker");
    let current_exe = env::current_exe().context("failed to resolve current executable")?;
    fs::copy(&current_exe, &cli_bin)
        .with_context(|| format!("failed to copy binary to {}", cli_bin.display()))?;
    set_executable(&cli_bin)?;

    for (name, contents) in SCRIPT_FILES {
        let to = cli_scripts_dir.join(name);
        fs::write(&to, contents).with_context(|| format!("failed to write {}", to.display()))?;
        set_executable(&to)?;
    }
    for (name, contents) in TEMPLATE_FILES {
        let to = cli_templates_dir.join(name);
        fs::write(&to, contents).with_context(|| format!("failed to write {}", to.display()))?;
    }

    let local_bin = home_dir().join(".local").join("bin");
    fs::create_dir_all(&local_bin)
        .with_context(|| format!("failed to create {}", local_bin.display()))?;
    let launcher = local_bin.join("token-tracker");
    let body = format!(
        "#!/usr/bin/env bash\nset -euo pipefail\nexec {} \"$@\"\n",
        shell_single_quote(&cli_bin.to_string_lossy())
    );
    fs::write(&launcher, body)
        .with_context(|| format!("failed to write {}", launcher.display()))?;
    set_executable(&launcher)?;

    Ok(PathLauncherResult {
        launcher,
        cli_bin,
        cli_root,
        local_bin,
    })
}

// ---------------------------------------------------------------------------
// JS: `installSkill`, `installMarkdownSetFeatureCommand`, `installGeminiCommands`
// ---------------------------------------------------------------------------

pub struct InstalledSkill {
    pub dest: PathBuf,
    pub label: &'static str,
    pub gemini_commands: Option<Vec<PathBuf>>,
    pub set_feature_command: Option<PathBuf>,
}

/// JS: `installSkill(targetKey)`.
pub fn install_skill(target_key: &str) -> Result<InstalledSkill> {
    let target = target_for(target_key)
        .ok_or_else(|| anyhow::anyhow!("unknown install target: {}", target_key))?;
    let dest = home_dir().join(target.skill_dir).join("token-tracker");
    let scripts_dest = dest.join("scripts");
    // JS: `path.join("~", target.skillDir, "token-tracker", "scripts")` — a
    // literal, unexpanded "~/..." string embedded into the rendered
    // SKILL.md text, not an actual filesystem path.
    let skill_bin = format!("~/{}/token-tracker/scripts", target.skill_dir);

    fs::create_dir_all(&dest).with_context(|| format!("failed to create {}", dest.display()))?;
    let skill_body = render_template(TEMPLATE_SKILL, &[("SKILL_BIN", skill_bin.as_str())]);
    fs::write(dest.join("SKILL.md"), skill_body)
        .with_context(|| format!("failed to write {}", dest.join("SKILL.md").display()))?;

    let _ = fs::remove_dir_all(&scripts_dest);
    fs::create_dir_all(&scripts_dest)
        .with_context(|| format!("failed to create {}", scripts_dest.display()))?;
    for (name, contents) in SCRIPT_FILES {
        let to = scripts_dest.join(name);
        fs::write(&to, contents).with_context(|| format!("failed to write {}", to.display()))?;
        set_executable(&to)?;
    }

    let gemini_commands = if target.gemini_command {
        Some(install_gemini_commands(&dest)?)
    } else {
        None
    };
    let set_feature_command = match target.slash_command_dir {
        Some(slash_dir) => Some(install_markdown_set_feature_command(slash_dir, &skill_bin)?),
        None => None,
    };

    Ok(InstalledSkill {
        dest,
        label: target.label,
        gemini_commands,
        set_feature_command,
    })
}

/// JS: `installMarkdownSetFeatureCommand(commandsDirRel, skillBin)`.
fn install_markdown_set_feature_command(commands_dir_rel: &str, skill_bin: &str) -> Result<PathBuf> {
    let commands_dir = home_dir().join(commands_dir_rel);
    fs::create_dir_all(&commands_dir)
        .with_context(|| format!("failed to create {}", commands_dir.display()))?;
    let body = render_template(TEMPLATE_SET_FEATURE, &[("SKILL_BIN", skill_bin)]);
    let dest = commands_dir.join("set-feature.md");
    fs::write(&dest, body).with_context(|| format!("failed to write {}", dest.display()))?;
    Ok(dest)
}

/// JS: `installGeminiCommands(skillRoot)`.
fn install_gemini_commands(skill_root: &Path) -> Result<Vec<PathBuf>> {
    let commands_dir = home_dir().join(".gemini").join("commands");
    fs::create_dir_all(&commands_dir)
        .with_context(|| format!("failed to create {}", commands_dir.display()))?;
    let report_script = skill_root.join("scripts").join("report-token-usage.js");
    let set_script = skill_root.join("scripts").join("set-token-context.js");
    let skill_root_str = skill_root.to_string_lossy().into_owned();
    let report_script_str = report_script.to_string_lossy().into_owned();
    let set_script_str = set_script.to_string_lossy().into_owned();

    let report_body = render_template(
        TEMPLATE_GEMINI_CMD,
        &[
            ("SKILL_ROOT", skill_root_str.as_str()),
            ("REPORT_SCRIPT", report_script_str.as_str()),
        ],
    );
    let set_body = render_template(
        TEMPLATE_GEMINI_SET_FEATURE,
        &[
            ("SKILL_ROOT", skill_root_str.as_str()),
            ("SET_SCRIPT", set_script_str.as_str()),
        ],
    );
    let report_dest = commands_dir.join("token-tracker.toml");
    let set_dest = commands_dir.join("set-feature.toml");
    fs::write(&report_dest, report_body)
        .with_context(|| format!("failed to write {}", report_dest.display()))?;
    fs::write(&set_dest, set_body)
        .with_context(|| format!("failed to write {}", set_dest.display()))?;
    Ok(vec![report_dest, set_dest])
}

// ---------------------------------------------------------------------------
// JS: `patchCliStatusLine`
// ---------------------------------------------------------------------------

/// JS: `patchCliStatusLine(scriptPath)`.
fn patch_cli_statusline(script_path: &Path) -> Result<bool> {
    let cli_config = home_dir().join(".cursor").join("cli-config.json");
    if !cli_config.exists() {
        println!("Skipped statusLine: {} not found", cli_config.display());
        return Ok(false);
    }
    let raw = fs::read_to_string(&cli_config)
        .with_context(|| format!("failed to read {}", cli_config.display()))?;
    let mut config: Value = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {} as JSON", cli_config.display()))?;
    let command = format!("node {}", shell_single_quote(&script_path.to_string_lossy()));
    let status_line = serde_json::json!({
        "type": "command",
        "command": command,
        "padding": 2,
        "timeoutMs": 1000,
    });
    if let Some(obj) = config.as_object_mut() {
        obj.insert("statusLine".to_string(), status_line);
    }
    let body =
        serde_json::to_string_pretty(&config).context("failed to serialize cli-config.json")?;
    fs::write(&cli_config, format!("{}\n", body))
        .with_context(|| format!("failed to write {}", cli_config.display()))?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// JS: `selectedTargets`
// ---------------------------------------------------------------------------

/// JS: `selectedTargets(argv)`.
pub fn selected_targets(argv: &[String]) -> Vec<&'static str> {
    let want_all = argv.iter().any(|a| a == "--all");
    if want_all {
        return TARGETS.iter().map(|(k, _)| *k).collect();
    }
    let explicit: Vec<&'static str> = TARGETS
        .iter()
        .filter(|(_, t)| argv.iter().any(|a| a == t.flag))
        .map(|(k, _)| *k)
        .collect();
    if !explicit.is_empty() {
        return explicit;
    }
    vec!["cursor"]
}

// ---------------------------------------------------------------------------
// JS: `install(argv)`
// ---------------------------------------------------------------------------

/// JS: `install(argv)`. Returns the process exit code (`2` on a validation
/// error, matching JS's `process.exit(2)`; `0` on success). Unexpected I/O
/// failures propagate via `Err` for the caller (`main.rs`) to report and
/// exit `1` with, matching an uncaught-exception Node crash.
pub fn install(argv: &[String]) -> Result<i32> {
    let mut allowed: Vec<&str> = vec!["--all", "--statusline", "--no-statusline"];
    for (_, t) in TARGETS {
        allowed.push(t.flag);
    }
    let unknown: Vec<&str> = argv
        .iter()
        .map(|a| a.as_str())
        .filter(|a| !allowed.contains(a))
        .collect();
    if !unknown.is_empty() {
        eprintln!("token-tracker: unknown install option(s): {}", unknown.join(", "));
        print_usage();
        return Ok(2);
    }

    let no_statusline = argv.iter().any(|a| a == "--no-statusline");
    let force_statusline = argv.iter().any(|a| a == "--statusline");
    let keys = selected_targets(argv);

    let mut installed = Vec::with_capacity(keys.len());
    for key in &keys {
        installed.push(install_skill(key)?);
    }

    let ensured_config = ensure_config()?;
    let ensured_prices = ensure_prices()?;
    let path_launcher = install_path_launcher()?;
    let price_pull = pull_prices::schedule_price_pull_if_stale(
        &ensured_prices.prices_path,
        "openrouter",
        60 * 60 * 1000,
    );

    let sep = std::path::MAIN_SEPARATOR;
    let cursor_marker = format!("{}.cursor{}", sep, sep);
    let cursor_install = installed
        .iter()
        .find(|item| item.dest.to_string_lossy().contains(cursor_marker.as_str()));

    let wants_statusline = force_statusline || (!no_statusline && keys.contains(&"cursor"));

    let mut statusline_path: Option<PathBuf> = None;
    let mut statusline_wired = false;
    if wants_statusline {
        if let Some(cursor_item) = cursor_install {
            let sp = cursor_item
                .dest
                .join("scripts")
                .join("statusline-token-usage.js");
            statusline_wired = patch_cli_statusline(&sp)?;
            statusline_path = Some(sp);
        } else {
            println!("Skipped statusLine: install --cursor (or --all) to wire Cursor CLI statusLine.");
        }
    }

    let mut notes: Vec<String> = Vec::new();
    if ensured_config.migration.migrated {
        let copied = ensured_config.migration.copied.clone().unwrap_or_default();
        let files = if copied.is_empty() {
            "files".to_string()
        } else {
            copied.join(", ")
        };
        notes.push(format!(
            "Migrated shared data from {} to {} ({}).",
            ensured_config.migration.legacy.display(),
            ensured_config.migration.data_dir.display(),
            files
        ));
    }
    if statusline_wired {
        notes.push("Restart Cursor CLI to pick up statusLine changes.".to_string());
    }
    if ensured_prices.created {
        notes.push(format!(
            "Seeded default price table at {}.",
            ensured_prices.prices_path.display()
        ));
    }
    if price_pull.scheduled {
        notes.push(
            "Fetching latest model prices in the background (also auto-refreshes hourly on report/statusline)."
                .to_string(),
        );
    }
    notes.push(format!(
        "CLI available as {} (run: token-tracker report).",
        path_launcher.launcher.display()
    ));
    if !path_has_local_bin(&path_launcher.local_bin) {
        notes.push(format!(
            "Add {} to your PATH if `token-tracker` is not found.",
            path_launcher.local_bin.display()
        ));
    }
    if keys.contains(&"gemini") {
        notes.push("In Gemini CLI run /commands reload and /skills reload.".to_string());
    }
    if keys.contains(&"cursor") || keys.contains(&"claude") {
        notes.push("Cursor/Claude: /set-feature is available after install (reload chat if needed).".to_string());
    }
    if keys.contains(&"codex") || keys.contains(&"agents") || keys.contains(&"continue") {
        notes.push("Restart Codex/Continue (or reload skills) if the new skill does not appear.".to_string());
    }

    let installed_json: Vec<Value> = installed
        .iter()
        .map(|item| {
            let mut m = Map::new();
            m.insert("label".to_string(), Value::String(item.label.to_string()));
            m.insert("path".to_string(), Value::String(item.dest.display().to_string()));
            if let Some(gc) = &item.gemini_commands {
                m.insert(
                    "gemini_commands".to_string(),
                    Value::Array(
                        gc.iter()
                            .map(|p| Value::String(p.display().to_string()))
                            .collect(),
                    ),
                );
            }
            if let Some(sfc) = &item.set_feature_command {
                m.insert(
                    "set_feature_command".to_string(),
                    Value::String(sfc.display().to_string()),
                );
            }
            Value::Object(m)
        })
        .collect();

    let mut out = Map::new();
    out.insert("installed".to_string(), Value::Array(installed_json));
    out.insert(
        "data_dir".to_string(),
        Value::String(paths::resolve_data_dir().display().to_string()),
    );
    out.insert(
        "config".to_string(),
        Value::String(ensured_config.config_path.display().to_string()),
    );
    out.insert("config_created".to_string(), Value::Bool(ensured_config.created));
    out.insert(
        "prices".to_string(),
        Value::String(ensured_prices.prices_path.display().to_string()),
    );
    out.insert("prices_created".to_string(), Value::Bool(ensured_prices.created));
    out.insert(
        "prices_pull_scheduled".to_string(),
        Value::Bool(price_pull.scheduled),
    );
    out.insert(
        "cli".to_string(),
        Value::String(path_launcher.cli_bin.display().to_string()),
    );
    out.insert(
        "cli_launcher".to_string(),
        Value::String(path_launcher.launcher.display().to_string()),
    );
    out.insert(
        "migrated_from_cursor".to_string(),
        Value::Bool(ensured_config.migration.migrated),
    );
    out.insert(
        "statusline".to_string(),
        statusline_path
            .map(|p| Value::String(p.display().to_string()))
            .unwrap_or(Value::Null),
    );
    out.insert("statusline_wired".to_string(), Value::Bool(statusline_wired));
    if !notes.is_empty() {
        out.insert("note".to_string(), Value::String(notes.join(" ")));
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&Value::Object(out)).context("failed to serialize install result")?
    );
    Ok(0)
}

// ---------------------------------------------------------------------------
// JS: `normalizeSetFeatureArgs`
// ---------------------------------------------------------------------------

/// JS: `normalizeSetFeatureArgs(argv)`. On a validation error, prints to
/// stderr + usage (matching JS) and returns `Err(exit_code)` instead of
/// calling `process.exit` directly, so `main.rs` is the single place that
/// exits the process.
///
/// Deviation: JS pushes `argv[++i]` unconditionally after `--feature`/
/// `--workspace`/`--project`, which is `undefined` (and later crashes
/// `path.resolve`/similar deep in `main()`) if the flag is the very last
/// argv token. Here a trailing flag with no value simply yields no
/// following token, which `set_context::parse_args` already treats as "no
/// value provided" gracefully — the same deliberate deviation `set_context.rs`
/// documents for its own `next()`-style parsing.
pub fn normalize_set_feature_args(argv: &[String]) -> std::result::Result<Vec<String>, i32> {
    let mut out: Vec<String> = Vec::new();
    let mut saw_feature = false;
    let mut i = 0usize;
    while i < argv.len() {
        let a = argv[i].as_str();
        if a == "--clear" || a == "--clear-feature" {
            out.push("--clear-feature".to_string());
            i += 1;
            continue;
        }
        if a == "--feature" {
            out.push("--feature".to_string());
            i += 1;
            if let Some(v) = argv.get(i) {
                out.push(v.clone());
            }
            saw_feature = true;
            i += 1;
            continue;
        }
        if a == "--workspace" {
            out.push("--workspace".to_string());
            i += 1;
            if let Some(v) = argv.get(i) {
                out.push(v.clone());
            }
            i += 1;
            continue;
        }
        if a == "--project" {
            out.push("--project".to_string());
            i += 1;
            if let Some(v) = argv.get(i) {
                out.push(v.clone());
            }
            i += 1;
            continue;
        }
        if a.starts_with('-') {
            eprintln!("token-tracker: unknown set-feature option: {}", a);
            print_usage();
            return Err(2);
        }
        if saw_feature {
            eprintln!("token-tracker: unexpected argument: {}", a);
            print_usage();
            return Err(2);
        }
        out.push("--feature".to_string());
        out.push(a.to_string());
        saw_feature = true;
        i += 1;
    }
    if !saw_feature && !out.iter().any(|s| s == "--clear-feature") {
        eprintln!("token-tracker: set-feature requires a feature name (or --clear)");
        print_usage();
        return Err(2);
    }
    Ok(out)
}
