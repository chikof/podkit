from lib import *
import json

NAV = [("projects", "Projects", "layout-grid", "projects.html"), ("templates", "Templates", "package", "templates.html"),
       ("server", "Servers", "server", "server.html"), ("playground", "Models", "brain", "playground.html"), ("settings", "Settings", "settings", "#")]
SCREEN_NAV = dict(projects="projects", environment="projects", app="projects", templates="templates", server="server", playground="playground", card="")
CRUMBS = dict(
    projects=[("acme", "projects.html"), ("Projects", None)],
    environment=[("acme", "projects.html"), ("storefront", "environment.html"), ("production", None)],
    app=[("acme", "projects.html"), ("storefront", "environment.html"), ("production", "environment.html"), ("web", None)],
    templates=[("acme", "projects.html"), ("Templates", None)],
    server=[("acme", "projects.html"), ("Servers", "server.html"), ("atlas", None)],
    playground=[("acme", "projects.html"), ("Models", "playground.html"), ("Qwen3-4B", None)],
    card=[("podkit", "../index.html"), ("Concept card", None)],
)
MARK = '<svg class="brand-svg" viewBox="0 0 24 24" aria-hidden="true"><rect x="2.5" y="6.5" width="19" height="11" rx="5.5" fill="none" stroke="currentColor" stroke-width="2"/><path d="M12 6.5v11" stroke="currentColor" stroke-width="2"/><circle cx="7.2" cy="12" r="1.6" fill="currentColor"/></svg>'

def brand(): return f'<a class="brand" href="projects.html"><span class="brand-mark">{MARK}</span><span class="brand-name">podkit</span></a>'
def team(): return '<button class="team" type="button" aria-label="Switch team. Current team: acme"><span class="avatar" aria-hidden="true">A</span><span class="team-name">acme</span>' + icon("chevrons-up-down") + '</button>'
def search(): return '<button class="search" type="button" data-open="cmdk" aria-label="Search, Control K">' + icon("search") + '<span class="search-label">Search or jump to…</span><kbd class="kbd">Ctrl K</kbd></button>'
def theme_btn(): return '<button class="icon-btn" type="button" data-theme-toggle aria-label="Toggle light and dark theme">' + icon("sun", "only-dark") + icon("moon", "only-light") + '</button>'
def navlinks(cur, kind="list"):
    items = []
    for k, label, ic, href in NAV:
        c = ' aria-current="page"' if k == cur else ""
        items.append(f'<li><a class="nav-link" href="{href}"{c}>{icon(ic)}<span class="nav-label">{label}</span></a></li>')
    return f'<nav class="nav" aria-label="Primary"><ul class="nav-list">{"".join(items)}</ul></nav>'
def crumbs(screen):
    out = []
    for n, h in CRUMBS[screen]:
        out.append(f'<li><a href="{h}">{n}</a></li>' if h else f'<li aria-current="page">{n}</li>')
    return f'<nav class="crumbs-wrap" aria-label="Breadcrumb"><ol class="crumbs">{"".join(out)}</ol></nav>'
def statusbar():
    return ('<footer class="statusbar"><a href="server.html" class="sb-item"><span class="dot dot--ok" aria-hidden="true"></span>atlas online</a>'
            '<span class="sb-item">CPU 14%</span><span class="sb-item">RAM 9.2 / 30 GB</span><span class="sb-item">GPU 31%</span><span class="sb-item">VRAM 3.4 / 12 GB</span>'
            '<span class="sb-item sb-end">podkit 0.9.2 · podman 5.4</span></footer>')
def varbar():
    def sel(label, val): return f'<button class="var" type="button"><span class="var-k">{label}</span><span class="var-v">{val}</span>{icon("chevron-down")}</button>'
    return ('<div class="varbar" role="group" aria-label="Dashboard variables">' + sel("team", "acme") + sel("project", "storefront") + sel("env", "production") + sel("server", "atlas")
            + '<span class="varbar-sp"></span><button class="var" type="button"><span class="var-k">range</span><span class="var-v">Last 6 hours</span>' + icon("clock") + '</button><button class="var var--icon" type="button" data-reload aria-label="Refresh">' + icon("refresh-cw") + '</button></div>')

