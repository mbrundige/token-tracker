## Learned User Preferences
- Not very technical: do not ask him to run bunches of diagnostic commands or gather more inputs — pick sensible defaults/variables and run them.
- Prefers performance work framed as a Rust rewrite; for this project, preferred direction is Hybrid — Node shell, Rust engine (not a full-app Rust rewrite).
- Code deep-dives belong in-repo exploration/producer reports; do not route “understand this feature” through `/research-deep` (that skill is vault research via Perplexity/X, not codebase analysis).
- Visual first: boards, ASCII sketches, and flowcharts over prose specs; keep reading at about sixth-grade level.
- Brand concepts should be radical (old-school terminal + modern elegance / arcade × history) — reject generic AI-slop logo concepts and quiet SaaS heat-grid marks.
- Brand direction locked to Florin: medieval **F** lettermark + serif `token-tracker` wordmark; keep ASCII coin + Charon/troll as campaign drops (Supreme-style multi-logo system).
- Logo/brand pipeline: Designpowers crits + BrandKit/svg-logo-designer (Higgsfield when generating); sync concepts under `docs/concepts` → LLM judge → he picks before shipping to `docs/logos/`.
- Brand voice: Norm Macdonald deadpan + Conner O’Malley brand-love (Stand Up Solutions energy); follow `voice.md` — `README.md` may use GODMODE keynote; invent florin jokes, never paste SUS/transcript lines; quiet twin `README.quiet.md` stays Norm.
- GitHub landing: keep Max’s screenshots/host marks; swap logos/branding only; aim for medieval/Renaissance epic craft — no badge spam, goofy charts, or SaaS flare.

## Learned Workspace Facts
- token-tracker is local multi-host token/cost tracking (Cursor, Claude Code, Gemini CLI, Codex, Continue, `~/.agents/skills`); shared ledger/config/prices live under `~/.token-tracker/` (override with `TOKEN_TRACKER_HOME`).
- Product default stays zero-dep Node (`npx @mbrundige/token-tracker`); optional report hot path is Rust crate `rust/token-tracker-fast/` via `report --fast` (binary often under `~/.token-tracker/bin/token-tracker-fast`).
- Install targets host skill dirs plus shared CLI under `~/.token-tracker/cli/` and launcher `~/.local/bin/token-tracker`; feature-scoped Cursor `statusLine` is optional host wiring.
- Florin brand kit lives under `docs/concepts/kit/` (`FLORIN.md`, `LORE.md`, marks/boards); shipped README assets are `docs/logos/token-tracker.png` + `docs/logos/icon.png`; Max originals archived at `docs/concepts/_archive/max-originals/`; Max demos/host marks kept (`docs/screenshots/`, `docs/logos/hosts.png`).
- Brand style lock: cyber-classical tech-etching · `#0B0B0B` · `#EDEAE2` · `#2B6CFF` chip only; lore unit = florin / ticker `flr` (product name stays `token-tracker`).
- README hero is Direction A 16:9 mural (`docs/logos/token-tracker.png`): palm F-coin is the in-image logo; quiet engraved `florin` + subtle “Count every token.”; no bright corner ASCII lockup; product name stays markdown under the mural.
- Brand voice intent lives in `voice.md` (GODMODE channel for `README.md` only; Norm default elsewhere); quieter twin is `README.quiet.md`.
- Higgsfield MCP (`https://mcp.higgsfield.ai/mcp`, Cursor server `user-higgsfield`) is installed for logo/image generation alongside Designpowers.
