# Quiet: wave B design review (DR-B)

Reviewed on `roadmap/DR-B` (from `roadmap/INT-B` at `b6d1665`), against the Quiet README, `components.html`, `REVIEW.md`, `REVIEW-infra.md`, `REVIEW-wave-A.md`.

**How.** One isolated podkit (own data dir `/tmp/pk-t_b6efe1ba`, own Postgres database, ports 26411-26413, none of chiko's) with a seeded team: an image app, a Git app, a team and an instance identity provider (GitHub), 11 sessions, audit entries. No agent was running, so nothing is deployed. Every new page and state was walked in **dark and light, 1440 and 390 px in Chromium**, and in **Firefox 160** at 1440 and 500 px (the narrowest its window goes) in both themes. A script checked, on every page and in every mode, horizontal overflow, one `h1`, contrast, controls with no accessible name, third-party requests and card/first-text alignment (32 pages in four modes in Chromium, 32 in four modes in Firefox). The save bar, the dirty dialog and every dialog of the new pages were opened by hand (focus, Esc, size at 390 px).

Pages covered: Settings → Authentication (and a provider's page), Import, Builds, Tailscale, Usage & limits, Audit log; Logs; Servers, a server's Firewall, Networks and Monitoring tabs; Notifications; Account and Account → Security; the sign-in page (with and without a provider button); an application's Git (autodeploy, previews, freeze windows), Build, Domains (access rules, basic auth, private access), Networks, Environment, Settings (replicas), Logs, Terminal, Monitoring and Security tabs.

Severity: **must** (visibly off the design or hurts use), **should** (noticeable), **nit**. Status: **F** fixed here, **FU** follow-up (needs Rust logic or a product call).

Screenshots are in `review-wave-B/`: `headline/` has the headline pages in dark and light at 1440 and 390 px, `*-before` / `*-after` pairs show a fix, `firefox/` has the Firefox passes, `run/` the scripts used to seed, walk and audit.

## Fixed here

| # | Sev | Where | Issue | Status | Shots |
|---|-----|-------|-------|--------|-------|
| 1 | must | A Git application, every tab | 17 tabs are wider than the page at 1440 px. The row scrolls with its scrollbar hidden, so the last tab (`Advanced`) was cut mid-word and nothing said there was more. | F: from 16 tabs the gaps close from 16 to 12 px (all 17 fit at 1440); each end of any tab row now fades while there are tabs beyond it (`@property` + a scroll-driven animation on the row, no script). | `git-tabs-before/after` |
| 2 | must | Settings → Authentication, the provider lists | A provider's row was flush with the card edge (13 px, every other list is 17) and sat between two rules. | F: `.ui-card > .ui-sec-list` takes the card inset and no rule of its own. | `providers-before/after` |
| 3 | must | Account → Security, Passkeys | The "Add a passkey" button touched the name field. | F: `[data-passkey-register-form]` is a grid with the form gap. | `passkeys-before/after` |
| 4 | should | Account → Security (Passkeys), Application → Logs | An empty state that shares its card body with something else (the passkey form, the log chooser) took the body's padding and its own, so its text sat 16 px right of the card's heading. | F: `.ui-card-b > .ui-empty:not(:only-child)` keeps the inset only. | `passkeys-before/after`, `app-logs-before/after` |
| 5 | should | Account, Account → Security | "Open security settings" and "Sign out all other sessions" were stretched to the card's 670 px. | F: a lone button in a card body is as wide as its content. | `sessions-before/after` |
| 6 | should | Usage & limits, Team limits | The unit was on a line of its own under the label (`CPU` / `(cores)`), so the fields in one row did not start at the same height. | F: the label is a block, the unit follows the name. | `usage-before/after` |
| 7 | should | A server's Firewall | The banner read "podkit cannot talk to the firewall helper. the agent did not answer" (a lower-case sentence, no full stop). | F: the reason is shown as a sentence (`sentence()` in `pages/firewall/mod.rs`, with a test). | `firewall-before/after` |
| 8 | should | A server's Firewall, "Always open" | A bold name and its text ran together on one line. | F: the name has an 11 rem column (one line under 40 rem). | `firewall-before/after` |
| 9 | should | Settings → Import | A team with a server that is not ready yet was told "Add a server first". | F: "No server is ready yet. Add one under Servers, or wait until it answers." | `import-before/after` |
| 10 | should | Sign-in, "Sign in with GitHub" | The provider's mark kept its 28 px tinted plate, heavier than the passkey icon under it. | F: a mark inside a button is 20 px with no plate, like the icons. | `signin-sso-before/after` |
| 11 | nit | Sign-in | The "Version / Sign-in" lines under the card started 8 px right of the form. | F: `.ui-auth-meta` takes no side padding. | `signin-sso-before/after` |

## Follow-ups (not done here)

| # | Sev | Where | Issue | Status |
|---|-----|-------|-------|--------|
| FU1 | should | Settings → Authentication, two-step policy | The policy form wraps only the first card, so its save bar appears **in the flow**, between two cards, and the cards below jump 46 px + the gap when it shows. On every other page the form wraps the page and the bar sits at the bottom (it is sticky there). Fix: one form around the whole page, or the policy card last. | FU (Rust, markup of the form) |
| FU2 | should | A server's Firewall: "Save ranges" | An edit form with a button of its own, where the README says the save bar. Same as wave A's FU1; it needs a signal-dirty wiring. The add forms (add allow, add provider, add user) rightly keep their button. | FU (Rust and signals) |
| FU3 | should | Logs | Two sets of the same filters: Range / Level chips above, All / Info / Warn / Error chips inside the viewer, and "Live tail" next to "Follow". Pick one set. | FU (product call) |
| FU4 | should | Sign-in, the lines under the card | "Sign-in: Local password" is fixed text, even when the page offers a provider and a passkey. | FU (Rust, `layout::auth_meta`) |
| FU5 | should | Application tabs | When the row scrolls (under about 1400 px, or a narrow window) the current tab can sit under the fade. `scrollIntoView({ inline: 'nearest', container: 'nearest' })` of `[aria-current]` on load, in `page.js`, would keep it in view. | FU (JS) |
| FU6 | should | Settings → Authentication | The page is two screens of forms: the team and the instance "Add a provider" cards are always open, 10 fields each. An "Add a provider" button that opens the form (or a dialog) would keep the list in view. | FU (product call) |
| FU7 | nit | Usage & limits | "Model tokens (tokens)" and "Build minutes (minutes)" repeat the unit. | FU (Rust, `input_unit`) |
| FU8 | nit | Servers → Networks | Header "Outbound" over a badge "Outbound". A header "Internet" with "Allowed" / "No outbound" reads better. Infra batch, not wave B. | FU (Rust) |
| FU9 | nit | Settings → Authentication | A provider named after its kind shows "GitHub / GitHub". Show the kind only when the name differs. | FU (Rust) |
| FU10 | nit | Firefox | The accessible-name audit lists the `<select>` button (`<button><selectedcontent>`) as unnamed on Authentication, Builds, Logs and Servers. It is the browser's own button of a customisable select, and the select has its label. Same as wave A's FU3. | none |
| FU11 | nit | Account, Account → Security | These two pages are 700 px wide, the settings pages 1016 px. Intended (a person's page, not a team's), but it is the only place the column narrows. | none |
| FU12 | nit | `components.css` | `.ui-facts` is still defined three times (wave A's FU2). | FU (cleanup) |

Not reproduced: the INT-B report said the server card on `/servers` shows no focus ring when tabbed. It does, in Chromium (Tab reaches `.ui-row-link`, the row draws a 2 px ring) and in Firefox (`:focus-visible` matches, the row outline is solid 2 px).

## Checked and fine

- **Save bar**: not rendered when clean; the same slide and fade as before; seen on Builds, Usage & limits, Logs retention, Audit retention, Application Settings (replicas), Git (autodeploy, freezes), Build, Domains (access rules), Tailscale. The dirty dialog opens on leaving a dirty form, focuses "Keep editing", closes on Esc. Only the in-flow bar of FU1 is off.
- **Dialogs**: Import into this team, Install the firewall helper, Migrate networks, New / Import network, the shared confirm (passkey remove) and the dirty dialog were opened and measured: 504 px wide, no overflow, a safe button focused first (Cancel, Not yet, Keep editing) or the first field. At 390 px they are bottom sheets, with no overflow.
- **Card and empty-state alignment**: after the fixes, every card's heading, first text and empty-state title start on the card inset on all wave B pages. Standalone empty states keep the dashed box.
- **Layout**: no horizontal overflow at 390 px (Chromium) or 500 px (Firefox) on any page, in both themes; one `h1` per page, including sign-in.
- **Contrast**: 0 failures on every page, both themes, both browsers.
- **Copy**: "Redirect URL", "Write-only: podkit stores it encrypted and never shows it again.", "Nothing metered yet. Usage is written once an hour ...", "This application isn't running." with a next step, the freeze windows hint ("Friday 16:00 until Monday 08:00 wraps over the weekend.") are plain and consistent.
- **Terminal tab**: the page and its empty state ("There is no container to open a shell in. Start it and come back.") were reviewed. The xterm screen itself was not (no container).
- **No third-party requests** from any page, both browsers; works over plain HTTP.

## Not reviewed

- Real agent and deploy flows: nothing was deployed, so the replica list, previews of a real pull request, access rules answering 403, build logs, the web terminal with a live shell and the log viewer with real lines were not seen with data. Their empty and error states were.
- An OIDC round trip, a passkey registration and an authenticator-app setup (the QR code and recovery codes), which need a provider, a virtual authenticator or a phone. Their static states (the cards, the empty states, the provider form) were.
- A keyboard walk with focus screenshots on every page: the focus ring was checked on dialogs, the save bar, tabs and the server row only.
