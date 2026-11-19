# Cockpit: implementation notes

How to build `01-terminal-ops` in `server/src/ui`. Today podkit ships about 71 KB of CSS (theme.css 5 KB + components.css 39.5 KB + encre-css utilities) and 148 KB of fonts (Geist sans + mono). Everything here is measured from this folder.

## 1. Mapping onto podkit's stack

| Layer | Today | For Cockpit |
|---|---|---|
| Shell (`shell/*`) | Sidebar + header | Sidebar gains a project tree; a **new** status bar shell piece fed by server metrics. |
| Lists | cards | Resource, project and template lists become rows. |
| Typography | Geist sans + mono | One mono family for everything. |
| Colour | neutral palette | Amber accent; `theme.snippet.css`. |

### Tokens → `theme.css`

`theme.snippet.css` (generated, oklch, `light-dark()`) is a paste-ready replacement for the colour block of `server/src/ui/theme.css`. podkit's names are shadcn's; the mapping is:

| theme.css token | Cockpit token | Note |
|---|---|---|
| `--background` | `--bg` |  |
| `--foreground` | `--text` |  |
| `--card` | `--surface` |  |
| `--card-foreground` | `--text` |  |
| `--popover` | `--surface` | menus, dialogs |
| `--popover-foreground` | `--text` |  |
| `--primary` | `--accent` | brand signal |
| `--primary-foreground` | `--accent-ink` |  |
| `--secondary` | `--surface-2` |  |
| `--secondary-foreground` | `--text` |  |
| `--muted` | `--surface-2` |  |
| `--muted-foreground` | `--text-2` |  |
| `--accent` | `--surface-3` | shadcn `accent` is a hover wash |
| `--accent-foreground` | `--text` |  |
| `--destructive` | `--err` |  |
| `--destructive-foreground` | `--err-ink` |  |
| `--success` | `--ok` |  |
| `--warning` | `--warn` |  |
| `--info` | `--info` |  |
| `--border` | `--border` | decorative hairline |
| `--input` | `--border-ui` | control border, 3:1 |
| `--ring` | `--focus` |  |
| `--chart-1` | `--chart-1` |  |
| `--chart-2` | `--chart-2` |  |
| `--chart-3` | `--chart-3` |  |
| `--chart-4` | `--chart-4` |  |
| `--chart-5` | `--chart-5` |  |
| `--chart-6` | `--chart-6` | new (podkit has 5) |
| `--sidebar` | `--surface` |  |
| `--primary-text` | `--accent-text` | new: accent as text |
| `--primary-hover` | `--accent-hover` | new |
| `--primary-soft` | `--accent-soft` | new |
| `--success-soft` | `--ok-soft` | new |
| `--warning-soft` | `--warn-soft` | new |
| `--destructive-soft` | `--err-soft` | new |
| `--info-soft` | `--info-soft` | new |
| `--muted-strong` | `--surface-3` | new |
| `--subtle-foreground` | `--text-3` | new |
| `--border-emphasis` | `--border-strong` | new |
| `--log` | `--log-bg` | new |
| `--overlay` | `--overlay` | new |

New tokens the current theme does not have, all colour tokens (so `build.rs` registers them with encre-css automatically): `primary-text`, `primary-hover`, `primary-soft`, `success-soft`, `warning-soft`, `destructive-soft`, `info-soft`, `muted-strong` (surface-3), `subtle-foreground` (text-3), `input-strong`… see the snippet. Non-colour tokens (type scale, spacing, radius, motion) go in a **second** `:root` block, because the first one is reserved for colours.

### Components → maud + `components.css`

Cockpit changes the look of every component but few APIs. The structural changes are rows instead of cards and the status bar.

| `src/ui/components/*.rs` | Change |
|---|---|
| `button.rs`, `chip.rs`, `kbd.rs` | Add `lg` size, `aria-busy` loading state (spinner via `::before`), `danger-solid`; control border uses `--input` (now 3:1). |
| `field.rs` | Label, hint, error (`.field-e`, shown by `:user-invalid` / `aria-invalid`), custom `select` wrapper, password reveal, copy field; add checkbox and `switch` (native `<input switch>` + painted fallback). |
| `card.rs`, `table.rs` | `card-h` / `card-b` parts; table cells carry `data-label` so the 390 px stack works without a second template. |
| `dialog.rs`, `menu.rs`, `tooltip.rs` | Open with `command="show-modal"` / `toggle-popover`; menu gets `focusgroup="menu nomemory"` and separators as `<hr>`; tooltip is `popover="hint"` + `interestfor`. `closedby="any"` on dialogs. |
| `tabs.rs` | Keep radio inputs; panels switch through `:has()` keyed on `value`. The Datastar version only needs to persist the selection. |
| `badge.rs`, `progress.rs` | Badge takes `data-status` (running, building, deploying, failed, stopped, pending, unreachable); dot shape is CSS. Two new statuses: `deploying`, `pending`, `unreachable`. |
| `log_viewer.rs`, `metrics.rs` | Log lines get `data-lv` level; filter chips; charts keep inline SVG with `non-scaling-stroke`, axis labels in HTML. |
| `empty_state.rs`, `skeleton.rs`, `toast.rs` | Empty state with icon, title, text and up to three actions; skeleton placeholders match the final row geometry; toast has an error variant. |
| `logo.rs`, `avatar.rs` | Monogram fallback with tint 1–6 (see Icons and logos). |
| `shell/*` (`sidebar.rs`, `header.rs`, `nav.rs`, `palette.rs`) | Frame differs per finalist (see table above). Palette becomes a combobox/listbox dialog; mobile adds the bottom tab bar (same `nav.rs` data). |

