# Project canvas (D1)

A map of one project in the **Quiet** design language: resources are nodes, relationships are links, environments and networks are frames. You click a node to open it and **draw a link to wire things together**.

This folder is the design hand-off for **I2** (the build card). Everything here is static HTML, CSS and a small behaviour prototype. It opens from `file://`, makes no external requests and uses only Quiet tokens and components from `../finalists/02-quiet-linear/`.

| File | What it is |
|---|---|
| `canvas.html` | The realistic project: 2 environments, 8 resources, 2 networks, 4 links, status colours, scan severity (S1). Pan, zoom, drag-to-connect, keyboard and the list view all work. |
| `canvas-wire.html` | Opens in the middle of a wiring gesture: `storefront-api` dropped on `postgres-16`, confirm popover open. Same page as `canvas.html`, staged. `?stage=drag` (mid-drag, valid), `?stage=invalid` (refused), `?stage=confirm` (default). |
| `canvas-empty.html` | The empty project. At 390 px it is the same page; the canvas never shows on a phone. |
| `canvas-states.html` | Every node, link and level-of-detail state, drawn by the real CSS. The reference for I2's visual tests. |
| `canvas.css` | The one new CSS block, `/* == canvas == */`, to append to `server/src/ui/components/…/components.css` (see §11). |
| `canvas-demo.js` | Behaviour prototype. The reference for `server/static/canvas.js`. Not shipped. |
| `tools/build.py` | Generates the four pages from one data model. It also contains the **server-side layout** (layers, bands, frames, edge routing) that I2 ports to Rust. `python3 design/canvas/tools/build.py` |
| `screenshots/` | Dark and light, 1440 and 390 px (Chromium); Firefox in `screenshots/firefox/`. |

Open with `?theme=dark|light` and `?still=1` (motion off), like the Quiet finalists.

**Surface.** This is an **Operate** surface (the user takes action on things, here wiring them), with **Monitor** as the secondary one (status and scan severity at a glance). So: dense, no hero, no decoration, action affordances (handles, Connect, menus) and selection state dominate. It is not a Decide/Learn page, hence no marketing framing, and the empty state is a single dashed panel with up to three actions, like every other Quiet empty state.

## 1. Anatomy

### 1.1 Node

`208 × 92` world px at zoom 1. A node is a hairline tile on the canvas colour, like a Quiet resource row folded into a card.

```
┌────────────────────────────────┐
│ [ic] storefront-api        ⋯   │  head: icon tile 24, name, menu button
│ Git · main                     │  sub: mono, --text-3, one line, ellipsis
│ ● Running        ◆ Critical 1  │  foot: status badge, scan badge
└────────────────────────────────┘
 ○ in handle                 out handle ○   (only on hover, focus, selected, connecting)
```

| Part | Spec | Notes |
|---|---|---|
| Tile | `--surface`, 1 px `--border`, `--r-lg`, padding `--s-3` `--s-4` | Same radius as cards. No left rail, no shadow. |
| Icon tile | 24 px, `--surface-3`, icon `--accent-text`, `--r-md` | Kind icons are the existing sprite: `git-branch`, `box`, `database`, `layers`, `brain`, `package`. |
| Name | `--fs-md`, `--fw-medium`, the **only** link in the node | An `<a href>` with a stretched `::after`, so the whole tile opens the resource. Failed turns it `--err`. |
| Sub | `--font-mono`, `--fs-xs`, `--text-3` | Kind and one fact. Git: `Git · main`. Image: `Image · 2.14.1`. Database: engine and version. Stack: `Stack · 4 services`. Model: `Model · llama.cpp`. |
| Status badge | The existing `.badge[data-status]` | Dot shape **and** word. Never colour alone. |
| Scan badge | `.sev[data-sev]`, `--fs-xs` | Text names the level, a shape precedes it (critical ◆, high ▲, medium ▲, low ●). Tint: `--err-soft`, `--warn-soft`, `--info-soft`, `--surface-3`. Shown only when there are findings. Apps and stacks only; databases and models have no scan. |
| Menu button | `.btn--icon.btn--sm.btn--ghost`, 30 px | Fades in on hover or focus; always visible on touch. `tabindex="-1"` (see §6). |
| Handles | 12 px circle, `--border-ui` ring, 24 px hit area | Pointer-only (`aria-hidden`). In on the left, out on the right. |
| Progress | 2 px hairline at the foot, `--st` fill | Only while Building. |

