# FLORIN — brand synthesis

## One idea
**Tokens are florins.** The unit of account for local AI spend is reminted from Florence’s *fiorino d’oro* — the trusted settlement coin of medieval Europe — for the terminal.

## Naming lock

| Layer | Name | Why |
| --- | --- | --- |
| **Product** (repo / npm / CLI) | `token-tracker` | Max’s package; don’t rename for lore |
| **Lore unit** | **florin** · ticker **`flr`** | Unit of account in UI / statusline (`flr · local`) |
| **Italian root** | *fiorino d’oro* | “little flower of gold” — purity + trust |
| **Lettermark** | **F** | *Fiorino* / Florin — not a random monogram |
| **Lily** | campaign / coin reverse | Historical civic stamp; secondary to F as icon |

**Not the product name:** Fiorino, Banco Fiorentino, Florin Ledger (those are lore flavor only unless Max wants a rename later).

## Historical spine (bake into copy, not cosplay)

- **1252 Florence** — first *fiorino d’oro*: ~3.5g near-pure gold; weight/fineness held for centuries → merchants trusted it as **settlement** money, not pocket change.
- **Faces** — Florentine **lily** (*fiore*) + **Saint John the Baptist** → civic brand of reliability.
- **Reach** — contracts across Europe named “florins of Florence”; name echoed later in gulden / forint.
- **Houses** — Bardi, Peruzzi, later **Medici**: branch banking, letters of credit, international credit — and early crises (e.g. Edward III default). Finance → Renaissance patronage.
- **Parallel to us** — florins cleared high-value obligations without hauling bullion; we clear **token spend** across agent hosts without a SaaS middleman. Local *libro*, one history.

## Lexicon (UX / future module flavor)

| Term | Meaning | Use here |
| --- | --- | --- |
| *Fiorino d’oro* | The coin | Lore, README story |
| *Fiore* / lily | Flower stamp | Campaign art, coin reverse |
| *Banco* / *banca* | Bank (from the bench) | Soft copy only |
| *Cambiatori* | Moneychangers | Host adapters / “exchange” of host reports → one ledger |
| *Libro di conto* | Account book | The JSONL under `~/.token-tracker/` |
| *Lettere di credito* | Letters of credit | Optional wink for “settle without moving gold” → settle without pasting invoices |

Keep Italian sparse in UI — one wink beats a museum label.

## Core marks (ship these)

| Mark | Role | Notes |
| --- | --- | --- |
| **F lettermark** | Icon / favicon / status chip | Serif medieval **F**; `#2B6CFF` chip window = silicon *saggio* (assay) |
| **token-tracker wordmark** | README / headers | Medieval-leaning readable serif |
| **F-coin** | Primary pictorial | Reeded florin; **F** obverse; lily optional reverse/campaign |

## Campaign imagery (Supreme drops — many logos, one world)

Same palette; never a second core wordmark:

- ASCII F disc (terminal mint)
- Charon / giant toll (fare / crossing — settlement drama)
- Insert-chute florin (spend as feed)
- Pacioli hand + chip (*libro* / double-entry craft)
- Lily + chip coin (historical face, campaign)
- `flr · local` statusline

## Palette

| Token | Hex | Use |
| --- | --- | --- |
| Ink / ground | `#0B0B0B` | Dark boards, UI chrome |
| Bone | `#EDEAE2` | Engraving ink |
| Chip | `#2B6CFF` | Sole accent — register / assay window |

## Type
- **Lettermark:** Custom serif F (simplify for ≤32px)
- **Wordmark:** Old-style serif, lowercase `token-tracker`
- **UI mono:** for `flr`, commands, statusline

## Taglines (README prose — not baked in PNG)

- Count every token.
- Local *libro di conto* for AI spend.
- A digital fiorino: one trusted unit across Cursor, Claude, Gemini, Codex…

## Avoid
Heat-grid · purple SaaS · shiny 3D crypto · Medici LARP as the skim · renaming npm to Fiorino without Max · fleur as sole icon · grim death as the brand · second accent
