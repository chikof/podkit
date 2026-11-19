"""Component sheet: every component podkit uses, every state shown (hover/focus/active are forced with data-state, see states.css)."""
from lib import *
from screens import *

def cell(title, body, cls=""): return f'<div class="cs-cell {cls}"><h3>{title}</h3>{body}</div>'
def sec(id_, title, body): return f'<section class="cs-sec" id="{id_}" aria-labelledby="{id_}-h"><h2 id="{id_}-h">{title}</h2>{body}</section>'
def st(label, inner): return f'<div class="cs-st"><span>{label}</span><div class="cs-row">{inner}</div></div>'
def b(label, cls, ic=None, state="", extra=""):
    a = f' data-state="{state}"' if state else ""
    return f'<button class="btn {cls}" type="button"{a} {extra}>{icon(ic) if ic else ""}<span>{label}</span></button>'

def buttons():
    states = [("default", ""), ("hover", "hover"), ("focus", "focus"), ("active", "active"), ("disabled", "dis"), ("loading", "load")]
    def row(cls, label, ic=None):
        out = ""
        for n, s in states:
            if s == "dis": out += f'<button class="btn {cls}" type="button" disabled>{icon(ic) if ic else ""}<span>{label}</span></button>'
            elif s == "load": out += f'<button class="btn {cls}" type="button" aria-busy="true">{icon(ic) if ic else ""}<span>{label}</span></button>'
            else: out += b(label, cls, ic, s)
        return out
    head_ = '<div class="cs-st"><span></span><div class="cs-row" style="color:var(--text-2);font-size:var(--fs-xs);font-family:var(--font-mono);gap:0">' + "".join(f'<span style="width:118px">{n}</span>' for n, _ in states) + '</div></div>'
    matrix = head_ + "".join(f'<div class="cs-st"><span>{n}</span><div class="cs-row">{row(c, l, ic)}</div></div>' for n, c, l, ic in [("primary", "btn--primary", "Deploy", "rocket"), ("secondary", "btn--secondary", "Restart", None), ("ghost", "btn--ghost", "Cancel", None), ("danger", "btn--danger", "Delete", "trash-2"), ("danger solid", "btn--danger-solid", "Delete", "trash-2")])
    sizes = (st("sm", b("Small", "btn--secondary btn--sm") + b("Primary", "btn--primary btn--sm", "plus")) + st("md", b("Medium", "btn--secondary") + b("Primary", "btn--primary", "plus")) + st("lg", b("Large", "btn--secondary btn--lg") + b("Sign in", "btn--primary btn--lg", "arrow-right"))
             + st("icon", f'<button class="btn btn--icon btn--secondary" type="button" aria-label="More">{icon("ellipsis")}</button><button class="btn btn--icon btn--ghost" type="button" aria-label="Refresh">{icon("refresh-cw")}</button><button class="icon-btn" type="button" aria-label="Toggle theme">{icon("sun")}</button><button class="btn btn--icon btn--secondary" type="button" aria-busy="true" aria-label="Loading"><span></span></button>'))
    return sec("buttons", "Buttons", f'<div class="cs-cell" style="overflow-x:auto"><div class="cs-col" style="min-width:760px">{matrix}</div></div><div class="cs-grid"><div class="cs-cell"><h3>Sizes</h3><div class="cs-col">{sizes}</div></div></div>')

