# Quiet: design system

**Quiet** is Linear-style restraint: a hushed neutral canvas, one cool signal colour, hairline lists, and short eased motion.

Round 2 finalist `02-quiet-linear` (round 1 concept 02). Everything below is generated from, or checked against, the files in this folder:
`tokens.css` (all decisions), `concept.css` (what makes it Quiet), `extra.css` (shared components), `base.css` / `structure.css` / `mobile.css` (mechanics), `components.html` (every component and state), the 13 screen files, `IMPLEMENTATION.md` (how to build it in podkit) and `contrast.py` (the audit; exits 0 when every pair passes).

Open `projects.html` (or any screen) from `file://`; theme with `?theme=dark|light`, motion off with `?still=1`. Cross-document view transitions run when served over http (`python3 -m http.server -d design/finalists 8765`).

## 1. Principles

1. **Quiet by default.** Four near-neutral surfaces; the ice-blue accent marks the primary action, selection and links.
2. **List before card.** Resources and templates are hairline rows; bordered cards are for forms and dialogs.
3. **Two voices.** Hanken Grotesk for people, Spline Sans Mono for ids, ports and numbers.
4. **Speed is the animation.** 90 / 160 / 240 ms on one expo-out curve; press scales to .98.
5. **Reveal on intent.** Row metrics appear on hover or focus and stay visible on touch.

**Dark and light.** Dark `#0f1011` with ice-blue `#62b6e3`; light `#fbfbfc` with a deeper `#0b6e99` so the accent holds 4.5:1.

## 2. Tokens

All colours are CSS custom properties in `tokens.css`; dark is the default, `[data-theme="light"]` overrides. Non-colour tokens are theme independent.

### 2.1 Colour

| Token | Role | Dark | Light |
|---|---|---|---|
| `--bg` | page background | `#0f1011` | `#fbfbfc` |
| `--surface` | cards, sidebar, panels | `#141517` | `#ffffff` |
| `--surface-2` | raised: table header, hover wash, readonly fields | `#1a1b1e` | `#f4f4f6` |
| `--surface-3` | pressed, selected row, badge fill | `#222327` | `#ebebef` |
| `--border` | decorative hairline (exempt from 1.4.11) | `#232428` | `#e8e8ec` |
| `--border-strong` | emphasis separators | `#383a40` | `#c9cad2` |
| `--border-ui` | control border: inputs, buttons, checkbox, switch (>= 3:1) | `#6a6b71` | `#86878d` |
| `--text` | primary text | `#e8e8ea` | `#1b1c20` |
| `--text-2` | secondary text, labels, icons | `#a3a4ab` | `#565862` |
| `--text-3` | meta, placeholders, timestamps (still >= 4.5:1) | `#8f909b` | `#686a76` |
| `--accent` | the brand signal: primary button, selected fill, progress | `#62b6e3` | `#0b6e99` |
| `--accent-hover` | primary hover | `#82c6ec` | `#085a7e` |
| `--accent-ink` | text/icon on `--accent` | `#06141c` | `#ffffff` |
| `--accent-soft` | tint behind accent text, selected rows | `#14262f` | `#e0f0f8` |
| `--accent-text` | accent used as text (links, active nav) | `#7cc4ea` | `#0b6e99` |
| `--ok` | running, success | `#4cc38a` | `#17744a` |
| `--warn` | building, warning | `#e5a93b` | `#8a5a00` |
| `--err` | failed, destructive | `#ef6b6b` | `#b3261f` |
| `--info` | deploying, informational | `#6ea8fe` | `#1d5fd1` |
| `--ok-soft` | tint behind ok text | `#12261d` | `#dcf2e6` |
| `--warn-soft` | tint behind warn text | `#2a2210` | `#f8ecd0` |
| `--err-soft` | tint behind err text, banners | `#2d1717` | `#fbe0de` |
| `--info-soft` | tint behind info text | `#14213a` | `#e0eafb` |
| `--err-ink` | text on solid destructive button | `#0f1011` | `#ffffff` |
| `--focus` | focus ring (3:1 on every surface) | `#7cc4ea` | `#0b6e99` |
| `--overlay` | dialog backdrop | `rgb(5 6 8 / .6)` | `rgb(20 22 30 / .35)` |
| `--grid` | chart gridlines (decorative) | `#1d1e22` | `#ececf0` |
| `--log-bg` | log viewer, code blocks | `#0b0c0d` | `#f4f4f6` |
| `--skeleton-a` | skeleton base | `#1a1b1e` | `#ececf0` |
| `--skeleton-b` | skeleton shimmer | `#26272b` | `#e0e0e6` |
| `--chart-1` | series 1 | `#62b6e3` | `#0b6e99` |
| `--chart-2` | series 2 | `#4cc38a` | `#17744a` |
| `--chart-3` | series 3 | `#e5a93b` | `#a86a00` |
| `--chart-4` | series 4 | `#b69cf0` | `#6d4bc2` |
| `--chart-5` | series 5 | `#ef6b6b` | `#c4302b` |
| `--chart-6` | series 6 | `#5fd0c5` | `#0c7f78` |

