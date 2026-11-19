import os, shutil, sys, json
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import *
import screens as S
from screens import page, head, btn, res_item
B = os.path.dirname(os.path.abspath(__file__))

OFL = "SIL OFL 1.1"
CONCEPTS = [
 dict(id="01-terminal-ops", name="Cockpit", theme="dark", nav="sidebar", tree=True, status=True, charts="step", structure=False,
      idea="A tmux-grade ops console. Everything is monospace, dense and keyboard-first; state is read from glyphs and colour, not from boxes.",
      mood=["dense", "instrument", "phosphor", "no-nonsense"],
      fonts=[("JetBrains Mono (variable)", OFL, "all text")],
      motion="Instant by design. 60–140 ms, stepped easing. Dialogs snap in with a 90 ms fade, no travel. Log lines type in with steps(). Status flips are a single colour blink, the caret blinks, nothing bounces. Page change is a 120 ms hard dissolve. Press has no scale: the row inverts instead.",
      swatch=["bg", "surface", "surface-2", "border", "text", "text-2", "accent", "ok", "warn", "err", "info", "chart-1"]),
 dict(id="02-quiet-linear", name="Quiet", theme="dark", nav="sidebar", tree=False, status=False, charts="line",
      idea="Linear-style restraint. A hushed neutral canvas, one cool signal colour, list rows instead of cards, and motion so quick it reads as responsiveness.",
      mood=["quiet", "fast", "list-first", "precise"],
      fonts=[("Hanken Grotesk (variable)", OFL, "UI + headings"), ("Spline Sans Mono (variable)", OFL, "code, ids, numbers")],
      motion="Short and eased-out. 90 / 160 / 240 ms on cubic-bezier(.2,.8,.2,1). Hover is a background wash only. Dialogs rise 6 px and fade; the page change is a 180 ms fade with a 6 px lift. Rows stagger in at 25 ms. Press scales to 0.98.",
      swatch=["bg", "surface", "surface-2", "border", "text", "text-2", "accent", "accent-soft", "ok", "warn", "err", "chart-1"]),
 dict(id="03-mono-precision", name="Hairline", theme="light", nav="topbar", tree=False, status=False, charts="line",
      idea="Vercel-grade monochrome. Pure black on white with 1 px hairlines and a two-tier top bar. Colour appears only when something is wrong or live.",
      mood=["monochrome", "hairline", "engineered", "confident"],
      fonts=[("Geist (variable)", OFL, "UI + headings"), ("Geist Mono (variable)", OFL, "code, metadata")],
      motion="Sharp and decisive. 100 / 150 / 220 ms, cubic-bezier(.4,0,.2,1). Hover flips a hairline from grey to black; primary buttons invert on hover. Dialogs scale from 0.97 with a 150 ms fade. Page change is a 160 ms fade. Charts draw their area in 220 ms.",
      swatch=["bg", "surface", "surface-2", "border", "border-strong", "text", "text-2", "accent", "ok", "warn", "err", "info"]),
 dict(id="04-tinted-dark", name="Emerald Night", theme="dark", nav="rail", tree=False, status=False, charts="area",
      idea="A tinted-dark workbench: deep green-black surfaces, one emerald accent, an icon rail and generous cards with glow-edged focus.",
      mood=["tinted", "developer", "emerald", "calm-dark"],
      fonts=[("DM Sans (variable)", OFL, "UI + headings"), ("Source Code Pro (variable)", OFL, "code, metrics")],
      motion="Smooth expo-out. 120 / 200 / 320 ms, cubic-bezier(.16,1,.3,1). Focus draws a soft emerald halo. Cards lift 2 px and their border brightens. Dialogs scale from 0.96 and fade; the rail shows labels as tooltips. Page change slides 12 px and fades in 260 ms.",
      swatch=["bg", "surface", "surface-2", "border", "text", "text-2", "accent", "accent-soft", "ok", "warn", "err", "chart-2"]),
 dict(id="05-editorial", name="Gazette", theme="light", nav="masthead", tree=False, status=False, charts="area",
      idea="A warm editorial ledger. A newspaper masthead, serif display type, ruled columns, and resources told as numbered entries on cream paper.",
      mood=["warm", "editorial", "ledger", "unhurried"],
      fonts=[("Fraunces (variable)", OFL, "display, headings"), ("Newsreader (variable)", OFL, "body"), ("Courier Prime", "SIL OFL 1.1", "code, figures")],
      motion="Slow, like turning a page. 200 / 360 / 640 ms, cubic-bezier(.45,.05,.2,1). Entries fade up in an 80 ms stagger, dialogs unfold from a 12 px drop. Page change is a 500 ms cross-dissolve. Link hovers draw an underline. Press is a tiny 0.99 settle.",
      swatch=["bg", "surface", "surface-2", "border", "border-strong", "text", "text-2", "accent", "ok", "warn", "err", "chart-1"]),
 dict(id="06-playful", name="Gumdrop", theme="light", nav="dock", tree=False, status=False, charts="bars",
      idea="Playful and colourful. Chunky outlined cards, one colour per resource type, a floating bottom dock and springy motion that rewards every click.",
      mood=["playful", "chunky", "springy", "colourful"],
      fonts=[("Bricolage Grotesque (variable)", OFL, "UI + headings"), ("Martian Mono (variable)", OFL, "code, numbers")],
      motion="Springy. 140 / 280 / 520 ms with a linear() spring that overshoots ~8%. Press squashes to 0.93, hover tilts a card 0.6°, status badge pops on change, dialogs bounce in from 24 px. Page change scales and slides in 360 ms. Toasts pop from the dock.",
      swatch=["bg", "surface", "border-strong", "text", "text-2", "accent", "accent-2", "accent-3", "ok", "warn", "err", "info"]),
 dict(id="07-glass-depth", name="Aurora Glass", theme="dark", nav="topbar", tree=False, status=False, charts="area",
      idea="Layered glass over a deep teal-to-amber aurora. A floating pill nav, real elevation tiers (blur, tint, edge light) and soft parallax-free depth.",
      mood=["depth", "translucent", "luminous", "layered"],
      fonts=[("Sora (variable)", OFL, "UI + headings"), ("Fira Code (variable)", OFL, "code, metrics")],
      motion="Fluid and weighty. 180 / 320 / 560 ms, cubic-bezier(.22,1,.36,1). Hover lifts a pane 3 px and brightens its edge light; dialogs scale from 0.94 and blur-in over a blurred backdrop. Page change is a 420 ms blur-fade. Backdrop blur is the only blur: it signals elevation, never decoration.",
      swatch=["bg", "surface", "surface-2", "border", "text", "text-2", "accent", "accent-2", "ok", "warn", "err", "chart-1"]),
 dict(id="08-brutalist", name="Slab", theme="light", nav="topbar", tree=False, status=False, charts="bars",
      idea="Brutalist and utilitarian. Heavy black rules, hard offset shadows, uppercase labels and a single hazard yellow. Obvious, loud, unambiguous.",
      mood=["brutalist", "loud", "utilitarian", "unmistakable"],
      fonts=[("Archivo (variable)", OFL, "display, UI"), ("Space Mono", OFL, "code, labels")],
      motion="Mechanical. 80 / 80 / 120 ms, steps(3). Hover shifts a box 4 px into its own hard shadow; press collapses the shadow completely. Dialogs snap in with no fade. Log lines slam in. Page change is a 200 ms stepped wipe. No easing curves anywhere.",
      swatch=["bg", "surface", "surface-2", "border", "text", "text-2", "accent", "accent-2", "ok", "warn", "err", "info"]),
 dict(id="09-data-dashboard", name="Panelboard", theme="dark", nav="topbar", tree=False, status=True, varbar=True, charts="area",
      idea="Grafana-ish panelboard. Everything is a titled panel on a tight grid under a variables bar; sparklines and series colour carry the information.",
      mood=["panels", "telemetry", "variables", "data-dense"],
      fonts=[("IBM Plex Sans (variable)", OFL, "UI + headings"), ("IBM Plex Mono", OFL, "values, ids")],
      motion="Telemetry-grade. 100 / 180 / 300 ms, ease-out. Panels refresh with a 300 ms series draw-in; the Reload control spins the skeleton shimmer across every panel. Dialogs fade with 8 px travel. Page change is a 200 ms fade. Hover reveals panel tools.",
      swatch=["bg", "surface", "surface-2", "border", "text", "text-2", "accent", "ok", "warn", "err", "info", "chart-4"]),
 dict(id="10-calm-soft", name="Meadow", theme="light", nav="sidebar", tree=False, status=False, charts="smooth",
      idea="Calm, light-first and soft. Sage and peach on warm white, a floating rounded sidebar, generous radii, and motion that eases like breathing.",
      mood=["calm", "soft", "airy", "friendly"],
      fonts=[("Figtree (variable)", OFL, "UI + headings"), ("DM Mono", OFL, "code, numbers")],
      motion="Breathing pace. 220 / 400 / 700 ms, cubic-bezier(.3,.7,.2,1). Cards float up 10 px in a 70 ms stagger; hover lifts 2 px with a longer shadow. Dialogs rise and fade over a frosted dim. Page change is a 600 ms soft cross-fade at 0.99 scale. Statuses glow, never flash.",
      swatch=["bg", "surface", "surface-2", "border", "text", "text-2", "accent", "accent-soft", "ok", "warn", "err", "chart-2"]),
]
SCREENS = ["projects", "environment", "app", "templates", "server", "playground"]
TITLES = dict(projects="Projects", environment="storefront / production", app="web · storefront", templates="Templates", server="atlas · Server", playground="Qwen3-4B Playground")

