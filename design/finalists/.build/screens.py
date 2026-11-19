"""HTML for every screen. One function per screen; the frame and a few copy strings vary per finalist (cfg)."""
from lib import *
import json

NAV = [("projects", "Projects", "layout-grid", "projects.html"), ("templates", "Templates", "package", "templates.html"),
       ("server", "Servers", "server", "server.html"), ("playground", "Models", "brain", "playground.html"), ("settings", "Settings", "settings", "settings.html")]
SCREEN_NAV = dict(projects="projects", environment="projects", app="projects", create="projects", database="projects", stack="projects", empty="projects",
                  templates="templates", server="server", playground="playground", settings="settings", notfound="", signin="", components="")
CRUMBS = dict(
    projects=[("acme", "projects.html"), ("Projects", None)],
    environment=[("acme", "projects.html"), ("storefront", "environment.html"), ("production", "SW")],
    app=[("acme", "projects.html"), ("storefront", "environment.html"), ("production", "environment.html"), ("web", None)],
    create=[("acme", "projects.html"), ("storefront", "environment.html"), ("production", "environment.html"), ("Create resource", None)],
    database=[("acme", "projects.html"), ("storefront", "environment.html"), ("production", "environment.html"), ("postgres-16", None)],
    stack=[("acme", "projects.html"), ("storefront", "environment.html"), ("production", "environment.html"), ("observability", None)],
    empty=[("acme", "projects.html"), ("sandbox", "empty.html"), ("production", None)],
    templates=[("acme", "projects.html"), ("Templates", None)],
    server=[("acme", "projects.html"), ("Servers", "server.html"), ("atlas", None)],
    playground=[("acme", "projects.html"), ("Models", "playground.html"), ("Qwen3-4B", None)],
    settings=[("acme", "projects.html"), ("Settings", "settings.html"), ("Members", None)],
    notfound=[("acme", "projects.html"), ("Not found", None)],
    components=[("podkit", "../index.html"), ("Component sheet", None)],
)
MARK = '<svg class="brand-svg" viewBox="0 0 24 24" aria-hidden="true"><rect x="2.5" y="6.5" width="19" height="11" rx="5.5" fill="none" stroke="currentColor" stroke-width="2"/><path d="M12 6.5v11" stroke="currentColor" stroke-width="2"/><circle cx="7.2" cy="12" r="1.6" fill="currentColor"/></svg>'
PANEL_KEYS = ["production", "staging", "1h", "24h", "7d", "connection", "backups", "services", "compose", "git", "image", "db", "stack", "template", "model"]

def brand(): return f'<a class="brand" href="projects.html"><span class="brand-mark">{MARK}</span><span class="brand-name">podkit</span></a>'
def team():
    return ('<button class="team" type="button" commandfor="team-menu" command="toggle-popover" aria-haspopup="menu" aria-label="Team: acme. Switch team">'
            '<span class="avatar" aria-hidden="true">A</span><span class="team-name">acme</span>' + icon("chevrons-up-down") + '</button>')
def team_menu():
    return ('<div id="team-menu" class="menu menu--sm anchored" popover><div focusgroup="menu nomemory" aria-label="Teams"><p class="menu-h" role="presentation">Teams</p>'
            f'<button class="menu-item" type="button" aria-checked="true" role="menuitemradio"><span class="avatar" aria-hidden="true">A</span><span><b>acme</b><small>5 members · atlas</small></span></button>'
            f'<button class="menu-item" type="button" role="menuitemradio" aria-checked="false"><span class="avatar" aria-hidden="true">C</span><span><b>personal</b><small>1 member</small></span></button><hr>'
            f'<a class="menu-item" href="settings.html">{icon("settings")}<span><b>Team settings</b></span></a><button class="menu-item" type="button">{icon("plus")}<span><b>Create team</b></span></button></div></div>')
def search(): return '<button class="search" type="button" commandfor="cmdk" command="show-modal" aria-label="Search or jump to (Control K)">' + icon("search") + '<span class="search-label">Search or jump to…</span><kbd class="kbd">Ctrl K</kbd></button>'
def theme_btn(): return '<button class="icon-btn" type="button" data-theme-toggle aria-label="Toggle light and dark theme">' + icon("sun", "only-dark") + icon("moon", "only-light") + '</button>'
def navlinks(cur):
    items = []
    for k, label, ic, href in NAV:
        c = ' aria-current="page"' if k == cur else ""
        items.append(f'<li><a class="nav-link" href="{href}"{c}>{icon(ic)}<span class="nav-label">{label}</span></a></li>')
    return f'<nav class="nav" aria-label="Primary"><ul class="nav-list">{"".join(items)}</ul></nav>'
def crumbs(screen):
    out = []
    for n, h in CRUMBS[screen]:
        if h == "SW":
            out.append(f'<li aria-current="page"><button class="crumb-sw" type="button" commandfor="env-menu" command="toggle-popover" aria-haspopup="menu">{n}{icon("chevrons-up-down")}</button></li>')
        elif h: out.append(f'<li><a href="{h}">{n}</a></li>')
        else: out.append(f'<li aria-current="page">{n}</li>')
    return f'<nav class="crumbs-wrap" aria-label="Breadcrumb"><ol class="crumbs">{"".join(out)}</ol></nav>'
def env_menu():
    return ('<div id="env-menu" class="menu menu--sm anchored" popover><div focusgroup="menu nomemory" aria-label="Environments"><p class="menu-h" role="presentation">Environment</p>'
            f'<a class="menu-item" href="environment.html" role="menuitemradio" aria-checked="true"><span><b>production</b><small>6 resources</small></span></a>'
            f'<a class="menu-item" href="environment.html" role="menuitemradio" aria-checked="false"><span><b>staging</b><small>2 resources</small></span></a><hr>'
            f'<button class="menu-item" type="button">{icon("plus")}<span><b>New environment</b></span></button></div></div>')
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
            tree = ('<div class="tree"><p class="tree-h">acme</p><ul><li><a href="environment.html">▾ storefront</a><ul><li><a href="environment.html" aria-current="page">production</a></li><li><a href="environment.html">staging</a></li></ul></li>'
                    '<li><a href="projects.html">▸ internal-tools</a></li><li><a href="projects.html">▸ analytics</a></li><li><a href="projects.html">▸ ai-lab</a></li></ul></div>')
        return (f'<div class="app app--sidebar"><aside class="chrome" aria-label="Application">{brand()}{team()}{navlinks(cur)}{tree}<div class="chrome-foot"><span class="avatar avatar--user" aria-hidden="true">C</span><span class="who">chiko</span></div></aside>'
                f'<div class="stage"><header class="bar">{crumbs(screen)}<div class="bar-actions">{search()}{theme_btn()}</div></header><main id="main" class="page">{body}</main>{st}</div></div>')
    vb = varbar() if cfg.get("varbar") else ""
    return (f'<div class="app app--topbar"><header class="chrome"><div class="chrome-row">{brand()}{team()}{navlinks(cur)}<div class="chrome-sp"></div>{search()}{theme_btn()}<span class="avatar avatar--user" aria-hidden="true">C</span></div>{vb}</header>'
            f'<div class="stage"><div class="bar">{crumbs(screen)}</div><main id="main" class="page">{body}</main>{st}</div></div>')

