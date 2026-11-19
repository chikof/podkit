# Self-review (design, round 1)

Every concept ships with its own `tokens.css` (dark and light), `concept.css` (tokens only, no raw hex), fonts, and a concept card with palette, type, component sheet, motion spec and licenses. Contrast: `_build/contrast.py` reports 0 failures at 4.5:1 across all token pairs in both themes. Verified in Chrome: no JS errors, no non-local requests, all fonts load from file://, status flip / live log / chat stream / dialog exit / skeleton work, reduced motion collapses everything, cross-document view transitions fire over http.

## What makes each concept distinct

**01 Cockpit (dense terminal / ops).** All JetBrains Mono, a text-only `./path` sidebar with a project tree, a statusline footer with live server numbers, and rows instead of cards. Status is a `[ ■ RUNNING ]` glyph, not a pill. Motion is stepped on purpose: `steps()` easing, typewriter log lines, no travel on dialogs, press inverts the row. The most "this is an ops tool" of the ten; densest (a 6-resource environment fits in one screen with metrics and sparklines per row).

**02 Quiet (Linear-style minimal).** Grouped sidebar, borderless hairline lists, status as dot plus text, metrics that only appear on hover, a single ice-blue signal colour. Motion is 90-240 ms ease-out with 4-6 px lifts. The lowest-risk and fastest-feeling option; also the closest to "generic dark SaaS", so it needs a stronger identity move in round 2 if it advances.

**03 Hairline (Vercel monochrome).** Light-first, black on white, a two-tier top bar, and every collection is one bordered grid with 1 px seams instead of floating cards. Geist plus Geist Mono labels, colour only for state, hover reveals an arrow, buttons invert. Crisp and engineered; resources, projects, templates and metrics all share the seam-grid, so it is very consistent.

**04 Emerald Night (Supabase-like tinted dark).** Icon rail with tooltips, green-black surfaces, one emerald accent, big rounded cards whose border and glow brighten on hover, logos in tinted tiles, filled area charts. Polished and calm; the best balance of density, scanability and "developer product" feel.

**05 Gazette (warm editorial).** Newspaper masthead with double rules, Fraunces headlines, Newsreader body, cream paper (or a night edition), resources as numbered ledger entries, a lead-story project tile, templates as classified columns. The most unexpected for infra, and the most readable for prose and metrics. Slowest motion (200-640 ms, 80 ms stagger, 500 ms page dissolve).

**06 Gumdrop (playful, colourful).** Floating bottom dock, outlined sticker cards with hard drop shadows, a pastel colour per resource kind, springy `linear()` easing, squash on press, tilted hardware stickers, bar charts. Personality-first; every click is rewarded. Risky for a serious ops tool but great to see what "fun" would take.

**07 Aurora Glass (glass / depth).** A fixed teal-and-amber aurora behind three glass tiers (panes, raised dock, overlays) with edge light and a floating pill nav. Blur is used only to show elevation. Glowing series lines, backdrop-blurred dialogs, 420 ms blur-fade between pages. The prettiest in screenshots; heaviest to render, and legibility depends on the backdrop.

**08 Slab (brutalist / utilitarian).** Heavy black rules, hard offset shadows, huge uppercase Archivo, bright status blocks (green / yellow / red slabs with black text), numbered nav, hazard-stripe progress. Motion is stepped and mechanical. Unmistakable and extremely scannable; deliberately loud, so it is the polarising one.

**09 Panelboard (Grafana-ish).** A variables bar (team / project / env / server / range) under a slim top bar, every thing is a titled panel, resources become stat panels with big coloured numbers and sparklines, classic series colours, a statusline. The server screen is the best of the ten for GPU/VRAM/network work, and it makes resource health readable at a distance.

**10 Meadow (calm, light-first, soft).** Floating rounded sidebar, warm white with sage and peach tints per resource kind, 26 px radii, long soft shadows, smoothed area charts, slow breathing easing, statuses that glow rather than flash. The friendliest and the most approachable light theme; least dense.

## My top 3

1. **04 Emerald Night.** Best all-round: clear status, comfortable density, a real brand colour that is not indigo, rail navigation that scales, and the motion is smooth without being showy. Safest route to something that is not shadcn.
2. **09 Panelboard.** podkit is an ops tool that also reports GPU/VRAM, tok/s and per-resource metrics; this concept makes those first-class and gives the server and model pages a purpose-built layout. Best fit for the actual data.
3. **01 Cockpit.** Strongest identity and the densest for people living in the dashboard. It is the opposite of generic. Worth carrying its statusline, tree sidebar and keyboard-first feel into whichever of the above wins, even if the all-mono look itself is too much.

Honourable mentions: **10 Meadow** if a light-first default matters, **05 Gazette** for distinctiveness, **03 Hairline** for the seam-grid idea. A natural hybrid for round 2 is 04's rail and surfaces + 09's metric panels + 01's statusline + 03's seam-grid for the template gallery.

## Limits and honest caveats

- Page-to-page `@view-transition` only runs over http; Chrome treats file:// pages as separate origins. Everything else works from file://.
- Verified in Chromium only (no Firefox/Safari). Anchor-positioned menus fall back to a bottom-right popover without `position-area`; dialog exit uses a `data-closing` fallback so it also animates without `overlay` transitions.
- Brand logos are trademarks used as placeholders (see `LICENSES.md`). The podkit mark in the frame is a throwaway glyph.
- Concept 01 does not load `structure.css` (fully standalone); 02-10 do. For DS2 the shared files are an implementation detail, not a design decision.
- Resource cards for stopped/failed show "—" metrics on purpose. All numbers are plausible mock data.