def card(cfg):
    sw = "".join(f'<li class="sw"><span class="sw-chip" style="background:var(--{t})"></span><code>--{t}</code><span class="sw-hex" data-var="--{t}"></span></li>' for t in cfg["swatch"])
    fonts = "".join(f'<tr><th scope="row">{n}</th><td>{lic}</td><td>{role}</td></tr>' for n, lic, role in cfg["fonts"])
    scale = "".join(f'<li style="font-size:var(--fs-{k})"><span class="spec-k">--fs-{k}</span>{t}</li>' for k, t in [("3xl", "Deploy succeeded"), ("2xl", "storefront / production"), ("xl", "6 resources, 4 running"), ("lg", "Qwen3-4B on llama.cpp CUDA"), ("md", "Body text sits here and has to be comfortable for long reads of logs and docs."), ("sm", "Secondary information, metadata, timestamps"), ("xs", "LABEL · CAPTION · 175 TOK/S")])
    d = RES_SAMPLE
    comp = f'''
<div class="cmp-grid">
<section class="cmp" aria-labelledby="c-btn"><h3 id="c-btn" class="cmp-h">Buttons</h3><div class="cmp-row"><button class="btn btn--primary" type="button">{icon("rocket")}<span>Deploy</span></button><button class="btn btn--secondary" type="button">Restart</button><button class="btn btn--ghost" type="button">Cancel</button><button class="btn btn--danger" type="button">{icon("trash-2")}<span>Delete</span></button><button class="btn btn--primary" type="button" disabled>Disabled</button><button class="btn btn--icon" type="button" aria-label="More">{icon("ellipsis")}</button></div></section>
<section class="cmp" aria-labelledby="c-in"><h3 id="c-in" class="cmp-h">Inputs</h3><div class="cmp-col"><label class="field"><span class="field-l">Domain</span><input class="input" value="shop.acme.dev"><span class="field-h">Hover, focus and error states are in the stylesheet.</span></label>
<label class="field"><span class="field-l">Environment</span><select class="input"><option>production</option><option>staging</option></select></label><label class="field field--search">{icon("search")}<input class="input" type="search" placeholder="Search…" aria-label="Search"></label></div></section>
<section class="cmp" aria-labelledby="c-bd"><h3 id="c-bd" class="cmp-h">Status and chips</h3><div class="cmp-row">{badge("running")}{badge("building")}{badge("failed")}{badge("stopped")}<span class="chip">analytics</span><span class="chip">v2.9.1</span><kbd class="kbd">Ctrl K</kbd></div>
<div class="progress" role="progressbar" aria-label="Demo progress" aria-valuenow="64" aria-valuemin="0" aria-valuemax="100"><span style="width:64%"></span></div></section>
<section class="cmp" aria-labelledby="c-tb"><h3 id="c-tb" class="cmp-h">Tabs</h3><div class="tabset"><fieldset class="tabs" role="radiogroup"><legend class="sr-only">Demo environment</legend><input type="radio" name="demo" id="d1" value="production" checked><label for="d1" class="tab">production <span class="tab-n">6</span></label><input type="radio" name="demo" id="d2" value="staging"><label for="d2" class="tab">staging <span class="tab-n">2</span></label></fieldset></div></section>
<section class="cmp cmp--wide" aria-labelledby="c-cd"><h3 id="c-cd" class="cmp-h">Resource card (running, building, failed)</h3><ul class="res-grid res-grid--demo">{res_item(d[0], 0)}{res_item(d[1], 1)}{res_item(d[2], 2)}</ul></section>
<section class="cmp" aria-labelledby="c-ov"><h3 id="c-ov" class="cmp-h">Dialog, toast, palette</h3><div class="cmp-row"><button class="btn btn--secondary" type="button" data-open="demo-dlg">Open dialog</button><button class="btn btn--secondary" type="button" data-toast="Deployment queued">Show toast</button><button class="btn btn--ghost" type="button" data-open="cmdk">{icon("command")}<span>Palette</span></button></div></section>
<section class="cmp" aria-labelledby="c-sk"><h3 id="c-sk" class="cmp-h">Skeleton</h3><div class="cmp-col" style="gap:var(--s-2)"><span class="sk" style="height:1.2em;width:50%"></span><span class="sk" style="height:.9em;width:90%"></span><span class="sk" style="height:.9em;width:70%"></span></div></section>
</div>'''
    dlg = f'''<dialog id="demo-dlg" class="dlg" aria-labelledby="demo-t"><form method="dialog" class="dlg-form"><header class="dlg-head"><div><p class="eyebrow">Confirm</p><h2 id="demo-t">Delete redis-cache?</h2></div><button class="icon-btn" type="button" data-close aria-label="Close">{icon("x")}</button></header>
<div class="dlg-body"><p>This removes the container, its volume (38 MB) and all backups. It cannot be undone.</p></div><footer class="dlg-foot"><button class="btn btn--ghost" type="button" data-close>Cancel</button><button class="btn btn--danger" type="button" data-close>Delete</button></footer></form></dialog>'''
    links = "".join(f'<li><a class="chip chip--link" href="{s}.html">{TITLES[s]}</a></li>' for s in SCREENS)
    body = f'''<header class="cc-hero"><p class="eyebrow">Concept {cfg["id"][:2]} of 10 · {cfg["id"][3:]}</p><h1>{cfg["name"]}</h1><p class="cc-idea">{cfg["idea"]}</p><ul class="chips" aria-label="Mood">{"".join(f'<li class="chip">{m}</li>' for m in cfg["mood"])}</ul><ul class="chips" aria-label="Screens">{links}</ul></header>
<section class="cc-sec" aria-labelledby="s-pal"><h2 id="s-pal" class="sec-h">Palette <span class="sec-n">live tokens, follows the theme toggle</span></h2><ul class="swatches">{sw}</ul></section>
<section class="cc-sec" aria-labelledby="s-type"><h2 id="s-type" class="sec-h">Type</h2><div class="spec-grid"><div class="spec-faces"><p class="face face--display">Aa <span>display</span></p><p class="face face--sans">Aa <span>sans</span></p><p class="face face--mono">Aa <span>mono</span></p>
<table class="lic"><caption class="sr-only">Font licenses</caption><thead><tr><th>Font</th><th>License</th><th>Use</th></tr></thead><tbody>{fonts}</tbody></table></div><ul class="spec-scale">{scale}</ul></div></section>
<section class="cc-sec" aria-labelledby="s-comp"><h2 id="s-comp" class="sec-h">Components</h2>{comp}</section>
<section class="cc-sec" aria-labelledby="s-mo"><h2 id="s-mo" class="sec-h">Motion spec</h2><p class="cc-motion">{cfg["motion"]}</p><dl class="motion-tokens" id="motion-tokens"></dl>
<p class="cc-note">Cross-document view transitions (<code>@view-transition</code>) run when served over http (e.g. <code>python3 -m http.server</code>); browsers treat <code>file://</code> pages as separate origins and skip them. Everything else works from <code>file://</code>. All motion is disabled under <code>prefers-reduced-motion</code>.</p></section>'''
    return body, dlg

