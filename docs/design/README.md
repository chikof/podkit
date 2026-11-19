# podkit design system

The spec for the web UI. Every page card follows it; `/_ui` shows each piece in every
state and both color schemes. Where this file and the code disagree, fix the one that is
wrong in the same change.

Code: tokens in `server/src/ui/theme.css`, platform-feature rules in
`server/src/ui/components.css`, components in `server/src/ui/components/` (one file each).
Assets are vendored in `server/static/` and listed in `NOTICE`.

## Principles

1. **Quiet and dense enough to work in.** podkit is an operator's tool for a homelab:
   neutral surfaces, 1px borders, small type, generous whitespace between groups. Color
   means state (running, failed), never decoration.
2. **Dark first, light kept.** The default look is dark. A system that asks for light, or
   a user who picks it, gets the matching light values. Both are designed, not inverted.
3. **Look like Dokploy, think like a form.** The reference is Dokploy's shadcn neutral
   palette, left sidebar and logo cards. Coolify is the contrast: busier, more color,
   heavier chrome. We take Dokploy's restraint. (We do not copy either product's images or
   layouts; only the principles.)
4. **Server-rendered, native first.** `<dialog>`, `popover`, `command`/`commandfor`,
   anchor positioning, `@starting-style`, view transitions, container queries and `:has()`
   do the work. Script is for what the platform lacks (`podkit.js`, ~150 lines) and
   Datastar handles actions.
5. **Works offline and over plain HTTP.** No CDN: fonts, icons and logos are served from
   the binary. Nothing depends on a secure context.
6. **Progressive enhancement.** Every component degrades to a plainer, working one in a
   browser without the feature. Motion is an enhancement and respects
   `prefers-reduced-motion`.
7. **Say what happened and what to do.** Empty, loading and error states are designed
   (see below), not left blank.

## Tokens

All colors are semantic CSS variables in `theme.css`, shadcn "neutral" values in oklch,
each written once as `light-dark(light, dark)`. Use them as utilities (`bg-card`,
`text-muted-foreground`, `border-border`, `ring-ring/50`). Never write a literal color in a
template or in `components.css`.

### Color

| Token | Use | Light | Dark |
|---|---|---|---|
| `background` / `foreground` | page, default text | `oklch(1 0 0)` / `oklch(0.145 0 0)` | `oklch(0.145 0 0)` / `oklch(0.985 0 0)` |
| `card` / `card-foreground` | cards, panels | `oklch(1 0 0)` | `oklch(0.205 0 0)` |
| `popover` / `popover-foreground` | menus, dialogs, tooltips, toasts | `oklch(1 0 0)` | `oklch(0.205 0 0)` |
| `primary` / `primary-foreground` | the one main action | `oklch(0.205 0 0)` | `oklch(0.922 0 0)` |
| `secondary` / `secondary-foreground` | supporting buttons | `oklch(0.97 0 0)` | `oklch(0.269 0 0)` |
| `muted` / `muted-foreground` | quiet surfaces, secondary text | `oklch(0.97 0 0)` / `oklch(0.5 0 0)` | `oklch(0.269 0 0)` / `oklch(0.708 0 0)` |
| `accent` / `accent-foreground` | hover and selected rows | `oklch(0.97 0 0)` | `oklch(0.269 0 0)` |
| `destructive` / `destructive-foreground` | delete, stop, failed | `oklch(0.577 0.245 27.3)` | `oklch(0.704 0.191 22.2)` |
| `success` | healthy, done | `oklch(0.5 0.14 150)` | `oklch(0.75 0.17 150)` |
| `warning` | needs attention | `oklch(0.52 0.13 70)` | `oklch(0.82 0.15 85)` |
| `info` | in progress | `oklch(0.5 0.17 255)` | `oklch(0.74 0.12 250)` |
| `border` | 1px dividers, card edges | `oklch(0.922 0 0)` | `oklch(1 0 0 / 10%)` |
| `input` | form control borders | `oklch(0.922 0 0)` | `oklch(1 0 0 / 15%)` |
| `ring` | focus ring | `oklch(0.708 0 0)` | `oklch(0.556 0 0)` |
| `sidebar`, `sidebar-foreground`, `sidebar-primary(-foreground)`, `sidebar-accent(-foreground)`, `sidebar-border`, `sidebar-ring` | the left navigation | as `card`/`primary`/`accent`/`border`/`ring`, with `sidebar` one step off the page (`0.985` light, `0.205` dark) | |
| `chart-1` … `chart-5` | data series, in this order | see `theme.css` | |

