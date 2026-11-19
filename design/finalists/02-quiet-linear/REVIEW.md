# Quiet: implementation review (Q8)

Reviewed on `quiet/Q8-design-review` (from `quiet/integration`), against this folder's README and the reference screens. Run isolated, dark and light, 1440 px; 390 px on the 10 main pages; real stack with a deployed app, a database, a stack, a template, and a downloaded and started model (Qwen3 4B, CPU) for the playground.

Screenshots are in `/tmp/pk-q8/shots/` (`<page>-<theme>-<width>-before|after.png`, reference shots `*-ref.png`). The six headline pairs are in `/tmp/pk-q8/headline/`.

Severity: **must** (visibly off the design or hurts use), **should** (noticeable), **nit**. Status: fixed (F) or follow-up (FU, needs Rust logic or a test).

## Fixed here

| # | Sev | Page | Issue | Status |
|---|-----|------|-------|--------|
| 1 | must | all | Tailwind-named text classes rendered at 12/14/16/18/24 px, off Quiet's 11.5/13.5/15/18/22 scale. Body text read about 0.5 px too large. | F: one mapping block in `components.css` ("design review") |
| 2 | must | app, database, stack | Resource page title at 28 px; the design uses 22 px (`--fs-2xl`) with 1.2 line height. | F: `.ui-head > .ui-head-id h1` |
| 3 | must | app | Deployment facts (Status, Trigger, Started, Duration) stacked into one column. A second `.ui-facts` rule later in the sheet overrode the row layout. | F: `.ui-dep .ui-facts` restores the row |
| 4 | must | template deploy, model set-up | Dialogs filled by a Datastar patch had no head/body/foot: title in plain text, close button absolutely positioned, fields in a bare `form`. | F: `deploy.rs`, `setup.rs`, `catalog.rs` use `ui-dialog-head/body/foot`; `.ui-dlg-panel` adds no box |
| 5 | must | template deploy | Variable fields had no `ui-label`/`ui-hint`; the secret note was loose text. | F: `var_field` markup |
| 6 | should | app webhooks, app create | "Copy the secret now" was red body text. | F: `ui-banner ui-banner-warn` |
| 7 | should | nav | Projects used a folder icon, Templates a stack-of-layers icon; the design has layout-grid and package. | F: `shell/nav.rs` |
| 8 | should | template detail | Eyebrow was uppercase with tracking; Quiet's label tracking is 0 and no uppercase. | F: `detail.rs` |
| 9 | should | environment | The hairline under the tab row stopped at the last tab; it runs the full width in the design. | F: `.ui-res-tabrow::after` |
| 10 | should | app | Timeline rail 1 px and 3 px off the dot centre. | F: 2 px, inset 3 px |
| 11 | should | create, database | Option cards: checked state used foreground colour; hover used the accent wash. | F: primary border on `--accent`; hover `--surface-2` |
| 12 | should | all | Scrollbar thumb at 40 % was near-invisible in light; text areas and log used the thick one. | F: 65 %, `scrollbar-width: thin` on text areas and log |
| 13 | should | forms | Labels in a grid stretched when a neighbour wrapped (hint text two lines). | F: `align-content: start` on `.ui-field-group`, `.ui-label` |
| 14 | should | 390 px, app | Breadcrumb let the middle crumbs push the current page's name out; it truncated first. | F: earlier crumbs shrink, current page keeps 6 rem minimum (CSS only; checked by screenshot before, not re-shot after) |
| 15 | should | playground | `.ui-pair` was defined twice (a 2-column grid for the backups page, a stack for an exchange). The later rule won, so the exchange worked by accident; a change to the first would have broken it. | F: the exchange is `.ui-xchg` |
| 16 | nit | head | Description under the page title was not left-aligned with the title when an id chip was present. | F: `.ui-head-sub:has(> .ui-head-desc)` |
| 17 | nit | code in text | Inline `code` set in `font-mono` looked 1 px too big beside body text. | F: `0.92em` |
| 18 | nit | tables | Row hover snapped; every other control eases at `--dur-1`. | F: `transition` on row cells |
| 19 | nit | breadcrumb, header | A stray `text-sm` on the crumb and the header button made both 13.5 px where the design has 12.5. | F: class removed |

## Checked and fine

- **Motion, live**: dialog open eases opacity and scale in about 150 ms, and Esc closes it and returns focus to the button that opened it. The menu opens with a 4 px slide and fade (sampled 0, 0.70, 0.91, 0.95, 0.99, 1.0 over about 150 ms). Primary button hover changes colour (press not sampled). The command palette opens with Ctrl+K, takes focus in the field, filters ("whoa" gives Whoami and the Deploy action), and closes with Esc. Streaming in the playground: first token at 571 ms, the "Thinking…" line is replaced by the text, then the usage figures (481 tokens, 19.5 tok/s). Restart on an app: the badge was sampled, but I did not capture a clean status-change frame; treat that as unreviewed.
- **Reduced motion**: transitions and animations are 0 ms, the dialog opens at full opacity at once.
- **Keyboard**: Tab through create-stack, create-app, create-database, catalog and destinations: 60 stops each, 23 to 46 distinct targets, no trap (no element held focus for more than 2 stops).
- **Theme**: follows the system with no cookie; the toggle sets it.
- **390 px**: screenshots of the 10 main pages were reviewed by eye; the only defect found is #14. Overflow was not re-measured after the fixes.
- **Light**: all 6 headline screens read cleanly; no dark remnants.

## Follow-ups (not done here: Rust logic, new behaviour, or needs a test)

| # | Sev | Page | Issue |
|---|-----|------|-------|
| A | should | app (not deployed) | Header shows Restart and Stop on an app that was never deployed. They should be disabled (`.ui-button[disabled]`) or hidden. Q7 reported it too. |
| B | should | destinations | The Garage logo fallback shows the monogram "Ga". The logo set has no Garage mark; pick a two-letter monogram rule that matches the other tiles or add the mark. |
| C | should | templates | Description mojibake (AList and some others) comes from the template source text, not the UI. Fix where the catalog is imported. |
| D | should | shell | The sidebar no longer folds to icons (Q2 dropped it). The README §5 still describes the fold. Decide: restore it or edit the README. I left the README alone as this is a product call. |
| E | nit | databases/general.rs:200 | `#schedule-hint` still has `text-sm font-normal`; it now lands on 13.5 px, a step bigger than the other hints (`text-xs`). One word to change; I left it because the file is under test. |
| F | nit | models | The playground shows "Thinking…" with no elapsed time while a thinking model works (22 s here). A running counter would help. New behaviour. |
| G | nit | focus | The focus ring on `.ui-link-card-link` is drawn by the parent card (`:has(:focus-visible)`); computed outline on the link itself is none. It shows on screen; a test would pin it. |

## Not reviewed

Firefox and Safari; page transitions between documents (cross-document view transitions are only partly covered: the `@view-transition` rule is present, but I could not observe the transition in headless Chrome); the 390 px pass for server, model and template detail beyond overflow; log streaming at high volume.