def cmdk():
    li = "".join(f'<li role="presentation"><a class="cmd-item" role="option" id="cmd-{i}" href="{h}">{icon(ic)}<span>{t}</span>{f"<kbd class=kbd>{k}</kbd>" if k else ""}</a></li>' for i, (ic, t, h, k) in enumerate(CMDK))
    return ('<dialog id="cmdk" class="dlg dlg--cmd" aria-label="Command palette" closedby="any"><div class="cmd"><div class="cmd-in">' + icon("search") +
            '<input id="cmd-input" type="text" role="combobox" aria-expanded="true" aria-controls="cmd-list" aria-autocomplete="list" placeholder="Search projects, resources, templates…" aria-label="Search" autocomplete="off" autofocus><kbd class="kbd">Esc</kbd></div>'
            f'<p class="cmd-h" id="cmd-h">Suggestions</p><ul id="cmd-list" class="cmd-list" role="listbox" aria-labelledby="cmd-h">{li}</ul><p class="cmd-empty" hidden>No matches. Try a project, a resource or “templates”.</p>'
            '<p class="cmd-foot"><span><kbd class="kbd">↑</kbd><kbd class="kbd">↓</kbd> move</span><span><kbd class="kbd">Enter</kbd> open</span><span><kbd class="kbd">Esc</kbd> close</span></p></div></dialog>')

def head_tags(cfg):
    css = ["tokens.css", "base.css"] + (["structure.css"] if cfg.get("structure", True) else []) + ["extra.css", "concept.css", "mobile.css"]
    return "\n".join(f'<link rel="stylesheet" href="{c}">' for c in css)

def page(cfg, screen, title, body, extra="", body_attrs="", head_extra="", bare=False):
    inner = (body if bare else frame(cfg, screen, body)) + ("" if bare else cmdk() + (team_menu() + env_menu())) + extra
    doc = (f'<!doctype html>\n<html lang="en" data-default-theme="{cfg["theme"]}" data-concept="{cfg["id"]}">\n<head>\n<meta charset="utf-8">\n<meta name="viewport" content="width=device-width,initial-scale=1">\n'
           f'<title>{esc(title)} · podkit · {cfg["name"]}</title>\n<link rel="icon" href="data:,">\n<meta name="color-scheme" content="dark light">\n'
           "<script>(function(){var q=new URLSearchParams(location.search),t=q.get('theme'),d=document.documentElement;try{t=t||localStorage.getItem('pk-theme')}catch(e){}d.dataset.theme=(t==='dark'||t==='light')?t:d.dataset.defaultTheme;if(q.has('still'))d.dataset.still=''})()</script>\n"
           + head_tags(cfg) + "\n" + head_extra + '<script src="page.js" defer></script>\n<script src="sim.js" defer></script>\n</head>\n'
           f'<body data-screen="{screen}" {body_attrs}>\n' + ('' if bare else '<a class="skip" href="#main">Skip to content</a>\n') + f'@@SPRITE@@\n{inner}\n</body>\n</html>\n')
    return doc.replace("@@SPRITE@@", sprite(doc))

# ---------- shared bits ----------
def head(title, lede, actions="", eyebrow=""):
    e = f'<p class="eyebrow">{eyebrow}</p>' if eyebrow else ""
    return f'<header class="page-head"><div class="page-title">{e}<h1>{title}</h1><p class="lede">{lede}</p></div><div class="page-actions">{actions}</div></header>'
def btn(label, ic=None, cls="", extra=""):
    return f'<button class="btn {cls}" type="button" {extra}>{icon(ic) if ic else ""}<span>{label}</span></button>'
def abtn(label, ic, cls, href):
    return f'<a class="btn {cls}" href="{href}">{icon(ic) if ic else ""}<span>{label}</span></a>'
def tabs(name, items, cls=""):
    """Native radio tabs: arrow keys move + select, panels switch with :has() (no JS). items = [(value, label, count)]"""
    o = f'<fieldset class="tabs {cls}"><legend class="sr-only">{name.title()}</legend>'
    for i, (v, l, n) in enumerate(items):
        o += f'<input type="radio" name="{name}" id="{name}-{v}" value="{v}"{" checked" if i == 0 else ""}><label for="{name}-{v}" class="tab">{l}{f" <span class=tab-n>{n}</span>" if n != "" else ""}</label>'
    return o + '</fieldset>'
def menu(id_, label, items):
    """Popover menu: focusgroup gives arrow keys natively (script fallback in page.js). items: (icon, label, kind, extra) or 'hr'."""
    o = f'<div id="{id_}" class="menu anchored" popover><div focusgroup="menu nomemory" aria-label="{label}">'
    for it in items:
        if it == "hr": o += "<hr>"; continue
        ic, t, kind, ex = it
        cls = "menu-item" + (" menu-item--danger" if kind == "danger" else "")
        o += f'<button class="{cls}" type="button" {ex}>{icon(ic)}<span><b>{t}</b></span></button>'
    return o + "</div></div>"
def tile(name, size=40, cls="tile"):
    return f'<span class="{cls}" style="--sz:{size}px">{logo(name, size)}</span>'
def field(label, control, hint="", err="", fid=None, req=False):
    ids = ""
    return f'<div class="field"><label class="field-l" for="{fid}">{label}{" <span class=req>required</span>" if req else ""}</label>{control}{f"<p class=field-h id={fid}-h>{hint}</p>" if hint else ""}{f"<p class=field-e id={fid}-e>{icon("circle-alert")}<span>{err}</span></p>" if err else ""}</div>'
def select(fid, opts, sel=0, cls=""):
    o = "".join(f'<option{" selected" if i == sel else ""}>{x}</option>' for i, x in enumerate(opts))
    return f'<span class="select {cls}"><select id="{fid}" class="input">{o}</select>{icon("chevron-down")}</span>'