Text and its background keep at least 4.5:1 (3:1 for large text and UI borders).
`muted-foreground` on `background`, `card` and `muted` passes in both schemes; do not use it
on `accent` for anything small.

### Radius, spacing, type, shadow, motion

| Token | Value | Notes |
|---|---|---|
| `--radius` | `0.625rem` (10px) | cards `rounded-xl`, controls `rounded-md`, badges `rounded-full`, tiles `rounded-lg` |
| Spacing | Tailwind's 4px scale | inside a card `px-6`/`py-6`, between its blocks `gap-5`, form fields `gap-2` (label to control) and `gap-4` (field to field), sections of a page `gap-6` or `gap-10` |
| Font | Hanken Grotesk (`--font-sans`), Spline Sans Mono (`--font-mono`) | variable woff2, `font-display: swap`; mono for ids, hashes, branches, log lines, keys |
| Type scale | `text-xs` 12 · `text-sm` 14 (default for UI) · `text-base` 16 (card titles) · `text-lg` 18 (dialog titles, section headings) · `text-2xl` 24 (page title) | weights 400, 500 (labels, buttons), 600 (titles); never bold above 600 |
| Shadows | `--shadow-xs` controls, `--shadow-sm` raised cards, `--shadow-md` menus, `--shadow-lg` dialogs and toasts | in dark the border does most of the work |
| Motion | `--duration-fast` 120ms, `--duration-base` 200ms, `--ease-out` | see Motion |
| Widths | page content `max-w-4xl` (forms, detail) to `max-w-7xl` (lists, gallery); dialogs 32rem, wide 48rem | |

Layout: a left sidebar (`sidebar` tokens) 15rem wide from `md` up (3.5rem folded to icons), a
modal drawer below `md`; a header strip with breadcrumb, search and the sidebar toggle;
content on `background`. The shell is described in "App shell" below.

## Theme

`light`, `dark` or `system` (default). The choice is the `theme` cookie, written by
`POST /theme` (the toggle in the header is a plain form) and read by the server, which
renders `<html class="dark">` or `class="light"` (no class means system) and a matching
`<meta name="color-scheme">`. The class is in the first byte of the response, so nothing
flashes on load or on reload. Do not add script to apply the theme.

Dark is the base in CSS; light applies when the system asks for it and there is no `dark`
class, or when `light` is chosen.

## Components

Each is a function or constant set in `server/src/ui/components/<name>.rs`, shown on
`/_ui`. Datastar attributes cannot pass through a maud function, so any element that
carries them (inputs with `data-bind`, buttons with `data-on:click`) is written in the page
with the class constants (`BUTTON`, `INPUT`, `LABEL`, `TEXTAREA`, `SELECT`, `MENU_ITEM`, …)
or `button_class(variant, size)`.

General do's and don'ts:

- **Do** one `Default` (primary) button per view; the rest `Outline`/`Ghost`.
- **Do** give every icon-only control an accessible name.
- **Don't** invent a variant or hand-roll a control: add it to the system.
- **Don't** put user data through `PreEscaped`; every component escapes its inputs.
- **Don't** convey state by color alone: a dot or badge always carries its word.

### Button (`button.rs`)

Variants: `Default` (the main action), `Secondary`, `Outline` (usual second button),
`Ghost` (toolbars, rows), `Destructive` (deletes or stops), `Link`. Sizes `Sm` (32px), `Md`
(36), `Lg` (40), `Icon` (36 square); 44px minimum on touch screens (handled in CSS).
`BUTTON`, `BUTTON_OUTLINE`, `BUTTON_DANGER` are the legacy constants (default, outline, and
a quiet bordered destructive). `link_button` renders an `<a>` styled as a button.
Do: use `Destructive` only behind a confirmation dialog. Don't: put two `Default` buttons
side by side; use a button for navigation (use a link).

### Icon button (`button.rs`)

`icon_button(variant, label, icon)`: square, `aria-label` required.

