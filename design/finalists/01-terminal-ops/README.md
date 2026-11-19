# Cockpit: design system

**Cockpit** is a terminal-style ops console: monospace everywhere, resources as rows, state as glyph plus word, stepped 60 to 140 ms feedback. Dark is a phosphor black with amber; light is a paper terminal.

Round 2 finalist `01-terminal-ops` (round 1 concept 01). Everything below is generated from, or checked against, the files in this folder:
`tokens.css` (all decisions), `concept.css` (what makes it Cockpit), `extra.css` (shared components), `base.css` / `structure.css` / `mobile.css` (mechanics), `components.html` (every component and state), the 13 screen files, `IMPLEMENTATION.md` (how to build it in podkit) and `contrast.py` (the audit; exits 0 when every pair passes).

Open `projects.html` (or any screen) from `file://`; theme with `?theme=dark|light`, motion off with `?still=1`. Cross-document view transitions run when served over http (`python3 -m http.server -d design/finalists 8765`).

## 1. Principles

1. **Everything is text on a grid.** One typeface (JetBrains Mono) in eight sizes; labels are uppercase with `.06em` tracking.
2. **Rows, not cards.** Lists are bordered rows; radius is 2 px at most; shadows only on overlays.
3. **State is glyph, colour and word.** Badges are bracketed, uppercase and carry a dot shape; amber is the single accent.
4. **Instant, stepped feedback.** `steps()` easing, no travel on overlays, 60 / 90 / 140 ms.
5. **Keyboard first.** Ctrl+K palette, menus that open into the first item, a visible 1 px focus ring on every control.

**Dark and light.** Dark `#0b0d0c` with amber `#ffb454`; light is warm paper `#f4f1e8` with ink `#1c211d` and an ochre accent `#9a5b00`. Status colours are re-derived per theme to hold 4.5:1.

## 2. Tokens

All colours are CSS custom properties in `tokens.css`; dark is the default, `[data-theme="light"]` overrides. Non-colour tokens are theme independent.

### 2.1 Colour

| Token | Role | Dark | Light |
|---|---|---|---|
| `--bg` | page background | `#0b0d0c` | `#f4f1e8` |
| `--surface` | cards, sidebar, panels | `#101311` | `#ece8db` |
| `--surface-2` | raised: table header, hover wash, readonly fields | `#161a17` | `#e4dfcf` |
| `--surface-3` | pressed, selected row, badge fill | `#1d221e` | `#d9d3c0` |
| `--border` | decorative hairline (exempt from 1.4.11) | `#252b27` | `#d3cdb9` |
| `--border-strong` | emphasis separators | `#46504a` | `#7c7a68` |
| `--border-ui` | control border: inputs, buttons, checkbox, switch (>= 3:1) | `#67716b` | `#7c7a68` |
| `--text` | primary text | `#d6dcd3` | `#1c211d` |
| `--text-2` | secondary text, labels, icons | `#9eaa9f` | `#465147` |
| `--text-3` | meta, placeholders, timestamps (still >= 4.5:1) | `#808c81` | `#4f5b52` |
| `--accent` | the brand signal: primary button, selected fill, progress | `#ffb454` | `#9a5b00` |
| `--accent-hover` | primary hover | `#ffc77d` | `#7c4800` |
| `--accent-ink` | text/icon on `--accent` | `#0b0d0c` | `#fffaf0` |
| `--accent-soft` | tint behind accent text, selected rows | `#2a2113` | `#f1ddb8` |
| `--accent-text` | accent used as text (links, active nav) | `#ffb454` | `#7c4800` |
| `--ok` | running, success | `#7ee787` | `#17692f` |
| `--warn` | building, warning | `#f2c14e` | `#6f4f00` |
| `--err` | failed, destructive | `#ff7b72` | `#a8201a` |
| `--info` | deploying, informational | `#6cb6ff` | `#0a56a2` |
| `--ok-soft` | tint behind ok text | `#10261a` | `#d5e8d3` |
| `--warn-soft` | tint behind warn text | `#2a2310` | `#f1e3b5` |
| `--err-soft` | tint behind err text, banners | `#2f1517` | `#f3d4cf` |
| `--info-soft` | tint behind info text | `#102233` | `#d3e2f1` |
| `--err-ink` | text on solid destructive button | `#0b0d0c` | `#ffffff` |
| `--focus` | focus ring (3:1 on every surface) | `#ffb454` | `#9a5b00` |
| `--overlay` | dialog backdrop | `rgb(2 4 3 / .72)` | `rgb(40 38 28 / .45)` |
| `--grid` | chart gridlines (decorative) | `#1d231f` | `#d9d3c0` |
| `--log-bg` | log viewer, code blocks | `#080a09` | `#e9e4d5` |
| `--skeleton-a` | skeleton base | `#161a17` | `#e4dfcf` |
| `--skeleton-b` | skeleton shimmer | `#232a25` | `#d3cdb9` |
| `--chart-1` | series 1 | `#ffb454` | `#9a5b00` |
| `--chart-2` | series 2 | `#7ee787` | `#17692f` |
| `--chart-3` | series 3 | `#6cb6ff` | `#0b5cad` |
| `--chart-4` | series 4 | `#d2a8ff` | `#7a3fb0` |
| `--chart-5` | series 5 | `#ff7b72` | `#b3261e` |
| `--chart-6` | series 6 | `#56d4dd` | `#0b7a82` |