def tip(id_, text): return f'<span id="{id_}" popover="hint" class="tip" role="tooltip">{text}</span>'

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
    sk = "".join('<li class="proj proj--sk" aria-hidden="true"><div class="proj-link"><span class="sk" style="height:1.2em;width:55%"></span><span class="sk" style="height:28px;width:70%;margin-top:var(--s-4)"></span><span class="sk" style="height:.9em;width:90%;margin-top:var(--s-4)"></span><span class="sk" style="height:6px;width:100%;margin-top:var(--s-4)"></span></div></li>' for _ in range(6))
    new = f'<li class="proj proj--new" style="--i:{len(PROJECTS)}"><button class="proj-link" type="button" commandfor="new-project" command="show-modal">{icon("plus")}<span>New project</span><span class="proj-new-sub">Group resources by product</span></button></li>'
    summary = '<ul class="summary" aria-label="Team summary"><li><b>31</b> resources</li><li data-status="running"><span class="dot" aria-hidden="true"></span><b>21</b> running</li><li data-status="building"><span class="dot" aria-hidden="true"></span><b>2</b> building</li><li data-status="failed"><span class="dot" aria-hidden="true"></span><b>1</b> failed</li><li data-status="stopped"><span class="dot" aria-hidden="true"></span><b>7</b> stopped</li></ul>'
    body = head("Projects", "Team acme · 6 projects, 2 on staging", btn("Reload", "refresh-cw", "btn--ghost", "data-reload") + btn("New project", "plus", "btn--primary", 'commandfor="new-project" command="show-modal"')) + summary
    body += f'<section aria-label="All projects"><ul class="proj-grid skeleton" aria-hidden="true">{sk}</ul><ul class="proj-grid real reveal">{"".join(items)}{new}</ul></section>'
    dlg = (f'<dialog id="new-project" class="dlg" aria-labelledby="np-t" closedby="any"><form method="dialog" class="dlg-form"><header class="dlg-head"><div><p class="eyebrow">Project</p><h2 id="np-t">New project</h2></div>'
           f'<button class="icon-btn" type="submit" value="cancel" aria-label="Close">{icon("x")}</button></header><div class="dlg-body">'
           + field("Name", '<input id="np-name" class="input" name="name" placeholder="storefront" autocomplete="off" autofocus required pattern="[a-z0-9-]+" aria-describedby="np-name-h">', "Lowercase letters, numbers and dashes. Used in internal DNS.", fid="np-name", req=True)
           + field("Environments", '<div class="chips"><label class="check"><input type="checkbox" checked> production</label><label class="check"><input type="checkbox"> staging</label></div>', fid="np-env")
           + '</div><footer class="dlg-foot"><button class="btn btn--ghost" type="submit" value="cancel">Cancel</button><button class="btn btn--primary" type="submit" value="create" data-toast="Project created">' + icon("plus") + '<span>Create project</span></button></footer></form></dialog>')
    return body, dlg

# ---------- 2 environment ----------
def res_item(r, i, sparkn=0, sw=True):
    st = r["st"]
    meta = "".join(f'<div><dt>{k}</dt><dd>{v}</dd></div>' for k, v in r["meta"])
    mt = "".join(f'<div><dt>{k}</dt><dd>{v}</dd></div>' for k, v in r["m"])
    extra = ""
    if st == "building": extra = f'<div class="progress" role="progressbar" aria-label="Build progress" aria-valuenow="{r.get("prog",50)}" aria-valuemin="0" aria-valuemax="100"><span style="width:{r.get("prog",50)}%"></span></div>'
    if st == "failed": extra = f'<p class="res-err">{icon("circle-alert")}<span>{r["err"]}</span></p>'
    sp = spark(i * 7 + 3, base=.4 + .05 * (i % 3)) if st == "running" else ""
    return f'''<li class="res" data-status="{st}" data-kind="{r["t"]}" data-id="{r["id"]}" style="--i:{i}"><a class="res-link" href="{r["href"]}">
<span class="res-logo">{logo(r["logo"], 36)}</span>
<span class="res-main"><span class="res-kind">{icon(r["t"])}{r["kind"]}</span><h3 class="res-name">{r["name"]}</h3><span class="res-sub">{r["sub"]}</span></span>
{badge(st)}
<dl class="res-meta">{meta}</dl><dl class="res-metrics">{mt}</dl>{sp}{extra}</a></li>'''

def res_skel(n=6):
    return "".join('<li class="res res--sk" aria-hidden="true"><div class="res-link"><span class="sk sk-logo"></span><span class="res-main"><span class="sk" style="height:.8em;width:30%"></span><span class="sk" style="height:1.1em;width:60%;margin-top:6px"></span><span class="sk" style="height:.8em;width:85%;margin-top:6px"></span></span><span class="sk" style="height:1.4em;width:72px"></span></div></li>' for _ in range(n))

def create_menu():
    items = [(ic, t, s, f"create.html#{k}") for (ic, t, s, _d, _f, _e), k in zip(RTYPES, ["git", "image", "db", "stack", "template", "model"])]
    li = "".join(f'<a class="menu-item" href="{h}">{icon(ic)}<span><b>{t}</b><small>{s}</small></span></a>' for ic, t, s, h in items)
    return f'<div id="create-menu" class="menu anchored" popover><div focusgroup="menu nomemory" aria-label="Create resource"><p class="menu-h" role="presentation">Create resource</p>{li}<hr><a class="menu-item" href="create.html">{icon("arrow-right")}<span><b>All options…</b></span></a></div></div>'

def environment(cfg):
    inv = 'commandfor="create-menu" command="toggle-popover" aria-haspopup="menu"'
    body = head("storefront", "6 resources in production · 4 running · 1 building · 1 failed · 1 stopped",
                btn("Reload", "refresh-cw", "btn--ghost", "data-reload") + btn("Deploy all", "rocket", "btn--secondary", 'data-toast="Queued 3 deployments"') + btn("Create resource", "plus", "btn--primary", inv), "Project")
    prod = "".join(res_item(r, i) for i, r in enumerate(RES))
    new = f'<li class="res res--new" style="--i:{len(RES)}"><a class="res-link res-new" href="create.html">{icon("plus")}<span class="res-new-t">Create resource</span><span class="res-new-s">Git repo · image · database · stack · template · AI model</span></a></li>'
    stg = "".join(res_item(r, i) for i, r in enumerate(STAGING))
    body += f'''<div class="tabset">{tabs("env", [("production", "production", 6), ("staging", "staging", 2)])}
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
    m = menu("app-menu", "More actions", [("undo-2", "Roll back to #147", "", ""), ("copy", "Duplicate", "", ""), ("folder-open", "Move to staging", "", ""), ("key-round", "Environment variables", "", ""), "hr", ("trash-2", "Delete…", "danger", 'commandfor="del-dlg" command="show-modal"')])
    tabs_ = "".join(f'<li><a class="tab" href="#"{" aria-current=\"page\"" if i==0 else ""}>{t}</a></li>' for i, t in enumerate(["Deployments", "Logs", "Environment", "Domains", "Storage", "Settings"]))
    log = json.dumps([list(x) for x in LOG])
    body = f'''<header class="app-head"><span class="app-logo">{logo("nextdotjs", 48)}</span>
