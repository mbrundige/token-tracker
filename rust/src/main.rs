//! Rust port of `bin/token-tracker.js` — the `token-tracker` CLI entry point.
//!
//! Mirrors the JS dispatch table (`install`, `report`, `save`, `set-context`,
//! `set-feature`, `statusline`, `prices`, `-h`/`--help`, unknown command)
//! exit-code-for-exit-code. See `install.rs`'s module doc comment for the
//! two deliberate deviations forced by this being a compiled binary rather
//! than a `node`-interpreted script.

mod ansi;
mod install;
mod paths;
mod pricing;
mod pull_prices;
mod report;
mod save;
mod set_context;
mod statusline;

use std::process;

fn main() {
    // JS: `const [cmd, ...rest] = process.argv.slice(2);`
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = argv.first().cloned();
    let rest: Vec<String> = if argv.is_empty() {
        Vec::new()
    } else {
        argv[1..].to_vec()
    };

    match cmd.as_deref() {
        // JS: `if (!cmd || cmd === "-h" || cmd === "--help") { usage(); return; }`
        None | Some("-h") | Some("--help") => {
            install::print_usage();
        }
        // JS: `if (cmd === "install") return install(rest);`
        Some("install") => match install::install(&rest) {
            Ok(code) => process::exit(code),
            Err(err) => {
                eprintln!("token-tracker: {}", err);
                process::exit(1);
            }
        },
        // JS: `if (cmd === "report") return delegate("report-token-usage.js", rest);`
        Some("report") => {
            if let Err(err) = report::run() {
                eprintln!("token-tracker: {}", err);
                process::exit(1);
            }
        }
        // JS: `if (cmd === "save") return delegate("save-token-usage.js", rest);`
        Some("save") => {
            process::exit(save::run(&rest));
        }
        // JS: `if (cmd === "set-context") return delegate("set-token-context.js", rest);`
        Some("set-context") => match set_context::run(&rest) {
            Ok(code) => process::exit(code),
            Err(err) => {
                eprintln!("token-tracker: {}", err);
                process::exit(1);
            }
        },
        // JS: `if (cmd === "set-feature") return delegate("set-token-context.js", normalizeSetFeatureArgs(rest));`
        Some("set-feature") => match install::normalize_set_feature_args(&rest) {
            Ok(mapped) => match set_context::run(&mapped) {
                Ok(code) => process::exit(code),
                Err(err) => {
                    eprintln!("token-tracker: {}", err);
                    process::exit(1);
                }
            },
            Err(code) => process::exit(code),
        },
        // JS: `if (cmd === "statusline") return delegate("statusline-token-usage.js", rest);`
        Some("statusline") => {
            if let Err(err) = statusline::run() {
                eprintln!("token-tracker: {}", err);
                process::exit(1);
            }
        }
        // JS: `if (cmd === "prices") { ... spawnSync(pull-prices.js, rest) ... }`
        Some("prices") => {
            process::exit(pull_prices::run(&rest));
        }
        // JS: `console.error(...); usage(); process.exit(2);`
        Some(other) => {
            eprintln!("token-tracker: unknown command: {}", other);
            install::print_usage();
            process::exit(2);
        }
    }
}
