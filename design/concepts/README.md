# podkit design round 1: 10 concepts × 6 screens

Open the gallery:

```sh
xdg-open design/concepts/index.html                 # works from file://
python3 -m http.server -d design/concepts 8765      # then http://localhost:8765 (also enables page-to-page view transitions)
```

Then fill in `RATING.md` (or reply with scores) and name your **top 3**.

## Layout

```
design/concepts/
  index.html          comparison gallery (rows of 6 screens, dark/light, "Compare 2")
  RATING.md           scoring sheet
  LICENSES.md         font / icon / logo licenses
  NN-slug/            one folder per concept
    index.html        concept card (palette, type, license table, component sheet, motion spec)
    projects.html environment.html app.html templates.html server.html playground.html
    tokens.css        ALL design decisions: colour (dark+light), type, spacing, radius, shadow, motion
    concept.css       what makes the concept distinct (frame, surfaces, layout), tokens only
    base.css          shared mechanics (dialog/popover enter-exit, view transitions, skeleton, log, reduced motion)
    structure.css     shared neutral component structure (not loaded by 01, which is fully standalone)
    page.js           vanilla enhancement layer (maps to Datastar attributes in podkit)
    fonts/            self-hosted woff2 (OFL)
  _assets/logos/      brand logos used as placeholders
  _build/             generator + audits (python3 only, no dependencies)
  _shots/             screenshots (git-ignored)
```

## URL parameters

`?theme=dark|light` picks the theme (carried across links), `?still=1` freezes animation (skips skeleton, streams, delays) which is what the gallery thumbnails and screenshots use.

## Rebuild / audit

```sh
python3 design/concepts/_build/build.py      # regenerates every html page, gallery, RATING.md, LICENSES.md
python3 design/concepts/_build/contrast.py   # WCAG 4.5:1 audit of every tokens.css pair, both themes
```

HTML is generated from `_build/screens.py` (the same six screen bodies for every concept); `tokens.css` and `concept.css` are hand-written per concept and are the source of truth for the visuals.

## What is mocked

All data is fake but realistic (acme team, storefront project, atlas server). The status flip, live log, skeleton, chat stream and dialogs are driven by `page.js` timers; in podkit they would be Datastar/SSE patches.