<div class="app-id"><p class="eyebrow">{icon("git-branch")} Git app</p><h1>web</h1>
<p class="app-sub">{badge("building", extra='id="deploy-badge"')}<a class="url" href="#">shop.acme.dev{icon("external-link")}</a><span class="gitref">{icon("git-commit-horizontal")}main · a3f9c21</span></p></div>
<div class="page-actions">{btn("Deploy","rocket","btn--primary",'data-redeploy')}{btn("Restart","rotate-ccw","btn--secondary",'data-toast="Restarting web…"')}{btn("Stop","square","btn--secondary",'data-toast="Stopping web…"')}
<button class="btn btn--icon" type="button" commandfor="app-menu" command="toggle-popover" aria-haspopup="menu" aria-label="More actions">{icon("ellipsis")}</button></div></header>
<dl class="stat-row" aria-label="Live stats">{sd}</dl>
<nav class="subnav" aria-label="App sections"><ul class="tabs-line">{tabs_}</ul></nav>
<div class="app-body"><section class="deploys" aria-labelledby="h-dep"><h2 id="h-dep" class="sec-h">Deployments</h2><ol class="timeline">{"".join(dep)}</ol></section>
<section class="build-log" aria-labelledby="h-log"><header class="log-head"><h2 id="h-log" class="sec-h">Build log <span class="log-id">#148</span></h2><span class="live-pill"><span class="dot" aria-hidden="true"></span>Live</span>
<label class="check follow"><input type="checkbox" id="follow" checked><span>Follow</span></label><button class="btn btn--ghost btn--sm" type="button" data-toast="Log copied">{icon("copy")}<span>Copy</span></button></header>
<pre class="log" id="build-log" role="log" aria-label="Build log output" tabindex="0"></pre><script type="application/json" id="log-script">{log}</script></section></div>'''
    dlg = confirm_dialog("del-dlg", "Delete web?", "This stops the container, removes its 2 volumes (310 MB) and all 148 deployments. It cannot be undone.", "Delete web")
    return body, m + dlg

def confirm_dialog(id_, title, text, action, danger=True):
    return (f'<dialog id="{id_}" class="dlg dlg--confirm" aria-labelledby="{id_}-t" aria-describedby="{id_}-d" closedby="any"><form method="dialog" class="dlg-form"><header class="dlg-head"><span class="dlg-ic dlg-ic--{"err" if danger else "info"}">{icon("triangle-alert" if danger else "info")}</span><div><h2 id="{id_}-t">{title}</h2></div></header>'
            f'<div class="dlg-body"><p id="{id_}-d">{text}</p></div><footer class="dlg-foot"><button class="btn btn--secondary" type="submit" value="cancel" autofocus>Cancel</button><button class="btn {"btn--danger-solid" if danger else "btn--primary"}" type="submit" value="ok" data-toast="{action}: queued">{icon("trash-2" if danger else "check")}<span>{action}</span></button></footer></form></dialog>')

# ---------- 4 templates ----------
def templates(cfg):
    cards = []
    for i, (t, lg, desc, cat, ver) in enumerate(TEMPLATES):
        cards.append(f'''<li class="tpl" data-name="{t.lower()}" data-cat="{cat}" data-title="{t}" style="--i:{i}"><button class="tpl-btn" type="button" commandfor="deploy" command="show-modal"><span class="tpl-logo">{logo(lg, 44)}</span>
<span class="tpl-main"><h3 class="tpl-name">{t}</h3><span class="tpl-desc">{desc}</span></span><span class="tpl-foot"><span class="chip">{cat}</span><span class="tpl-ver">v{ver}</span></span></button></li>''')
    cats = "".join(f'<li><button class="chip chip--btn" type="button" data-cat="{k}" aria-pressed="{"true" if k=="all" else "false"}">{n}<span class="chip-n">{c}</span></button></li>' for k, n, c in CATS)
    body = head("Templates", '<span id="tpl-count">20 templates</span> shown · 484 in the catalog', btn("Import compose file", "upload", "btn--secondary"), "One-click apps")
    body += f'''<div class="tpl-tools"><div class="field field--search"><label class="sr-only" for="tpl-search">Search templates</label>{icon("search")}<input id="tpl-search" class="input" type="search" placeholder="Search 484 templates: gotify, plausible, n8n…" autocomplete="off"></div>
<ul class="cats" aria-label="Category">{cats}</ul></div><ul class="tpl-grid reveal">{"".join(cards)}</ul><p class="tpl-none" hidden>{icon("search")} No template matches. Try “analytics” or clear the category.</p>'''
    dlg = f'''<dialog id="deploy" class="dlg" aria-labelledby="deploy-title" closedby="any"><form method="dialog" class="dlg-form">
<header class="dlg-head"><span id="deploy-logo" class="dlg-logo">{logo("gotify", 44)}</span><div><p class="eyebrow">Deploy template</p><h2 id="deploy-title">Deploy <span id="deploy-name">Gotify</span></h2></div><button class="icon-btn" type="submit" value="cancel" aria-label="Close">{icon("x")}</button></header>
<div class="dlg-body"><div class="field-row">{field("Project", select("dp-p", ["storefront", "internal-tools", "analytics"]), fid="dp-p")}{field("Environment", select("dp-e", ["production", "staging"]), fid="dp-e")}</div>
{field("Domain", '<input id="deploy-dom" class="input" value="gotify.acme.dev" autocomplete="off" autofocus aria-describedby="deploy-dom-h">', "DNS points at atlas (10.0.0.12). TLS is issued automatically.", fid="deploy-dom")}
{field("Server", select("dp-s", ["atlas · 14% CPU · 20 GB free"]), fid="dp-s")}
<div class="note">{icon("key-round")}<p>podkit generates <b>2 secrets</b> (admin password, app key) and stores them as environment variables you can reveal later.</p></div></div>
<footer class="dlg-foot"><button class="btn btn--ghost" type="submit" value="cancel">Cancel</button><button id="deploy-go" class="btn btn--primary" type="submit" value="deploy">{icon("rocket")}<span>Deploy</span></button></footer></form></dialog>'''
    return body, dlg

# ---------- 5 server ----------
def server(cfg):
    hw = [("cpu", "CPU", "Ryzen 7 9850X3D", "8 cores / 16 threads"), ("memory-stick", "RAM", "30 GB", "DDR5-6000"), ("zap", "GPU", "RTX 5070", "12 GB VRAM · CUDA 12.9"), ("hard-drive", "Disk", "NVMe 2 TB", "612 GB used")]
    hws = "".join(f'<li class="hw-item">{icon(i)}<div><p class="hw-k">{k}</p><p class="hw-v">{v}</p><p class="hw-s">{s}</p></div></li>' for i, k, v, s in hw)
    cards = []
    for mi, m in enumerate(METRICS):
        panels = ""
        for rg in RANGES:
            panels += f'<div data-panel="{rg}">{chart(cfg["charts"], metric_series(m, rg, mi), m["ymax"], m["fmt"], RANGES[rg][1], label=f"{m["name"]} over {rg}, now {m["cur"]} {m["unit"]}")}</div>'
        unit = f'<span class="u">{m["unit"]}</span>'
        cards.append(f'''<article class="metric" data-metric="{m["id"]}" style="--i:{mi};--c:var(--chart-{mi%6+1})"><header class="metric-head"><h2 class="metric-name">{icon(m["icon"])}{m["name"]}</h2><p class="metric-val">{m["cur"]}{unit}</p></header>