def frame(cfg, screen, body):
    cur = SCREEN_NAV[screen]; nav = cfg["nav"]
    st = statusbar() if cfg.get("status") else ""
    if nav == "sidebar":
        tree = ""
        if cfg.get("tree"):
            tree = ('<div class="tree" aria-label="Quick tree"><p class="tree-h">acme</p><ul><li><a href="environment.html" aria-current="false">▾ storefront</a><ul><li><a href="environment.html">production</a></li><li><a href="environment.html">staging</a></li></ul></li>'
                    '<li><a href="projects.html">▸ internal-tools</a></li><li><a href="projects.html">▸ analytics</a></li><li><a href="projects.html">▸ ai-lab</a></li></ul></div>')
        return (f'<div class="app app--sidebar"><aside class="chrome" aria-label="Application">{brand()}{team()}{navlinks(cur)}{tree}<div class="chrome-foot"><span class="avatar avatar--user" aria-hidden="true">C</span><span class="who">chiko</span></div></aside>'
                f'<div class="stage"><header class="bar">{crumbs(screen)}<div class="bar-actions">{search()}{theme_btn()}</div></header><main id="main" class="page">{body}</main>{st}</div></div>')
    if nav == "topbar":
        vb = varbar() if cfg.get("varbar") else ""
        return (f'<div class="app app--topbar"><header class="chrome"><div class="chrome-row">{brand()}{team()}{navlinks(cur)}<div class="chrome-sp"></div>{search()}{theme_btn()}<span class="avatar avatar--user" aria-hidden="true">C</span></div>{vb}</header>'
                f'<div class="stage"><div class="bar">{crumbs(screen)}</div><main id="main" class="page">{body}</main>{st}</div></div>')
    if nav == "rail":
        return (f'<div class="app app--rail"><aside class="chrome" aria-label="Application">{brand()}{navlinks(cur)}<div class="chrome-foot">{theme_btn()}<span class="avatar avatar--user" aria-hidden="true">C</span></div></aside>'
                f'<div class="stage"><header class="bar">{team()}{crumbs(screen)}<div class="bar-actions">{search()}</div></header><main id="main" class="page">{body}</main>{st}</div></div>')
    if nav == "dock":
        return (f'<div class="app app--dock"><div class="stage"><header class="bar">{brand()}{team()}{crumbs(screen)}<div class="bar-actions">{search()}{theme_btn()}</div></header><main id="main" class="page">{body}</main></div>'
                f'<div class="chrome dock-wrap">{navlinks(cur)}</div></div>')
    if nav == "masthead":
        return (f'<div class="app app--masthead"><header class="chrome"><div class="mast-top"><p class="mast-issue">The acme desk <span>· Thursday 19 November</span></p>{brand()}<div class="mast-tools">{search()}{theme_btn()}</div></div>{navlinks(cur)}</header>'
                f'<div class="stage"><div class="bar">{team()}{crumbs(screen)}</div><main id="main" class="page">{body}</main></div></div>')
    raise ValueError(nav)

def cmdk():
    li = "".join(f'<li><a class="cmd-item" href="{h}">{icon(i)}<span>{t}</span></a></li>' for i, t, h in CMDK)
    return ('<dialog id="cmdk" class="dlg dlg--cmd" aria-label="Command palette"><div class="cmd"><div class="cmd-in">' + icon("search") + '<input type="search" placeholder="Search projects, resources, templates…" aria-label="Search" autocomplete="off"><kbd class="kbd">Esc</kbd></div>'
            f'<p class="cmd-h">Suggestions</p><ul class="cmd-list">{li}</ul></div></dialog>')

def page(cfg, screen, title, body, extra="", body_attrs="", head_extra=""):
    inner = frame(cfg, screen, body) + cmdk() + extra
    doc = (f'<!doctype html>\n<html lang="en" data-default-theme="{cfg["theme"]}" data-concept="{cfg["id"]}">\n<head>\n<meta charset="utf-8">\n<meta name="viewport" content="width=device-width,initial-scale=1">\n'
           f'<title>{esc(title)} · podkit · {cfg["name"]}</title>\n<link rel="icon" href="data:,">\n'
           "<script>(function(){var q=new URLSearchParams(location.search),t=q.get('theme'),d=document.documentElement;try{t=t||localStorage.getItem('pk-theme')}catch(e){}d.dataset.theme=(t==='dark'||t==='light')?t:d.dataset.defaultTheme})()</script>\n"
           '<link rel="stylesheet" href="tokens.css">\n<link rel="stylesheet" href="base.css">\n' + ('<link rel="stylesheet" href="structure.css">\n' if cfg.get("structure", True) else '') + '<link rel="stylesheet" href="concept.css">\n' + head_extra +
           '<script src="page.js" defer></script>\n</head>\n'
           f'<body data-screen="{screen}" {body_attrs}>\n<a class="skip" href="#main">Skip to content</a>\n@@SPRITE@@\n{inner}\n</body>\n</html>\n')
    return doc.replace("@@SPRITE@@", sprite(doc))