`--border` and `--grid` are decorative hairlines (exempt from 1.4.11); `--border-ui` is the **control border**: every input, select, button, checkbox and switch outline uses it, because the round 1 borders (`--border-strong` and lighter) failed 3:1. `--err-ink` is the text colour on a solid destructive button.

### 2.2 Contrast

Computed with `contrast.py` (WCAG 2.x relative luminance; alpha colours composited over the pair's background). **194 pairs checked, 0 failures.** Full list: [`CONTRAST.md`](CONTRAST.md); raw script output: [`contrast.txt`](contrast.txt). Rendered pages were also audited in a browser (every text node, both themes, desktop and 390 px, with each dialog and popover open): no text below its threshold.

**Dark, text (need 4.5):**

| Text token | on `--bg` | on `--surface` | on `--surface-2` | on `--surface-3` |
|---|---|---|---|---|
| `--text` | 15.57 | 14.93 | 14.07 | 12.83 |
| `--text-2` | 7.67 | 7.36 | 6.93 | 6.32 |
| `--text-3` | 6.01 | 5.77 | 5.44 | 4.96 |
| `--accent-text` | 9.92 | 9.52 | 8.97 | 8.18 |
| `--ok` | 8.60 | 8.25 | 7.77 | 7.09 |
| `--warn` | 9.13 | 8.76 | 8.26 | 7.53 |
| `--err` | 6.34 | 6.08 | 5.73 | 5.22 |
| `--info` | 7.89 | 7.57 | 7.13 | 6.50 |

**Light, text (need 4.5):**

| Text token | on `--bg` | on `--surface` | on `--surface-2` | on `--surface-3` |
|---|---|---|---|---|
| `--text` | 16.46 | 17.02 | 15.50 | 14.32 |
| `--text-2` | 6.84 | 7.08 | 6.44 | 5.95 |
| `--text-3` | 5.19 | 5.37 | 4.89 | 4.52 |
| `--accent-text` | 5.48 | 5.67 | 5.16 | 4.77 |
| `--ok` | 5.59 | 5.78 | 5.26 | 4.86 |
| `--warn` | 5.73 | 5.93 | 5.40 | 4.98 |
| `--err` | 6.32 | 6.53 | 5.95 | 5.50 |
| `--info` | 5.63 | 5.82 | 5.30 | 4.89 |

**Dark, UI components (need 3.0):**

| Token | Use | Ratio (needs 3.00) |
|---|---|---|
| `--border-ui` | input, select, button, checkbox, switch outline | bg: 3.59 / surface: 3.44 / surface-2: 3.24 |
| `--focus` | focus ring | bg: 9.92 / surface: 9.52 / surface-2: 8.97 |
| `--accent` | checked fill, switch, tab bar, progress | bg: 8.45 / surface: 8.11 / surface-2: 7.64 |
| `--ok` | status dot | bg: 8.60 / surface: 8.25 |
| `--warn` | status dot | bg: 9.13 / surface: 8.76 |
| `--err` | status dot | bg: 6.34 / surface: 6.08 |
| `--info` | status dot | bg: 7.89 / surface: 7.57 |
| `--chart-1` | series 1 | bg: 8.45 / surface: 8.11 |
| `--chart-2` | series 2 | bg: 8.60 / surface: 8.25 |
| `--chart-3` | series 3 | bg: 9.13 / surface: 8.76 |
| `--chart-4` | series 4 | bg: 8.17 / surface: 7.83 |
| `--chart-5` | series 5 | bg: 6.34 / surface: 6.08 |
| `--chart-6` | series 6 | bg: 10.26 / surface: 9.84 |

**Light, UI components (need 3.0):**

| Token | Use | Ratio (needs 3.00) |
|---|---|---|
| `--border-ui` | input, select, button, checkbox, switch outline | bg: 3.46 / surface: 3.58 / surface-2: 3.26 |
| `--focus` | focus ring | bg: 5.48 / surface: 5.67 / surface-2: 5.16 |
| `--accent` | checked fill, switch, tab bar, progress | bg: 5.48 / surface: 5.67 / surface-2: 5.16 |
| `--ok` | status dot | bg: 5.59 / surface: 5.78 |
| `--warn` | status dot | bg: 5.73 / surface: 5.93 |
| `--err` | status dot | bg: 6.32 / surface: 6.53 |
| `--info` | status dot | bg: 5.63 / surface: 5.82 |
| `--chart-1` | series 1 | bg: 5.48 / surface: 5.67 |
| `--chart-2` | series 2 | bg: 5.59 / surface: 5.78 |
| `--chart-3` | series 3 | bg: 4.30 / surface: 4.44 |
| `--chart-4` | series 4 | bg: 5.91 / surface: 6.11 |
| `--chart-5` | series 5 | bg: 5.34 / surface: 5.52 |
| `--chart-6` | series 6 | bg: 4.70 / surface: 4.86 |

### 2.3 Type

**Hanken Grotesk** (variable) for UI and headings, **Spline Sans Mono** (variable) for machine values. Latin subsets.

| Step | Size | Line height | Used for |
|---|---|---|---|
| `--fs-xs` | 11.5 px | 1.5 (17.2 px) | meta, captions, kbd, tab counts |
| `--fs-sm` | 12.5 px | 1.5 (18.8 px) | secondary text, buttons, table cells |
| `--fs-md` | 13.5 px | 1.5 (20.2 px) | body, inputs, names |
| `--fs-lg` | 15 px | 1.2 (18.0 px) | card titles |
| `--fs-xl` | 18 px | 1.2 (21.6 px) | dialog titles, empty-state titles, stat values |
| `--fs-2xl` | 22 px | 1.2 (26.4 px) | page titles, including the name on an application, database or stack page |
| `--fs-3xl` | 28 px | 1.1 (30.8 px) | sign-in title |
| `--fs-4xl` | 38 px | 1.1 (41.8 px) | 404 title, large display figure |

Weights: regular 400, medium 500, bold 600. Label tracking `--tracking-label: 0`; `text-wrap: pretty` on body text; tabular numerals on every number column (`font-variant-numeric: tabular-nums`).

### 2.4 Spacing, radius, size

Spacing scale (2 px base with a 4 / 8 rhythm: 2, 4, 8, 12, 16, 24, 32, 48):

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
| `--r-sm` | 4px |
| `--r-md` | 6px |
| `--r-lg` | 8px |
| `--r-pill` | 999px |

Layout tokens:

| Token | Value |
|---|---|
| `--nav-w` | `232px` |
| `--ctl-h` | `30px` |
| `--page-max` | `1080px` |
| `--page-pad` | `var(--s-7)` |
| `--page-gap` | `var(--s-6)` |
| `--card-pad` | `var(--s-5)` |
| `--bw` | `1px` |
| `--icon-sw` | `1.75` |
| `--press-scale` | `.98` |

### 2.5 Elevation

Almost flat: cards sit on a hairline; menus and dialogs add a soft shadow.

| Token | Dark | Light |
|---|---|---|
| `--sh-1` | `0 1px 0 rgb(255 255 255 / .02) inset` | `none` |
| `--sh-2` | `0 4px 16px rgb(0 0 0 / .3)` | `0 4px 16px rgb(20 22 30 / .08)` |
| `--sh-3` | `0 16px 48px rgb(0 0 0 / .55),0 0 0 1px var(--border-strong)` | `0 16px 48px rgb(20 22 30 / .18),0 0 0 1px var(--border)` |

### 2.6 Motion tokens

| Token | Value |
|---|---|
| `--dur-1` | 90ms |
| `--dur-2` | 160ms |
| `--dur-3` | 240ms |
| `--dur-vt` | 180ms |
| `--ease` | cubic-bezier(.2,.8,.2,1) |
| `--ease-out` | cubic-bezier(.2,.8,.2,1) |
| `--ease-spring` | cubic-bezier(.2,.8,.2,1) |
| `--stagger` | 25ms |

## 3. Layout and grid

**Frame:** sidebar `--nav-w` wide with team switcher and nav; sticky top bar with breadcrumb, search and theme. **Content:** centred, max width `--page-max`, padded with `--page-pad`, gap `--page-gap`. **Lists:** resources and templates are hairline rows; projects are tiles. See `concept.css` for the exact tracks.

**Breakpoints.** One real breakpoint at **760 px** (`mobile.css`): the frame becomes a brand strip on top and a five-item tab bar at the bottom (60 px, safe-area aware), page padding drops to `--s-4`, grids collapse to one column, tables become stacked rows with `data-label` captions, dialogs become bottom sheets, and every control reaches 44 px. List rows drop secondary columns on narrow viewports.

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

**Quiet skin.** Rounded 4 to 8 px, borderless tiles that gain a hairline on hover, status as a dot plus word, borderless charts, underline tabs, accent primary button.

## 5. Motion spec

One curve, `cubic-bezier(.2,.8,.2,1)`, three durations; motion explains cause and effect and never loops except skeletons and the building dot.

| What | Duration | Easing | Notes |
|---|---|---|---|
| Hover, press | `--dur-1` 90 ms | ease-out | background wash; press scales .98 |
| Dialog, popover | `--dur-2` 160 ms | ease-out | opacity + small translate and scale |
| Toast, status flip, list reveal | `--dur-3` 240 ms | ease-out | 25 ms stagger on lists |
| Page change | `--dur-vt` 180 ms | ease-out | cross-document view transition |
| Skeleton | loop | linear | stopped under reduced motion |

**Reduced motion.** `@media (prefers-reduced-motion: reduce)` zeroes every animation and transition duration (`base.css`), stops the skeleton shimmer and the caret, and disables cross-document view transitions. State changes then happen instantly but are still announced by text (badge label, toast). `?still=1` does the same for screenshots.

## 6. Patterns: empty, loading, error

- **Empty:** dashed panel, icon tile, one sentence, up to three actions.
- **Loading:** skeleton rows with the final geometry, then a staggered reveal; `aria-busy` buttons keep their width.
- **Error:** `role=alert` banner with an action; failed resources turn red with the real message; field errors under the field; the 404 stays inside the frame.

## 7. Iconography and logos

Lucide (`--icon-sw: 1.75`). Logos on rounded tiles; services without a logo get a two-letter monogram tile with a tint.

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

## 10. Where podkit goes beyond the mockups (from the Q8 review)

States and parts the mockups did not draw, as built in podkit and kept because they follow the system above. `REVIEW.md` has the full punch list.

- **Environment tab row**: the tabs and their actions share one hairline that runs the whole width; the 2 px accent underline sits on it.
- **Dialogs filled by a patch** (template deploy, model set-up): the same head, body and foot as every other dialog. The wrapper that carries the id for the patch adds no box (`.ui-dlg-panel`, `display: contents`); the close button lives in the head.
- **Copy-once secrets** (webhook secret): a warning banner (`Copy the secret now.`), not coloured body text.
- **Option cards** (database engine, recipe): the selected card takes the primary border on `--accent`; hover is `--surface-2`.
- **Type scale for utility classes**: templates written with Tailwind names map onto Quiet's steps (`text-xs` 11.5, `text-sm` 13.5, `text-base` 15, `text-lg` 18, `text-2xl` 22 px) in one place (`components.css`, "design review"); no page has a size off the scale.
- **Scrollbars**: thin, thumb at 65 % of the muted text colour on both themes, also in text areas and logs.
- **On a phone**, the breadcrumb keeps the current page's name and gives the crumbs before it up first.

## 11. Patterns from the infra batch (from the DR review)

New parts and rules from the infrastructure batch, as built. `REVIEW-infra.md` has the punch list and `review-infra/` the screenshots.

**Page head.** A page you reach from the sidebar (Projects, Servers, Networks, Keys, Registries, Git providers, Security, Overview) has no eyebrow: the sidebar group and the breadcrumb already say where you are, and the title must not move when you go down the sidebar. An eyebrow names the parent only on a page below one (project, server, application, the IaC page). The title takes the row first and the actions follow, so a long description wraps beside the buttons; below 760 px the buttons go under it.

**Save bar** (`components/save_bar.rs`, `.ui-save-bar`). The one place a settings form is saved from. It is not rendered while the form is clean; once a field differs from the snapshot taken at load it slides up 24 px and fades in (160 ms, `--ease-out`) at the bottom of the page, sticks there while the form is taller than the window, and says "Unsaved changes" with Discard and Save. Surface is `--popover` with `--sh-3`, radius `--r-lg`, padding `--s-3` `--card-pad`. On a phone it sits above the tab bar. Reduced motion: no slide. Leaving a dirty page asks first (`guard.js`, one definition of "unsaved" for the bar and the guard). One bar per page.

**Security badge** (`components/security_badge.rs`, `.ui-sev`). A pill with a shape and a word: Clean (check, success), Low (ring, muted), Medium (rounded square, warning), High (triangle) and Critical (diamond), both destructive. `Act now` is a separate outlined pill beside it. Text is `--fs-xs`, padding `--s-1` `--s-3`, gap `--s-2`, radius `--r-pill`. A resource that was never scanned has no badge; a list says "Not scanned yet" in plain text, not as a pill. Never colour alone.

**Network chips** (`.ui-infra-chip`, `.ui-infra-count`). A network or a resource named on the overview: 28 px, 1 px `--border`, `--r-md`, `--fs-sm`, hover `--accent`, with the member count in `--muted-foreground` after it. The kind of a network is a badge in the cell: Environment in the progress tone (blue), User and Imported neutral; the outbound column says "Outbound" (neutral) or "No outbound" (warning). Every environment has a network, so Environment is the common case.

**Rows inside a card.** Use `ul.ui-card-rows > li.ui-card-row`, never utilities: a row's text starts on the card's own inset (`--card-pad`, 16 px), the same line as the header and the body. An empty state inside a card is the card's content (`.ui-card .ui-empty`, flush, no box of its own), with no wrapper.

**Tab panels.** The first card is 32 px under the tab row on every application tab. A status slot (`#saved`) that holds nothing takes no space (`display: none` until a message is patched in), so it adds no gap.

**Stacked tables** (below 36 rem of their own width). A row whose last cell is only a row menu puts the menu in the card's top-right corner, not on a row of its own.

**Sidebar rail.** The rail folds to 56 px by easing the grid column; the icons never change `justify-content`, so each stays on one vertical line through the fold and only its side padding eases to the centered value (use fixed numbers, not a percentage, which resolves against the column while it is still wide).

**Canvas** (`design/canvas/README.md` is the spec; `server/src/ui/pages/canvas` the build). Differences from the mockup that stay: nodes with no links all sit in layer 0, so a project without links is one tall column; a frame narrower than 340 px gets a tighter two-line head (name, then the count) that ends above its first node; the Canvas / List switch is the boxed group (2 px inset, selected tab on `--muted-strong`), not underline tabs.

**IaC plan.** Each change is a card: a kind tag (Create on success, Update on warning, Delete on destructive), the resource kind and name, the environment, and "Will redeploy" in the info tone; under it the changed fields as a diff, one line each with a sign (`+` or `-`), tinted `--success-soft` or `--destructive-soft`. Sign, word and tint together: never colour alone. The summary row counts each kind. Apply is a primary button below the last change; it opens a confirm dialog.

## 12. Patterns from wave A (from the DR-A review)

New parts and rules from wave A (notifications, audit log, shared variables, health gate, scheduled tasks, MCP servers, volume backups, uptime and the status page, environment clone, charts). `REVIEW-wave-A.md` has the punch list and `review-wave-A/` the screenshots.

**Page rhythm.** A page's head and its cards sit in one grid with the page gap (`--page-gap`, 24 px): use `ui-page-body` (with `ui-wide` where the page needs the wide column). A bare `ui-wide` div, or a `ui-stack` (12 px), is not a grid with that gap, and the first card then sat 12 px under the lede and touched the head's buttons (scheduled tasks, notification history).

**Groups on a long settings page** (Settings, Notifications). Inside a section the heading, the list and its add form are 12 px apart; between sections it is 32 px (`.ui-notif-sections`), so a section reads as a group. A stack of sections that are only 12 px apart reads as one long list.

**Empty states.** Inside a card the icon and text start on the card's inset (`--card-pad`); alone on a page the box is dashed. The icon must be in the sprite (`server/static/icons.svg`, names in `scripts/vendor-ui-assets.py`): a missing one draws an empty box and nothing reports it. `plug` was added for the MCP tab.

**A list of rows in a card body** (the uptime checks): the body takes no vertical padding (`.ui-card-b:has(> ul > .ui-probe-row)`), the rows keep their own 12 px, so the first row is 16 px under the header like every other list.

**A lone chart takes the row.** The two-column chart grid lets a single chart (a check's response time) span both columns (`#metrics-body > div > :only-child`). Two or more charts keep the two columns.

**Facts as a row** (volume backup settings): label over value, one column per fact, wrapping to two at 390 px (`.ui-vb-facts`, `repeat(auto-fit, minmax(8rem, 1fr))`). Facts as lines (`.ui-fact`, `.ui-dest-facts`) stay for a resource's detail card. `.ui-facts` is defined three times in `components.css`; a new use takes its own class instead of relying on which rule wins.

**Public pages follow the reader's scheme.** The status page sets no `class` on `<html>` and `color-scheme: light dark`, so a dark system gets the dark tokens, with the same banner, strip and chip colours as the app. It still loads no script and no cookie.

**Notification rows.** An unread row is tinted `--primary-soft` and carries a word (`New`), a severity chip and the time; the time and the repeat count are separated by a middle dot. At 390 px the Open / Mark read buttons go under the text instead of taking a column from it.

**Choice cards.** Three options (the overlap policy of a task) share the row (`repeat(auto-fit, minmax(min(100%, 13rem), 1fr))`); at 390 px they stack.

**Dialogs.** The first field has `autofocus`; a destructive confirmation focuses the safe button (Cancel, Keep it). Every wave A dialog was opened and checked; the volume backup settings dialog used to focus the close button.

**Where a form is saved from (open).** The application Settings, a scheduled task's settings and the audit retention use the save bar. The uptime check's settings, the status page card, the email server and the noise control keep a `Save` in their card footer. Both work; pick one for edit forms (the save bar) and keep footer buttons for add forms.