def forms():
    def inp(state, **kw):
        a = f' data-state="{state}"' if state in ("hover", "focus") else ""
        extra = " ".join(f'{k}' for k in kw)
        return f'<input class="input" value="{kw.get("value", "shop.acme.dev")}"{a}{" aria-invalid=true" if state == "invalid" else ""}{" disabled" if state == "dis" else ""}{" readonly" if state == "ro" else ""} aria-label="{state}">'
    inputs = "".join(st(n, inp(s)) for n, s in [("default", ""), ("hover", "hover"), ("focus", "focus"), ("invalid", "invalid"), ("disabled", "dis"), ("readonly", "ro")])
    chk = (st("unchecked", '<label class="check"><input type="checkbox"> Keep me signed in</label>') + st("checked", '<label class="check"><input type="checkbox" checked> Production</label>') + st("mixed", '<label class="check"><input type="checkbox" id="cs-mixed" data-mixed> 2 of 3 selected</label>')
           + st("focus", '<label class="check"><input type="checkbox" checked data-state="focus"> Focus</label>') + st("disabled", '<label class="check"><input type="checkbox" disabled> Disabled</label><label class="check"><input type="checkbox" checked disabled> On</label>'))
    sw = (st("off", '<label class="switch"><input type="checkbox" switch role="switch"> Expose publicly</label>') + st("on", '<label class="switch"><input type="checkbox" switch role="switch" checked> Require TLS</label>') + st("focus", '<label class="switch"><input type="checkbox" switch role="switch" checked data-state="focus"> Focus</label>')
          + st("disabled", '<label class="switch"><input type="checkbox" switch role="switch" disabled> Off</label><label class="switch"><input type="checkbox" switch role="switch" checked disabled> On</label>') + '<label class="switch switch--row"><input type="checkbox" switch role="switch" checked><span><b>Automatic backups</b><small>Runs on atlas, kept for 7 days.</small></span></label>')
    fld = (field("Domain", '<input id="cs-dom" class="input" value="shop.acme.dev" aria-describedby="cs-dom-h">', "DNS must point at atlas (10.0.0.12).", fid="cs-dom") + field("Project name", '<input id="cs-pn" class="input" value="Store Front" aria-invalid="true" aria-describedby="cs-pn-e">', "", "Use lowercase letters, numbers and dashes.", fid="cs-pn", req=True))
    other = (field("Server", select("cs-sel", ["atlas · 14% CPU · 20 GB free", "nova · offline"]), fid="cs-sel") + field("System prompt", '<textarea id="cs-ta" class="input" rows="3">You are a concise senior infrastructure engineer.</textarea>', fid="cs-ta")
             + f'<div class="field field--search"><label class="sr-only" for="cs-s">Search</label>{icon("search")}<input id="cs-s" class="input" type="search" placeholder="Search templates…"></div>'
             + field("Password", f'<span class="pw"><input id="cs-pw" class="input" type="password" value="hunter2hunter2"><button class="pw-btn" type="button" data-reveal="cs-pw" aria-label="Show password" aria-pressed="false">{icon("eye")}</button></span>', fid="cs-pw")
             + copyfield("cs-cp", "Host", "postgres-16.storefront.internal") + f'<label class="field"><span class="field-l">Temperature <output>0.7</output></span><input type="range" class="range" min="0" max="2" step="0.1" value="0.7"></label>')
    return sec("forms", "Inputs and form controls", f'<div class="cs-grid"><div class="cs-cell"><h3>Text input · states</h3><div class="cs-col">{inputs}</div></div><div class="cs-cell"><h3>Checkbox</h3><div class="cs-col">{chk}</div></div><div class="cs-cell"><h3>Switch</h3><div class="cs-col">{sw}</div></div>'
               f'<div class="cs-cell"><h3>Field with help and error</h3><div class="cs-col">{fld}</div></div><div class="cs-cell"><h3>Select, textarea, search, password, copy, range</h3><div class="cs-col">{other}</div></div></div>')

def status():
    sts = ["running", "building", "deploying", "failed", "stopped", "pending", "unreachable"]
    dots = "".join(f'<div class="cs-st"><span>{s}</span><div class="cs-row"><span class="sdot" data-status="{s}" role="img" aria-label="{s}"></span>{badge(s)}</div></div>' for s in sts)
    prog = '<div class="progress" role="progressbar" aria-label="Build" aria-valuenow="64" aria-valuemin="0" aria-valuemax="100"><span style="width:64%"></span></div>'
    chips = '<div class="chips"><span class="chip">analytics</span><span class="chip">v2.9.1</span><button class="chip chip--btn" type="button" aria-pressed="true">AI<span class="chip-n">37</span></button><button class="chip chip--btn" type="button" aria-pressed="false">Database<span class="chip-n">35</span></button></div>'
    banners = "".join(f'<p class="banner banner--{k}">{icon(ic)}<span><b>{t}</b> {m}</span></p>' for k, ic, t, m in [("err", "circle-alert", "prometheus exited 137.", "Raise the memory limit."), ("warn", "triangle-alert", "Image is 2.4 MB.", "Consider compressing."), ("info", "info", "Backups run at 03:00.", "Next in 14 h."), ("ok", "circle-check", "Deployment #148 live.", "shop.acme.dev")])
    return sec("status", "Status, badge, chip, banner", f'<div class="cs-grid"><div class="cs-cell"><h3>Status dot + badge (colour, shape and text)</h3><div class="cs-col">{dots}</div></div><div class="cs-cell"><h3>Chips · progress</h3>{chips}{prog}<h3 style="margin-top:var(--s-4)">Kbd</h3><div class="cs-row"><kbd class="kbd">Ctrl K</kbd><kbd class="kbd">Esc</kbd><kbd class="kbd">↑</kbd><kbd class="kbd">↓</kbd></div></div><div class="cs-cell"><h3>Banner</h3><div class="cs-col">{banners}</div></div></div>')

