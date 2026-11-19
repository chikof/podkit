# podkit design round 2: three finalists

```sh
xdg-open design/finalists/index.html                 # works from file://
python3 -m http.server -d design/finalists 8765      # http://localhost:8765 also enables page-to-page view transitions
```

`index.html` shows the three finalists side by side: one screen, one theme, one viewport for all, a **Play motion** button (skeleton, building → running, live log, streamed answer, backup progress). `PICK.md` compares them and has the field for the final pick.

```
design/finalists/
  index.html  PICK.md  README.md  LICENSES.md
  01-terminal-ops/  02-quiet-linear/  09-data-dashboard/     one folder per finalist:
    README.md            the design system: principles, tokens (dark+light), contrast, type, spacing, components, motion, patterns, icons, screens
    IMPLEMENTATION.md    how it maps onto maud / theme.css / encre-css / components.css, native features and fallbacks, weight, risks
    CONTRAST.md  contrast.txt  contrast.py  contrast_core.py   the audit (python3 contrast.py exits 0 when everything passes)
    theme.snippet.css    paste-ready colour block for server/src/ui/theme.css (oklch, light-dark())
    components.html      every component, every state
    projects environment app templates server playground      the 6 round 1 screens, polished
    signin create database stack settings empty 404            the 7 new screens
    tokens.css concept.css extra.css base.css structure.css mobile.css states.css   styles (states.css only feeds components.html)
    page.js (interaction, ships)  sim.js (simulation, prototype only)  fonts/
  .assets/logos/     brand logos used as placeholders (see LICENSES.md)
  .build/            generator and audits (python3 stdlib only)
```

## Rebuild and audit

```sh
python3 design/finalists/.build/build.py                  # regenerates every html page, states.css, gallery, stats
python3 design/finalists/.build/docs.py                   # regenerates README / IMPLEMENTATION / CONTRAST / theme snippet / PICK
for d in design/finalists/*/; do python3 $d/contrast.py; done      # 0 failures
grep -rEn "https?://|url\(//" design/finalists --include=*.html --include=*.css --include=*.js   # prints nothing
```

`concept.css` and `tokens.css` are hand-written per finalist and are the source of truth for the visuals; the screen markup is generated from `.build/screens.py` (the same 13 screen bodies for every finalist, only the frame and a little copy differ).

## What is mocked

All data is fake (team acme, project storefront, server atlas). Status flips, the live log, the streamed answer, the backup progress, skeletons and toasts are driven by `sim.js` timers; in podkit they are Datastar/SSE patches. The variables bar in Panelboard and the team/environment switchers are visual only. Cross-document view transitions need http.
