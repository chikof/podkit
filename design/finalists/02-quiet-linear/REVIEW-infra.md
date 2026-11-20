# Quiet: infra batch design review (DR)

Reviewed on `infra/DR-design-review` (from `infra/integration` at `03b30aa`), against the Quiet README, `components.html`, `REVIEW.md` (Q8) and the canvas design in `design/canvas/` (D1).

**How.** One isolated podkit (own data dir, own Postgres, server on `127.0.0.1:18950`, none of chiko's ports) with a seeded team: a project with 4 apps, 2 databases, 2 registries, 2 SSH keys and a Gitea provider (a local stub), plus a second project. No agent was running, so nothing is deployed and the scanner never ran (see "Not reviewed"). Every new page and state in **dark and light, 1440 and 390 px in Chromium**; **Firefox 160** at 1440 and 500 px (the narrowest its window goes) in both themes, plus its own passes for Ctrl-K, the sidebar fold, the save bar and a dialog. Two measured checks ran across the pages: a card-alignment check (every card's header text against the first text of each of its other parts, 41 pages) and the gap between the tab row and the first card on the 11 application tabs that have a card (the 12th, Monitoring, has none). Layout widths were read in the page, not from the screenshots. Final audit on the finished build, 41 pages in both themes at 1440 and 390 px: no horizontal overflow, one `h1` on every page, no icon-only control without a name (the two "unnamed" links on Templates at 390 px have text the audit does not read).

Severity: **must** (visibly off the design or hurts use), **should** (noticeable), **nit**. Status: **F** fixed here, **FU** follow-up (needs Rust logic or a product call).

Screenshots are in `review-infra/` next to this file. `<name>-before` / `-after` pairs show a fix; `headline/` has the 8 headline screens in dark and light at 1440 and 390 px.

## Fixed here

