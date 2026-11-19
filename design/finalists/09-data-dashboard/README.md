# Panelboard: design system

**Panelboard** treats podkit as a telemetry board: every block is a titled panel on a tight grid under a variables bar, numbers are large and coloured by meaning, charts draw in.

Round 2 finalist `09-data-dashboard` (round 1 concept 09). Everything below is generated from, or checked against, the files in this folder:
`tokens.css` (all decisions), `concept.css` (what makes it Panelboard), `extra.css` (shared components), `base.css` / `structure.css` / `mobile.css` (mechanics), `components.html` (every component and state), the 13 screen files, `IMPLEMENTATION.md` (how to build it in podkit) and `contrast.py` (the audit; exits 0 when every pair passes).

Open `projects.html` (or any screen) from `file://`; theme with `?theme=dark|light`, motion off with `?still=1`. Cross-document view transitions run when served over http (`python3 -m http.server -d design/finalists 8765`).

## 1. Principles

1. **Everything is a titled panel.** A 1 px bordered block with a header band and a body; no free-floating cards.
2. **The variables bar scopes the page.** Team, project, env, server and range sit in one bar under the top navigation. In the prototype it is visual; in podkit it should filter every panel.
3. **Numbers are the hero.** Values use IBM Plex Mono; labels are small mono caps.
4. **Dense but aligned.** Radius 2 to 3 px, 12 px gaps, no shadows except overlays.
5. **Telemetry motion.** 100 / 180 / 300 ms, ease-out; charts draw in.

**Dark and light.** Dark `#0f1115` with orange `#ff9830`; light `#f1f2f4` grey board with white panels and a deeper `#bd5200` orange. Series colours are re-derived per theme to hold 3:1 as strokes.

## 2. Tokens

All colours are CSS custom properties in `tokens.css`; dark is the default, `[data-theme="light"]` overrides. Non-colour tokens are theme independent.

### 2.1 Colour

| Token | Role | Dark | Light |
|---|---|---|---|
| `--bg` | page background | `#0f1115` | `#f1f2f4` |
| `--surface` | cards, sidebar, panels | `#181b20` | `#ffffff` |
| `--surface-2` | raised: table header, hover wash, readonly fields | `#1f2329` | `#f6f7f8` |
| `--surface-3` | pressed, selected row, badge fill | `#282d35` | `#e9ebee` |
| `--border` | decorative hairline (exempt from 1.4.11) | `#2a2f37` | `#dcdfe4` |
| `--border-strong` | emphasis separators | `#454c57` | `#aeb4be` |
| `--border-ui` | control border: inputs, buttons, checkbox, switch (>= 3:1) | `#6d737c` | `#82878e` |
| `--text` | primary text | `#d9dade` | `#1f2329` |
| `--text-2` | secondary text, labels, icons | `#a5a9b1` | `#4a515c` |
| `--text-3` | meta, placeholders, timestamps (still >= 4.5:1) | `#9399a2` | `#5d6572` |
| `--accent` | the brand signal: primary button, selected fill, progress | `#ff9830` | `#bd5200` |
| `--accent-hover` | primary hover | `#ffad5c` | `#9c4300` |
| `--accent-ink` | text/icon on `--accent` | `#111111` | `#ffffff` |
| `--accent-soft` | tint behind accent text, selected rows | `#3a2810` | `#fde5d0` |
| `--accent-text` | accent used as text (links, active nav) | `#ffa447` | `#a64700` |
| `--ok` | running, success | `#73bf69` | `#1e7a2c` |
| `--warn` | building, warning | `#f2cc0c` | `#7d6500` |
| `--err` | failed, destructive | `#f9697a` | `#c2203a` |
| `--info` | deploying, informational | `#5794f2` | `#1f5fc2` |
| `--ok-soft` | tint behind ok text | `#1a2c19` | `#dcf1dd` |
| `--warn-soft` | tint behind warn text | `#322c07` | `#f7ecb8` |
| `--err-soft` | tint behind err text, banners | `#35141a` | `#fbdde2` |
| `--info-soft` | tint behind info text | `#14233f` | `#dbe7fb` |
| `--err-ink` | text on solid destructive button | `#0f1115` | `#ffffff` |
| `--focus` | focus ring (3:1 on every surface) | `#5794f2` | `#1f5fc2` |
| `--overlay` | dialog backdrop | `rgb(5 6 8 / .7)` | `rgb(20 24 30 / .4)` |
| `--grid` | chart gridlines (decorative) | `#262b33` | `#e6e8eb` |
| `--log-bg` | log viewer, code blocks | `#0b0d10` | `#f6f7f8` |
| `--skeleton-a` | skeleton base | `#1f2329` | `#e9ebee` |
| `--skeleton-b` | skeleton shimmer | `#2e343d` | `#dadde2` |
| `--chart-1` | series 1 | `#73bf69` | `#2a8a38` |
| `--chart-2` | series 2 | `#5794f2` | `#1f5fc2` |
| `--chart-3` | series 3 | `#f2cc0c` | `#a37f00` |
| `--chart-4` | series 4 | `#ff9830` | `#c25a00` |
| `--chart-5` | series 5 | `#f2495c` | `#c2203a` |
| `--chart-6` | series 6 | `#b877d9` | `#8a3fb0` |