<p class="metric-sub">{m["sub"]}</p>{panels}<p class="metric-stats"><span>{m["stats"][0]}</span><span>{m["stats"][1]}</span></p></article>''')
    body = f'''<header class="srv-head"><span class="srv-ic">{icon("server")}</span><div class="srv-id"><p class="eyebrow">Server · 10.0.0.12</p><h1>atlas</h1><p class="app-sub">{badge("online")}<span class="gitref">NixOS 25.11 · Podman 5.4 · up 41 days</span></p></div><div class="page-actions">{btn("Terminal","terminal","btn--secondary")}{btn("Prune images","trash-2","btn--secondary",'data-toast="Pruned 3.2 GB"')}</div></header>
<ul class="hw" aria-label="Hardware">{hws}</ul>
<div class="tabset"><div class="range-bar"><h2 class="sec-h">Metrics</h2>{tabs("rg", [("1h","1h",""),("24h","24h",""),("7d","7d","")], "tabs--range").replace('<input type="radio" name="rg" id="rg-24h" value="24h">','<input type="radio" name="rg" id="rg-24h" value="24h" checked>').replace('<input type="radio" name="rg" id="rg-1h" value="1h" checked>','<input type="radio" name="rg" id="rg-1h" value="1h">')}</div>
<div class="metrics reveal">{"".join(cards)}</div></div>'''
    return body

# ---------- 6 playground ----------
def playground(cfg):
    stats = [("Prompt", "148", "tokens"), ("Completion", "312", "tokens"), ("Speed", "175", "tok/s"), ("First token", "84", "ms"), ("Context", "2.1k / 32k", ""), ("VRAM", "3.4 / 12", "GB")]
    st = "".join(f'<div class="ustat"><dt>{k}</dt><dd>{v}<span class="u">{u}</span></dd></div>' for k, v, u in stats)
    body = f'''<header class="model-head"><span class="app-logo">{logo("qwen", 48)}</span><div class="app-id"><p class="eyebrow">AI model · {icon("brain")} reasoning</p><h1>Qwen3-4B</h1>