def cards():
    r = RES
    res = f'<ul class="res-grid res-grid--demo">{res_item(r[0], 0)}{res_item(r[1], 1)}{res_item(r[4], 2)}</ul>'
    hov = f'<ul class="res-grid res-grid--demo"><li class="res" data-status="running" data-id="hov" style="--i:0"><a class="res-link" href="#" data-state="hover">{logo("postgres", 36).replace("<img", "<img")}<span class="res-main"><span class="res-kind">{icon("database")}Database</span><h3 class="res-name">hover state</h3><span class="res-sub">row and card hover</span></span>{badge("running")}</a></li></ul>'
    tpl = '<ul class="tpl-grid" style="grid-template-columns:1fr">' + "".join(f'<li class="tpl"><button class="tpl-btn" type="button"{" data-state=hover" if i else ""}><span class="tpl-logo">{logo(lg, 44)}</span><span class="tpl-main"><h3 class="tpl-name">{t}</h3><span class="tpl-desc">{d}</span></span><span class="tpl-foot"><span class="chip">{c}</span><span class="tpl-ver">v{v}</span></span></button></li>' for i, (t, lg, d, c, v) in enumerate(TEMPLATES[:2])) + '</ul>'
    gen = f'<section class="card"><header class="card-h"><h2 class="sec-h">Card</h2><span class="card-sub">header · body</span></header><div class="card-b"><p>Generic surface used for settings, credentials, tables and forms.</p>{btn("Action", None, "btn--secondary btn--sm")}</div></section>'
    return sec("cards", "Cards", f'<div class="cs-grid cs-grid--w"><div class="cs-cell" style="grid-column:1/-1"><h3>Resource card · running, building, failed · hover (last)</h3>{res}{hov}</div><div class="cs-cell"><h3>Template tile · default, hover</h3>{tpl}</div><div class="cs-cell"><h3>Generic card</h3>{gen}</div></div>')

def nav_():
    tb = tabs("cs-t1", [("a", "production", 6), ("b", "staging", 2), ("c", "preview", 0)]).replace('<input type="radio" name="cs-t1"', '<input type="radio" name="cs-t1" data-x')
    line = '<ul class="tabs-line"><li><a class="tab" href="#" aria-current="page">Deployments</a></li><li><a class="tab" href="#">Logs</a></li><li><a class="tab" href="#">Environment</a></li></ul>'
    rg = tabs("cs-t2", [("1h", "1h", ""), ("24h", "24h", ""), ("7d", "7d", "")], "tabs--range")
    bc = crumbs("environment") + '<p class="field-h">The last crumb is an environment switcher (menu button).</p>'
    sw = f'<div class="cs-row"><button class="team" type="button" commandfor="team-menu" command="toggle-popover" aria-haspopup="menu" aria-label="Team: acme. Switch team"><span class="avatar" aria-hidden="true">A</span><span class="team-name">acme</span>{icon("chevrons-up-down")}</button><span class="field-h">Team switcher. Opens a native popover menu.</span></div>'
    return sec("navigation", "Tabs, breadcrumb, switcher", f'<div class="cs-grid"><div class="cs-cell"><h3>Tabs (native radios, arrow keys)</h3><div class="tabset">{tb}</div><h3>Section links</h3>{line}<h3>Range</h3><div class="tabset">{rg}</div></div><div class="cs-cell"><h3>Breadcrumb + switcher</h3>{bc}{sw}</div></div>')