### Input, textarea, select, checkbox, switch, label, help and error text (`field.rs`)

`text_field`, `textarea_field`, `select_field`, `checkbox_field`, `switch_field` render a
whole group (label for the control, help, error, `aria-describedby`, `aria-invalid`).
`help_text(id, text)` is muted; `error_text(id, text)` is `role="alert"`.
Invalid styling uses `:user-invalid`: a field turns red after the user touched it, never on
load; a server-side error sets `aria-invalid="true"` and renders `error_text`.
The select is a native `<select>`; with `appearance: base-select` support (Chrome, Safari)
the picker is styled too, elsewhere the platform picker stays. Passwords are never echoed.
Do: a visible label on every control; put the format in help text. Don't: use placeholder
text as a label; use a switch for a form answer (it applies at once).

### Card (`card.rs`)

`card`, `card_header` (an `<h1>`), `card_section` (an `<h2>` with an optional action),
`card_content`, `card_footer` (actions at the end), `card_title`, `card_description`.
A card is a container-query container: its header wraps the action below the text when the
card is narrow, and a footer stacks. Do: one subject per card. Don't: nest cards.

### Logo card (`card.rs`, `logo.rs`)

`link_card(href, logo, title, description, meta)`: one service or resource; the title's
link covers the whole card, so there is a single tab stop. In wide containers the logo sits
beside the text, in narrow ones above. Use it in a `ui-card-grid` (auto-fill columns).
`logo(name, LogoSize::{Sm, Md, Lg})` shows the mark when podkit has one (Simple Icons, in
the text color, so it reads in both schemes), else a monogram of the name's first letter.
It is decorative; the title carries the name. Add marks in
`scripts/vendor-ui-assets.py`, not by hand.

### Badge and status dot (`badge.rs`)

`badge(Tone, text)` labels a fact. `status_dot(Status)` and `status_badge(Status)` show the
state of something that runs: **Running**, **Building**, **Failed**, **Stopped**,
**Pending**. Status colors:

| Status | Tone | Dot | Meaning |
|---|---|---|---|
| Running | success | green | healthy and serving |
| Building | info | blue, pulsing | podkit is working on it |
| Failed | destructive | red | it ended in error; say why next to it |
| Stopped | neutral | grey | deliberately not running, or cancelled |
| Pending | warning | amber | waiting on something outside podkit (DNS, a host key) |

Page-specific enums (deployment, server, backup) map onto these through `ui::status`.

### Tabs (`tabs.rs`)

`tabs(label, &[TabItem { label, href, current }])`: plain links in a `<nav>`, the current
one has `aria-current="page"`. Each click is a page load. Scrolls sideways when narrow.
Don't: use it for a client-side panel switch.

### Table (`table.rs`)

`table(id, caption, headers, rows)`: semantic `<table>`, sticky header, first cell of each
row a row header, wrapped in a keyboard-focusable named region that scrolls. Below 36rem of
its own width every row becomes a block and each cell shows its column name. Truncate long
cells with `truncate` and a `title`, never wrap them into narrow columns. Do: right-align
nothing but numbers; put row actions in a last-column menu.

### Chart and metrics section (`ui/charts.rs`, `components/metrics.rs`)

Charts are inline SVG drawn on the server, with no library and no script. `line_chart(&Chart)`
is a pure function (same data, same markup; every label escaped): a `<figure>` with the
title and each series' latest value, an `<svg role="img">` with five y ticks and five x ticks
(UTC), one path per run of readings (a missing reading, or a hole longer than three usual
steps, breaks the line rather than being bridged), and a hover `<title>` per reading. It is
described by `aria-describedby` to a visually hidden sentence (count, latest, average, min,
max) and has a table of the readings (at most 30 rows) behind the `_tables` signal. Series
colors are `chart-1` … `chart-5`, in that order; a percentage of a whole fixes its axis at
100, a byte count takes the capacity (or ticks at powers of two). `sparkline` is a card's
decoration: no axes, `aria-hidden`, the number sits beside it in text.