<ul class="chips" aria-label="Model details"><li class="chip">Q4_K_M</li><li class="chip">llama.cpp CUDA</li><li class="chip">32k context</li><li class="chip">2.5 GB</li><li>{badge("running")}</li></ul></div>
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
<label class="switch"><input type="checkbox" switch role="switch" checked><span>Show thinking</span></label></section></aside></div>'''
    return body

# ---------- 7 sign in ----------
VOICE = {
 "01-terminal-ops": dict(h1="login", lede="atlas · 10.0.0.12 · podkit 0.9.2", sub="Authenticate to continue.", e404="route not found", e404h="No such route", e404p="No route matches <code>/projects/storefront/staging/web-2</code>. It was deleted, renamed, or you lack access.", emp_h="Nothing running in sandbox / production", emp_p="This environment has no resources yet. Pick a source and podkit builds and starts it on atlas."),
 "02-quiet-linear": dict(h1="Sign in to podkit", lede="Self-hosted on atlas", sub="Use your email and password. Ask an owner for an invite if you do not have an account yet.", e404="404", e404h="This page doesn’t exist", e404p="Nothing matches <code>/projects/storefront/staging/web-2</code>. It may have been deleted or renamed, or you may not have access.", emp_h="sandbox / production is empty", emp_p="Create the first resource. A Git repository, an image, a database or a template all work."),
 "09-data-dashboard": dict(h1="Sign in", lede="podkit · atlas · 10.0.0.12", sub="Local account. OIDC is not configured on this server.", e404="HTTP 404", e404h="No data at this route", e404p="The path <code>/projects/storefront/staging/web-2</code> matched no panel. It may have been deleted, or you may not have access.", emp_h="No panels yet", emp_p="sandbox / production has no resources. Create one and its status, logs and metrics appear here."),
}
def signin(cfg):
    v = VOICE[cfg["id"]]
    form = (f'<form class="auth-form" action="projects.html" method="get" novalidate>'
            + field("Email", '<input id="si-email" class="input" name="email" type="email" autocomplete="username" required autofocus placeholder="you@acme.dev" aria-describedby="si-email-e">', "", "Enter a valid email address.", "si-email")
            + field("Password", f'<span class="pw"><input id="si-pw" class="input" name="password" type="password" autocomplete="current-password" required aria-describedby="si-pw-e"><button class="pw-btn" type="button" data-reveal="si-pw" aria-label="Show password" aria-pressed="false">{icon("eye")}</button></span>', "", "Enter your password.", "si-pw")
            + f'<div class="auth-row"><label class="check"><input type="checkbox" checked> Keep me signed in</label><a class="link" href="#">Forgot password?</a></div>'
            + f'<p class="banner banner--err" id="si-err" role="alert" hidden>{icon("circle-alert")}<span>Email or password is incorrect. 2 attempts left before a 5 minute lock.</span></p>'
            + f'<button class="btn btn--primary btn--lg btn--block" type="submit" id="si-go"><span>Sign in</span>{icon("arrow-right")}</button></form>')
    meta = (f'<dl class="auth-meta" aria-label="Server"><div><dt>Server</dt><dd>atlas {badge("online")}</dd></div><div><dt>Version</dt><dd>podkit 0.9.2</dd></div><div><dt>Sign-in</dt><dd>Local password</dd></div></dl>')
    body = (f'<main id="main" class="auth"><div class="auth-wrap"><section class="auth-card" aria-labelledby="auth-t"><a class="brand" href="projects.html"><span class="brand-mark">{MARK}</span><span class="brand-name">podkit</span></a>'
            f'<div class="auth-head"><h1 id="auth-t">{v["h1"]}</h1><p class="lede">{v["lede"]}</p></div>{form}<p class="auth-foot">{v["sub"]}</p></section>{meta}</div><div class="auth-theme">{theme_btn()}</div></main>')
    return page(cfg, "signin", "Sign in", body, extra="", bare=True)

# ---------- 8 create picker ----------
def create(cfg):
    keys = ["git", "image", "db", "stack", "template", "model"]
    targets = ["environment.html", "environment.html", "database.html", "stack.html", "templates.html", "playground.html"]
    radios, panels = "", ""
    for i, ((ic, t, s, d, fields, eta), k, tg) in enumerate(zip(RTYPES, keys, targets)):
        radios += (f'<label class="pick-item"><input type="radio" name="rtype" value="{k}" id="rt-{k}"{" checked" if i == 0 else ""}><span class="pick-ic">{icon(ic)}</span><span class="pick-t"><b>{t}</b><small>{s}</small></span>{icon("chevron-right","pick-go")}</label>')
        fl = "".join(f'<div><dt>{a}</dt><dd>{b}</dd></div>' for a, b in fields)
        panels += (f'<section class="pick-detail" data-panel="{k}" aria-label="{t}"><span class="pick-big">{icon(ic)}</span><h2>{t}</h2><p>{d}</p><dl class="pick-fields" aria-label="You will provide">{fl}</dl>'
                   f'<p class="pick-eta">{icon("clock")}{eta}</p><div class="page-actions"><a class="btn btn--primary" href="{tg}">Continue{icon("arrow-right")}</a><a class="btn btn--ghost" href="environment.html">Cancel</a></div></section>')
    body = head("Create resource", "in storefront / production · choose what to run", abtn("Browse templates", "package", "btn--secondary", "templates.html"), "Step 1 of 2")
    body += f'<div class="pick tabset"><fieldset class="pick-list"><legend class="sr-only">Resource type</legend>{radios}</fieldset><div class="pick-panels">{panels}</div></div>'
    return body, ""

# ---------- 9 database ----------
def copyfield(fid, label, value, secret=False):
    ctl = f'<input id="{fid}" class="input mono" readonly value="{value}"{" type=password" if secret else ""} aria-describedby="tip-copy">'
    rev = f'<button class="btn btn--icon" type="button" data-reveal="{fid}" aria-label="Show {label.lower()}" aria-pressed="false" interestfor="tip-reveal">{icon("eye")}</button>' if secret else ""
    return f'<div class="field copyfield"><label class="field-l" for="{fid}">{label}</label><div class="cf">{ctl}{rev}<button class="btn btn--icon" type="button" data-copy="{fid}" aria-label="Copy {label.lower()}" interestfor="tip-copy">{icon("copy")}</button></div></div>'

def database(cfg):
    stats = [("Size", "2.1 GB"), ("Connections", "18 / 100"), ("CPU", "1.4%"), ("Memory", "640 MB")]
    sd = "".join(f'<div><dt>{k}</dt><dd>{v}</dd></div>' for k, v in stats)
    m = menu("db-menu", "More actions", [("copy", "Duplicate to staging", "", ""), ("download", "Export dump", "", ""), ("key-round", "Rotate password", "", ""), "hr", ("trash-2", "Delete…", "danger", 'commandfor="db-del" command="show-modal"')])
    conn = (f'<div class="grid-2"><section class="card" aria-labelledby="h-cred"><header class="card-h"><h2 id="h-cred" class="sec-h">Credentials</h2><span class="card-sub">internal network</span></header><div class="card-b">'
            + copyfield("c-host", "Host", "postgres-16.storefront.internal") + '<div class="field-row">' + copyfield("c-port", "Port", "5432") + copyfield("c-db", "Database", "storefront") + '</div>'
            + copyfield("c-user", "User", "app") + copyfield("c-pw", "Password", "k3mFq9xVw2LpZs7R", True) + '</div></section>'
            f'<div class="stack-v"><section class="card" aria-labelledby="h-str"><header class="card-h"><h2 id="h-str" class="sec-h">Connection string</h2></header><div class="card-b">'
            f'<div class="codebox"><code id="c-str">postgresql://app:••••••••@postgres-16.storefront.internal:5432/storefront</code><button class="btn btn--icon" type="button" data-copy-text="postgresql://app:…@postgres-16.storefront.internal:5432/storefront" aria-label="Copy connection string" interestfor="tip-copy">{icon("copy")}</button></div>'
            f'<p class="field-h">Apps in the same project reach it as <code>DATABASE_URL</code> automatically.</p></div></section>'
            f'<section class="card" aria-labelledby="h-net"><header class="card-h"><h2 id="h-net" class="sec-h">Network</h2></header><div class="card-b">'
            f'<label class="switch switch--row"><input type="checkbox" switch role="switch" aria-describedby="pub-h"><span><b>Expose publicly</b><small id="pub-h">Binds 0.0.0.0:5432 on atlas. Only enable behind a firewall.</small></span></label>'
            f'<label class="switch switch--row"><input type="checkbox" switch role="switch" checked><span><b>Require TLS</b><small>Clients without sslmode=require are refused.</small></span></label></div></section></div></div>')
    rows = "".join(f'<tr><td data-label="Started"><b>{d}</b></td><td data-label="Trigger">{trg}</td><td data-label="Size" class="num">{sz}</td><td data-label="Status">{badge(st, "Completed" if st=="running" else "Failed")}</td>'
                   f'<td class="tbl-act"><button class="btn btn--icon btn--sm" type="button" commandfor="bk-menu" command="toggle-popover" aria-haspopup="menu" aria-label="Actions for backup {d}">{icon("ellipsis")}</button></td></tr>' for d, trg, sz, _s, st in BACKUPS)
    bk = (f'<div class="grid-2 grid-2--wide"><section class="card" aria-labelledby="h-bk"><header class="card-h"><h2 id="h-bk" class="sec-h">Backups</h2>{btn("Back up now","archive","btn--secondary btn--sm","data-backup")}</header>'
          f'<div class="tbl-wrap"><table class="tbl"><caption class="sr-only">Backups of postgres-16</caption><thead><tr><th scope="col">Started</th><th scope="col">Trigger</th><th scope="col" class="num">Size</th><th scope="col">Status</th><th scope="col"><span class="sr-only">Actions</span></th></tr></thead><tbody>{rows}</tbody></table></div></section>'
          f'<section class="card" aria-labelledby="h-sch"><header class="card-h"><h2 id="h-sch" class="sec-h">Schedule</h2></header><div class="card-b">'
          f'<label class="switch switch--row"><input type="checkbox" switch role="switch" checked><span><b>Automatic backups</b><small>Runs on atlas, stored on the backup volume.</small></span></label>'
          + field("Frequency", select("bk-f", ["Daily at 03:00", "Every 6 hours", "Weekly, Sunday 03:00"]), fid="bk-f")
          + field("Keep", '<input id="bk-k" class="input" type="number" min="1" max="90" value="7" inputmode="numeric" aria-describedby="bk-k-h">', "Older backups are deleted automatically. 7 backups ≈ 2.8 GB.", fid="bk-k")
          + f'<button class="btn btn--primary" type="button" data-toast="Schedule saved">{icon("save")}<span>Save schedule</span></button></div></section></div>')
    bm = menu("bk-menu", "Backup actions", [("history", "Restore…", "", 'commandfor="db-restore" command="show-modal"'), ("download", "Download", "", ""), "hr", ("trash-2", "Delete", "danger", "")])
    body = f'''<header class="app-head"><span class="app-logo">{logo("postgres", 48)}</span><div class="app-id"><p class="eyebrow">{icon("database")} Database · PostgreSQL 16.4</p><h1>postgres-16</h1>
