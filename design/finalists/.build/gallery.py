"""design/finalists/index.html: the three finalists side by side. Same screen, same theme, one set of controls. Static HTML + a small script."""
import os, json
HERE = os.path.dirname(os.path.abspath(__file__)); ROOT = os.path.dirname(HERE)

FIN = [
 dict(id="01-terminal-ops", n="01", name="Cockpit", tag="terminal-ops", idea="A tmux-grade ops console. JetBrains Mono everywhere, amber on phosphor black, rows instead of cards, a status bar that never leaves.", effort="M"),
 dict(id="02-quiet-linear", n="02", name="Quiet", tag="quiet-linear", idea="Linear-style restraint. Hanken Grotesk on a hushed neutral canvas, one ice-blue signal, hairline lists and 90 ms eased motion.", effort="S"),
 dict(id="09-data-dashboard", n="09", name="Panelboard", tag="data-dashboard", idea="Every block is a titled panel on a tight grid under a variables bar. IBM Plex, orange accent, series colours carry the data.", effort="M"),
]
SCREENS = [("projects", "Projects"), ("environment", "Environment"), ("app", "App"), ("templates", "Templates"), ("server", "Server"), ("playground", "Model"),
           ("signin", "Sign in"), ("create", "Create"), ("database", "Database"), ("stack", "Stack"), ("settings", "Settings"), ("empty", "Empty"), ("404", "404")]

def seg(name, items, checked):
    return "".join(f'<input type="radio" name="{name}" id="{name}-{v}" value="{v}"{" checked" if v == checked else ""}><label for="{name}-{v}">{l}</label>' for v, l in items)