Names are never truncated in the list view and the accessible name of a node is its name; its description (`aria-describedby` on the link) reads kind, status and scan: *"Git app, running, 1 critical scan finding. Git · main"*.

### 1.2 Edge (link)

One SVG layer under the nodes. Three kinds, **told apart by dash pattern and by label, never by colour alone**:

| Kind | Meaning | Stroke | Label |
|---|---|---|---|
| **Variable** | app → database. The consumer got a connection variable (`DATABASE_URL`, `REDIS_URL`) and shares a network with it | solid, 1.5 px, `--border-ui` | the variable name, mono pill |
| **Model gateway** | app → model through the podkit gateway (AI 05) | dotted, 2.25 px, round caps | `gateway` with the brain icon |
| **Network** | explicit membership of a user network, drawn only **between frames** (inside one frame the frame is the tie) | dashed `7 5`, 1.5 px | the network name |

Direction: always **consumer → provider** (arrowhead at the provider), because that is the dependency order and it makes the layout read left to right. Stack services are not separate nodes; the stack node carries `4 services` and expands on demand (§2.3).

Routing: a cubic from the consumer's out handle to the provider's in handle, horizontal tangents, control offset `max(40, dx/2)`. Same row means a straight line. The label sits at the midpoint inside the 112 px column gap. Hover or focus on a node, or on a label, turns its links `--accent` 2 px and gives the neighbours a stronger border.

Label pill: `--bg` fill (so it masks the line), 1 px `--border`, `--r-pill`, mono `--fs-xs`, 20 px tall. It is a real `<button>` that opens the link menu (§6).

### 1.3 Group frames