def table_():
    rows = "".join(f'<tr{" data-state=hover" if i == 1 else ""}><td data-label="Member"><div class="who-cell"><span class="avatar avatar--lg" data-h="{i+1}" aria-hidden="true">{ini}</span><div><b>{n}</b><small>{mail}</small></div></div></td><td data-label="Role">{role}</td><td data-label="Status">{badge("running" if i != 3 else "stopped", "Active" if i != 3 else "Invited")}</td><td class="tbl-act"><button class="btn btn--icon btn--sm btn--ghost" type="button" aria-label="Actions for {n}">{icon("ellipsis")}</button></td></tr>'
                   for i, (n, mail, role, last, ini) in enumerate(MEMBERS[:4]))
    return sec("table", "Table", f'<div class="cs-cell"><h3>Data table · row hover (2nd row) · stacks to rows at 390 px</h3><div class="card"><div class="tbl-wrap"><table class="tbl"><caption class="sr-only">Members</caption><thead><tr><th scope="col">Member</th><th scope="col">Role</th><th scope="col">Status</th><th scope="col"><span class="sr-only">Actions</span></th></tr></thead><tbody>{rows}</tbody></table></div></div></div>')

def overlays():
    dlg = ('<div class="dlg dlg--confirm cs-inline" role="group" aria-labelledby="cs-d1"><div class="dlg-form"><header class="dlg-head"><span class="dlg-ic dlg-ic--err">' + icon("triangle-alert") + '</span><div><h2 id="cs-d1">Delete redis-cache?</h2></div></header>'
           '<div class="dlg-body"><p>This removes the container, its volume (38 MB) and all backups. It cannot be undone.</p></div><footer class="dlg-foot"><button class="btn btn--secondary" type="button">Cancel</button><button class="btn btn--danger-solid" type="button">' + icon("trash-2") + '<span>Delete</span></button></footer></div></div>')
    mn = ('<div class="menu cs-inline" role="group"><div><p class="menu-h">Create resource</p>' + "".join(f'<a class="menu-item" href="#"{" data-state=hover" if i == 1 else ""}>{icon(ic)}<span><b>{t}</b><small>{s}</small></span></a>' for i, (ic, t, s) in enumerate([("git-branch", "Git repository", "GitHub, GitLab or any Git URL"), ("box", "Docker image", "From a registry, with auth"), ("database", "Database", "Postgres, MySQL, MongoDB, Redis")]))
          + f'<hr><button class="menu-item menu-item--danger" type="button">{icon("trash-2")}<span><b>Delete…</b></span></button></div></div>')
    tp = f'<div class="cs-row" style="padding:var(--s-4) 0"><button class="btn btn--icon btn--secondary" type="button" aria-label="Copy host" interestfor="tip-copy">{icon("copy")}</button><span class="tip cs-inline" style="display:inline-block!important" role="tooltip">Copy</span><span class="field-h">Hover or focus the button. Native <code>popover=hint</code> + <code>interestfor</code>.</span></div>'
    ts = (f'<div class="toast cs-inline" style="display:flex!important;animation:none">{icon("circle-check")}<span>Deployment #148 succeeded: shop.acme.dev is live</span></div>'
          f'<div class="toast toast--err cs-inline" style="display:flex!important;animation:none">{icon("circle-alert")}<span>Could not reach atlas. Retrying in 5 s…</span></div>')
    pal = ('<div class="dlg dlg--cmd cs-inline" role="group" aria-label="Command palette"><div class="cmd"><div class="cmd-in">' + icon("search") + '<span style="flex:1;color:var(--text-3)">ob</span><kbd class="kbd">Esc</kbd></div><p class="cmd-h">Results</p><ul class="cmd-list">'
           f'<li><a class="cmd-item" href="#" aria-selected="true">{icon("layers")}<span>observability · Services</span></a></li><li><a class="cmd-item" href="#">{icon("scroll-text")}<span>observability · Logs</span><kbd class="kbd">G L</kbd></a></li></ul><p class="cmd-foot"><span><kbd class="kbd">↑</kbd><kbd class="kbd">↓</kbd> move</span><span><kbd class="kbd">Enter</kbd> open</span></p></div></div>')
    live = (f'<div class="cs-row">{b("Open dialog", "btn--secondary", None, "", "commandfor=cs-dlg command=show-modal")}{b("Open menu", "btn--secondary", "ellipsis", "", "commandfor=cs-menu command=toggle-popover aria-haspopup=menu")}'
            f'{b("Palette", "btn--secondary", "command", "", "commandfor=cmdk command=show-modal")}{b("Show toast", "btn--secondary", "bell", "", "data-toast=Deployment queued")}</div><p class="field-h">These four open the real components: native dialog, popover, palette, toast.</p>')
    return sec("overlays", "Dialog, menu, tooltip, toast, command palette",
               f'<div class="cs-grid cs-grid--w"><div class="cs-cell"><h3>Dialog (confirm · destructive, Cancel has autofocus)</h3>{dlg}</div><div class="cs-cell"><h3>Dropdown menu (hover on 2nd item)</h3>{mn}</div>'
               f'<div class="cs-cell"><h3>Tooltip</h3>{tp}<h3>Toast · success, error</h3><div class="cs-col">{ts}</div></div><div class="cs-cell"><h3>Command palette</h3>{pal}</div><div class="cs-cell"><h3>Try them</h3>{live}</div></div>')