`metrics_section(&MetricsSection, &metrics_body(..))` is the frame a resource page embeds:
range links (`?range=1h|24h|7d`, `aria-current`), a "Data tables" toggle, and a Datastar
refresh of `#metrics-body` every 30 seconds written as `document.hidden || @get(...)`, so a
hidden tab sends nothing, plus one request when the tab is visible again. `resource_charts`
gives CPU and memory for an application, database, model or stack; the page's refresh route
answers with `sse::patch(&resource_charts(..))` from `metrics::service`. Do: pass the window
(`from`, `to`) so an empty hour still shows an hour. Don't: bridge a gap, draw a chart for no
readings (say so instead), or put a figure in a chart that is not in its table.

### Dialog (`dialog.rs`)

A native modal `<dialog>`. `dialog_open_button(id, class, label)` renders the invoker
(`command="show-modal" commandfor=…`) plus a Datastar fallback for browsers without invoker
commands. The browser provides the inert page, focus trap, focus return, Esc and
`::backdrop`; do not add `role="dialog"`, `aria-modal`, a focus trap or an Esc handler.
`Dismiss::Esc` (default for confirmations and forms) or `Dismiss::EscOrOutside` (read-only
content). Put `autofocus` on the safe action in a destructive confirmation, on the first
field in a form. Close with `<form method="dialog">` buttons (`dialog_cancel`). The enter
and exit use `@starting-style` and discrete transitions; Firefox and Safari skip the exit
animation (graceful).
Don't: open a dialog from a dialog; put a toast behind a modal.

### Dropdown menu (`menu.rs`)

`dropdown_menu(id, trigger_class, label, trigger, items)` with `menu_link`, `menu_label`,
`menu_separator` and command buttons using `MENU_ITEM` / `MENU_ITEM_DANGER`. A popover
anchored below its trigger with flip fallbacks (a bottom sheet where anchor positioning is
missing). Esc and a click outside close it; `podkit.js` adds arrow keys, Home/End, roles
and `aria-expanded`. Use it for actions on one thing; navigation is `tabs` or the sidebar.

### Tooltip (`tooltip.rs`)

`tooltip(id, text, trigger)`: shows on hover and keyboard focus, above the control, flipping
below at the viewport edge; stays open while the pointer is over it; Esc hides it. Give the
trigger `aria-describedby=<id>`. A tooltip names or clarifies a control; it is never the
only place something needed is said (touch users do not see it).

### Toast region and toast (`toast.rs`)

`toast_region()` is on every page (`<section id="toasts" aria-live="polite">`).
`toast(tone, title, description)` appends into it (Datastar patch into `#toasts`); a
`Danger` toast is `role="alert"`. Each has a Dismiss button. Toasts confirm a result; they
never carry the only copy of an error: show it inline too. Not for long messages.

### Skeleton (`skeleton.rs`)

`skeleton("h-4 w-40")`: the shape of what is loading, `aria-hidden`; mark the region
`aria-busy="true"`. Match the final size so the page does not jump.

### Empty state (`empty_state.rs`)

An icon, a headline that says what is missing, one sentence, at most one action. "No
applications yet. Deploy a repository or a template to see it here. [Create application]".

### Breadcrumb (`breadcrumb.rs`)

`breadcrumb(&[(label, Some(href)) …, (label, None)])`: links down to the current page,
which is `aria-current="page"`. Separators are decoration.

### Avatar (`avatar.rs`)

Initials in a round tile, `role="img"` named by the person. There are no profile pictures.

### Kbd (`kbd.rs`)

`kbd("Esc")`, `kbd_combo(&["Ctrl", "K"])`: a hint, not a shortcut handler.

### Icon (`icon.rs`)

`icon(name)` / `icon_with(name, classes)`: a Lucide icon (ISC) from `/static/icons.svg`
(hashed URL, `immutable`), `1em` square, in the text color, `aria-hidden`. The subset is
listed in `scripts/vendor-ui-assets.py`; add a name there and re-run it. Prefer an icon
plus text; icon-only needs an `aria-label` on its control.

### Theme toggle (`theme_toggle.rs`)

Three-button group (system, light, dark) in the header; a form posting to `/theme`.

## App shell

Code: `server/src/ui/shell/` (`nav`, `sidebar`, `header`, `palette`, `prefs`), entered through
`layout::app_page(ctx, title, crumbs, body)`; rules in the "App shell" part of `components.css`;
the palette's key handling is section 5 of `podkit.js`. Pages live in
`server/src/ui/pages/<area>/` (one `router()` per area, merged in `ui/mod.rs`).