# ---------- shared bits ----------
def head(title, lede, actions="", eyebrow=""):
    e = f'<p class="eyebrow">{eyebrow}</p>' if eyebrow else ""
    return f'<header class="page-head"><div class="page-title">{e}<h1>{title}</h1><p class="lede">{lede}</p></div><div class="page-actions">{actions}</div></header>'
def btn(label, ic=None, cls="", extra=""):
    return f'<button class="btn {cls}" type="button" {extra}>{icon(ic) if ic else ""}<span>{label}</span></button>'

# ---------- 1 projects ----------
def projects(cfg):
    items = []
    for i, p in enumerate(PROJECTS):
        r, b, f, s = p["h"]
        logos = "".join(f'<li>{logo(l, 28)}</li>' for l in p["logos"])
        seg = "".join(f'<span class="seg seg--{k}" style="flex:{n}"></span>' for k, n in (("running", r), ("building", b), ("failed", f), ("stopped", s)) if n)
        txt = " · ".join(x for x in (f"{r} running" if r else "", f"{b} building" if b else "", f"{f} failed" if f else "", f"{s} stopped" if s else "") if x)
        items.append(f'''<li class="proj" style="--i:{i}"><a class="proj-link" href="environment.html">
<div class="proj-head"><h2 class="proj-name">{p["name"]}</h2><span class="proj-env">{p["envs"]} env{"s" if p["envs"]>1 else ""}</span></div>
<ul class="logo-stack" aria-label="Contains">{logos}</ul>
<dl class="proj-meta"><div><dt>Resources</dt><dd>{p["res"]}</dd></div><div><dt>Last deploy</dt><dd>{p["what"]} · {p["last"]}</dd></div></dl>
<div class="health" role="img" aria-label="{txt}">{seg}</div><p class="health-txt">{txt}</p></a></li>''')
    sk = "".join(f'<li class="proj proj--sk" aria-hidden="true"><div class="proj-link"><span class="sk" style="height:1.2em;width:55%"></span><span class="sk" style="height:28px;width:70%;margin-top:var(--s-4)"></span><span class="sk" style="height:.9em;width:90%;margin-top:var(--s-4)"></span><span class="sk" style="height:6px;width:100%;margin-top:var(--s-4)"></span></div></li>' for _ in range(6))
    new = f'<li class="proj proj--new" style="--i:{len(PROJECTS)}"><button class="proj-link" type="button" data-toast="New project dialog (mockup)">{icon("plus")}<span>New project</span><span class="proj-new-sub">Group resources by product</span></button></li>'
    summary = '<ul class="summary" aria-label="Team summary"><li><b>31</b> resources</li><li data-status="running"><span class="dot"></span><b>21</b> running</li><li data-status="building"><span class="dot"></span><b>2</b> building</li><li data-status="failed"><span class="dot"></span><b>1</b> failed</li><li data-status="stopped"><span class="dot"></span><b>7</b> stopped</li></ul>'
    body = head("Projects", "Team acme · 6 projects, 2 on staging", btn("Reload", "refresh-cw", "btn--ghost", "data-reload") + btn("New project", "plus", "btn--primary", 'data-toast="New project dialog (mockup)"')) + summary
    body += f'<section aria-label="All projects"><ul class="proj-grid skeleton" aria-hidden="true">{sk}</ul><ul class="proj-grid real reveal">{"".join(items)}{new}</ul></section>'
    return body