RES_SAMPLE = S.RES
def build():
    # ensure base.css cleanup
    for c in CONCEPTS:
        d = f"{ROOT}/{c['id']}"
        os.makedirs(d, exist_ok=True)
        for f in ("base.css", "structure.css", "page.js", "card.css"):
            shutil.copy(f"{B}/{f}", f"{d}/{f}")
        b = S.projects(c); open(f"{d}/projects.html", "w").write(page(c, "projects", "Projects", b))
        b, ex = S.environment(c); open(f"{d}/environment.html", "w").write(page(c, "environment", "storefront / production", b, ex))
        b, ex = S.app(c); open(f"{d}/app.html", "w").write(page(c, "app", "web", b, ex))
        b, ex = S.templates(c); open(f"{d}/templates.html", "w").write(page(c, "templates", "Templates", b, ex))
        open(f"{d}/server.html", "w").write(page(c, "server", "atlas", S.server(c)))
        open(f"{d}/playground.html", "w").write(page(c, "playground", "Qwen3-4B", S.playground(c)))
        b, ex = card(c); open(f"{d}/index.html", "w").write(page(c, "card", c["name"] + " · concept card", b, ex, head_extra='<link rel="stylesheet" href="card.css">\n'))
        print("built", c["id"])
    import gallery
    gallery.write_gallery(CONCEPTS, json.load(open(f"{B}/font_licenses.json")))
    print("gallery written")
if __name__ == "__main__": build()