| # | Sev | Where | Issue | Status | Shots |
|---|-----|-------|-------|--------|-------|
| 1 | must | Application tabs: Environment, Domains, Volumes, Networks | The first card sat 56 px under the tab row; on Deployments, Models, Webhooks, Security, Settings it sits 32 px. An empty status slot (`#saved`, `#networks-saved`) is a flex item, so the column's 24 px gap applied to nothing. Content jumped 24 px when you switched tabs. Measured on the 11 tabs that have a card: now 32 px on every one. | F: `.flex-col > div[id]:empty:not([class]) { display: none }` (the slot is shown again as soon as a message is patched in) | `app-networks-before`, `app-networks-after` |
| 2 | must | Application, Networks tab | The network row's text began at 353 px, the card header's at 345 px (the row used `px-6`, the card inset is 16 px). The card-alignment check found nothing else in 41 pages. The empty state in the same card sat in a second wrapper with its own padding. | F: the rows use `ui-card-rows` and `ui-card-row` like Domains, Volumes and Environment; the empty state is the card's own content (`pages/networks/panel.rs`) | `app-networks-before`, `app-networks-after` |
| 3 | must | Servers, 390 px | The status chips ("Unreachable", "Local") were a third grid item with no column. They landed in the 40 px logo column and ran off the left edge, so "Unreachable" was cut to its last letter. | F: at 760 px and below `.ui-row > .ui-row-end` takes the text column | `servers-mobile-before`, `servers-mobile-after` |
| 4 | must | Canvas | In a one-column frame (about 270 px, the case for a project with no links) the count line wrapped under the name and ran into the first node ("5 resources · 2 failed · 3 pending" was cut through its middle). | F: a frame narrower than 340 px gets a tighter two-line head that ends above the node (container query on the frame) | `canvas-before`, `canvas-after` |
| 5 | should | Sidebar, fold | Clicking Collapse made the icons of short labels (Keys, Models) jump 6 px sideways before the width eased: the rail centered them with `justify-content`, which flips at once. Sampled with the animations paused at 70 ms: the Keys icon was at 22 px while the others were at 16. | F: the icon stays at the start and its side padding eases to the centered value (fixed numbers: a percentage overshot by 12 px). Sampled at 20, 70, 140 ms: every icon is at the same x (24.5, 27, 27.8) and ends at 28. Same in Firefox. | `collapse-mid-before` |
| 6 | should | Keys, Registries, Git providers | An "Infrastructure" eyebrow over the title. The other sidebar pages (Servers, Networks, Security, Overview, Projects) have none, so the title moved 20 px when you went down the sidebar. The breadcrumb and the sidebar group already say where you are. | F: eyebrow removed (`pages/{keys,registries,git_providers}/mod.rs`) | `keys-before`, `keys-after` |
| 7 | should | Git providers | The three connect buttons dropped under the long description, on a row of their own, while Keys has its buttons beside the title. | F: `.ui-page-head > .ui-page-title` takes the row first, so a long lede wraps beside the actions. 390 px unchanged (the buttons still go under) | `git-before`, `git-after` |
| 8 | should | Canvas | The Canvas / List switch was underline tabs. D1 draws a boxed group (2 px inset, selected tab filled). | F: `.ui-cv-bar .ui-tabs` | `canvas-before`, `canvas-after` |
| 9 | should | IaC, plan form | A checkbox and its hint were 28 px apart (the label row is 30 px high, the hint a grid row below). The hint read as belonging to the next option. Also 60 px between the form and the plan, against 8 between plan items. | F: the hint is pulled to 4 px under its label; the plan sits one page gap below | `iac-before`, `iac-after` |
| 10 | should | Keys, Git providers, 390 px | In the stacked rows the row menu took a line of its own, left-aligned under the values. | F: in a stacked row the menu sits in the card's top-right corner (container query, `:has(> .ui-menu-trigger)`) | `keys-mobile-before`, `keys-mobile-after` |
| 11 | should | Keys, Import key dialog | Focus opened on the close button (a focus ring on the X); the Generate dialog focuses its first field. | F: `autofocus` on the name field | `import-dialog-before` |
| 12 | nit | Sidebar | Overview and Projects both used a grid icon (`layout-dashboard` next to `layout-grid`, near-identical at 14 px). | F: Overview uses `activity` | |
| 13 | nit | Security badge | Off-token: 12 px text (the scale has 11.5), 6 px gap, 999 px radius, rem padding. | F: tokens in the DR block of `components.css` | |

## Checked and fine

- **Ctrl-K in Firefox** (the reported input bug): opens on the shortcut, the input has focus, no outline or border of its own, typing filters, Esc closes. Light and dark. Not reproduced.
- **Sidebar fold, Chromium and Firefox**: 232 to 56 px in about 200 ms with the page eased in step; no frame where labels wrap or overlap (sampled with the animations paused at 20, 70 and 140 ms).
- **Save bar**: appears only after a field changes (not rendered when clean); slides 24 px and fades in over about 160 ms (sampled: opacity 0, 0.41, 0.84, 0.95, 1.0). Leaving a dirty page asks first. Works in Firefox.
- **Dialogs** (Generate key, Import key, in both browsers): head, body and foot as every other dialog; Esc closes; discarding resets the fields.
- **Chart tooltip** (gallery chart, Chromium): crosshair, one dot per series, a card beside it with the time and every value, "no reading" for a gap; the card flips at the plot's edge. Real server charts were not available (no agent).
- **Card and empty-state alignment**: across 41 pages the card header text and the first text of every other part line up (except finding 2). Standalone empty states keep the dashed box; inside a card they are flush with the header's edge.
- **Layout**: no horizontal overflow at 390 px (Chromium) or 500 px (Firefox) on any page checked; one `h1` per page; the guided setup, networks and IaC plan are fine in both themes.
- **Canvas against D1** (`design/canvas/`): node anatomy, frames, the dashed network frame with its `auto` tag, the legend, the hint bar, the zoom group, the pressed Connect button, handles on focus and the focus ring all match. Nodes with no links all sit in layer 0, so a project with no links is one tall column; D1's own rule says so, and its mockup has links.
- **Stepper, checklist, banners, diff**: use only Quiet tokens; the IaC plan shows each change by sign, word and tint, never by color alone.

