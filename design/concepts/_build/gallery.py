import json, os
from lib import ROOT

SCREENS = [("projects", "Projects"), ("environment", "Environment"), ("app", "App + build log"), ("templates", "Templates + deploy"), ("server", "Server"), ("playground", "Playground")]

GALLERY = r'''<!doctype html>
<html lang="en" data-theme="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>podkit design round 1: 10 concepts</title>
<link rel="icon" href="data:,">
<style>
:root{--bg:#101113;--panel:#17181b;--panel-2:#1e2024;--line:#2a2c31;--text:#ececef;--text-2:#a8abb3;--accent:#f5c451;--accent-ink:#171204;--focus:#7cc4ff;color-scheme:dark}
:root[data-theme="light"]{--bg:#f3f3f1;--panel:#ffffff;--panel-2:#ecece8;--line:#d9d9d3;--text:#17181b;--text-2:#55575f;--accent:#b4780a;--accent-ink:#ffffff;--focus:#0b5fcc;color-scheme:light}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--text);font:14px/1.5 system-ui,-apple-system,"Segoe UI",Roboto,sans-serif}
a{color:inherit}
:focus-visible{outline:2px solid var(--focus);outline-offset:2px}
header.top{position:sticky;top:0;z-index:20;background:color-mix(in oklab,var(--bg) 90%,transparent);backdrop-filter:blur(8px);border-bottom:1px solid var(--line)}
.top-in{display:flex;gap:16px;align-items:center;flex-wrap:wrap;padding:12px 24px;max-width:1800px;margin:auto}
h1{font-size:18px;margin:0;font-weight:650;letter-spacing:-.01em}
.sub{color:var(--text-2);font-size:13px}
.grow{flex:1}
.seg{display:inline-flex;border:1px solid var(--line);border-radius:8px;overflow:hidden;background:var(--panel)}
.seg button{border:0;background:none;color:var(--text-2);padding:7px 14px;font:inherit;cursor:pointer}
.seg button[aria-pressed="true"]{background:var(--accent);color:var(--accent-ink);font-weight:600}
.seg button:not([aria-pressed="true"]):hover{background:var(--panel-2);color:var(--text)}
.btn{border:1px solid var(--line);background:var(--panel);color:var(--text);padding:7px 14px;border-radius:8px;font:inherit;cursor:pointer;text-decoration:none;display:inline-block}
.btn:hover{background:var(--panel-2)}
label.chk{display:inline-flex;gap:6px;align-items:center;color:var(--text-2);cursor:pointer}
main{max-width:1800px;margin:auto;padding:8px 24px 80px}
.concept{padding:28px 0 8px;border-bottom:1px solid var(--line)}
.c-head{display:flex;gap:16px;align-items:baseline;flex-wrap:wrap;margin-bottom:4px}
.c-num{font:600 13px ui-monospace,monospace;color:var(--accent)}
.c-name{font-size:20px;font-weight:650;margin:0}
.c-idea{color:var(--text-2);margin:0 0 12px;max-width:100ch}
.moods{display:flex;gap:6px;flex-wrap:wrap}
.moods span{font-size:12px;border:1px solid var(--line);border-radius:99px;padding:1px 10px;color:var(--text-2)}
.strip{display:flex;gap:14px;overflow-x:auto;padding:6px 2px 18px;scroll-snap-type:x proximity}
.shot{flex:none;scroll-snap-align:start}
.frame{position:relative;width:274px;height:171px;overflow:hidden;border:1px solid var(--line);border-radius:8px;background:var(--panel)}
.frame iframe{position:absolute;left:0;top:0;width:1440px;height:900px;border:0;transform:scale(.19);transform-origin:0 0;pointer-events:none;background:#fff}
.frame a.cover{position:absolute;inset:0;z-index:2}
.frame a.cover:hover{background:rgb(255 255 255 / .05);outline:2px solid var(--accent);outline-offset:-2px}
.cap{font-size:12px;color:var(--text-2);margin-top:6px;display:flex;justify-content:space-between}
/* compare */
#compare{display:none;padding:20px 0}
body.is-compare #rows{display:none}body.is-compare #compare{display:block}
.cmp-bar{display:flex;gap:12px;flex-wrap:wrap;align-items:center;margin-bottom:16px}
select{background:var(--panel);color:var(--text);border:1px solid var(--line);border-radius:8px;padding:7px 10px;font:inherit}
.cmp-grid{display:grid;grid-template-columns:1fr 1fr;gap:16px}
.cmp-col h2{font-size:15px;margin:0 0 8px}
.cmp-col h2 a{color:var(--text-2);font-weight:400;font-size:12px;margin-left:8px}
.cmp-frame{position:relative;width:100%;aspect-ratio:1440/900;border:1px solid var(--line);border-radius:8px;overflow:hidden;background:var(--panel)}
.cmp-frame iframe{position:absolute;left:0;top:0;width:1440px;height:900px;border:0;transform-origin:0 0;background:#fff}
@media (max-width:900px){.cmp-grid{grid-template-columns:1fr}}
.note{color:var(--text-2);font-size:12.5px;max-width:110ch;margin:8px 0 0}
</style>
</head>
<body>
<header class="top"><div class="top-in">
  <div><h1>podkit: design round 1</h1><div class="sub">10 directions, same 6 screens. Click any thumbnail to open the live page.</div></div>
  <div class="grow"></div>
  <div class="seg" role="group" aria-label="Theme"><button type="button" data-theme-btn="dark" aria-pressed="true">Dark</button><button type="button" data-theme-btn="light" aria-pressed="false">Light</button></div>
  <div class="seg" role="group" aria-label="View"><button type="button" data-view="rows" aria-pressed="true">All concepts</button><button type="button" data-view="compare" aria-pressed="false">Compare 2</button></div>
  <label class="chk"><input type="checkbox" id="play"> Play animations</label>
  <a class="btn" href="RATING.md">RATING.md</a><a class="btn" href="LICENSES.md">Licenses</a>
</div></header>
<main>
<div id="rows"></div>
<section id="compare" aria-label="Compare two concepts">
  <div class="cmp-bar"><label>Left <select id="selA"></select></label><label>Right <select id="selB"></select></label><label>Screen <select id="selS"></select></label><span class="sub">Frames are live: scroll, hover, press. Use "Play animations" to watch the motion.</span></div>
  <div class="cmp-grid"><div class="cmp-col" id="colA"></div><div class="cmp-col" id="colB"></div></div>
</section>
<p class="note">Thumbnails render the first 900 px of each page at 1440 px wide, with animation frozen (<code>?still=1</code>). Cross-document view transitions only run over http (<code>python3 -m http.server -d design/concepts 8765</code>); file:// pages are opaque origins.</p>
</main>
<script>
const DATA = @@DATA@@;
const SCREENS = @@SCREENS@@;
const $ = (s, r = document) => r.querySelector(s), $$ = (s, r = document) => [...r.querySelectorAll(s)];
let theme = 'dark', view = 'rows';
try { theme = localStorage.getItem('pk-gallery-theme') || (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark'); } catch (e) {}
const play = () => $('#play').checked;
const url = (c, s, still = true) => `${c.id}/${s}.html?theme=${theme}${still && !play() ? '&still=1' : ''}`;
const rows = $('#rows');
rows.innerHTML = DATA.map(c => `<section class="concept" id="c-${c.id}" aria-labelledby="h-${c.id}">
  <div class="c-head"><span class="c-num">${c.id.slice(0, 2)}</span><h2 class="c-name" id="h-${c.id}">${c.name}</h2><a class="btn" href="${c.id}/index.html">Concept card</a><div class="moods">${c.mood.map(m => `<span>${m}</span>`).join('')}</div></div>
  <p class="c-idea">${c.idea}</p>
  <div class="strip">${SCREENS.map(([k, n]) => `<div class="shot"><div class="frame"><iframe loading="lazy" tabindex="-1" title="${c.name}: ${n}" data-c="${c.id}" data-s="${k}"></iframe><a class="cover" data-c="${c.id}" data-s="${k}" target="_blank" rel="noopener" aria-label="Open ${c.name}: ${n}"></a></div><div class="cap"><span>${n}</span></div></div>`).join('')}</div>
</section>`).join('');
const sel = (id, opts, val) => { const e = $(id); e.innerHTML = opts.map(([v, t]) => `<option value="${v}">${t}</option>`).join(''); e.value = val; };
sel('#selA', DATA.map(c => [c.id, c.id.slice(0, 2) + ' ' + c.name]), DATA[0].id);
sel('#selB', DATA.map(c => [c.id, c.id.slice(0, 2) + ' ' + c.name]), DATA[2].id);
sel('#selS', SCREENS, 'environment');
function refresh() {
  document.documentElement.dataset.theme = theme;
  $$('[data-theme-btn]').forEach(b => b.setAttribute('aria-pressed', b.dataset.themeBtn === theme));
  $$('[data-view]').forEach(b => b.setAttribute('aria-pressed', b.dataset.view === view));
  document.body.classList.toggle('is-compare', view === 'compare');
  $$('#rows iframe').forEach(f => { const u = url({ id: f.dataset.c }, f.dataset.s); if (f.dataset.u !== u) { f.dataset.u = u; f.src = u; } });
  $$('#rows a.cover').forEach(a => a.href = url({ id: a.dataset.c }, a.dataset.s, false));
  if (view === 'compare') renderCompare();
}
function renderCompare() {
  const s = $('#selS').value;
  [['A', '#colA'], ['B', '#colB']].forEach(([k, id]) => {
    const c = DATA.find(x => x.id === $('#sel' + k).value); const col = $(id); const u = url(c, s, true);
    let fr = $('iframe', col);
    if (!fr) { col.innerHTML = `<h2></h2><div class="cmp-frame"><iframe title=""></iframe></div>`; fr = $('iframe', col); }
    $('h2', col).innerHTML = `${c.id.slice(0, 2)} ${c.name} <a href="${c.id}/index.html">concept card</a> <a href="${url(c, s, false)}" target="_blank" rel="noopener">open page</a>`;
    fr.title = c.name + ' ' + s; if (fr.dataset.u !== u) { fr.dataset.u = u; fr.src = u; }
  });
  fit();
}
function fit() { $$('.cmp-frame').forEach(f => { const k = f.clientWidth / 1440; $('iframe', f).style.transform = `scale(${k})`; }); }
new ResizeObserver(fit).observe(document.body);
$$('[data-theme-btn]').forEach(b => b.addEventListener('click', () => { theme = b.dataset.themeBtn; try { localStorage.setItem('pk-gallery-theme', theme); } catch (e) {} refresh(); }));
$$('[data-view]').forEach(b => b.addEventListener('click', () => { view = b.dataset.view; refresh(); }));
['#selA', '#selB', '#selS', '#play'].forEach(i => $(i).addEventListener('change', () => { $$('#rows iframe').forEach(f => f.dataset.u = ''); refresh(); }));
refresh();
</script>
</body>
</html>
'''