def feedback():
    sk = ('<div class="cs-col"><span class="sk" style="height:1.2em;width:50%"></span><span class="sk" style="height:.9em;width:90%"></span><span class="sk" style="height:.9em;width:70%"></span></div>')
    sk2 = f'<ul class="res-grid res-grid--demo skeleton-demo">{res_skel(2)}</ul>'
    v = VOICE
    emp = f'<div class="empty"><span class="empty-ic">{icon("box")}</span><h2>Nothing running yet</h2><p>Create the first resource in this environment.</p><div class="empty-act">{abtn("Create resource", "plus", "btn--primary", "create.html")}{abtn("Browse templates", "package", "btn--secondary", "templates.html")}</div></div>'
    err = f'<p class="banner banner--err" role="alert">{icon("circle-alert")}<span><b>Could not load metrics from atlas.</b> The request timed out after 10 s.</span><button class="btn btn--secondary btn--sm" type="button">Retry</button></p>'
    return sec("feedback", "Skeleton, empty state, error pattern", f'<div class="cs-grid cs-grid--w"><div class="cs-cell"><h3>Skeleton (shimmer; static with reduced motion)</h3>{sk}{sk2}</div><div class="cs-cell"><h3>Empty state</h3>{emp}</div><div class="cs-cell"><h3>Error (inline, retryable)</h3>{err}<h3>Loading button · busy form</h3><div class="cs-row">{b("Back up now", "btn--secondary", "archive", "", "aria-busy=true")}{b("Saving…", "btn--primary", "save", "", "aria-busy=true")}</div></div></div>')

def data_():
    lines = [("00:21", "STEP", "[6/7] RUN pnpm build"), ("00:22", "INFO", "▲ Next.js 15.3 · Creating an optimized production build"), ("00:31", "INFO", "✓ Compiled successfully in 8.7s"), ("00:32", "WARN", "Image `/hero.png` is 2.4 MB · consider compressing"), ("00:33", "ERR", "Error: ENOSPC: no space left on device, write"), ("00:38", "OK", "Health check passed (3/3)")]
    ll = "".join(f'<span class="log-line"><span class="ts">{t}</span><span class="lv" data-lv="{l}">{l}</span><span class="tx">{esc(x)}</span></span>' for t, l, x in lines)
    flt = '<div class="chips">' + "".join(f'<button class="chip chip--btn" type="button" aria-pressed="{"true" if i == 0 else "false"}">{n}</button>' for i, n in enumerate(["All", "Info", "Warn", "Error"])) + '</div>'
    lg = f'<div class="build-log"><header class="log-head"><h2 class="sec-h">Build log <span class="log-id">#148</span></h2><span class="live-pill"><span class="dot" aria-hidden="true"></span>Live</span><label class="check follow"><input type="checkbox" checked><span>Follow</span></label>{btn("Copy", "copy", "btn--ghost btn--sm")}</header><div class="card-b" style="padding-block:var(--s-3);border-bottom:var(--bw) solid var(--border)">{flt}</div><pre class="log log--demo" tabindex="0" aria-label="Log lines">{ll}</pre></div>'
    ch = chart("area", metric_series(METRICS[0], "24h", 0), 100, METRICS[0]["fmt"], RANGES["24h"][1], label="CPU over 24 hours")
    ch2 = chart("line", metric_series(METRICS[3], "1h", 3), 200, METRICS[3]["fmt"], RANGES["1h"][1], label="Network over 1 hour") .replace('class="chart"', 'class="chart" style="--c:var(--chart-3);--c2:var(--chart-1)"')
    legend = '<p class="legend"><span><i style="--c:var(--chart-3)"></i>rx</span><span><i style="--c:var(--chart-1)"></i>tx</span></p>'
    chart_ = f'<div class="metric" style="--c:var(--chart-1)"><header class="metric-head"><h2 class="metric-name">{icon("cpu")}CPU</h2><p class="metric-val">14<span class="u">%</span></p></header><p class="metric-sub">Ryzen 7 9850X3D · 8c / 16t</p>{ch}<p class="metric-stats"><span>avg 17%</span><span>max 62%</span></p></div>'
    tiles = ('<div class="cs-row">' + "".join(tile(n, s) for n, s in [("nextdotjs", 56), ("postgres", 40), ("grafana", 32), ("gotify", 24)]) + "".join(tile(n, 40) for n in ["prometheus", "loki", "my-app"]) + '</div><p class="field-h">Brand logo when one exists; otherwise a two-letter monogram on a tinted tile.</p>')
    av = ('<div class="cs-row"><span class="avatar" aria-hidden="true">A</span><span class="avatar avatar--lg" data-h="1" aria-hidden="true">M</span><span class="avatar avatar--lg" data-h="3" aria-hidden="true">T</span><div class="avatars" aria-label="3 members">'
          '<span class="avatar avatar--lg" data-h="2" aria-hidden="true">P</span><span class="avatar avatar--lg" data-h="4" aria-hidden="true">C</span><span class="avatar avatar--lg" data-h="5" aria-hidden="true">+2</span></div></div>')
    return sec("data", "Log viewer, chart, logo tile, avatar", f'<div class="cs-grid cs-grid--w"><div class="cs-cell" style="grid-column:1/-1">{lg}</div><div class="cs-cell"><h3>Chart · area with axis labels</h3>{chart_}</div><div class="cs-cell"><h3>Chart · bars with legend, sparkline</h3>{ch2}{legend}<div style="width:120px">{spark(5)}</div></div><div class="cs-cell"><h3>Logo tile + monogram fallback</h3>{tiles}</div><div class="cs-cell"><h3>Avatar</h3>{av}</div></div>')