# ---------- 2 environment ----------
def res_item(r, i, sparkn=0):
    st = r["st"]
    meta = "".join(f'<div><dt>{k}</dt><dd>{v}</dd></div>' for k, v in r["meta"])
    mt = "".join(f'<div><dt>{k}</dt><dd>{v}</dd></div>' for k, v in r["m"])
    extra = ""
    if st == "building": extra = f'<div class="progress" role="progressbar" aria-label="Build progress" aria-valuenow="{r.get("prog",50)}" aria-valuemin="0" aria-valuemax="100"><span style="width:{r.get("prog",50)}%"></span></div>'
    if st == "failed": extra = f'<p class="res-err">{icon("circle-alert")}<span>{r["err"]}</span></p>'
    sp = spark(i * 7 + 3, base=.4 + .05 * (i % 3)) if st == "running" else ""
    return f'''<li class="res" data-status="{st}" data-kind="{r["t"]}" style="--i:{i}"><a class="res-link" href="{r["href"]}">
<span class="res-logo">{logo(r["logo"], 36)}</span>
<span class="res-main"><span class="res-kind">{icon(r["t"])}{r["kind"]}</span><h3 class="res-name">{r["name"]}</h3><span class="res-sub">{r["sub"]}</span></span>
{badge(st)}
<dl class="res-meta">{meta}</dl><dl class="res-metrics">{mt}</dl>{sp}{extra}</a></li>'''

def res_skel(n=6):
    return "".join('<li class="res res--sk" aria-hidden="true"><div class="res-link"><span class="sk sk-logo"></span><span class="res-main"><span class="sk" style="height:.8em;width:30%"></span><span class="sk" style="height:1.1em;width:60%;margin-top:6px"></span><span class="sk" style="height:.8em;width:85%;margin-top:6px"></span></span><span class="sk" style="height:1.4em;width:72px"></span></div></li>' for _ in range(n))

def create_menu():
    items = [("git-branch", "Git repository", "GitHub, GitLab or any Git URL", "#"), ("box", "Docker image", "From a registry, with auth", "#"), ("database", "Database", "Postgres, MySQL, MongoDB, Redis", "#"),
             ("layers", "Compose stack", "Paste or import docker-compose.yml", "#"), ("package", "Template", "484 one-click apps", "templates.html"), ("brain", "AI model", "llama.cpp, Ollama, vLLM, colibri", "playground.html")]
    li = "".join(f'<li><a class="menu-item" href="{h}">{icon(ic)}<span><b>{t}</b><small>{s}</small></span></a></li>' for ic, t, s, h in items)
    return f'<div id="create-menu" class="menu anchored" popover><p class="menu-h">Create resource</p><ul>{li}</ul></div>'

def environment(cfg):
    body = head("storefront", "6 resources in production · 4 running · 1 building · 1 failed · 1 stopped",
                btn("Reload", "refresh-cw", "btn--ghost", "data-reload") + btn("Deploy all", "rocket", "btn--secondary", 'data-toast="Queued 3 deployments"') + btn("Create resource", "plus", "btn--primary", 'popovertarget="create-menu"'), "Project")
    prod = "".join(res_item(r, i) for i, r in enumerate(RES))
    new = f'<li class="res res--new" style="--i:{len(RES)}"><button class="res-link res-new" type="button" popovertarget="create-menu">{icon("plus")}<span class="res-new-t">Create resource</span><span class="res-new-s">Git repo · image · database · stack · template · AI model</span></button></li>'
    stg = "".join(res_item(r, i) for i, r in enumerate(STAGING))
    body += f'''<div class="tabset"><fieldset class="tabs" role="radiogroup"><legend class="sr-only">Environment</legend>
<input type="radio" name="env" id="env-prod" value="production" checked><label for="env-prod" class="tab">production <span class="tab-n">6</span></label>
<input type="radio" name="env" id="env-stg" value="staging"><label for="env-stg" class="tab">staging <span class="tab-n">2</span></label></fieldset>
<section data-panel="production" aria-label="Production resources"><ul class="res-grid skeleton" aria-hidden="true">{res_skel()}</ul><ul class="res-grid real reveal">{prod}{new}</ul></section>
<section data-panel="staging" aria-label="Staging resources"><ul class="res-grid reveal">{stg}{new.replace(str(len(RES)), "2")}</ul></section></div>'''
    return body, create_menu()