<p class="app-sub">{badge("running")}<span class="gitref">{icon("hard-drive")}volume pg16-data · 2.1 GB</span></p></div>
<div class="page-actions">{btn("Restart","rotate-ccw","btn--secondary",'data-toast="Restarting postgres-16…"')}{btn("Stop","square","btn--secondary",'data-toast="Stopping postgres-16…"')}<button class="btn btn--icon" type="button" commandfor="db-menu" command="toggle-popover" aria-haspopup="menu" aria-label="More actions">{icon("ellipsis")}</button></div></header>
<dl class="stat-row" aria-label="Live stats">{sd}</dl>
<div class="tabset">{tabs("dbtab", [("connection", "Connection", ""), ("backups", "Backups", 6)])}<div data-panel="connection">{conn}</div><div data-panel="backups">{bk}</div></div>'''
    extra = (m + bm + tip("tip-copy", "Copy") + tip("tip-reveal", "Show / hide")
             + confirm_dialog("db-del", "Delete postgres-16?", "The container, its volume (2.1 GB) and all 6 backups are removed. Apps that use DATABASE_URL will stop connecting.", "Delete database")
             + confirm_dialog("db-restore", "Restore this backup?", "postgres-16 stops for about 40 seconds and its data is replaced with the backup from 2026-11-19 03:00. Changes made since then are lost.", "Restore", danger=False))
    return body, extra

# ---------- 10 stack ----------
def stack(cfg):
    rows = ""
    for s in SERVICES:
        note = f'<p class="svc-note">{icon("circle-alert")}{s["note"]}</p>' if s["note"] else ""
        rows += (f'<tr data-status="{s["st"]}"><td data-label="Service"><div class="svc">{tile(s["logo"], 32)}<div><b>{s["name"]}</b><small class="mono">{s["img"]}</small>{note}</div></div></td>'
                 f'<td data-label="Status">{badge(s["st"])}</td><td data-label="Ports" class="mono">{s["ports"]}</td><td data-label="CPU" class="num">{s["cpu"]}</td><td data-label="Memory" class="num">{s["mem"]}</td><td data-label="Restarts" class="num">{s["rs"]}</td>'
                 f'<td class="tbl-act"><button class="btn btn--icon btn--sm" type="button" commandfor="svc-menu" command="toggle-popover" aria-haspopup="menu" aria-label="Actions for {s["name"]}">{icon("ellipsis")}</button></td></tr>')
    svc = (f'<section class="card" aria-labelledby="h-svc"><header class="card-h"><h2 id="h-svc" class="sec-h">Services</h2><span class="card-sub">1 failed · 3 running</span></header><div class="tbl-wrap"><table class="tbl tbl--svc"><caption class="sr-only">Services in observability</caption>'
           f'<thead><tr><th scope="col">Service</th><th scope="col">Status</th><th scope="col">Ports</th><th scope="col" class="num">CPU</th><th scope="col" class="num">Memory</th><th scope="col" class="num">Restarts</th><th scope="col"><span class="sr-only">Actions</span></th></tr></thead><tbody>{rows}</tbody></table></div></section>')
    code = "".join(f'<span class="log-line"><span class="ts">{i+1}</span><span class="tx">{esc(l)}</span></span>' for i, l in enumerate(COMPOSE.split("\n")))
    comp = f'<section class="card" aria-labelledby="h-cmp"><header class="card-h"><h2 id="h-cmp" class="sec-h">docker-compose.yml</h2>{btn("Edit","pencil","btn--secondary btn--sm")}</header><pre class="log log--static" tabindex="0" aria-label="Compose file">{code}</pre></section>'
    m = menu("stack-menu", "More actions", [("copy", "Duplicate", "", ""), ("download", "Export compose file", "", ""), "hr", ("trash-2", "Delete…", "danger", 'commandfor="stack-del" command="show-modal"')])
    sm = menu("svc-menu", "Service actions", [("rotate-ccw", "Restart", "", ""), ("scroll-text", "View logs", "", ""), ("terminal", "Open shell", "", ""), ("pencil", "Edit memory limit", "", "")])
    body = f'''<header class="app-head"><span class="app-logo">{logo("grafana", 48)}</span><div class="app-id"><p class="eyebrow">{icon("layers")} Compose stack</p><h1>observability</h1>
