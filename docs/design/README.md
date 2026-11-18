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
| Font | Geist Sans (`--font-sans`), Geist Mono (`--font-mono`) | variable woff2, `font-display: swap`; mono for ids, hashes, branches, log lines, keys |
| Type scale | `text-xs` 12 · `text-sm` 14 (default for UI) · `text-base` 16 (card titles) · `text-lg` 18 (dialog titles, section headings) · `text-2xl` 24 (page title) | weights 400, 500 (labels, buttons), 600 (titles); never bold above 600 |
| Shadows | `--shadow-xs` controls, `--shadow-sm` raised cards, `--shadow-md` menus, `--shadow-lg` dialogs and toasts | in dark the border does most of the work |
| Motion | `--duration-fast` 120ms, `--duration-base` 200ms, `--ease-out` | see Motion |
| Widths | page content `max-w-4xl` (forms, detail) to `max-w-7xl` (lists, gallery); dialogs 32rem, wide 48rem | |

Layout: a left sidebar (`sidebar` tokens) 14rem wide from `md` up, a drawer-style menu
below; a header strip with breadcrumb, theme toggle and user; content on `background`.
The shell is owned by the F2 card; this file only fixes its tokens and rules.

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
