# Typography — Florin system

## Product name in HTML/README prose
Keep the string **token-tracker** in markdown (repo name). Do **not** bake it beside the F in the hero PNG — two T’s fight the monogram.

## Google fonts (variable / stylable)

### Primary — modern variable (for “florin”, UI chrome, captions)
**[Fraunces](https://fonts.google.com/specimen/Fraunces)** (variable)

- Axes: soft, wonk, opsz, weight  
- Use for: lore word *florin*, section labels, refined captions  
- Cool styles: soft↑ for display · soft↓ + opsz small for UI · slight wonk for campaign posters  

Fallback stack: `Fraunces, "Iowan Old Style", "Palatino Linotype", Palatino, serif`

### Secondary — neo grotesque (optional, status / code-adjacent)
**[Bricolage Grotesque](https://fonts.google.com/specimen/Bricolage+Grotesque)** (variable)

- Axes: wdth, opsz, weight  
- Use for: `flr · local`, dense labels, install callouts  

### Display — gothic-inspired (for “Count every token.”)
**[Cinzel Decorative](https://fonts.google.com/specimen/Cinzel+Decorative)**  
or softer **[Cinzel](https://fonts.google.com/specimen/Cinzel)**

- Roman monumental / blackletter-adjacent without illegible Fraktur  
- Use only for the Charon banner line: **Count every token.**  
- Avoid for long README body  

CSS example:

```html
<link rel="preconnect" href="https://fonts.googleapis.com" />
<link href="https://fonts.googleapis.com/css2?family=Cinzel+Decorative:wght@700&family=Fraunces:opsz,wght@9..144,500..700&display=swap" rel="stylesheet" />
```

```css
.florin-word { font-family: Fraunces, serif; font-optical-sizing: auto; font-variation-settings: "SOFT" 50, "WONK" 0.4; }
.count-line { font-family: "Cinzel Decorative", Cinzel, serif; letter-spacing: 0.06em; }
```

## Hero composition (16:9 mural)

One PNG: `docs/logos/token-tracker.png` (= `florin-mural.png`)

1. **Left lockup** — ASCII F disc + **florin** (Fraunces)  
2. **Soft melt** into Charon / palm-coin myth  
3. **Count every token.** — Cinzel-adjacent, ~0.35–0.45× florin, low opacity whisper  

Product name `token-tracker` stays in markdown under the mural only.

Spec from design-lead (2026-07-22): logo band ≤28%H left-biased; melt 18–35%H; myth field mid–low; tagline shelf bottom ~12%.
