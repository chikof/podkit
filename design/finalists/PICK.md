# Pick sheet: round 2 → round 3

Open `index.html` (same screen, same theme, three columns, **Play motion** button), then fill in the last section.

| Finalist | Strengths | Weaknesses | Build effort | CSS / fonts (prototype) |
|---|---|---|:-:|---|
| **01 Cockpit** | Highest round 1 score; most distinctive; densest at a glance; status is glyph plus word; one font file. | Mono body copy tires on long text; amber doubles as accent and warning; tight at 390 px; needs a status bar piece. | M | 63 KB / 16 KB gz · 39 KB fonts |
| **02 Quiet** | Closest to the current component set (lowest build risk); calm; best long-text and chat readability; easiest at 390 px. | Least distinctive; a healthy environment shows little colour; metrics hidden until hover; blurred sticky bar. | S | 69 KB / 18 KB gz · 70 KB fonts |
| **09 Panelboard** | Best for server and metrics pages; clear grouping through panels; large coloured values; strongest data hierarchy. | Busiest palette; variables bar needs real filtering; heaviest CSS; extra chrome above the content. | L | 74 KB / 19 KB gz · 74 KB fonts |

Effort: **S** = token swap plus re-skinned components, a few days. **M** = new layouts for several pages and some new shell pieces, about two weeks. **L** = new shell behaviour (needs backend or state work beyond styling), three weeks or more. Current baseline for comparison: 71 KB CSS, 140–148 KB fonts.

## What all three share

- 13 screens x dark and light x desktop and 390 px, one component sheet, a keyboard-complete prototype with native `<dialog>`, popover, invokers, radio tabs and a command palette.
- Every text pair 4.5:1 and every control border / focus ring / selected fill / status dot / chart stroke 3:1, in both themes (`python3 <slug>/contrast.py`, exit code 0).
- No external requests; works from `file://`.

## Designer's read (opinion, not a verdict)

- **Lowest risk to ship, easiest to extend: 02 Quiet.** Its list-first structure is closest to how `table.rs` and the resource rows already work, and it has the smallest delta from today's component set.
- **Most distinctive and most opinionated: 01 Cockpit.** Highest first-impression score in round 1; the price is mono everywhere (long prose in the playground, narrower rows at 390 px) and amber doing two jobs (accent and "building").
- **Best for the server and metrics pages: 09 Panelboard.** The variables bar only pays off when it actually scopes data, which is backend work, hence L. Worth lifting its metric panels into whichever finalist wins.
- A mix is possible and cheap at this stage: tokens and components are separate from layout, e.g. Quiet's frame with Panelboard's server page.

## Final pick (chiko)

- **Pick:** [ ] 01 Cockpit  [x] 02 Quiet  [ ] 09 Panelboard  [ ] mix: ______________________
- **Keep / drop / change before it is built** (free text): 

- **Theme to ship as default:** [ ] dark  [ ] light  [x] follow system
- **Decision date / by:** 