| Frame | Look | Contains | Header |
|---|---|---|---|
| **Environment** | 1 px `--border`, `--r-lg`, translucent `--surface` | everything in the environment | h3 `production` (a link to the environment page), a count line (`6 resources · 4 running · 1 failed · 1 building`), a zoom-to-this-environment icon button |
| **Network** | 1 px **dashed** `--border-strong`, `--r-md`, translucent `--surface-2` | the members of that network | mono name with the network icon, plus `auto` (the environment's own network) or `user` |

Rules the layout engine follows:

1. Environments sit side by side (left to right in the environment order), top aligned. If the combined width would force the zoom under 60 % at the common 1440 px width, the engine stacks them instead.
2. Inside an environment, nodes are in **layers** (columns): layer 0 are nodes nothing points to (apps, stacks), layer *n* is one step further down a link. Databases and models land in the last layer.
3. Rows are **banded per network**: each network gets a band of rows, plus one unnetworked band. A frame is the bounding box of its band, so a frame can **never overlap a non-member**.
4. A node belongs to exactly one *drawing* network (its first). If it is in more, the others are shown as a **ghost** reference (dashed outline, name only, links to the real node) inside the other frame, and its links to that frame use the **Network** kind. Not in this mockup (every node has at most one network); I2 builds it when it appears in real data.
5. Networks with no member in an environment are not drawn.

## 2. States

All states are in `canvas-states.html`. Every one is reachable with the real pseudo-class **and** forced with `data-state="…"` (same convention as `components.html`).

### 2.1 Node

| State | Look | When |
|---|---|---|
| Running | default | `data-status=running` |
| Building | `--warn` pulsing dot, hairline progress at the foot | `building` |
| Deploying | `--info` ringed dot | after a wire is confirmed, until the redeploy is healthy |
| Failed | border `--err`, name `--err`, diamond dot, the word | `failed` |
| Stopped | hollow dot | `stopped` |
| Hover | border `--border-strong`, fill `--surface-2`, handles and menu button appear, links highlight | pointer |
| Focus | 2 px `--focus` ring, 2 px offset, around the **whole node** | keyboard (`:has(.cv-open:focus-visible)`) |
| Selected | border `--accent`, fill `--accent-soft` | the node last focused, or whose menu is open |
| Dragging: source | like selected plus a 4 px `--accent-soft` halo, out handle lit | the node you started the link from |
| Dragging: candidate | dashed `--accent` border, in handle visible | a valid target while connecting |
| Dragging over: valid | solid `--accent`, halo, in handle filled, tip above it | pointer over a valid target |
| Dragging over: invalid | border `--err`, fill `--err-soft`, not-allowed cursor, the reason in the tip | pointer over a refused target |
| Not a target | opacity .45, `inert` for keyboard connect | everything else while connecting |

Failed and building keep their meaning while connecting; only the connect roles above override the border.

### 2.2 Edge

| State | Look |
|---|---|
| Default | 1.5 px `--border-ui` (3:1 on the canvas), arrowhead via `marker-end` with `context-stroke` |
| Highlighted (hover or focus on either end) | `--accent`, 2 px, label pill border `--accent` |
| Selected (label menu open) | same as highlighted, stays until dismissed |
| Rubber band | `--accent` 2 px, follows the pointer, snaps to the target's in handle when valid |
| Rubber band, refused | `--err`, dashed `4 4`, ends at the pointer |

### 2.3 Stack

A stack is one node. `Enter` on the stack's `4 services` text, or the **Expand** menu item, patches in four small service nodes (name and status dot, 160 × 44) as children inside a dashed frame, with `prometheus → grafana` style edges only where the compose file declares `depends_on`. Services are never wiring targets (you wire the stack). Collapsed by default, because a stack's internals are a different question than the project's topology. Not drawn in the mockup.

## 3. Wiring

Three ways in, one state machine, **one server rule set**.

| Gesture | Steps |
|---|---|
| **Drag** (pointer) | Hover a node to reveal its handles. Press on the out handle, drag. Candidates light up, the rubber band follows. Release on a valid target to open the confirm popover; release anywhere else to cancel. Dragging from a provider's handle onto a consumer is accepted and **reversed** (the arrow always points to the provider). |
| **Click, click** (pointer, touch, no dragging) | Toolbar **Connect** (or the node menu's *Connect to…*) → click the first resource → click the second. Required by WCAG 2.5.7 (a single-pointer alternative to dragging). |
| **Keyboard / menu** | Focus a node, press **C** (or Space → *Connect to…*). Only valid targets stay in the tab order; Tab, Shift+Tab or arrows move between them, Enter picks, Esc cancels. |

The list view's **Connect…** button (§7) opens the same confirm as a dialog, with a select of valid targets.

### 3.1 What a drop means

The target's kind picks the intent. `from` is the consumer after any reversal.

| From → to | Intent | What changes | Redeploy |
|---|---|---|---|
| app / stack / template → **database** | **Variable link** | join the database's network if not already; inject its connection variable (`DATABASE_URL`, `REDIS_URL`, …) into the app as a secret | yes (rolling) |
| app / stack / template → **model** | **Model gateway link** | register the link with the gateway; inject `OPENAI_BASE_URL` and a new `OPENAI_API_KEY` scoped to that model | yes |
| any → any non-model, no database (app ↔ app, database ↔ database) | **Network** | put both on one network: reuse the one either is on, else create a user network (name field in the popover, default `<from>-net`) | **no**, attached live |

### 3.2 Refusals (shown in the tip while hovering, and as a toast on release)

| Reason | Text |
|---|---|
| same node | *Pick a different resource* |
| other environment | *postgres-16 is in staging. Wiring stays inside one environment* |
| model as source | *A model can only be linked from an app* |
| not an app | *Only apps and stacks can use a model* |
| already linked | *Already connected through DATABASE_URL* |
| already on a shared network | *Already on network env-prod* |

The client lights candidates from a server-rendered `data-targets` list per node, and the server **re-validates on POST**: same rules, 404 for a resource in another team, 422 with the reason for the others. The client's rules are a convenience, never the check.

### 3.3 Confirm popover

Anchored to the target node (right of it, flipped left if it would leave the viewport), `.dlg` skin, 380 px. Light dismiss and Esc cancel. **Focus lands on the primary button**, because nothing destructive is on offer (a wire adds, never removes) and a keyboard user should be able to confirm with one Enter. Closing returns focus to the source node.

```
(plug)  Connect storefront-api to postgres-16
        Add `storefront-api` to network `env-prod`, inject `DATABASE_URL`, redeploy?
        ┌──────────────────────────────────────────────────┐
        │ Network   Add storefront-api to env-prod          │
        │ Env var   Inject DATABASE_URL into storefront-api │
        │             Secret, write-only. Never shown.      │
        │ Redeploy  Rolling redeploy of storefront-api, ~40 s│
        └──────────────────────────────────────────────────┘
        Nothing changes until you confirm. Secret values stay write-only.
                                       [Cancel] [Connect and redeploy]
```

The lead sentence is the one-line version for people who read one line; the table is the exact list of changes, ordered as they will run. Primary labels: `Connect and redeploy`, `Link and redeploy`, `Join network`. The variable's **value is never rendered** (secrets are write-only). A new network row reads *"New user network, created for you."*

On confirm the node flips to **Deploying** (240 ms status flip), the link draws in, a toast says *"storefront-api linked to postgres-16. Redeploying"*, and the live status stream moves it back to Running.

### 3.4 Removing a link

Click the link's label (or Enter on it) → menu: **Open variables**, **Unlink…**. Unlink opens a standard destructive confirm dialog (Cancel focused) naming what is removed: the variable, and the network membership only if nothing else in the project needs it. Not mocked beyond the menu.

## 4. Pan, zoom and level of detail

| Input | Action |
|---|---|
| Drag empty canvas | pan (`cursor: grab` → `grabbing`) |
| Wheel / two-finger scroll | pan (so a trackpad feels native and the page never zooms by accident) |
| **Ctrl or ⌘ + wheel**, pinch | zoom at the pointer, `exp(-deltaY × .01)` (pinch is two-pointer tracking, specified here, not in the prototype) |
| `+` `-` `0` `F` | zoom in, out, 100 %, fit (also the toolbar buttons) |
| Shift + arrows | pan 80 px |
| Environment zoom button | fit that environment |
| Double-click empty canvas | fit |

Range 35 % to 200 %. **Initial view is Fit, capped at 100 %** (the mockup fits at 90 % at 1440 px). The viewport height is `clamp(480px, 100dvh − 250px, 760px)`, always shorter than the screen, so there is page area outside it to scroll with; over the viewport the wheel pans (`overscroll-behavior: contain`). Window resize keeps the zoom.

State lives in three registered custom properties (`@property --tx --ty --z` on the viewport). The dot grid and the world both read them, so they move as one, and a `transition` on those properties animates zoom and fit without any JS loop.

### 4.1 Level of detail

`canvas.js` sets `data-lod` on the viewport from the zoom; the rest is CSS (`canvas-states.html` shows all three).

| Zoom | `data-lod` | What shows |
|---|---|---|
| ≥ 75 % | `full` | everything: sub line, status word, scan level and count, link labels, handles on hover |
| 50 to 75 % | `compact` | sub line and link labels hidden (a label returns on hover or focus), scan shows the count only |
| < 50 % | `overview` | name and status dot only; menu buttons, handles and labels hidden; names and environment titles are **counter-scaled** (`font-size: calc(var(--fs-sm) / var(--z))`) so text stays ~12 px on screen |

Below 50 % nothing is interactive except open (click) and focus; wiring needs `compact` or better, because the handles would be under 10 px (tell the user with a one-line tip: *Zoom in to connect*).

## 5. Minimap: none

A project has at most about 100 nodes (§9) laid out in a **fixed, server-computed layout** with a reliable Fit. A minimap earns its place on free-form infinite canvases where you can get lost; here:

- **Fit** (`F`), the per-environment zoom buttons and the list view get you anywhere in one step.
- A minimap is a second render of every node plus its own hit-testing and drag logic (about 1.2 KB of JS and a second SVG per page), and on a 1440 px screen it would cover the legend or the corner you need for the confirm popover.
- On a 100-node project the overview LOD *is* a minimap that you can also click.

Revisit if a single environment ever goes past 100 nodes, or if the layout becomes user-editable.

## 6. Keyboard model

The canvas is a `role="group"` named **Project canvas** with a one-line description (`aria-describedby` points at the key hint). It does **not** use `role="application"`, so a screen reader keeps its normal reading keys.

| Key | On | Action |
|---|---|---|
| **Tab / Shift+Tab** | canvas | visits **nodes in reading order** (environment → network band → layer → row), then link labels, then leaves the canvas. Each node is one stop (its name link). A "Skip canvas" link is the first stop when the project has more than 30 nodes, and above that the canvas switches to a **roving tabindex** (one stop in, arrows move) |
| **→** | node | focus the first node this one **links to**; if none, say so (live region) |
| **←** | node | focus the first node that **links to** this one |
| **↑ / ↓** | node | previous / next node **in the same column** of the same environment |
| **Enter** | node | open the resource (a plain link; View Transition like the list rows) |
| **Space** or **Menu** or **Shift+F10** | node | open the node menu: *Open*, *Connect to…*, *Show in list* |
| **C** | node | start *Connect to…* from this node |
| **Tab / arrows / Enter / Esc** | while connecting | only valid targets are focusable (the rest are `inert`); Enter picks and opens the confirm popover; Esc cancels and returns focus to the source |
| **Enter** | link label | open the link menu: *Open variables*, *Unlink…* |
| **+ − 0 F**, **Shift+arrows** | canvas or node | zoom in, out, 100 %, fit, pan |
| **Esc** | popover, menu, connect mode | close it, return focus to what opened it |

Focus moving to a node **auto-pans minimally** so the node is 32 px inside the viewport (no `scrollIntoView`). Each arrow move announces its result in the live region (*"qwen3-4b, linked from here"*, *"observability has no outgoing links"*). The node menu button is **not** a tab stop (`tabindex=-1`): it would double the stops, and Space or the Menu key reaches the same menu. It remains in the accessibility tree for virtual-cursor users.

Roles and names: nodes are `<li>` in a `<ul aria-label="Resources">`; the name link has a visible text name; handles are `aria-hidden` (pointer affordance only, the keyboard path above replaces them); link labels have names like *"DATABASE_URL variable: checkout-worker to postgres-16"* and `aria-haspopup="menu"`.

## 7. Screen reader and no-canvas alternative: the list view

The toolbar's **Canvas | List** switch (a native radio group, no JS) swaps the viewport for **one table per environment**. It carries every fact the canvas does, and it is the only view on phones.

| Column | Contents |
|---|---|
| Resource | icon, name (link), kind and fact |
| Status | the same badge |
| Network | the network names as mono chips, or *none* |
| Connected to | `to postgres-16 DATABASE_URL`, `to qwen3-4b model gateway`, `from storefront-api`, or *No links* |
| Scan | severity badge, *Clean* or *n/a* |
| Actions | **Connect…**, the keyboard-and-touch way to wire (opens the dialog) |

Each table has a caption (*"Resources in production and how they are connected"*), `scope="col"` headers, and one row per resource, so a screen reader announces *"row 3 of 6, checkout-worker, Building, env-prod, to postgres-16 DATABASE_URL, to redis-cache REDIS_URL, High 2"*. **Server-render the list always** (hidden by CSS when the canvas view is selected, `display:none` removes it from the accessibility tree) so no JS or round trip is needed to switch, and a user whose last choice was *List* gets it with no flash: store it in the same cookie as the theme and render `checked` on the right radio.

Live region (`role="status"`, polite, in the canvas): link and connect announcements, drop refusals, deploy results. Dragging and zooming never announce.

WCAG notes: dragging has three single-pointer or keyboard alternatives (2.5.7); handles have a 24 px hit area and node targets are far larger (2.5.8); link strokes use `--border-ui` (1.4.11, ≥ 3:1) and every status is shape plus word (1.4.1); no timing (2.2.x); focus is never trapped; the viewport itself takes a 2 px focus ring.

## 8. Motion

Quiet's curve and durations only: `--ease` / `--ease-out`, `--dur-1` 90 ms, `--dur-2` 160 ms, `--dur-3` 240 ms.

| What | Duration | Notes |
|---|---|---|
| Node hover, border and fill | `--dur-1` | |
| Handles and menu button fade in | `--dur-1` | |
| Candidate, dim, valid, invalid | `--dur-2` | border, fill, opacity |
| Tip above the target | `--dur-1` | opacity only |
| Confirm popover, node menu | `--dur-2` | the existing `[popover]` enter/exit (`@starting-style`) |
| Zoom buttons, Fit, env zoom, auto-pan on focus | `--dur-3` | by transitioning `--tx --ty --z`; none while dragging or wheel-zooming (it would lag the hand) |
| New link | `--dur-3` | opacity fade (`cv-in`); the stroke style is unchanged so dotted and dashed links stay crisp |
| Status flip (Running → Deploying → Running) | `--dur-3` | the existing flip |
| Progress | `--dur-3` | width |
| Loops | only the Building dot's pulse | same as the rest of the app |

**Reduced motion**: `prefers-reduced-motion: reduce` zeroes every duration above (the global rule in `base.css` already does), so zoom and fit jump, the link appears at once, and the state change is carried by text (the badge word and the live region). The pointer-follow rubber band is direct manipulation, not animation, and stays. `?still=1` does the same for screenshots.

## 9. Performance limits

Design target: **up to about 100 nodes and 150 links per project**, one project per page.

- **Cost model.** The world is one element with a single `transform`; panning and zooming change three custom properties, never layout. Pointer handlers are delegated on the viewport (no per-node listeners) and `pointermove` is coalesced to one `requestAnimationFrame`, using the latest event.
- **No layout reads in the loop.** Node rectangles come from server-rendered `data-x` / `data-y` (world px); the client never measures nodes while dragging. The only `getBoundingClientRect` calls are on the viewport (once per gesture) and the hovered node.
- **`will-change: transform`** only while panning or dragging, never at rest.
- **Weight.** About 1.2 KB of HTML per node, 1.6 KB including its link and label: 100 nodes is about 160 KB raw, about 25 to 30 KB compressed. Links are one `<path>` each; frames are plain divs.
- **Past 100 nodes**: render environments **collapsed** to a frame with a count and an *Expand* button (the others stay as lightweight frames), default to the `overview` LOD, and show a banner: *Large project: showing the list is faster. Open list.* Past 300 nodes show the list only.
- **Live updates** patch single nodes (`#n-<id>` status badge and progress), never the whole world; positions never change on a status flip, so no relayout.
- **No canvas element, no WebGL, no library.** HTML and SVG keep text selectable, translatable, focusable and printable, and keep the whole thing inspectable.

## 10. Phones and small screens

At **≤ 760 px** the canvas does not render at all: the toolbar, viewport and handles are `display: none`, and the list view is the page. Nobody should pan a 1200 px map with a thumb. The table already stacks into Quiet's 2-column rows (`data-label` captions), actions collapse to a 44 px icon button, and **Connect…** opens the same dialog as a **bottom sheet** (the existing `dlg` mobile treatment) with a select, the exact change list and the primary action pinned at the foot. At 761 to 1024 px the canvas shows with larger touch targets (`pointer: coarse` bumps handles to 24 px) and one-finger pan; wiring on touch uses click-click.

The empty state at 390 px is the same panel, one column, with its three actions.

Screenshots: `canvas-{dark,light}-390.png` (the list), `canvas-wire-{dark,light}-390.png` (the sheet), `canvas-empty-{dark,light}-390.png`.

## 11. Implementation notes (no-build stack: maud, Datastar, vanilla JS)

### 11.1 What is rendered where

| Piece | Where | Why |
|---|---|---|
| Layout (layers, bands, frames, node `x`/`y`, edge paths and label points) | **Server**, a pure function `layout(&Graph) -> Layout` in `server/src/ui/canvas_layout.rs`, ported from `tools/build.py` | Deterministic, unit-testable with plain structs, zero client measuring, identical on first paint and after every patch |
| Frames, edges, labels, nodes | **Server**, maud components in a new `server/src/ui/components/canvas.rs` | Datastar patches replace parts of this markup |
| Edges and arrowheads | **One server-rendered `<svg>`** (paths) | as asked |
| Nodes | HTML `<li>` (not SVG `<g>`) positioned in the same transformed world | A node is text, a link, a menu button and badges: HTML gives a real focus ring, wrapping control, `:hover`/`:has()`, the existing `.badge`, `.btn`, and translation for free. SVG `<foreignObject>` is the usual workaround and costs more than it saves (focus and popover quirks in Firefox). The edge layer stays SVG, which is where SVG earns its keep |
| Pan, zoom, LOD, drag, click-click, keyboard focus along edges, hover highlight | `server/static/canvas.js` | the only things HTML cannot do |
| Everything that changes data | **Datastar actions**, server-validated | the canvas has no write path of its own |

The viewport element `#cv-view` is **never replaced** by a patch; it owns the pan/zoom custom properties. Patches target `#cv-world` (or single `#n-<id>`, `#e-<id>`), and Datastar's morph keeps the node that has focus.

Server data contract on each node (`<li class="cv-node">`): `id="n-<id>"`, `data-id`, `data-name`, `data-kind`, `data-env`, `data-nets`, `data-status`, `data-href`, `data-x`, `data-y`, `data-var` (the connection variable a database offers), and **`data-targets`** (space-separated ids this node may connect to, computed server-side; the mockup computes it in JS instead). Each edge `<path>` has `data-from`, `data-to`, `data-kind`, `data-label`. Ids are the resources' public ids, never database keys.

### 11.2 Datastar flow

```
drop / Enter on target
  canvas.js:  $cv.from = 'api'; $cv.to = 'pg'          (sets two signals, dispatches 'cv-wire' on #cv)
  #cv         data-on:cv-wire="@get('/p/<id>/canvas/wire/preview')"      server validates, renders:
              → patch #cv-pop (the popover body, with hidden from/to/csrf) and data-init="el.showPopover()"
  canvas.js:  on the popover's 'toggle' (capture), position it next to the target (skipped ≤ 760 px)
confirm
  form        data-on:submit="@post('/p/<id>/canvas/wire')"              server re-validates, then:
              attaches the network, writes the secret (state.secrets, encrypted), enqueues the redeploy
              → SSE patches: #cv-world edge + label, #n-<id> status badge, #cv-pop emptied, a toast
status        the existing per-project SSE stream patches #n-<id> .badge and .cv-prog only
unlink        @delete('/p/<id>/canvas/wire?from=…&to=…') after the confirm dialog
```

Endpoints (suggested): `GET canvas` (page), `GET canvas/wire/preview?from&to`, `POST canvas/wire`, `DELETE canvas/wire`. Every one authorises through the existing service layer (the UI never touches the DB or RBAC), returns 404 across teams and 422 for rule violations, and **never renders a secret value**. Preview and POST share one `plan(from, to) -> Plan` function so what the popover promises is what the POST does. The list dialog posts to the same endpoint.

### 11.3 `canvas.js`: scope and size

What it contains: viewport (pan, wheel and pinch zoom, fit, LOD, `@property` vars), pointer drag-to-connect with pointer capture, click-click and keyboard connect (the `inert` trick), arrow navigation along edges, hover highlight, node menu positioning, live-region announcements, the two Datastar hand-offs. **What it does not contain** (the server owns them): the wiring rules and their reasons, the change list text, the list dialog (a plain server-rendered `<dialog>`), menu content.

Measured from the prototype, `canvas-demo.js`, sectioned:

| Section | Bytes (whitespace-collapsed) |
|---|---|
| viewport (pan, zoom, fit, LOD) | 2.5 K |
| connect state machine (drag, band, over, cancel) | ~2.0 K of the 5.2 K (the rest is the popover builder, server-rendered in prod) |
| pointer handlers | 1.8 K |
| click-click | 0.9 K |
| hover highlight + keyboard | 2.9 K |
| node menu positioning | ~0.8 K of 2.5 K |
| header, helpers | ~1.0 K |
| Datastar hand-off, announcements | ~1.0 K (new) |
| **Total, production** | **≈ 13 KB readable, ≈ 9 KB minified, ≈ 3.5 KB gzip** |

The prototype itself is 23.8 KB raw (8.1 KB gzip) because it also stands in for the server: `can()`, `describe()`, the commit, the list dialog and the staging code (about 10 KB that does not ship). **Estimate for I2: 12 to 15 KB source, one file, no dependencies.** Ship it unminified (no build step) behind the server's normal compression, about 4 KB on the wire. It loads `defer`, only on the canvas page, and is independent of `page.js` (which stays under 10 KB).

### 11.4 CSS

`canvas.css` is a single block, 17.9 KB raw. Append it to `components.css` as `/* == canvas == */` **at the end**, never reordering existing blocks; the `ui-*` classes in maud map 1:1 (`cv-*` → `ui-cv-*`). It does **not** ship the `states sheet` rules (the `.cv-sheet`, `.cv-cell`, `.cv-spec` block near the end, ~1.3 KB). Every colour, radius, space, font and duration is an existing token; the literals that remain are component dimensions: node 208 × 92, handles 12 px, the label pill's 20 px, the 24 px icon tile, grid 24 px. `@property --tx --ty --z` and `color-mix()` are Baseline.

Utilities used from maud stay as full string literals under `server/src/ui` so the scanner picks them up.

### 11.5 Tests I2 should add

- `canvas_layout`: layers for chains, diamonds and cycles (break the cycle by edge id order); frames never overlap a non-member (property test over random graphs up to 100 nodes); a node in two networks produces one ghost; positions stable when an unrelated status changes.
- Rules: every refusal in §3.2, reversal, duplicates, cross-team 404, cross-environment 422, forged POST (from/to the user cannot see), secret value never in the preview or the response.
- Escaping: names, network names and variable names through maud only; no `PreEscaped` with user data.
- Smoke: `scripts/smoke.d/canvas.sh` (page renders, one edge per link, list view has every resource, preview returns the plan, POST then GET shows the edge).
- Browser: Chromium and Firefox, drag (pointer capture), keyboard connect, reduced motion, 390 px list.

### 11.6 Decisions made here, so I2 does not reopen them

1. Click opens the resource; there is no separate selection or detail drawer.
2. Layout is **server-computed and not user-editable** (no x/y to persist, no layout conflicts). If users ask for manual positions, store per-project offsets later.
3. Nodes are HTML, edges and the rubber band are SVG, frames are divs.
4. Direction is always consumer → provider; reverse drops are accepted and flipped.
5. The wheel pans, Ctrl or ⌘ + wheel zooms.
6. List view is always rendered and is the phone view.
7. No minimap.

### 11.7 Not in this card

Persisting the *Canvas | List* choice (cookie, mentioned above), the stack expansion (§2.3), ghost nodes (§1.3), unlink confirm dialog, undo after a wire. Open points for chiko: whether *Connect and redeploy* should offer *Connect without redeploying* (stage the change for the next deploy), and whether network wiring between two databases belongs in v1.

## 12. Tokens used

Colour: `--bg --surface --surface-2 --surface-3 --border --border-strong --border-ui --text --text-2 --text-3 --accent --accent-soft --accent-text --ok --warn --err --info --err-soft --warn-soft --info-soft --focus --grid`. Type: `--font-sans --font-mono --fs-xs --fs-sm --fs-md --fs-lg --fw-medium --fw-bold --lh-heading`. Space and shape: `--s-1…--s-6 --r-sm --r-md --r-lg --r-pill --bw --ctl-h --sh-2 --sh-3`. Motion: `--dur-1 --dur-2 --dur-3 --ease`. Existing components reused: `.badge`, `.dot`, `.btn` (+ `--sm --icon --ghost --primary --secondary`), `.chip--btn`, `.tabs--range`, `.tbl`, `.dlg` (+ `--confirm`), `.menu` / `.menu-item`, `.field` / `.select` / `.input`, `.empty`, `.kbd`, `.toast`.

New components (all in `canvas.css`): `.cv` (panel, toolbar), `.cv-view` / `.cv-world`, `.cv-env`, `.cv-net`, `.cv-node` (+ ports, progress), `.cv-edge` / `.cv-elabel`, `.sev` (scan badge, also usable on the resource pages), `.cv-confirm` / `.cv-change`, `.cv-list`.

## 13. Screenshots

Dark and light, Chromium, `screenshots/`:

| | 1440 | 390 |
|---|---|---|
| Canvas | `canvas-{dark,light}-1440.png` | `canvas-{dark,light}-390.png` (list) |
| Wire, confirm popover | `canvas-wire-confirm-…-1440.png` | `canvas-wire-{dark,light}-390.png` (sheet) |
| Wire, mid-drag valid | `canvas-wire-drag-…-1440.png` | |
| Wire, refused | `canvas-wire-invalid-…-1440.png` | |
| Zoom, compact and overview | `canvas-zoom-compact-…`, `canvas-zoom-overview-…` | |
| List view | `canvas-list-…-1440.png` | |
| Empty | `canvas-empty-…-1440.png` | `canvas-empty-…-390.png` |
| States | `canvas-states-…-1440.png` | |

Firefox 160 (`screenshots/firefox/`): canvas dark and light, drag valid, confirm, refused, 390 list.

## 14. Verification

- Chromium 157: all four pages open with **no console errors**; real mouse drag from handle to handle (valid and refused), release outside cancels, Esc cancels, confirm adds the link and flips the status, focus returns to the source; keyboard walk (arrows along links, `C`, Tab among valid targets only, Enter opens the popover, Esc, Enter on the primary); wheel and button zoom, LOD at 90 %, 57 %, 35 %; list view and its dialog; 390 px list, sheet and empty.
- Firefox 160 (headless, WebDriver BiDi): no console errors; mouse drag valid and refused, popover, confirm by click, keyboard `C` / Tab / Enter. Headless Firefox did not deliver key events to a popover button, so Enter on the popover's primary was verified in Chromium only (a harness limit, not a page error).
- Contrast: only Quiet tokens, so the existing audit applies. New pairs: `.sev` text on its `*-soft` fill (the same pairs the badges use); edge strokes `--border-ui` on `--bg` (3.59 dark, 3.46 light).
- Not verified: Safari; screen readers (the markup follows the patterns in §6 and §7, but NVDA and VoiceOver passes belong to I2's visual QA).