### Utilities vs the component layer

- **encre-css utilities in maud** (as today): page and card layout (`flex`, `grid`, `gap-*`, `p-*`, `max-w-*`), type size and colour (`text-sm`, `text-muted-foreground`), responsive tweaks. Roughly everything in `structure.css` and the layout half of `concept.css` is utility-shaped.
- **`components.css`** (`ui-*` classes): what utilities cannot say: top-layer enter/exit (`@starting-style`, `transition-behavior: allow-discrete`), anchor positioning for menus and tooltips, `:has()` radio tabs, custom checkbox and switch paint, status-dot shapes, skeleton shimmer, log viewer, chart strokes (`vector-effect: non-scaling-stroke`), the mobile table stack, view transitions. That is `base.css`, `extra.css` and the skin half of `concept.css`.
- Rule from `build.rs` still holds: every class in a template is either an encre utility or defined in `components.css`.

## 2. Native CSS / HTML features and fallbacks

| Feature | Used for | Support (check caniuse before build) | Fallback |
|---|---|---|---|
| Invoker commands (`commandfor`, `command`) | open dialogs and menus without script | newer; Chromium first | small click handler in `page.js`, feature-detected |
| `<dialog closedby>` | light dismiss | newer; not everywhere | Esc still closes; backdrop-click handler in `page.js` |
| `popover`, `popover=hint`, `interestfor` | menus, tooltips | popover widely; `interestfor` newest | hover and focus handler shows the hint popover |
| CSS anchor positioning | menu and tooltip placement | newer; not in every engine | fixed position computed in `page.js` |
| `focusgroup` | arrow keys inside menus | newest; Chromium first | keydown handler in `page.js` (arrows, Home, End) |
| `:has()` | radio tabs, selected picker item | widely available | none needed |
| `@starting-style` + `allow-discrete` | dialog, popover and toast enter/exit | widely available | open and close without animation |
| Cross-document view transitions | page-to-page transition, logo morph | Chromium and Safari, not Firefox | plain navigation |
| `color-mix()` | monogram and avatar tints | widely available | flat `--surface-3` |
| `light-dark()` + `color-scheme` | all colour tokens | already used by podkit | none |
| `:user-invalid` | field error state | widely available | Datastar sets `aria-invalid`; the selector covers both |

## 3. Weight

| File | Size | What it becomes in podkit |
|---|---|---|
| `tokens.css` | 2.7 KB | `theme.css` colour block + a second `:root` for type, spacing, radius, motion |
| `base.css` | 8.4 KB | `components.css`: top layer, skeleton, log, reduced motion |
| `extra.css` | 21.6 KB | `components.css`: shared components |
| `concept.css` | 24.9 KB | `components.css`: the finalist's skin and list layouts |
| `mobile.css` | 5.4 KB | `components.css` `@media (max-width: 760px)` |
| **total CSS** | **63.1 KB** | **16.0 KB gzip** (source, unminified, as shipped in this prototype) |

JavaScript: `page.js` 7.0 KB (interaction layer: theme, Ctrl+K, palette, fallbacks) and `sim.js` 9.6 KB (prototype simulation only, **not shipped**: in podkit those updates arrive as Datastar/SSE patches). Delivered JS is `page.js` (2.6 KB gzip) next to the existing `podkit.js` (12 KB) and Datastar (36 KB).

**Estimate for podkit.** See the measured table above; this is the smallest font payload of the three (one file). Real numbers for podkit depend on how much moves into utilities; treat the prototype total as an upper bound.

Fonts:

| Font file (latin subset, woff2, OFL) | Size |
|---|---|
| `jetbrains-mono-var.woff2` | 39.5 KB |

**Fonts total: 39 KB** (today 148 KB). One font file. JetBrains Mono is wide, so a 390 px viewport fits roughly 38 characters per line.

## 4. Icons and logos

- Lucide stays, drawn at `stroke-width: 1.75` through the existing `static/icons.svg` sprite. These screens use **59** distinct icons; **5** are not in the current sprite: `git-commit-horizontal`, `layout-grid`, `microchip`, `send`, `sparkles`. Add them to the sprite (same ISC licence).
- Brand logos keep coming from `static/logos.svg` / `image_logo.rs`. For a service without a logo, `logo.rs` renders a **monogram tile**: first two letters, `font-weight: bold`, on `color-mix(in oklab, var(--chart-N) 26%, var(--surface-2))` where N = 1 + (sum of the name's bytes mod 6), computed server-side so the tint is stable. Fallback without `color-mix()`: plain `--surface-3`.

## 5. Risks

- **Mono everywhere hurts long prose.** Playground answers and descriptions are paragraphs; they stay readable but a docs-heavy future would want a proportional face.
- **Amber does two jobs.** Accent and `building` are close in hue. They are separated by shape and word, but test with colour-blind users.
- **Distance from today's UI.** Furthest from the current look, so every page needs a pass in the terminal voice.

Shared by all three finalists:

- **Invoker commands, `popover`, `interestfor`, `focusgroup`, anchor positioning** are all in current Chromium; Firefox and Safari lag on some (see the table above). Each has a small fallback in `page.js`; budget a day to port them to Datastar handlers and test in Firefox ESR and Safari.
- **Cross-document view transitions** exist in Chromium and Safari only; elsewhere pages simply navigate. Nothing depends on them.
- **Latin-only font subsets.** Names with Cyrillic, Greek or CJK fall back to the system font until the subset list is widened (+15–40 KB per script).
- **The prototype data is fake** (acme, storefront, atlas). Empty/loading/error states are designed, but real error copy and the full list of resource kinds still need a content pass.
