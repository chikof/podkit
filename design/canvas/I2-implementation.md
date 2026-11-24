# Canvas: I2 implementation notes

What shipped against `README.md` (D1), what differs from the mockups, and how it behaves in
Firefox. Screenshots are in `screenshots/i2/` (Chromium 157 and Firefox 160, dark and light,
1440 and 390 px, from an isolated stack seeded with the D1 `storefront` project).

## `canvas.js` size

| | D1 estimate | Shipped |
|---|---|---|
| readable source | 12 to 15 KB | **17,666 bytes** |
| minified (esbuild, measured) | about 9 KB | **12,038 bytes** |
| gzip of the source | about 3.5 KB (of the minified) | 6,050 bytes (4,525 minified) |

The source is about 2.7 KB over the top of the estimate, and I left it there on purpose.
D1 sized a prototype that does not wire anything through a server. The shipped file also
carries parts the estimate did not count: the per-node and per-link action menu (built with
DOM calls, never markup, so labels stay escaped), the three ways to start a connection
(drag a handle, click then click, the `C` key) sharing one state machine, the live-region
announcements for every step of that machine, the arrow-key walk along links and columns,
and the popover placement and focus return. Cutting them would remove keyboard and
screen-reader paths the D1 accessibility section requires. It loads `defer`, only on the
canvas page, and goes out compressed.

## Against the D1 mockups

Compared by eye, 1440 and 390, dark and light, against `screenshots/canvas-*.png`.

Matches:
- Toolbar (view switch, link labels, networks, Connect, zoom, fit), dotted grid, legend and
  hint pills in the same places and treatments; environment frames with a title, summary and
  zoom-to-frame button; dashed network frames; node card anatomy (logo tile, name, mono
  detail, status dot and word, menu button); link labels as pills at the midpoint; the model
  gateway as a dotted line with a gateway pill; red border on failed nodes; handles on the
  left and right edges.
- Wire flow: dragging from a handle highlights valid targets, release opens the confirm
  popover listing the exact changes (Gateway, Env vars, Redeploy), with Cancel and a primary
  button that is focused. `Esc` closes it and returns focus to the source node.
- 390 px: no canvas, the list view is the page, with one section per environment and
  captioned rows.

Differences:
1. **Page chrome.** D1's header has a primary "Create resource" button and a counts line
   (`6 running · 1 building · 1 failed · 2 networks`). Shipped has the ghost "Resource list"
   button and `8 resources in 2 environments`. Create resource is only shown on the empty
   state. The counts are on each environment frame instead.
2. **View switch.** D1 draws Canvas/List as a segmented pill; shipped uses the existing Quiet
   tabs underline, to match the other project pages.
3. **Network frame names.** D1 shows `env-prod`; shipped shows the N1 network name, for
   example `production (storefront)`, with `auto` after it for environment networks.
4. **Scan badges.** D1's mockup shows Critical, High and Medium badges. The seed has no scan
   results, so none render here; the list view reads `Not scanned` where D1 shows `n/a`. The
   node card renders the badge when a scan exists (the graph test fixture sets a Critical one);
   I did not screenshot it.
5. **Statuses.** The seeded stack and two apps show Failed because wiring redeploys them and
   there is no agent behind the isolated server; D1 shows Running. This is data, not layout.
6. **Content width.** The shipped canvas sits in the standard page width (about 1020 px of
   map at 1440), so the initial fit is 80%; D1's mockup used the wide layout at 90%.
7. **Mobile rows.** D1 has an icon-only 44 px plug button per row; shipped has a labelled
   "Connect..." button, which needs no extra markup to have an accessible name. Its height
   was not measured against 44 px.
8. **Fit is once.** The map is fitted when the view first has a size. Resizing the window
   afterwards does not refit; `F` or the fit button does.

## Firefox 160

- Rendering matches Chromium: frames, edges, labels, the `@property`-driven pan and zoom,
  and the popover (top layer) all look the same. No console errors.
- Drag from a handle to a valid node opens the confirm popover with the same text; `Esc`
  closes it; `C` on a focused node starts connecting (`is-connecting`).
- `focusgroup` (the menu's arrow keys) is not implemented in Firefox, so the menu has a small
  `keydown` handler for Up and Down; Tab still reaches every item.
- Headless Firefox does not deliver synthetic key events to an open popover (also seen in
  D1), so `Enter` on the primary button was checked in Chromium only. The pointer path
  (drag, release, popover) was checked in both.
- Safari and a screen reader were not tried.

## Keyboard walk (Chromium)

Focus a node, `C` (live region: "Connecting checkout-worker. Valid targets: ..."), `ArrowRight`
moves among valid targets, `Enter` opens the popover, `Esc` closes it and focus returns to
`checkout-worker`.

## Live test

`crates/core/src/networks/tests/canvas_live.rs`, ignored. It creates an app and a Postgres in one
environment on a real Podman and runs `projects::graph::wire::wire`, the function the canvas
POST calls. It checks that `DATABASE_URL` is absent before, is stored as an ordinary
variable by the wire, is present in the redeployed container, names the Postgres, and that
the container gets a Postgres protocol answer from it; then unwires and checks the variable
is gone. Run with:

```text
PODKIT_LIVE_PODMAN_SOCKET=/path/podman.sock \
  cargo test -p podkit-core --all-features -- --ignored canvas_live
```