`--border` and `--grid` are decorative hairlines (exempt from 1.4.11); `--border-ui` is the **control border**: every input, select, button, checkbox and switch outline uses it, because the round 1 borders (`--border-strong` and lighter) failed 3:1. `--err-ink` is the text colour on a solid destructive button.

### 2.2 Contrast

Computed with `contrast.py` (WCAG 2.x relative luminance; alpha colours composited over the pair's background). **194 pairs checked, 0 failures.** Full list: [`CONTRAST.md`](CONTRAST.md); raw script output: [`contrast.txt`](contrast.txt). Rendered pages were also audited in a browser (every text node, both themes, desktop and 390 px, with each dialog and popover open): no text below its threshold.

**Dark, text (need 4.5):**

| Text token | on `--bg` | on `--surface` | on `--surface-2` | on `--surface-3` |
|---|---|---|---|---|
| `--text` | 13.96 | 13.39 | 12.59 | 11.57 |
| `--text-2` | 8.08 | 7.75 | 7.29 | 6.70 |
| `--text-3` | 5.56 | 5.33 | 5.01 | 4.61 |
| `--accent-text` | 11.06 | 10.60 | 9.97 | 9.16 |
| `--ok` | 12.69 | 12.17 | 11.44 | 10.51 |
| `--warn` | 11.62 | 11.14 | 10.48 | 9.63 |
| `--err` | 7.73 | 7.41 | 6.97 | 6.41 |
| `--info` | 9.07 | 8.70 | 8.18 | 7.52 |

**Light, text (need 4.5):**

| Text token | on `--bg` | on `--surface` | on `--surface-2` | on `--surface-3` |
|---|---|---|---|---|
| `--text` | 14.47 | 13.34 | 12.26 | 10.92 |
| `--text-2` | 7.35 | 6.78 | 6.23 | 5.55 |
| `--text-3` | 6.30 | 5.81 | 5.34 | 4.75 |
| `--accent-text` | 6.68 | 6.15 | 5.66 | 5.04 |
| `--ok` | 6.00 | 5.53 | 5.09 | 4.53 |
| `--warn` | 6.65 | 6.13 | 5.64 | 5.02 |
| `--err` | 6.44 | 5.94 | 5.46 | 4.86 |
| `--info` | 6.49 | 5.98 | 5.50 | 4.90 |

**Dark, UI components (need 3.0):**

| Token | Use | Ratio (needs 3.00) |
|---|---|---|
| `--border-ui` | input, select, button, checkbox, switch outline | bg: 3.85 / surface: 3.70 / surface-2: 3.48 |
| `--focus` | focus ring | bg: 11.06 / surface: 10.60 / surface-2: 9.97 |
| `--accent` | checked fill, switch, tab bar, progress | bg: 11.06 / surface: 10.60 / surface-2: 9.97 |
| `--ok` | status dot | bg: 12.69 / surface: 12.17 |
| `--warn` | status dot | bg: 11.62 / surface: 11.14 |
| `--err` | status dot | bg: 7.73 / surface: 7.41 |
| `--info` | status dot | bg: 9.07 / surface: 8.70 |
| `--chart-1` | series 1 | bg: 11.06 / surface: 10.60 |
| `--chart-2` | series 2 | bg: 12.69 / surface: 12.17 |
| `--chart-3` | series 3 | bg: 9.07 / surface: 8.70 |
| `--chart-4` | series 4 | bg: 10.01 / surface: 9.60 |
| `--chart-5` | series 5 | bg: 7.73 / surface: 7.41 |
| `--chart-6` | series 6 | bg: 11.01 / surface: 10.56 |

**Light, UI components (need 3.0):**

| Token | Use | Ratio (needs 3.00) |
|---|---|---|
| `--border-ui` | input, select, button, checkbox, switch outline | bg: 3.84 / surface: 3.54 / surface-2: 3.25 |
| `--focus` | focus ring | bg: 4.80 / surface: 4.43 / surface-2: 4.07 |
| `--accent` | checked fill, switch, tab bar, progress | bg: 4.80 / surface: 4.43 / surface-2: 4.07 |
| `--ok` | status dot | bg: 6.00 / surface: 5.53 |
| `--warn` | status dot | bg: 6.65 / surface: 6.13 |
| `--err` | status dot | bg: 6.44 / surface: 5.94 |
| `--info` | status dot | bg: 6.49 / surface: 5.98 |
| `--chart-1` | series 1 | bg: 4.80 / surface: 4.43 |
| `--chart-2` | series 2 | bg: 6.00 / surface: 5.53 |
| `--chart-3` | series 3 | bg: 5.91 / surface: 5.44 |
| `--chart-4` | series 4 | bg: 5.87 / surface: 5.41 |
| `--chart-5` | series 5 | bg: 5.79 / surface: 5.33 |
| `--chart-6` | series 6 | bg: 4.51 / surface: 4.16 |

### 2.3 Type

One family: **JetBrains Mono** (variable, latin subset) for UI, headings, numbers and code.

| Step | Size | Line height | Used for |
|---|---|---|---|
| `--fs-xs` | 11 px | 1.5 (16.5 px) | labels, kbd, badges, table headers |
| `--fs-sm` | 12 px | 1.5 (18.0 px) | secondary text, buttons, menu items |
| `--fs-md` | 13 px | 1.5 (19.5 px) | body, inputs, table cells |
| `--fs-lg` | 15 px | 1.2 (18.0 px) | brand, row titles |
| `--fs-xl` | 18 px | 1.2 (21.6 px) | dialog titles, stat values |
| `--fs-2xl` | 22 px | 1.2 (26.4 px) | page titles, metric values |
| `--fs-3xl` | 28 px | 1.1 (30.8 px) | sign-in title |
| `--fs-4xl` | 40 px | 1.1 (44.0 px) | 404 title, large display figure |

Weights: regular 400, medium 500, bold 700. Label tracking `--tracking-label: .06em`; `text-wrap: pretty` on body text; tabular numerals on every number column (`font-variant-numeric: tabular-nums`).

### 2.4 Spacing, radius, size

Spacing scale (2 px base: 2, 4, 8, 12, 16, 24, 32, 48):

| Token | Value |
|---|---|
| `--s-1` | 2px |
| `--s-2` | 4px |
| `--s-3` | 8px |
| `--s-4` | 12px |
| `--s-5` | 16px |
| `--s-6` | 24px |
| `--s-7` | 32px |
| `--s-8` | 48px |

Radius:

| Token | Value |
|---|---|
| `--r-sm` | 0 |
| `--r-md` | 2px |
| `--r-lg` | 2px |
| `--r-pill` | 2px |

Layout tokens:

| Token | Value |
|---|---|
| `--nav-w` | `224px` |
| `--bar-h` | `36px` |
| `--row-h` | `44px` |
| `--ctl-h` | `28px` |
| `--bw` | `1px` |
| `--icon-sw` | `1.75` |
| `--press-scale` | `1` |

### 2.5 Elevation

Flat: 1 px borders separate surfaces; only dialog, menu, palette and toast carry a shadow.

| Token | Dark | Light |
|---|---|---|
| `--sh-1` | `none` | `none` |
| `--sh-2` | `0 0 0 1px var(--border-strong)` | `0 0 0 1px var(--border-strong)` |
| `--sh-3` | `0 12px 40px rgb(0 0 0 / .6)` | `0 12px 40px rgb(60 50 20 / .25)` |

### 2.6 Motion tokens

| Token | Value |
|---|---|
| `--dur-1` | 60ms |
| `--dur-2` | 90ms |
| `--dur-3` | 140ms |
| `--dur-vt` | 120ms |
| `--ease` | steps(3,end) |
| `--ease-out` | cubic-bezier(0,0,.2,1) |
| `--ease-spring` | steps(4,end) |
| `--stagger` | 18ms |

## 3. Layout and grid

**Frame:** sidebar `--nav-w` wide with team switcher, nav and a project tree; a `--bar-h` top bar with breadcrumb, search and theme; a status bar pinned to the bottom. **Content:** fluid, padded with the page tokens below. **Lists:** resource and project lists are bordered rows (`--row-h`) laid out as CSS grid tracks; templates are a tile grid. See `concept.css` for the exact tracks.

**Breakpoints.** One real breakpoint at **760 px** (`mobile.css`): the frame becomes a brand strip on top and a five-item tab bar at the bottom (60 px, safe-area aware), page padding drops to `--s-4`, grids collapse to one column, tables become stacked rows with `data-label` captions, dialogs become bottom sheets, and every control reaches 44 px. Dense rows drop secondary columns on narrow viewports.

## 4. Components

Every component is on `components.html` with hover, focus, active, disabled, invalid and loading shown side by side (hover/focus/active are forced with `data-state`; the real pseudo-classes work too).

| Component | Anatomy | States |
|---|---|---|
| Button | label, optional icon; variants primary, secondary, ghost, danger, danger-solid; sizes sm, md, lg; icon-only (needs `aria-label`) | default, hover, focus, active, disabled, loading (`aria-busy`) |
| Input, textarea | control with optional leading icon or trailing action | default, hover, focus, filled, invalid, disabled, readonly |
| Select | native `<select>` styled to the input height | default, hover, focus, disabled |
| Checkbox | native input, painted box and check | unchecked, checked, mixed, hover, focus, disabled |
| Switch | native checkbox with track and thumb; row variant with title and help | off, on, hover, focus, disabled |
| Field | label, control, help text, error text with icon | default, help, invalid, disabled |
| Card / panel | header and body parts, or a flush table | static; interactive cards add hover and pressed |
| Badge | status dot (shape + colour) and word | running, building, deploying, failed, stopped, pending, unreachable |
| Status dot | filled / pulsing / ringed / diamond / hollow / dashed / slashed shapes | 7 statuses, always with a word or `aria-label` |
| Chip | text with optional count; toggle chips use `aria-pressed` | default, hover, pressed, focus |
| Tabs | native radio group; panels switch through `:has()` | default, hover, selected, focus-visible, disabled |
| Table | header, rows, numeric columns, row menu; stacks at 390 px | default, hover, failed row |
| Dialog | native `<dialog>`: head, body, foot; bottom sheet at 390 px | closed, open; destructive confirms focus Cancel |
| Dropdown menu | `[popover]` with items, headings, separators, danger item | hover, focus, checked, danger |
| Tooltip | `popover=hint` + `interestfor` | hidden, shown on hover and keyboard focus |
| Toast | icon and one line in a `role=status` region | success, error |
| Skeleton | placeholders with the final geometry | loading |
| Empty state | icon, title, sentence, up to three actions | empty environment, no results |
| Breadcrumb, switcher | path with an environment switcher menu; team switcher in the frame | hover, current, open |
| Command palette | dialog with combobox input and listbox of commands | closed, open, filtered, selected, no results |
| Log viewer | header, level filters, monospaced lines, `role=log` | live, paused, filtered |
| Chart | inline SVG with HTML axis labels and legend | 1h / 24h / 7d, two series |
| Logo tile | brand logo or two-letter monogram on a tile | logo, monogram (6 tints) |
| Avatar | initial on tint | single, group |
| Kbd | key cap | static |
| Banner | icon, lead, text, optional action | error, warning, info, success |
| Progress | determinate bar with `role=progressbar` | building |

**Cockpit skin.** Square corners (`--r-md: 2px`), uppercase labels, bracketed badges, prompt-style heading prefixes, inverse hover on menu items and palette rows, stepped progress bar, 1 px focus ring.

## 5. Motion spec

Instant by design: durations are at most 140 ms, easing is `steps()`, and overlays do not translate.

| What | Duration | Easing | Notes |
|---|---|---|---|
| Hover, press | `--dur-1` 60 ms | `steps(3)` | background fill |
| Dialog, popover, toast | `--dur-2` 90 ms | `steps(3)` | opacity only |
| Status flip | `--dur-3` 140 ms | `steps(4)` | scale blink when building turns running |
| List reveal | `--dur-3` + 18 ms stagger | `steps(4)` | after the skeleton |
| Page change | `--dur-vt` 120 ms | `steps(3)` | cross-document view transition |
| Skeleton, live dot | loop | linear / `steps()` | stopped under reduced motion |

**Reduced motion.** `@media (prefers-reduced-motion: reduce)` zeroes every animation and transition duration (`base.css`), stops the skeleton shimmer and the caret, and disables cross-document view transitions. State changes then happen instantly but are still announced by text (badge label, toast). `?still=1` does the same for screenshots.

## 6. Patterns: empty, loading, error

- **Empty:** dashed box, one sentence, up to three actions.
- **Loading:** skeleton rows with the final geometry; stepped progress bar; `aria-busy` buttons keep their width.
- **Error:** `role=alert` banner with an action; a failed resource shows its real message on its row; field errors sit under the field; the 404 stays inside the app frame.

## 7. Iconography and logos

Lucide (`--icon-sw: 1.75`), `currentColor`. Logos sit on square tiles; a service without a logo gets a two-letter monogram tile in the same shape.

## 8. Screens (13, each in dark and light, desktop and 390 px)

| File | What it shows |
|---|---|
| `projects.html` | Team summary and project list, New project dialog, skeleton to content. |
| `environment.html` | Production/staging tabs, resources of every kind and status, building progress, a failed resource, Create menu, environment switcher. |
| `app.html` | Git app: status, stats, tabs, deployment timeline, streaming build log, more-actions menu, delete dialog. |
| `templates.html` | Template catalog: search, category chips, tile grid, no-results state, Deploy dialog. |
| `server.html` | Hardware summary and metric charts with a 1h / 24h / 7d switch. |
| `playground.html` | Model page: throughput, streamed chat with thinking block, parameters, usage. |
| `signin.html` | Email and password with validation, reveal, loading and error banner. |
| `create.html` | Create-resource picker with a detail panel. |
| `database.html` | Header, stats, **Connection** tab (credentials, connection string) and **Backups** tab (table, Back up now, schedule), restore and delete dialogs. |
| `stack.html` | Compose stack with a failed service: banner, **Services** tab with logo and monogram tiles, Compose file tab. |
| `settings.html` | Team settings, **Members**: roles, invite form, pending invitations, remove dialog. |
| `empty.html` | Empty environment with next actions. |
| `404.html` | Route not found inside the app frame. |

## 9. Accessibility and keyboard

- Dialogs are native `<dialog>` opened by `command="show-modal"`; Esc and the backdrop close them (`closedby="any"`), focus moves to the `autofocus` control (the safe action in a destructive confirm) and returns to the opener. A dialog opened from a menu item returns focus to the menu's button (`page.js`, 3 lines).
- Menus are `[popover]` with `focusgroup="menu nomemory"`: Enter / Space / ArrowDown on the trigger opens and enters the menu, Up/Down/Home/End move, Esc closes and focus returns to the trigger, Tab leaves.
- Tabs are native radio groups: Tab lands on the selected tab, arrow keys move and select, the panel follows through `:has()`. No script.
- Command palette: Ctrl/Cmd+K or the Search button; type to filter, Up/Down to move, Enter to open, Esc to close; combobox/listbox roles with `aria-activedescendant`.
- Tooltips are `popover="hint"` + `interestfor`: they open on hover **and** keyboard focus, and the control keeps its own accessible name.
- Every status is colour + shape + word (never colour alone). Focus ring: 2 px `--focus` outline with 2 px offset (1 px in Cockpit's square system), 3:1 against every surface.
- Touch targets are 44 px at 390 px; at desktop density controls are 28px high (above the 24 px WCAG 2.5.8 minimum).