- **Sidebar:** team switcher on top (a menu), the nav (Projects, Templates, Servers, Models,
  S3 Destinations; Settings: Team, Members, Registration, Hugging Face token, Appearance), the
  user menu at the bottom (theme, sign out). `shell::nav` is the one list; a link the role may not
  use is not rendered, and the palette uses the same list. It folds to icons (the panel button in
  the header; the choice is the `sidebar` cookie, so the server renders the folded state in the
  first byte and nothing jumps). Below `md` the same content is a modal `<dialog>` drawer opened
  from the header; Esc, a click outside or its close button close it.
- **Breadcrumbs:** team › project › environment › resource. Pass only the part from the section
  down; the shell adds the team and, on a project's pages (`ctx.scope`), the environment, and puts
  a switcher menu on team, project and environment. `?environment=<id>` picks the environment in
  view (the first one by default). Narrow: all but the last two fold into a "…" menu.
- **Command palette:** Ctrl/⌘-K or the header's search button open a native `<dialog>` with a
  combobox. Typing sends `GET /teams/{team}/search?q=` (debounced); the answer is a Datastar patch
  of the listbox. Focus stays in the input, ↑ ↓ move the selection, Enter opens it, Esc closes and
  focus goes back. Results are the team's own (the `search` service scopes and checks
  permissions) plus pages and actions the role may use. Actions open the page where the action
  is done; none of them mutates.
- **Flash:** an action leaves a message with `shell::flash(&cookies, Tone, "text")` before it
  redirects; the next page shows it once as a toast and drops the cookie.
- **Transitions:** same-origin navigations cross-fade; the sidebar (`ui-chrome`) and the header
  (`ui-header`) keep their place, and `<link rel="expect" href="#main" blocking="render">` holds the
  first paint until `<main>` is parsed.
- **Keyboard:** the first Tab stop is "Skip to content"; then the sidebar in reading order.

## Projects and environments

Code: `server/src/ui/pages/projects/{list,environment,picker,cards}.rs`; service reads in
`projects::overview`; rules at the end of `components.css`.

- **Projects grid** (`/teams/{t}/projects`, an Explore surface): one logo card per project: name
  and slug, up to five resource logos then "+N", counts, environments and last deploy. The whole
  card leads to the project's default environment (a stretched link); a menu holds Rename and
  Delete, each a dialog (delete asks for the slug). New project is a dialog; success redirects
  to the new project and leaves a toast (`shell::flash`). No projects: an empty state with the
  same action. Projects have no description yet (there is no column), so the card shows the slug.