# ---------- 3 app ----------
def app(cfg):
    dep = []
    for i, (n, t, c, who, when, st) in enumerate(DEPLOYS):
        lbl = {"running": "Succeeded", "failed": "Failed", "stopped": "Superseded", "building": "Building…"}[st]
        idattr = ' id="dep-live"' if i == 0 else ""
        dep.append(f'<li class="tl-item"{idattr} data-status="{st}" style="--i:{i}"><span class="tl-dot" aria-hidden="true"></span><div class="tl-body"><p class="tl-title"><b>{n}</b> {t}</p><p class="tl-meta">{c}</p><p class="tl-meta2"><span>{who}</span><span class="tl-state">{lbl}</span><time>{when}</time></p></div></li>')
    stats = [("CPU", "3.2%"), ("Memory", "412 MB"), ("Restarts", "0"), ("Uptime", "12 min")]
    sd = "".join(f'<div><dt>{k}</dt><dd>{v}</dd></div>' for k, v in stats)
    menu = ('<div id="app-menu" class="menu anchored" popover><ul>' + "".join(f'<li><button class="menu-item" type="button">{icon(i)}<span><b>{t}</b></span></button></li>' for i, t in [("undo-2", "Roll back to #147"), ("copy", "Duplicate"), ("folder-open", "Move to staging"), ("key-round", "Environment variables"), ("trash-2", "Delete")]) + '</ul></div>')
    tabs = "".join(f'<li><a class="tab" href="#"{" aria-current=\"page\"" if i==0 else ""}>{t}</a></li>' for i, t in enumerate(["Deployments", "Logs", "Environment", "Domains", "Storage", "Settings"]))
    log = json.dumps([list(x) for x in LOG])
    body = f'''<header class="app-head"><span class="app-logo">{logo("nextdotjs", 48)}</span>
<div class="app-id"><p class="eyebrow">{icon("git-branch")} Git app</p><h1>web</h1>
<p class="app-sub">{badge("building", extra='id="deploy-badge"')}<a class="url" href="#">shop.acme.dev{icon("external-link")}</a><span class="gitref">{icon("git-commit-horizontal")}main · a3f9c21</span></p></div>
<div class="page-actions">{btn("Deploy","rocket","btn--primary",'data-redeploy')}{btn("Restart","rotate-ccw","btn--secondary",'data-toast="Restarting web…"')}{btn("Stop","square","btn--secondary",'data-toast="Stopping web…"')}
<button class="btn btn--icon" type="button" popovertarget="app-menu" aria-label="More actions">{icon("ellipsis")}</button></div></header>
<dl class="stat-row" aria-label="Live stats">{sd}</dl>
<nav class="subnav" aria-label="App sections"><ul class="tabs-line">{tabs}</ul></nav>
<div class="app-body"><section class="deploys" aria-labelledby="h-dep"><h2 id="h-dep" class="sec-h">Deployments</h2><ol class="timeline">{"".join(dep)}</ol></section>
<section class="build-log" aria-labelledby="h-log"><header class="log-head"><h2 id="h-log" class="sec-h">Build log <span class="log-id">#148</span></h2><span class="live-pill"><span class="dot"></span>Live</span>
<label class="follow"><input type="checkbox" id="follow" checked><span>Follow</span></label><button class="btn btn--ghost btn--sm" type="button" data-toast="Log copied">{icon("copy")}<span>Copy</span></button></header>
<pre class="log" id="build-log" role="log" aria-label="Build log output" tabindex="0"></pre><script type="application/json" id="log-script">{log}</script></section></div>'''
    return body, menu