## Follow-ups (Rust logic or a product call, not done here)

| # | Sev | Where | Issue |
|---|-----|-------|-------|
| A | nit | Auth | Twice during this run (after a Firefox sign-in, and after a curl sign-in) the Chromium session was gone on the next load. I could not reproduce it on purpose: a second sign-in left the first session valid. Worth a look if chiko sees himself signed out for no reason. |
| B | should | Breadcrumbs | Networks and Setup show "Infrastructure / Networks"; Keys, Registries, Git providers, Security and Servers show only the page. Pick one: with the sidebar group already named, the shorter form is enough. |
| C | nit | Canvas | D1 draws a primary "Create resource" next to "Resource list" in the page head; the build has only "Resource list". |
| D | nit | Tables | Row heights differ on the new tables (Security 38 px, Networks 39, Keys and Git providers 47, overview 55). Pick a single row height for a table with chips. |
| E | nit | Server networks | "Used by" has no separator in the table (`cache db`) and a comma on the team Networks page (`cache, db`). |
| F | nit | Sidebar | The fold animates `padding`, `margin` and `max-width` on about 80 elements. It holds 60 fps here; a low-end phone's tablet mode may not. An alternative is clipping one wrapper per label. |
| G | nit | 390 px header | The team crumb shrinks to "Sm" when the current page has a long name. Give the team crumb a 4 rem minimum. |
| H | nit | Registries | The card repeats the host in the subtitle and in the "Registry" row. |

## Not reviewed

- **Scan findings**: no scanner ran, so the badge with findings (levels and `Act now`), the Security tab with results and the Security page with severities were reviewed only as CSS. `/_ui` shows the badge states; they use tokens after finding 13.
- **A running app**: no agent or Podman in this run, so deploy, logs and live metrics were not looked at; the "Not deployed", "Failed" and "Unreachable" states were.
- **Safari**, the GitHub App manifest flow and "add a remote server" in the wizard.
- **Firefox at 390 px**: its window will not go below 500 px; 500 px was used.

## Run guide

From the repo root, on this branch, without touching your own instance (other ports, other data, other database):

```sh
git checkout infra/DR-design-review
podman run -d --rm --name pk-dr-pg -e POSTGRES_PASSWORD=postgres -p 127.0.0.1:55481:5432 docker.io/library/postgres:18

export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55481/postgres
export AGE_SECRET_KEY=AGE-SECRET-KEY-1...        # a key of your own; not shared with your real instance
export DATA_DIR=/tmp/pk-dr/data HOST=127.0.0.1 PORT=18950 INGRESS_PORT=18960 HTTPS_PORT=18961
export REGISTRATION=open PODKIT_UI_GALLERY=1       # the gallery at /_ui shows the badge and chart states

SQLX_OFFLINE=true cargo run -p server              # http://127.0.0.1:18950, make an account, then a team
# For deploys, scans and live charts, also run the agent (second terminal, private Podman socket):
#   cargo run -p agent -- --data-dir /tmp/pk-dr/agent
#   and start the server with LOCAL_AGENT_SOCKET=/tmp/pk-dr/agent/agent.sock
```

To look at it the way this review did: create a project with a few apps and two databases (through the API or the UI; nothing has to deploy), add two keys, a registry and a Gitea provider, then walk Infrastructure (Overview, Servers, Networks, Keys, Registries, Git providers, Security), the guided setup, a project's Canvas and IaC page (plan a `podkit.toml` that adds an app and changes a port), and an application's Networks and Security tabs, in both themes and at 390 px.

To stop: Ctrl-C the server, `podman stop pk-dr-pg`, and delete `/tmp/pk-dr`.
