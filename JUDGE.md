# JUDGE — token-tracker concept finalists

Sixth-grade flow:

```
Look at 5 folders
    ↓
Score each 1–5 on 6 checks
    ↓
Add scores
    ↓
Rank + one sentence each
    ↓
Recommend a winner (Stephen still picks)
```

## Product (fixed)

`@mbrundige/token-tracker` — local AI token ledger across Cursor/Claude/Gemini/Codex/Continue. Data in `~/.token-tracker/`. GitHub landing = README hero PNG + icon.

## Craft bar (fixed)

Refs in `refs/`:

1. Stipple hand + one blue EPROM window
2. Doré blue dither / myth scale
3. Hard B/W Nous cut

## Candidates (read `concept.md` + primary hero in each)

| # | Path | Primary file |
| --- | --- | --- |
| 01 | `candidates/01-florin-coin/` | `hero-icon.png` |
| 02 | `candidates/02-fleur-chip/` | `hero.png` |
| 03 | `candidates/03-ascii-f/` | `hero.png` |
| 04 | `candidates/04-insert-coin/` | `hero.png` |
| 05 | `candidates/05-charon-toll/` | `hero.png` |

Alts are optional context — score the **primary** unless an alt clearly ships better as the hero.

## Rubric (1–5 each · max 30)

| Criterion | 1 | 5 |
| --- | --- | --- |
| **Product skim** | Cosplay / poster / game only | 3s: local AI token spend / ledger |
| **Icon ≤32px** | Mud / illegible | Silhouette still reads |
| **Craft bar** | Soft SaaS / multi-accent / purple | Stipple or hard B/W · one accent · refs match |
| **README hero** | Weak at top of GitHub README | Strong wordmark + mark lockup |
| **Max-safe PR** | Grim death / crypto-bro / IP risk | Friendly OSS PR Max can merge |
| **Ownable** | Generic CLI heat-map twin | Distinct mark you remember |

**Kill overrides (cap that criterion at 2):** purple SaaS glow · baked tagline · shiny 3D crypto coin · boat/ferryman as sole mark · heat-grid DNA.

## Panel roles

| Role | Focus |
| --- | --- |
| design-critic | Intent, craft, ownability, brand argument |
| accessibility-reviewer | Contrast, small-size, text-in-image, skim |
| heuristic-evaluator | Recognition, match to mental model “token spend tool” |

## Output format (each reviewer)

```markdown
## Scores

| ID | Product | Icon | Craft | Hero | Max-safe | Ownable | Total |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 01-florin-coin | | | | | | | |
| 02-fleur-chip | | | | | | | |
| 03-ascii-f | | | | | | | |
| 04-insert-coin | | | | | | | |
| 05-charon-toll | | | | | | | |

## One sentence each
- 01: …
- 02: …
- 03: …
- 04: …
- 05: …

## Recommended winner
**ID** — one reason.

## Needs changes (if any)
- …
```

## Score sheet (supervisor tally · 2026-07-22)

After three reviewers, average totals; break ties with Max-safe then Icon.

| ID | Critic | A11y | Heuristic | Avg | Rank |
| --- | --- | --- | --- | --- | --- |
| 01-florin-coin | 25 | 24 | 26 | **25.0** | **1** |
| 02-fleur-chip | 25 | 21 | 23 | 23.0 | 2 |
| 03-ascii-f | 24 | 21 | 22 | 22.3 | 3 |
| 04-insert-coin | 23 | 19 | 20 | 20.7 | 4 |
| 05-charon-toll | 19 | 18 | 19 | 18.7 | 5 |

**Panel recommendation:** **01-florin-coin** — all three judges; best icon silhouette + unit-of-account skim; needs wordmark lockup for README hero.  
**Stephen pick:** _(empty until CD decides)_

### Shared needs-changes (if 01 ships)
- Pair `hero-icon.png` with mark + `token-tracker` wordmark lockup (SVG/HTML OK)
- Simplify hatch for 16–24px sibling (solid F + flat blue disc)
- Keep flat engraving — no shiny 3D crypto