- **Environment** (`/teams/{t}/projects/{p}` and `/{env_slug}`, an Operate surface): the title, one
  primary action (**Create**, which opens the picker), environment tabs (links, one `aria-current`)
  with a "+" and an actions menu (rename, delete dialogs; a refusal such as "still has
  resources" is the dialog's inline error), a search box and a type filter, and a grid of
  resource cards. `?environment=<id>` still picks one by id. An environment's slug cannot be
  `apps`, `databases`, `models` or `environments`: those are the project's other pages.
- **Resource card**: logo (the git host's mark or Docker's for an application, the engine's for a
  database, Ollama's or Hugging Face's for a model, else the name's monogram), name, a type
  badge, status dot with its word, last deployment (applications) or update (models), the engine
  or repository, and the application's public URL. While any shown resource is changing the grid
  re-fetches itself every 3 seconds with the current filter and stops when nothing changes.
- **Filter**: a real GET form (`?q=&type=`), enhanced by Datastar to patch the grid in place
  (`GET .../{env}/resources`, signals `q` and `type`). A filter that matches nothing says so and
  offers to clear it.
- **Create-resource picker**: a wide modal `<dialog>` listing what the caller may create, grouped
  as Application (Git repository), Database (PostgreSQL, MySQL, MongoDB, Redis), Compose and
  templates, and Model. Every link carries `?environment=<id>`. Options whose page does not exist
  yet show a "Soon" badge and are not links (`COMPOSE_HREF`, `TEMPLATES_HREF`); the Docker image
  option stays hidden until its API exists (`DOCKER_IMAGE_READY`).
- **Width**: a list page puts `ui-wide` on its root so the shell's wrapper grows from 4xl to 80rem.

## Empty, loading and error patterns

- **Empty:** `empty_state` in place of the list, with the single next action. A filtered
  list that matches nothing says so and offers to clear the filter.
- **Loading:** server-rendered pages are not "loading". For a Datastar action, disable the
  button with `data-indicator` and show a spinner icon (`ui-icon-spin`) and a present-tense
  label ("Deploying"). For a region that arrives later, a `skeleton` of its shape inside an
  `aria-busy` container. Never an indefinite spinner without a timeout message.
- **Error:** next to where it happened. A field error under its field (`error_text`); an
  action error in the form's `_error` signal (`form_error`, `role="alert"`); a page that
  cannot load, the error page with the HTTP status and a way back. Say what failed and what
  the person can do ("podkit could not open an SSH connection to 10.0.0.5. Check the address
  and try again."), not an internal message. A toast may add to an inline error, never
  replace it.
- **Permission:** an action the user may not take is not rendered. A forged request is
  refused by the service, and cross-team resources answer 404.

## Motion

Motion explains a change; it never decorates and never delays. Durations 120–200ms,
`--ease-out`, properties `opacity` and `translate`/`scale` only.

- Dialog, menu, select picker, toast: fade plus a 0.5rem translate on enter
  (`@starting-style`); exit uses `transition-behavior: allow-discrete` where supported.
- Page navigation: a 140ms cross-fade (`@view-transition { navigation: auto }`); elements
  that should stay put can carry `.ui-vt-chrome`.
- A pressed button moves 1px; a progress dot pulses; a skeleton pulses.
- **`prefers-reduced-motion: reduce`:** transforms and the view transition are off, and a
  global rule shortens every animation and transition to ~0ms. New motion must sit inside
  `@media (prefers-reduced-motion: no-preference)` or be a plain fade.

## Accessibility

- WCAG 2.2 AA. Contrast as above; focus is always visible (`:focus-visible` ring from
  `ring`); never `outline: none` without a replacement.
- Everything works by keyboard: links and buttons in a logical order, dialogs trap and
  return focus and close on Esc, menus open with Enter/Space/ArrowDown and close on Esc or
  outside click, tooltips show on focus.
- Native elements first (`<button>`, `<a>`, `<dialog>`, `<details>`); ARIA only to name or
  describe. Icon-only controls have `aria-label`; decorative icons and dots are
  `aria-hidden`.
- Every control has a visible `<label>`; help and error text are linked with
  `aria-describedby`; invalid fields set `aria-invalid`; errors are `role="alert"`.
- One `<h1>` per page, headings in order. Landmarks: `<header>`, `<nav aria-label>`,
  `<main>`, the `aria-live` toast region.
- Touch targets are at least 44px on coarse pointers. Text uses `rem`, so the OS text scale
  applies; layouts hold at 200% zoom and at 320px wide.
- Tables are real tables; a scrollable region is focusable and named.
- State is never color alone: status shows its word, errors show text.

## Browser support

Baseline widely available features need no fallback. Newer ones degrade as above:

| Feature | Where missing |
|---|---|
| `command` / `commandfor` | `podkit.js` does the toggle, show-modal and close; the dialog button also has a Datastar fallback that opens it |
| Anchor positioning | menus become a bottom sheet; tooltips use plain absolute placement |
| `appearance: base-select` | the platform select |
| `@view-transition` | instant navigation |
| `closedby` | podkit.js closes on click outside |
| `light-dark()` | needs a 2024 browser; no fallback |

## Adding to the system

1. Look for an existing component; extend it before adding one.
2. New components go in a NEW file under `server/src/ui/components/`, with render tests
   (aria attributes, escaping), a section in `/_ui` (`gallery.rs`) and an entry here.
3. Classes: whole string literals; a utility encre-css knows or a `.ui-*` rule in
   `components.css`. `build.rs` fails the build on an unknown class.
4. Use only tokens; no literal colors; motion inside `prefers-reduced-motion: no-preference`.
5. Check `/_ui` in dark and light, at 1440 and 390px, with the keyboard.
