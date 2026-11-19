# Round 1 rating sheet

Open the gallery: `xdg-open design/concepts/index.html` (or `python3 -m http.server -d design/concepts 8765`, then http://localhost:8765).

Score each concept **1 (weak) to 5 (strong)**. Judge the concept as a whole across all six screens, in the theme you would actually use. Use the notes column for anything specific: what to keep, what to drop, what to steal for another concept.

| # | Concept | First impression | Clarity / scanability | Consistency | Smoothness | Distinctiveness | Fits podkit | Notes |
|---|---|:-:|:-:|:-:|:-:|:-:|:-:|---|
| 01 | **Cockpit** (terminal-ops) | 5 | 5 | 5 | 5 | 5 | 5 | |
| 02 | **Quiet** (quiet-linear) | 5 | 5 | 5 | 4 | 5 | 5 | |
| 03 | **Hairline** (mono-precision) | 0 | 3 | 4 | 4 | 3 | 3 | |
| 04 | **Emerald Night** (tinted-dark) | 2 | 4 | 4 | 4 | 4 | 3 | |
| 05 | **Gazette** (editorial) | 1 | 1 | 1 | 1 | 1 | 1 | |
| 06 | **Gumdrop** (playful) | 1 | 1 | 1 | 1 | 1 | 1 | |
| 07 | **Aurora Glass** (glass-depth) | 1 | 1 | 1 | 1 | 1 | 1 | |
| 08 | **Slab** (brutalist) | 1 | 1 | 1 | 1 | 1 | 1 | |
| 09 | **Panelboard** (data-dashboard) | 4 | 5 | 4 | 5 | 5 | 4 | |
| 10 | **Meadow** (calm-soft) | 3 | 3 | 3 | 3 | 3 | 2 | |

**Top 3:** 1. Cockpit  2. Quiet  3. Panelboard

**Hybrid ideas (optional):** e.g. "navigation of 04, density of 01, motion of 06"

## What the columns mean

- **First impression**: gut reaction on the Projects and Environment screens.
- **Clarity / scanability**: can you tell what is running, building or broken in two seconds?
- **Consistency**: does every screen feel like the same product (frame, spacing, components, type)?
- **Smoothness**: hover/press/focus feedback, the building to running flip, dialog enter/exit, live log, page-to-page transition. Open the concept card and the app screen with "Play animations" on. Needs http for the page transitions.
- **Distinctiveness**: is it clearly not shadcn / not the current redesign?
- **Fits podkit**: self-hosted ops tool with AI models; can it carry 484 templates, GPU charts and a chat playground without strain?