def write_gallery(concepts, lic):
    data = [dict(id=c["id"], name=c["name"], idea=c["idea"], mood=c["mood"]) for c in concepts]
    html = GALLERY.replace("@@DATA@@", json.dumps(data)).replace("@@SCREENS@@", json.dumps(SCREENS))
    open(f"{ROOT}/index.html", "w").write(html)

    # RATING.md
    rows = []
    for c in concepts:
        rows.append(f"| {c['id'][:2]} | **{c['name']}** ({c['id'][3:]}) | | | | | | | |")
    md = ("# Round 1 rating sheet\n\n"
          "Open the gallery: `xdg-open design/concepts/index.html` (or `python3 -m http.server -d design/concepts 8765`, then http://localhost:8765).\n\n"
          "Score each concept **1 (weak) to 5 (strong)**. Judge the concept as a whole across all six screens, in the theme you would actually use. Use the notes column for anything specific: what to keep, what to drop, what to steal for another concept.\n\n"
          "| # | Concept | First impression | Clarity / scanability | Consistency | Smoothness | Distinctiveness | Fits podkit | Notes |\n"
          "|---|---|:-:|:-:|:-:|:-:|:-:|:-:|---|\n" + "\n".join(rows) + "\n\n"
          "**Top 3:** 1. ____  2. ____  3. ____\n\n"
          "**Hybrid ideas (optional):** e.g. \"navigation of 04, density of 01, motion of 06\"\n\n"
          "## What the columns mean\n\n"
          "- **First impression**: gut reaction on the Projects and Environment screens.\n"
          "- **Clarity / scanability**: can you tell what is running, building or broken in two seconds?\n"
          "- **Consistency**: does every screen feel like the same product (frame, spacing, components, type)?\n"
          "- **Smoothness**: hover/press/focus feedback, the building to running flip, dialog enter/exit, live log, page-to-page transition. Open the concept card and the app screen with \"Play animations\" on. Needs http for the page transitions.\n"
          "- **Distinctiveness**: is it clearly not shadcn / not the current redesign?\n"
          "- **Fits podkit**: self-hosted ops tool with AI models; can it carry 484 templates, GPU charts and a chat playground without strain?\n")
    open(f"{ROOT}/RATING.md", "w").write(md)

    # LICENSES.md
    fl = lic
    lines = ["# Licenses", "", "Everything under `design/concepts` is permissively licensed or a trademark used only as a design placeholder. No runtime network requests: all fonts and icons are local files.", "",
             "## Fonts (self-hosted, latin subset, woff2)", "",
             "All fonts are SIL Open Font License 1.1 (OFL), obtained as latin-subset woff2 from the Fontsource packages (npm, https://fontsource.org), which redistribute the upstream OFL fonts. The OFL permits embedding, self-hosting and redistribution; the font name is reserved, so the files are shipped unmodified.", "",
             "| Concept | Font | License | Package (version) |", "|---|---|---|---|"]
    for c in concepts:
        for name, l, role in c["fonts"]:
            base = name.split(" (")[0]
            pk = [(k, v) for k, v in fl.items() if k.split("/")[-1] == base.lower().replace(" ", "-")]
            pkg = f"{pk[0][0]} ({pk[0][1][1]}), declared `{pk[0][1][0]}`" if pk else "fontsource"
            lines.append(f"| {c['id']} | {base}, {role} | {l} | {pkg} |")
    lines += ["", "## Icons", "", "Lucide (ISC License), the same sprite podkit already ships in `server/static/icons.svg`; the few extra glyphs come from `lucide-static` on npm. Icons are inlined per page as an SVG sprite.", "",
              "## Brand and product logos (`_assets/logos`)", "",
              "Copied from podkit's existing `server/static/logos` (sourced from homarr-labs/dashboard-icons, Apache-2.0, see its `SOURCES.tsv`) and from the `logo.svg|png` files of podkit's template catalog. The marks are trademarks of their owners and are used here only as realistic placeholders so the mockups show real resource names. They are not part of any concept's identity and must be swapped or confirmed before anything ships.", "",
              "## podkit mark", "", "The inline pod glyph in the app frame is a throwaway placeholder drawn for these mockups (two rounded rectangles and a dot); it is not a proposed logo.", ""]
    open(f"{ROOT}/LICENSES.md", "w").write("\n".join(lines))
