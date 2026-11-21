# Quiet: wave A design review (DR-A)

Reviewed on `roadmap/DR-A` (from `roadmap/INT-A` at `3235a24`), against the Quiet README, `components.html`, `REVIEW.md`, `REVIEW-infra.md`.

**How.** One isolated podkit (own data dir `/tmp/pk-t_87d44fd2`, own Postgres database, ports 24731-24733, none of chiko's) with a seeded team: notification channels and a feed with unread items, shared variables, a health gate, a scheduled task with runs, volume backups, uptime checks with an incident and a public status page, an environment clone, audit entries. Every new page and state in **dark and light, 1440 and 390 px in Chromium**; **Firefox** at 1440 and 500 px (the narrowest its window goes) in both themes. A script checked, on every page, horizontal overflow, one `h1`, controls with no accessible name and card/first-text alignment. Dialogs were opened and checked one by one (focus, Esc, backdrop). The save bar was sampled while animating.

Severity: **must** (visibly off the design or hurts use), **should** (noticeable), **nit**. Status: **F** fixed here, **FU** follow-up (needs Rust logic or a product call).

Screenshots are in `review-wave-A/`: `headline/` has the 12 headline pages in dark and light at 1440 and 390 px (48 images), `*-before` / `*-after` pairs show a fix, `firefox/` has the Firefox passes, `run/` the scripts used.

## Fixed here

| # | Sev | Where | Issue | Status | Shots |
|---|-----|-------|-------|--------|-------|
| 1 | must | Scheduled tasks (list, task, form) and Notification history | The head and the first card sat in a 12 px `ui-stack` / bare `ui-wide`, not the page grid, so the first card was 12 px under the lede and touched the head's buttons (24 px everywhere else). | F: `ui-page-body` on the three pages (`pages/cron/*`, `pages/notifications/history.rs`) | `cron-list-before/after` |
| 2 | must | MCP servers tab | The empty state drew an empty box: the `plug` icon was not in the sprite and nothing reports a missing icon. | F: `plug` added to `scripts/vendor-ui-assets.py` and `static/icons.svg` | `app-mcp-before/after` |
| 3 | must | Public status page, dark system | The page forced `class="light"`, so a dark system got a white page with the app's dark banner colours around it. | F: no class on `<html>`, `color-scheme: light dark`; same banner, strip and chip tokens as the app. Still no script, no cookie. Test updated. | `status-dark-before/after` |
| 4 | must | Public status page, 390 px | The page was wider than the screen. | F (INT-A `40613f6`); rechecked in both themes | `headline/status-page-390-*` |
| 5 | should | Notification settings | Channels, email server, noise control and the rest were all 12 px apart, so the page read as one long list. | F: `.ui-notif-sections`, 32 px between sections | `headline/notification-settings-*` |
| 6 | should | Notifications feed, 390 px | The Open / Mark read buttons took a column from the text; the time and the repeat count ran together. | F: buttons under the text at 30rem and below; a middle dot between time and count | `notifications-390-before/after` |
| 7 | should | Scheduled task form | The three overlap-policy cards sat in four 12rem tracks with one empty, and wrapped badly at 390 px. | F: `repeat(auto-fit, minmax(min(100%, 13rem), 1fr))` | `headline/cron-task-*` |
| 8 | should | Uptime, the checks list | The card body's padding and the row's own made the first row 40 px under the header. | F: `.ui-card-b:has(> ul > .ui-probe-row)` takes no vertical padding | `uptime-list-before/after` |
| 9 | should | Uptime check, Metrics | A single chart took half the row and left a hole. | F: a lone chart spans both columns | `uptime-check-before/after` |
| 10 | should | Volume backups | Settings were label-value lines that wrapped under the fact above at 390 px. | F: `.ui-vb-facts`, one row of figures, two per row at 390 px | `volume-facts-before/after` |
| 11 | should | Volume backup settings dialog | Focus opened on the close button. | F: `autofocus` on the destination select | `volume-dialog` |
| 12 | should | Tablet widths | The page column did not shrink below its header's content, so tablets scrolled sideways (INT-A `8819115`). | F; the overflow audit found nothing after it | |
| 13 | nit | Uptime check detail | "Made by: a person" and a row called "Checks" that holds the address. | F: "Added by: a team member", "Address" | `headline/uptime-check-*` |
| 14 | nit | Backup destinations | The page said databases only and still had an eyebrow the other infrastructure pages do not have; the scheduled-runs line was larger than the other hints. | F: copy now names volumes too, eyebrow removed, `text-xs` | |

## Follow-ups (not done here)

| # | Sev | Where | Issue | Status |
|---|-----|-------|-------|--------|
| FU1 | should | Edit forms | Two patterns for saving an edit form live side by side: the save bar (application Settings, task settings, audit retention) and a `Save` in the card footer (uptime check settings, status page card, email server, noise control). Both work and both are smooth. The README now says: the save bar for edit forms, footer buttons for add forms. Moving the four footer forms onto the save bar needs a signal-dirty wiring per form. | FU (Rust and signals) |
| FU2 | nit | `components.css` | `.ui-facts` is defined three times; which rule wins depends on order. New uses take their own class (`.ui-vb-facts` did). | FU (cleanup of old rules) |
| FU3 | nit | Firefox | The accessible-name audit lists the `<select>` button (`<button><selectedcontent>`) as unnamed on the cron task, volumes, environment and MCP pages. It is the browser's own internal button of a customisable select, and the select itself has its label. False positive, no change. | none |

## Checked and fine

- **Save bar**: not rendered when clean; same slide and fade as in the infra review; the dirty dialog (`dirty-dialog.png`) and the bar on the health gate (`savebar-health.png`) were captured in Chromium. Not re-sampled frame by frame this round.
- **Dialogs**: the dirty dialog and the volume backup settings dialog were opened and checked (finding 11); the others follow the shared dialog component. Only these two were opened by hand this round.
- **Card and empty-state alignment**: card header text and the first text of every other part line up on all wave A pages. Standalone empty states keep the dashed box; inside a card they sit on the card inset.
- **Layout**: no horizontal overflow at 390 px (Chromium) or 500 px (Firefox) on any page; one `h1` per page; contrast clean in both themes.
- **Copy**: "Sends to", "Added by", "Next runs, in <timezone>", "Still down", "No readings in this range yet. podkit samples every 30 seconds." are plain and consistent. The backup destinations lede names the next step ("Add a bucket below, test it, and pick it ...").
- **Audit log**: screenshots in `headline/audit-log-*`; no overflow in either browser.
- **No third-party requests** from any page; works over plain HTTP.

## Not reviewed

- Real agent and deploy flows: no agent was running, so nothing is deployed, and charts come from the check's own samples only.
- Keyboard walk with focus screenshots on every page: the focus ring was checked on dialogs, the save bar and tabs only.