# ---------- 4 templates ----------
def templates(cfg):
    cards = []
    for i, (t, lg, desc, cat, ver) in enumerate(TEMPLATES):
        cards.append(f'''<li class="tpl" data-name="{t.lower()}" data-cat="{cat}" data-title="{t}" style="--i:{i}"><button class="tpl-btn" type="button" data-open="deploy"><span class="tpl-logo">{logo(lg, 44)}</span>
<span class="tpl-main"><h3 class="tpl-name">{t}</h3><span class="tpl-desc">{desc}</span></span><span class="tpl-foot"><span class="chip">{cat}</span><span class="tpl-ver">v{ver}</span></span></button></li>''')
    cats = "".join(f'<li><button class="chip chip--btn" type="button" data-cat="{k}" aria-pressed="{"true" if k=="all" else "false"}">{n}<span class="chip-n">{c}</span></button></li>' for k, n, c in CATS)
    body = head("Templates", '<span id="tpl-count">20 templates</span> shown · 484 in the catalog', btn("Import compose file", "upload", "btn--secondary"), "One-click apps")
    body += f'''<div class="tpl-tools"><label class="field field--search"><span class="sr-only">Search templates</span>{icon("search")}<input id="tpl-search" class="input" type="search" placeholder="Search 484 templates: gotify, plausible, n8n…" autocomplete="off"></label>
<ul class="cats" aria-label="Category">{cats}</ul></div><ul class="tpl-grid reveal">{"".join(cards)}</ul>'''
    dlg = f'''<dialog id="deploy" class="dlg" aria-labelledby="deploy-title"><form method="dialog" class="dlg-form">
<header class="dlg-head"><span id="deploy-logo" class="dlg-logo">{logo("gotify", 44)}</span><div><p class="eyebrow">Deploy template</p><h2 id="deploy-title">Deploy <span id="deploy-name">Gotify</span></h2></div><button class="icon-btn" type="button" data-close aria-label="Close">{icon("x")}</button></header>
<div class="dlg-body"><div class="field-row"><label class="field"><span class="field-l">Project</span><select class="input"><option>storefront</option><option>internal-tools</option><option>analytics</option></select></label>
<label class="field"><span class="field-l">Environment</span><select class="input"><option>production</option><option>staging</option></select></label></div>
<label class="field"><span class="field-l">Domain</span><input id="deploy-dom" class="input" value="gotify.acme.dev" autocomplete="off"><span class="field-h">DNS points at atlas (10.0.0.12). TLS is issued automatically.</span></label>
<label class="field"><span class="field-l">Server</span><select class="input"><option>atlas · 14% CPU · 20 GB free</option></select></label>
<div class="note">{icon("key-round")}<p>podkit generates <b>2 secrets</b> (admin password, app key) and stores them as environment variables you can reveal later.</p></div></div>
<footer class="dlg-foot"><button class="btn btn--ghost" type="button" data-close>Cancel</button><button id="deploy-go" class="btn btn--primary" type="button" data-close>{icon("rocket")}<span>Deploy</span></button></footer></form></dialog>'''
    return body, dlg

# ---------- 5 server ----------
def server(cfg):
    hw = [("cpu", "CPU", "Ryzen 7 9850X3D", "8 cores / 16 threads"), ("memory-stick", "RAM", "30 GB", "DDR5-6000"), ("zap", "GPU", "RTX 5070", "12 GB VRAM · CUDA 12.9"), ("hard-drive", "Disk", "NVMe 2 TB", "612 GB used")]
    hws = "".join(f'<li class="hw-item">{icon(i)}<div><p class="hw-k">{k}</p><p class="hw-v">{v}</p><p class="hw-s">{s}</p></div></li>' for i, k, v, s in hw)
    cards = []
    for mi, m in enumerate(METRICS):
        panels = ""
        for rg in RANGES:
            panels += f'<div data-panel="{rg}">{chart(cfg["charts"], metric_series(m, rg, mi), m["ymax"], m["fmt"], RANGES[rg][1])}</div>'
        unit = f'<span class="u">{m["unit"]}</span>'
        cards.append(f'''<article class="metric" data-metric="{m["id"]}" style="--i:{mi};--c:var(--chart-{mi%6+1})"><header class="metric-head"><h2 class="metric-name">{icon(m["icon"])}{m["name"]}</h2><p class="metric-val">{m["cur"]}{unit}</p></header>
<p class="metric-sub">{m["sub"]}</p>{panels}<p class="metric-stats"><span>{m["stats"][0]}</span><span>{m["stats"][1]}</span></p></article>''')
    body = f'''<header class="srv-head"><span class="srv-ic">{icon("server")}</span><div class="srv-id"><p class="eyebrow">Server · 10.0.0.12</p><h1>atlas</h1><p class="app-sub">{badge("running","Online")}<span class="gitref">NixOS 25.11 · Podman 5.4 · up 41 days</span></p></div><div class="page-actions">{btn("Terminal","terminal","btn--secondary")}{btn("Prune images","trash-2","btn--secondary",'data-toast="Pruned 3.2 GB"')}</div></header>
<ul class="hw" aria-label="Hardware">{hws}</ul>
<div class="tabset"><div class="range-bar"><h2 class="sec-h">Metrics</h2><fieldset class="tabs tabs--range" role="radiogroup"><legend class="sr-only">Time range</legend>
<input type="radio" name="rg" id="rg1" value="1h"><label for="rg1" class="tab">1h</label><input type="radio" name="rg" id="rg2" value="24h" checked><label for="rg2" class="tab">24h</label><input type="radio" name="rg" id="rg3" value="7d"><label for="rg3" class="tab">7d</label></fieldset></div>
<div class="metrics reveal">{"".join(cards)}</div></div>'''
    return body