def components(cfg):
    nav = '<nav class="cs-nav" aria-label="Sections">' + "".join(f'<a class="chip chip--link" href="#{i}">{t}</a>' for i, t in [("buttons", "Buttons"), ("forms", "Inputs"), ("status", "Status"), ("cards", "Cards"), ("navigation", "Tabs & crumbs"), ("table", "Table"), ("overlays", "Overlays"), ("feedback", "Feedback"), ("data", "Data")]) + "</nav>"
    top = (f'<header class="page-head"><div class="page-title"><p class="eyebrow">podkit · {cfg["name"]}</p><h1>Component sheet</h1><p class="lede">Every component and state. Hover, focus and active are forced so they can be compared side by side; the real states work as well.</p></div>'
           f'<div class="page-actions">{abtn("Projects", "arrow-left", "btn--ghost", "projects.html")}{theme_btn()}</div></header>{nav}')
    body = top + buttons() + forms() + status() + cards() + nav_() + table_() + overlays() + feedback() + data_()
    dlg = ('<dialog id="cs-dlg" class="dlg dlg--confirm" aria-labelledby="cs-dlg-t" closedby="any"><form method="dialog" class="dlg-form"><header class="dlg-head"><span class="dlg-ic dlg-ic--err">' + icon("triangle-alert") + '</span><div><h2 id="cs-dlg-t">Delete redis-cache?</h2></div></header>'
           '<div class="dlg-body"><p>Native dialog. Esc, the backdrop and Cancel close it and focus returns to the button that opened it.</p></div><footer class="dlg-foot"><button class="btn btn--secondary" type="submit" value="cancel" autofocus>Cancel</button><button class="btn btn--danger-solid" type="submit" value="ok">' + icon("trash-2") + '<span>Delete</span></button></footer></form></dialog>')
    mn = menu("cs-menu", "Actions", [("copy", "Duplicate", "", ""), ("folder-open", "Move to staging", "", ""), "hr", ("trash-2", "Delete…", "danger", "")])
    html = f'<div class="cs">{body}</div>'
    return page(cfg, "components", "Component sheet", html, extra=cmdk() + team_menu() + dlg + mn + tip("tip-copy", "Copy") + tip("tip-reveal", "Show / hide"), bare=True, head_extra='<link rel="stylesheet" href="states.css">\n')