<p class="app-sub">{badge("failed")}<span class="gitref">{icon("layers")}4 services · project observability</span></p></div>
<div class="page-actions">{btn("Redeploy","rocket","btn--primary",'data-toast="Redeploying observability…"')}{btn("Stop","square","btn--secondary")}<button class="btn btn--icon" type="button" commandfor="stack-menu" command="toggle-popover" aria-haspopup="menu" aria-label="More actions">{icon("ellipsis")}</button></div></header>
<p class="banner banner--err" role="alert">{icon("circle-alert")}<span><b>prometheus exited with code 137 (OOM-killed)</b> 5 restarts in the last 2 h. The memory limit is 512 MB; peak use was 740 MB.</span><button class="btn btn--secondary btn--sm" type="button">Raise limit to 1 GB</button></p>
<div class="tabset">{tabs("sttab", [("services", "Services", 4), ("compose", "Compose file", "")])}<div data-panel="services">{svc}</div><div data-panel="compose">{comp}</div></div>'''
    extra = m + sm + confirm_dialog("stack-del", "Delete observability?", "All 4 services, their volumes (1.8 GB) and the compose file are removed. It cannot be undone.", "Delete stack")
    return body, extra

# ---------- 11 settings ----------
def settings(cfg):
    sn = "".join(f'<li><a class="snav-link" href="#"{" aria-current=\"page\"" if i==1 else ""}>{icon(ic)}<span>{t}</span></a></li>' for i, (ic, t) in enumerate([("settings", "General"), ("users", "Members"), ("key-round", "API tokens"), ("server", "Servers"), ("bell", "Notifications")]))
    rows = ""
    for i, (n, mail, role, last, ini) in enumerate(MEMBERS):
        ctl = f'<span class="role-fixed">{role}</span>' if role == "Owner" else select(f"role-{i}", ["Admin", "Developer", "Viewer"], ["Admin", "Developer", "Viewer"].index(role), "select--sm")
        act = "" if role == "Owner" else f'<button class="btn btn--icon btn--sm" type="button" commandfor="mem-menu" command="toggle-popover" aria-haspopup="menu" aria-label="Actions for {n}">{icon("ellipsis")}</button>'
        rows += (f'<tr><td data-label="Member"><div class="who-cell"><span class="avatar avatar--lg" data-h="{i%6+1}" aria-hidden="true">{ini}</span><div><b>{n}</b><small>{mail}</small></div></div></td>'
                 f'<td data-label="Role">{ctl}</td><td data-label="Last active">{last}</td><td class="tbl-act">{act}</td></tr>')
    inv = "".join(f'<tr><td data-label="Email"><b>{m}</b></td><td data-label="Role">{r}</td><td data-label="Sent">{s}</td><td class="tbl-act"><button class="btn btn--ghost btn--sm" type="button" data-toast="Invite revoked">Revoke</button></td></tr>' for m, r, s in INVITES)
    body = head("Settings", "Team acme · manage who can deploy, view and administer", "", "Team")
    body += f'''<div class="settings"><nav class="snav" aria-label="Settings sections"><ul>{sn}</ul></nav><div class="settings-main">
<section class="card" aria-labelledby="h-mem"><header class="card-h"><h2 id="h-mem" class="sec-h">Members <span class="tab-n">5</span></h2></header><div class="tbl-wrap"><table class="tbl tbl--mem"><caption class="sr-only">Team members</caption><thead><tr><th scope="col">Member</th><th scope="col">Role</th><th scope="col">Last active</th><th scope="col"><span class="sr-only">Actions</span></th></tr></thead><tbody>{rows}</tbody></table></div></section>
<section class="card" aria-labelledby="h-inv"><header class="card-h"><h2 id="h-inv" class="sec-h">Invite a member</h2></header><form class="card-b invite" action="settings.html" data-sim="invite"><div class="field-row field-row--inv">{field("Email", '<input id="inv-email" class="input" type="email" placeholder="name@acme.dev" autocomplete="off" required>', fid="inv-email")}{field("Role", select("inv-role", ["Developer", "Admin", "Viewer"]), fid="inv-role")}<button class="btn btn--primary inv-go" type="submit">{icon("plus")}<span>Send invite</span></button></div>
<p class="field-h">Developers deploy and edit. Admins also manage members and servers. Viewers are read-only.</p></form></section>
<section class="card" aria-labelledby="h-pend"><header class="card-h"><h2 id="h-pend" class="sec-h">Pending invitations <span class="tab-n">2</span></h2></header><div class="tbl-wrap"><table class="tbl"><caption class="sr-only">Pending invitations</caption><thead><tr><th scope="col">Email</th><th scope="col">Role</th><th scope="col">Sent</th><th scope="col"><span class="sr-only">Actions</span></th></tr></thead><tbody>{inv}</tbody></table></div></section></div></div>'''
    mm = menu("mem-menu", "Member actions", [("key-round", "Reset password", "", ""), ("log-out", "Sign out everywhere", "", ""), "hr", ("trash-2", "Remove from team", "danger", 'commandfor="mem-del" command="show-modal"')])
    return body, mm + confirm_dialog("mem-del", "Remove this member?", "They lose access to all projects in acme immediately. Their deployments and logs stay.", "Remove member")

# ---------- 12 empty ----------
def empty(cfg):
    v = VOICE[cfg["id"]]
    body = head("sandbox", "0 resources in production", btn("Reload", "refresh-cw", "btn--ghost") + abtn("Create resource", "plus", "btn--primary", "create.html"), "Project")
    body += f'<div class="tabset">{tabs("env2", [("production", "production", 0), ("staging", "staging", 0)])}'
    emp = (f'<div class="empty" role="status"><span class="empty-ic">{icon("box")}</span><h2>{v["emp_h"]}</h2><p>{v["emp_p"]}</p>'
           f'<div class="empty-act"><a class="btn btn--primary" href="create.html">{icon("plus")}<span>Create resource</span></a><a class="btn btn--secondary" href="templates.html">{icon("package")}<span>Browse templates</span></a><a class="btn btn--ghost" href="create.html#stack">{icon("upload")}<span>Import compose file</span></a></div></div>')
    body += f'<section data-panel="production" aria-label="Production resources">{emp}</section><section data-panel="staging" aria-label="Staging resources">{emp}</section></div>'
    return body, ""

# ---------- 13 404 ----------
def notfound(cfg):
    v = VOICE[cfg["id"]]
    rec = "".join(f'<li><a class="rec" href="{h}">{icon(ic)}<span>{t}</span><span class="rec-s">{s}</span></a></li>' for ic, t, h, s in [("folder-open", "storefront / production", "environment.html", "6 resources"), ("rocket", "web", "app.html", "deploying now"), ("database", "postgres-16", "database.html", "Connection")])
    body = (f'<section class="nf" aria-labelledby="nf-t"><p class="nf-code">{v["e404"]}</p><h1 id="nf-t">{v["e404h"]}</h1><p class="lede">{v["e404p"]}</p>'
            f'<div class="page-actions"><a class="btn btn--primary" href="projects.html">{icon("arrow-left")}<span>Back to Projects</span></a><button class="btn btn--secondary" type="button" commandfor="cmdk" command="show-modal">{icon("search")}<span>Search</span><kbd class="kbd">Ctrl K</kbd></button></div>'
            f'<div class="nf-rec"><h2 class="sec-h">Recently visited</h2><ul>{rec}</ul></div><p class="nf-id">Request <code>9f3c1a7e</code> · logged on atlas</p></section>')
    return body, ""