# ---------- 6 playground ----------
def playground(cfg):
    stats = [("Prompt", "148", "tokens"), ("Completion", "312", "tokens"), ("Speed", "175", "tok/s"), ("First token", "84", "ms"), ("Context", "2.1k / 32k", ""), ("VRAM", "3.4 / 12", "GB")]
    st = "".join(f'<div class="ustat"><dt>{k}</dt><dd>{v}<span class="u">{u}</span></dd></div>' for k, v, u in stats)
    body = f'''<header class="model-head"><span class="app-logo">{logo("qwen", 48)}</span><div class="app-id"><p class="eyebrow">AI model · {icon("brain")} reasoning</p><h1>Qwen3-4B</h1>
<ul class="chips" aria-label="Model details"><li class="chip">Q4_K_M</li><li class="chip">llama.cpp CUDA</li><li class="chip">32k context</li><li class="chip">2.5 GB</li>{"<li>"+badge("running")+"</li>"}</ul></div>
<p class="big-stat"><span class="big-n">≈175</span><span class="big-u">tok/s</span></p><div class="page-actions">{btn("Unload","power","btn--secondary",'data-toast="Unloading qwen3-4b…"')}{btn("API","terminal","btn--secondary")}</div></header>
<div class="pg"><section class="chat" aria-label="Conversation"><div class="msgs">
<article class="msg msg--user"><p class="msg-who">You</p><div class="msg-body"><p>Write a Containerfile for a Rust axum service. Small final image, nonroot.</p></div></article>
<article class="msg msg--bot"><p class="msg-who">{icon("sparkles")} Qwen3-4B <span class="msg-meta">175 tok/s · 312 tokens</span></p>
<details id="think" class="think" open><summary class="think-sum">{icon("brain")}<span id="think-state">Thought for 4.1 s</span>{icon("chevron-down","think-chev")}</summary><p class="think-body" data-full="{esc(THINK)}">{THINK}</p></details>
<div id="answer" class="answer msg-body" data-state="done">{ANSWER}</div><script type="application/json" id="answer-html">{json.dumps(ANSWER)}</script>
<div class="msg-tools"><button class="btn btn--ghost btn--sm" type="button" data-replay>{icon("rotate-ccw")}<span>Replay stream</span></button><button class="btn btn--ghost btn--sm" type="button" data-toast="Copied">{icon("copy")}<span>Copy</span></button></div></article></div>
<form id="composer" class="composer"><label class="sr-only" for="prompt">Message</label><textarea id="prompt" class="input" rows="2" placeholder="Message Qwen3-4B…  (Shift+Enter for newline)"></textarea><button class="btn btn--primary" type="submit">{icon("send")}<span>Send</span></button></form></section>
<aside class="side" aria-label="Session"><section class="side-sec"><h2 class="sec-h">Usage</h2><dl class="ustats">{st}</dl></section>
<section class="side-sec"><h2 class="sec-h">Parameters</h2><label class="field"><span class="field-l">Temperature <output>0.7</output></span><input type="range" class="range" min="0" max="2" step="0.1" value="0.7"></label>
<label class="field"><span class="field-l">Top-p <output>0.95</output></span><input type="range" class="range" min="0" max="1" step="0.05" value="0.95"></label>
<label class="field"><span class="field-l">System prompt</span><textarea class="input" rows="3">You are a concise senior infrastructure engineer.</textarea></label>
<label class="check"><input type="checkbox" checked><span>Show thinking</span></label></section></aside></div>'''
    return body