def build():
    cols = "".join(f'''<section class="col" data-id="{f["id"]}" aria-labelledby="h-{f["n"]}">
<header class="col-h"><div><p class="n">{f["n"]} · {f["tag"]}</p><h2 id="h-{f["n"]}">{f["name"]}</h2></div><span class="effort" title="Estimated build effort">Build {f["effort"]}</span></header>
<p class="idea">{f["idea"]}</p>
<div class="stage"><div class="phone"><iframe title="{f["name"]} prototype" loading="lazy"></iframe></div></div>
<nav class="col-l" aria-label="{f["name"]} links"><a class="open" href="{f["id"]}/projects.html">Open prototype</a><a href="{f["id"]}/components.html">Components</a><a href="{f["id"]}/README.md">Spec</a><a href="{f["id"]}/IMPLEMENTATION.md">Build notes</a></nav></section>''' for f in FIN)
    html = f'''<!doctype html>
<html lang="en" data-theme="dark"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>podkit · design round 2 · three finalists</title><link rel="icon" href="data:,">
<style>
@font-face{{font-family:"Hanken Grotesk";src:url(02-quiet-linear/fonts/hanken-grotesk-var.woff2) format("woff2");font-weight:100 900;font-display:swap}}
:root{{--bg:#101113;--panel:#17181b;--line:#2a2c31;--ui:#6b6d75;--text:#ececee;--text-2:#a8a9b1;--acc:#8ab4f8;--acc-ink:#0b1220;color-scheme:dark}}
*{{box-sizing:border-box}}body{{margin:0;background:var(--bg);color:var(--text);font:14px/1.5 "Hanken Grotesk",system-ui,sans-serif}}
a{{color:inherit}}h1,h2,p{{margin:0}}
.top{{position:sticky;top:0;z-index:5;background:color-mix(in oklab,var(--bg) 92%,transparent);backdrop-filter:blur(8px);border-bottom:1px solid var(--line)}}
.top-in{{display:grid;gap:12px;padding:14px 24px;max-width:1800px;margin:auto}}
.title{{display:flex;flex-wrap:wrap;gap:8px 24px;align-items:baseline;justify-content:space-between}}.title h1{{font-size:20px;letter-spacing:-.01em}}.title p{{color:var(--text-2)}}
.ctl{{display:flex;flex-wrap:wrap;gap:12px 24px;align-items:center}}
fieldset{{border:0;margin:0;padding:0;display:flex;gap:0;flex-wrap:wrap;min-width:0}}legend{{position:absolute;width:1px;height:1px;overflow:hidden;clip-path:inset(50%)}}
.grp{{display:flex;align-items:center;gap:8px}}.grp>span{{color:var(--text-2);font-size:12px}}
fieldset input{{position:absolute;opacity:0;width:1px;height:1px}}
fieldset label{{padding:6px 12px;border:1px solid var(--ui);margin-left:-1px;cursor:pointer;font-size:13px;min-height:32px;display:inline-flex;align-items:center;background:var(--bg)}}
fieldset label:first-of-type{{border-radius:6px 0 0 6px;margin-left:0}}fieldset label:last-of-type{{border-radius:0 6px 6px 0}}
fieldset label:hover{{background:var(--panel)}}
fieldset input:checked+label{{background:var(--acc);border-color:var(--acc);color:var(--acc-ink);position:relative;z-index:1;font-weight:600}}
fieldset input:focus-visible+label{{outline:2px solid var(--acc);outline-offset:2px;z-index:2}}
.screens label{{padding:6px 10px}}
button.play{{font:inherit;font-weight:600;padding:6px 16px;min-height:34px;border-radius:6px;border:1px solid var(--acc);background:transparent;color:var(--acc);cursor:pointer;display:inline-flex;gap:8px;align-items:center}}
button.play:hover{{background:color-mix(in oklab,var(--acc) 14%,transparent)}}button.play:focus-visible{{outline:2px solid var(--acc);outline-offset:2px}}
button.play[aria-pressed="true"]{{background:var(--acc);color:var(--acc-ink)}}
.hint{{color:var(--text-2);font-size:12px}}
main{{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:16px;padding:16px 24px 40px;max-width:1800px;margin:auto}}
.col{{display:grid;gap:10px;align-content:start;min-width:0}}
.col-h{{display:flex;justify-content:space-between;align-items:flex-end;gap:12px}}.n{{font:12px/1.2 ui-monospace,monospace;color:var(--text-2)}}.col-h h2{{font-size:22px;letter-spacing:-.01em}}
.effort{{font-size:12px;color:var(--text-2);border:1px solid var(--line);padding:2px 8px;border-radius:999px}}
.idea{{color:var(--text-2);font-size:13px;min-height:3.9em}}
.stage{{--k:1;position:relative;background:var(--panel);border:1px solid var(--line);border-radius:8px;overflow:hidden;height:calc(900px * var(--k))}}
.phone{{width:1440px;height:900px;transform:scale(var(--k));transform-origin:0 0}}
.phone iframe{{width:100%;height:100%;border:0;background:#000;display:block}}
body[data-vp="m"] .stage{{height:calc(844px * var(--km,1));display:grid;place-items:start center}}
body[data-vp="m"] .phone{{width:390px;height:844px;transform:scale(var(--km,1));transform-origin:50% 0;outline:1px solid var(--line)}}
.col-l{{display:flex;flex-wrap:wrap;gap:4px 16px;font-size:13px}}.col-l a{{color:var(--acc);text-underline-offset:3px}}
.foot{{max-width:1800px;margin:auto;padding:0 24px 40px;color:var(--text-2);font-size:13px;display:flex;flex-wrap:wrap;gap:8px 24px}}
@media (max-width:1100px){{main{{grid-template-columns:1fr}}}}
</style></head>
<body data-vp="d">
<header class="top"><div class="top-in">
<div class="title"><h1>Three finalists, one screen at a time</h1><p>Same screen, same theme, same data in every column. Pick a screen, flip the theme, then play the motion.</p></div>
<div class="ctl">
<div class="grp screens"><span id="lbl-s">Screen</span><fieldset aria-labelledby="lbl-s"><legend>Screen</legend>{seg("screen", SCREENS, "environment")}</fieldset></div>
<div class="grp"><span id="lbl-t">Theme</span><fieldset aria-labelledby="lbl-t"><legend>Theme</legend>{seg("theme", [("dark", "Dark"), ("light", "Light")], "dark")}</fieldset></div>
<div class="grp"><span id="lbl-v">Viewport</span><fieldset aria-labelledby="lbl-v"><legend>Viewport</legend>{seg("vp", [("d", "Desktop"), ("m", "390 px")], "d")}</fieldset></div>
<button class="play" type="button" id="play" aria-pressed="false">▶ Play motion</button><span class="hint" id="hint">Reloads the three frames with motion on: skeleton, building → running, live log, streamed answer, backup progress.</span>
</div></div></header>
<main id="main">{cols}</main>
<p class="foot"><span>Keyboard: arrow keys move within a control group.</span><a href="PICK.md">Comparison and pick sheet (PICK.md)</a><a href="README.md">What is in each folder</a><a href="LICENSES.md">Licenses</a></p>
<script>
(() => {{
  const $ = (s, r = document) => r.querySelector(s), $$ = (s, r = document) => [...r.querySelectorAll(s)];
  const val = n => $(`input[name="${{n}}"]:checked`).value;
  let live = false;
  const url = id => `${{id}}/${{val('screen')}}.html?theme=${{val('theme')}}` + (live ? '&play=1' : '&still=1');
  const fit = () => {{ const st = $('.stage'); if (!st) return; const w = st.clientWidth, k = Math.min(1, w / 1440).toFixed(4), km = Math.min(1, w / 390).toFixed(4); $$('.stage').forEach(s => {{ s.style.setProperty('--k', k); s.style.setProperty('--km', km); }}); $$('.phone').forEach(p => {{ p.style.setProperty('--k', k); p.style.setProperty('--km', km); }}); }};
  const load = () => {{ document.body.dataset.vp = val('vp'); fit(); $$('.col').forEach(c => {{ const f = $('iframe', c), u = url(c.dataset.id); if (f.dataset.u !== u) {{ f.dataset.u = u; f.src = u; }} }}); try {{ history.replaceState(null, '', `#${{val('screen')}}/${{val('theme')}}/${{val('vp')}}`); }} catch (e) {{}} }};
  const sync = () => $$('.col-l .open').forEach(a => {{ a.href = `${{a.closest('.col').dataset.id}}/${{val('screen')}}.html?theme=${{val('theme')}}`; }});
  const stillBtn = () => {{ live = false; $('#play').setAttribute('aria-pressed', 'false'); $('#play').textContent = '▶ Play motion'; }};
  $$('input[type=radio]').forEach(i => i.addEventListener('change', () => {{ stillBtn(); load(); sync(); }}));
  $('#play').addEventListener('click', e => {{ live = !live; e.currentTarget.setAttribute('aria-pressed', live); e.currentTarget.textContent = live ? '■ Back to still frames' : '▶ Play motion'; $$('iframe').forEach(f => delete f.dataset.u); load(); }});
  const h = (location.hash || '').slice(1).split('/'); if (h.length === 3) {{ [['screen', h[0]], ['theme', h[1]], ['vp', h[2]]].forEach(([n, v]) => {{ const r = $(`input[name="${{n}}"][value="${{v}}"]`); if (r) r.checked = true; }}); }}
  new ResizeObserver(fit).observe(document.body); fit(); load(); sync();
}})();
</script></body></html>'''
    open(f"{ROOT}/index.html", "w").write(html)
    print("gallery written", len(html))
if __name__ == "__main__": build()