`--border` and `--grid` are decorative hairlines (exempt from 1.4.11); `--border-ui` is the **control border**: every input, select, button, checkbox and switch outline uses it, because the round 1 borders (`--border-strong` and lighter) failed 3:1. `--err-ink` is the text colour on a solid destructive button.

### 2.2 Contrast

Computed with `contrast.py` (WCAG 2.x relative luminance; alpha colours composited over the pair's background). **194 pairs checked, 0 failures.** Full list: [`CONTRAST.md`](CONTRAST.md); raw script output: [`contrast.txt`](contrast.txt). Rendered pages were also audited in a browser (every text node, both themes, desktop and 390 px, with each dialog and popover open): no text below its threshold.

**Dark, text (need 4.5):**

| Text token | on `--bg` | on `--surface` | on `--surface-2` | on `--surface-3` |
|---|---|---|---|---|
| `--text` | 13.53 | 12.36 | 11.30 | 9.91 |
| `--text-2` | 8.02 | 7.32 | 6.70 | 5.87 |
| `--text-3` | 6.59 | 6.02 | 5.50 | 4.82 |
| `--accent-text` | 9.59 | 8.76 | 8.01 | 7.02 |
| `--ok` | 8.45 | 7.72 | 7.05 | 6.19 |
| `--warn` | 12.07 | 11.03 | 10.08 | 8.85 |
| `--err` | 6.60 | 6.03 | 5.51 | 4.83 |
| `--info` | 6.23 | 5.69 | 5.20 | 4.56 |

**Light, text (need 4.5):**

| Text token | on `--bg` | on `--surface` | on `--surface-2` | on `--surface-3` |
|---|---|---|---|---|
| `--text` | 14.09 | 15.78 | 14.71 | 13.21 |
| `--text-2` | 7.15 | 8.01 | 7.47 | 6.70 |
| `--text-3` | 5.25 | 5.88 | 5.48 | 4.93 |
| `--accent-text` | 5.32 | 5.96 | 5.56 | 4.99 |
| `--ok` | 4.84 | 5.42 | 5.05 | 4.54 |
| `--warn` | 5.02 | 5.62 | 5.24 | 4.71 |
| `--err` | 5.26 | 5.90 | 5.50 | 4.94 |
| `--info` | 5.40 | 6.04 | 5.64 | 5.06 |

**Dark, UI components (need 3.0):**

| Token | Use | Ratio (needs 3.00) |
|---|---|---|
| `--border-ui` | input, select, button, checkbox, switch outline | bg: 3.95 / surface: 3.61 / surface-2: 3.30 |
| `--focus` | focus ring | bg: 6.23 / surface: 5.69 / surface-2: 5.20 |
| `--accent` | checked fill, switch, tab bar, progress | bg: 8.81 / surface: 8.04 / surface-2: 7.35 |
| `--ok` | status dot | bg: 8.45 / surface: 7.72 |
| `--warn` | status dot | bg: 12.07 / surface: 11.03 |
| `--err` | status dot | bg: 6.60 / surface: 6.03 |
| `--info` | status dot | bg: 6.23 / surface: 5.69 |
| `--chart-1` | series 1 | bg: 8.45 / surface: 7.72 |
| `--chart-2` | series 2 | bg: 6.23 / surface: 5.69 |
| `--chart-3` | series 3 | bg: 12.07 / surface: 11.03 |
| `--chart-4` | series 4 | bg: 8.81 / surface: 8.04 |
| `--chart-5` | series 5 | bg: 5.29 / surface: 4.84 |
| `--chart-6` | series 6 | bg: 6.01 / surface: 5.49 |

**Light, UI components (need 3.0):**

| Token | Use | Ratio (needs 3.00) |
|---|---|---|
| `--border-ui` | input, select, button, checkbox, switch outline | bg: 3.23 / surface: 3.62 / surface-2: 3.37 |
| `--focus` | focus ring | bg: 5.40 / surface: 6.04 / surface-2: 5.64 |
| `--accent` | checked fill, switch, tab bar, progress | bg: 4.29 / surface: 4.80 / surface-2: 4.48 |
| `--ok` | status dot | bg: 4.84 / surface: 5.42 |
| `--warn` | status dot | bg: 5.02 / surface: 5.62 |
| `--err` | status dot | bg: 5.26 / surface: 5.90 |
| `--info` | status dot | bg: 5.40 / surface: 6.04 |
| `--chart-1` | series 1 | bg: 3.91 / surface: 4.38 |
| `--chart-2` | series 2 | bg: 5.40 / surface: 6.04 |
| `--chart-3` | series 3 | bg: 3.35 / surface: 3.75 |
| `--chart-4` | series 4 | bg: 3.94 / surface: 4.42 |
| `--chart-5` | series 5 | bg: 5.26 / surface: 5.90 |
| `--chart-6` | series 6 | bg: 5.48 / surface: 6.14 |

### 2.3 Type

**IBM Plex Sans** (variable) for UI, **IBM Plex Mono** 400 and 500 for values, ids and labels. Latin subsets.

| Step | Size | Line height | Used for |
|---|---|---|---|
| `--fs-xs` | 11 px | 1.45 (15.9 px) | mono labels, badges, kbd, axis labels |
| `--fs-sm` | 12 px | 1.45 (17.4 px) | secondary text, buttons, variables bar, table cells |
| `--fs-md` | 13 px | 1.45 (18.8 px) | body, inputs, panel titles |
| `--fs-lg` | 15 px | 1.2 (18.0 px) | brand, dialog titles |
| `--fs-xl` | 18 px | 1.2 (21.6 px) | mono stat values |
| `--fs-2xl` | 24 px | 1.2 (28.8 px) | page titles, panel stat values |
| `--fs-3xl` | 32 px | 1.1 (35.2 px) | large stat, sign-in |
| `--fs-4xl` | 44 px | 1.1 (48.4 px) | 404 title |

Weights: regular 400, medium 500, bold 600. Label tracking `--tracking-label: .03em`; `text-wrap: pretty` on body text; tabular numerals on every number column (`font-variant-numeric: tabular-nums`).

### 2.4 Spacing, radius, size

Spacing scale (2 px base: 2, 4, 8, 12, 16, 20, 28, 40):

| Token | Value |
|---|---|
| `--s-1` | 2px |
| `--s-2` | 4px |
| `--s-3` | 8px |
| `--s-4` | 12px |
| `--s-5` | 16px |
| `--s-6` | 20px |
| `--s-7` | 28px |
| `--s-8` | 40px |

Radius:

| Token | Value |
|---|---|
| `--r-sm` | 2px |
| `--r-md` | 2px |
| `--r-lg` | 3px |
| `--r-pill` | 999px |

Layout tokens:

| Token | Value |
|---|---|
| `--ctl-h` | `30px` |
| `--page-max` | `none` |
| `--page-pad` | `var(--s-4) var(--s-5) var(--s-7)` |
| `--page-gap` | `var(--s-4)` |
| `--card-pad` | `var(--s-4)` |
| `--bw` | `1px` |
| `--icon-sw` | `1.75` |
| `--press-scale` | `.98` |

### 2.5 Elevation

Flat: panels separate by 1 px borders; menus and dialogs add a large soft shadow.

| Token | Dark | Light |
|---|---|---|
| `--sh-1` | `none` | `none` |
| `--sh-2` | `0 0 0 1px var(--border-strong)` | `0 0 0 1px var(--border-strong)` |
| `--sh-3` | `0 14px 40px rgb(0 0 0 / .6),0 0 0 1px var(--border-strong)` | `0 14px 40px rgb(20 24 30 / .22),0 0 0 1px var(--border-strong)` |

### 2.6 Motion tokens

| Token | Value |
|---|---|
| `--dur-1` | 100ms |
| `--dur-2` | 180ms |
| `--dur-3` | 300ms |
| `--dur-vt` | 200ms |
| `--ease` | cubic-bezier(.3,0,.3,1) |
| `--ease-out` | cubic-bezier(.2,.7,.3,1) |
| `--ease-spring` | cubic-bezier(.3,.9,.3,1) |
| `--stagger` | 30ms |

## 3. Layout and grid

**Frame:** sticky top bar (brand, team switcher, nav, search, theme), then the **variables bar**, a breadcrumb, content, and a status bar. **Content:** fluid (`--page-max: none`), padded with `--page-pad`, gap `--page-gap`. **Panels:** auto-fill grids of bordered panels; detail pages use a header panel, a stat strip, then panels. See `concept.css` for the exact tracks.

**Breakpoints.** One real breakpoint at **760 px** (`mobile.css`): the frame becomes a brand strip on top and a five-item tab bar at the bottom (60 px, safe-area aware), page padding drops to `--s-4`, grids collapse to one column, tables become stacked rows with `data-label` captions, dialogs become bottom sheets, and every control reaches 44 px. The variables bar becomes a horizontally scrolling strip and stops being sticky.

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

**Panelboard skin.** Header bands in `--surface-2`, mono uppercase micro-labels, mono status badges, coloured values, dashed chart gridlines with legend swatches, underline for the current nav item.

## 5. Motion spec

Quick enough for dashboards that refresh; data draws in and little else moves.

| What | Duration | Easing | Notes |
|---|---|---|---|
| Hover | `--dur-1` 100 ms | `--ease` | border and title colour |
| Dialog, popover | `--dur-2` 180 ms | ease-out | opacity + 8 px translate |
| Panel reveal, toast, chart draw | `--dur-3` 300 ms | ease-out | 30 ms stagger on panels |
| Page change | `--dur-vt` 200 ms | ease-out | cross-document view transition |
| Skeleton | loop | linear | stopped under reduced motion |

**Reduced motion.** `@media (prefers-reduced-motion: reduce)` zeroes every animation and transition duration (`base.css`), stops the skeleton shimmer and the caret, and disables cross-document view transitions. State changes then happen instantly but are still announced by text (badge label, toast). `?still=1` does the same for screenshots.

## 6. Patterns: empty, loading, error

- **Empty:** dashed panel, one sentence about what will appear, up to three actions.
- **Loading:** every panel shows a skeleton with its final geometry; `aria-busy` buttons keep their width.
- **Error:** `role=alert` banner with an action; a failed resource panel shows the message in a red strip; the 404 stays inside the frame.

## 7. Iconography and logos

Lucide (`--icon-sw: 1.75`). Top navigation is text only; icons appear in the mobile tab bar and panel headers. Logos on square tiles; services without a logo get a two-letter monogram tile.

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
- Touch targets are 44 px at 390 px; at desktop density controls are 30px high (above the 24 px WCAG 2.5.8 minimum).
