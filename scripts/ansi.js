#!/usr/bin/env node
"use strict";

/**
 * Tiny ANSI helpers (no deps).
 * Colors when stdout is a TTY, or FORCE_COLOR is set.
 * Disabled when NO_COLOR is set (https://no-color.org/).
 */

function wantsColor(stream = process.stdout) {
  // Match Node's precedence: FORCE_COLOR wins over NO_COLOR.
  if (process.env.FORCE_COLOR === "0") return false;
  if (process.env.FORCE_COLOR != null && process.env.FORCE_COLOR !== "") return true;
  if (process.env.NO_COLOR != null && process.env.NO_COLOR !== "") return false;
  return Boolean(stream && stream.isTTY);
}

const RESET = "\x1b[0m";

function wrap(enabled, code, text) {
  if (!enabled || text == null || text === "") return String(text ?? "");
  return `\x1b[${code}m${text}${RESET}`;
}

function createAnsi(enabled = wantsColor()) {
  return {
    enabled,
    bold: (t) => wrap(enabled, "1", t),
    dim: (t) => wrap(enabled, "2", t),
    cyan: (t) => wrap(enabled, "36", t),
    green: (t) => wrap(enabled, "32", t),
    yellow: (t) => wrap(enabled, "33", t),
    blue: (t) => wrap(enabled, "34", t),
    magenta: (t) => wrap(enabled, "35", t),
    // Heat intensity: grey → amber → bright yellow
    heat: (level, ch) => {
      if (!enabled) return ch;
      const codes = ["2;37", "33", "33;1", "93", "93;1"]; // dim white, yellow, bold yellow, bright yellow
      const code = codes[Math.max(0, Math.min(level, codes.length - 1))];
      return `\x1b[${code}m${ch}${RESET}`;
    },
  };
}

module.exports = { wantsColor, createAnsi };
