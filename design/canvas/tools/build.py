#!/usr/bin/env python3
"""Generates the canvas mockups from one small data model.

It is also the reference for the server side of the real feature (I2): a layered layout, edge routing and group frames,
all computed from the resource graph with no client-side measuring. Port it to Rust in `server/src/ui/canvas_layout.rs`.

	python3 design/canvas/tools/build.py          # writes canvas.html, canvas-wire.html, canvas-empty.html, canvas-states.html
"""
import html, pathlib, re

HERE = pathlib.Path(__file__).resolve().parent
OUT = HERE.parent
Q = OUT.parent / 'finalists' / '02-quiet-linear'
QP = '../finalists/02-quiet-linear/'

# ---- geometry (world pixels at zoom 1) ----
W, H = 208, 92          # node
RG, CG = 16, 112        # row gap, column gap (the column gap holds the edge label)
PE, NP = 16, 14         # env padding, network padding
EH, NH = 44, 30         # env header, network header
BG, EG = 20, 28         # gap between bands, gap between envs

ICON = {'git': 'i-git-branch', 'image': 'i-box', 'db': 'i-database', 'stack': 'i-layers', 'model': 'i-brain', 'template': 'i-package'}
KIND = {'git': 'Git app', 'image': 'Image app', 'db': 'Database', 'stack': 'Compose stack', 'model': 'AI model', 'template': 'Template'}
STATUS = {'running': 'Running', 'building': 'Building', 'failed': 'Failed', 'stopped': 'Stopped', 'deploying': 'Deploying'}
SEV = {'critical': 'Critical', 'high': 'High', 'medium': 'Medium', 'low': 'Low'}

RES = [
	dict(id='api', env='production', kind='git', name='storefront-api', sub='Git · main', status='running', sev=('critical', 1), nets=[], href='app.html', v=None),
	dict(id='obs', env='production', kind='stack', name='observability', sub='Stack · 4 services', status='failed', sev=None, nets=[], href='stack.html', v=None),
	dict(id='qwen', env='production', kind='model', name='qwen3-4b', sub='Model · llama.cpp', status='running', sev=None, nets=[], href='playground.html', v='OPENAI_BASE_URL'),
	dict(id='worker', env='production', kind='image', name='checkout-worker', sub='Image · 2.14.1', status='building', sev=('high', 2), nets=['env-prod'], href='app.html', v=None, progress=64),
	dict(id='pg', env='production', kind='db', name='postgres-16', sub='PostgreSQL 16.4', status='running', sev=None, nets=['env-prod'], href='database.html', v='DATABASE_URL'),
	dict(id='redis', env='production', kind='db', name='redis-cache', sub='Redis 7.4', status='running', sev=None, nets=['env-prod'], href='database.html', v='REDIS_URL'),
	dict(id='sapi', env='staging', kind='git', name='storefront-api', sub='Git · develop', status='running', sev=('medium', 3), nets=['env-staging'], href='app.html', v=None),
	dict(id='spg', env='staging', kind='db', name='postgres-16', sub='PostgreSQL 16.4', status='running', sev=None, nets=['env-staging'], href='database.html', v='DATABASE_URL'),
]
EDGES = [  # consumer -> provider
	('api', 'qwen', 'gateway', 'gateway'),
	('worker', 'pg', 'env', 'DATABASE_URL'),
	('worker', 'redis', 'env', 'REDIS_URL'),
	('sapi', 'spg', 'env', 'DATABASE_URL'),
]
NETS = {'env-prod': ('production', 'auto'), 'env-staging': ('staging', 'auto')}
ENVS = ['production', 'staging']
byid = {r['id']: r for r in RES}
e = html.escape


def layout():
	"""Layered layout. Layer = longest path from a source. Rows are banded per network so frames never overlap a non-member."""
	x0 = 0
	for env in ENVS:
		ns = [r for r in RES if r['env'] == env]
		layer = {}
		def L(i):
			if i in layer: return layer[i]
			layer[i] = 0
			ins = [a for a, b, *_ in EDGES if b == i and byid[a]['env'] == env]
			layer[i] = 1 + max([L(a) for a in ins]) if ins else 0
			return layer[i]
		for r in ns: L(r['id'])
		ncols = max(layer.values()) + 1
		bands = [None] + sorted({n for r in ns for n in r['nets'][:1]})
		y = EH
		rowof = {}
		frames = []
		for b in bands:
			mem = [r for r in ns if (r['nets'][:1] or [None])[0] == b]
			if not mem: continue
			rows = {}
			for c in range(ncols):
				col = [r for r in mem if layer[r['id']] == c]
				col.sort(key=lambda r: (sum(rowof[a] for a, bb, *_ in EDGES if bb == r['id'] and a in rowof) / max(1, sum(1 for a, bb, *_ in EDGES if bb == r['id'] and a in rowof)), r['id']))
				for i, r in enumerate(col): rowof[r['id']] = i
				rows[c] = len(col)
			nrows = max(rows.values())
			top = y + (NH if b else 0)
			for r in mem:
				r['x'] = x0 + PE + NP + layer[r['id']] * (W + CG)
				r['y'] = top + rowof[r['id']] * (H + RG)
				r['layer'] = layer[r['id']]
			cmin = min(layer[r['id']] for r in mem); cmax = max(layer[r['id']] for r in mem)
			bh = nrows * H + (nrows - 1) * RG
			if b:
				frames.append(dict(net=b, x=x0 + PE + cmin * (W + CG), y=y, w=(cmax - cmin) * (W + CG) + W + 2 * NP, h=NH + bh + NP))
				y += NH + bh + NP + BG
			else:
				y += bh + BG
		ew = 2 * PE + 2 * NP + ncols * W + (ncols - 1) * CG
		for f in frames: f['env'] = env
		byenv = dict(env=env, x=x0, y=0, w=ew, h=y - BG + PE, frames=frames)
		LAY.append(byenv)
		x0 += ew + EG
	return x0 - EG, max(l['h'] for l in LAY)


LAY = []
WW, WH = layout()


def ic(i, cls=''):
	return f'<svg class="ic {cls}" aria-hidden="true"><use href="#{i}"/></svg>'


def counts(env):
	ns = [r for r in RES if r['env'] == env]
	c = {}
	for r in ns: c[r['status']] = c.get(r['status'], 0) + 1
	return len(ns), ' · '.join(f'{v} {k}' for k, v in c.items())


def edge_geom(a, b):
	x0, y0, x1, y1 = a['x'] + W, a['y'] + H // 2, b['x'], b['y'] + H // 2
	dx = max(40, (x1 - x0) / 2)
	d = f'M{x0} {y0}L{x1} {y1}' if y0 == y1 else f'M{x0} {y0}C{x0 + dx:g} {y0} {x1 - dx:g} {y1} {x1} {y1}'
	return d, (x0 + x1) / 2, (y0 + y1) / 2


def sev_badge(r, full=True):
	if not r.get('sev'): return ''
	k, n = r['sev']
	return f'<span class="sev" data-sev="{k}" title="Image scan: {n} {SEV[k].lower()} finding{"s" if n != 1 else ""}"><span class="cv-sev-w">{SEV[k]}</span><span class="cv-sev-n">{n}</span><span class="sr-only"> scan findings</span></span>'


def badge(st):
	return f'<span class="badge" data-status="{st}"><span class="dot" aria-hidden="true"></span><span class="lbl">{STATUS[st]}</span></span>'


def node(r, extra='', state=None, tabular=True):
	prog = f'<span class="cv-prog" style="--p:{r["progress"]}%" role="progressbar" aria-label="Build progress" aria-valuenow="{r["progress"]}" aria-valuemin="0" aria-valuemax="100"></span>' if r.get('progress') and r['status'] == 'building' else ''
	attrs = f'id="n-{r["id"]}" data-id="{r["id"]}" data-name="{e(r["name"])}" data-kind="{r["kind"]}" data-env="{r["env"]}" data-nets="{" ".join(r["nets"])}" data-status="{r["status"]}" data-href="{r["href"]}" data-x="{r.get("x", 0)}" data-y="{r.get("y", 0)}"' + (f' data-var="{r["v"]}"' if r.get('v') else '')
	st = f' data-state="{state}"' if state else ''
	pos = f'style="--x:{r.get("x", 0)}px;--y:{r.get("y", 0)}px"'
	desc = f'{KIND[r["kind"]]}, {STATUS[r["status"]].lower()}' + (f', {r["sev"][1]} {r["sev"][0]} scan finding{"s" if r["sev"][1] != 1 else ""}' if r.get('sev') else '')
	return (f'<li class="cv-node" {attrs}{st} {pos}><div class="cv-head"><span class="cv-ic">{ic(ICON[r["kind"]])}</span>'
		f'<a class="cv-open" href="{r["href"]}" aria-describedby="d-{r["id"]}">{e(r["name"])}</a>'
		f'<button class="btn btn--icon btn--sm btn--ghost cv-more" type="button" tabindex="-1" aria-label="Actions for {e(r["name"])}" aria-haspopup="menu">{ic("c-ellipsis")}</button></div>'
		f'<span class="cv-sub" id="d-{r["id"]}"><span class="sr-only">{desc}. </span>{e(r["sub"])}</span>'
		f'<div class="cv-foot">{badge(r["status"])}{sev_badge(r)}</div>{prog}'
		f'<span class="cv-port cv-port--in" aria-hidden="true"></span><span class="cv-port cv-port--out" aria-hidden="true"></span></li>')


SPRITE = ('<svg width="0" height="0" style="position:absolute" aria-hidden="true" focusable="false"><defs>'
	'<symbol id="c-minus" viewBox="0 0 24 24"><path d="M5 12h14"/></symbol>'
	'<symbol id="c-fit" viewBox="0 0 24 24"><path d="M8 3H5a2 2 0 0 0-2 2v3"/><path d="M21 8V5a2 2 0 0 0-2-2h-3"/><path d="M3 16v3a2 2 0 0 0 2 2h3"/><path d="M16 21h3a2 2 0 0 0 2-2v-3"/></symbol>'
	'<symbol id="c-list" viewBox="0 0 24 24"><path d="M3 12h.01"/><path d="M3 18h.01"/><path d="M3 6h.01"/><path d="M8 12h13"/><path d="M8 18h13"/><path d="M8 6h13"/></symbol>'
	'<symbol id="c-ellipsis" viewBox="0 0 24 24"><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/><circle cx="5" cy="12" r="1"/></symbol>'
	'<symbol id="c-plug" viewBox="0 0 24 24"><path d="M12 22v-5"/><path d="M9 8V2"/><path d="M15 8V2"/><path d="M18 8v5a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4V8Z"/></symbol>'
	'<symbol id="c-network" viewBox="0 0 24 24"><rect x="16" y="16" width="6" height="6" rx="1"/><rect x="2" y="16" width="6" height="6" rx="1"/><rect x="9" y="2" width="6" height="6" rx="1"/><path d="M5 16v-3a1 1 0 0 1 1-1h12a1 1 0 0 1 1 1v3"/><path d="M12 12V8"/></symbol>'
	'<symbol id="c-ban" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><path d="m4.9 4.9 14.2 14.2"/></symbol>'
	'<symbol id="c-unlink" viewBox="0 0 24 24"><path d="m18.84 12.25 1.72-1.71h-.02a5.004 5.004 0 0 0-.12-7.07 5.006 5.006 0 0 0-6.95 0l-1.72 1.71"/><path d="m5.17 11.75-1.71 1.71a5.004 5.004 0 0 0 .12 7.07 5.006 5.006 0 0 0 6.95 0l1.71-1.71"/><path d="M8 2v3"/><path d="M2 8h3"/><path d="M16 19v3"/><path d="M19 16h3"/></symbol>'
	'<symbol id="c-zoom-focus" viewBox="0 0 24 24"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/><path d="M11 8v6"/><path d="M8 11h6"/></symbol>'
	'<symbol id="c-keyboard" viewBox="0 0 24 24"><path d="M10 8h.01"/><path d="M12 12h.01"/><path d="M14 8h.01"/><path d="M16 12h.01"/><path d="M18 8h.01"/><path d="M6 8h.01"/><path d="M7 16h10"/><path d="M8 12h.01"/><rect width="20" height="16" x="2" y="4" rx="2"/></symbol>'
	'</defs></svg>')


def shell(title, body, demo=''):
	src = (Q / 'environment.html').read_text()
	lines = src.split('\n')
	head = '\n'.join(lines[:18])
	sprite = lines[20]
	l22 = lines[21]
	pre = l22[:l22.index('<main id="main"')]
	tail = src[src.index('</main>'):]
	def fix(s):
		s = re.sub(r'(href|src)="((?:[a-z0-9-]+)\.(?:html|css|js))((?:#[^"]*)?)"', lambda m: f'{m.group(1)}="{QP}{m.group(2)}{m.group(3)}"', s)
		return s.replace('src="../.assets/', f'src="{QP}../.assets/')
	head = re.sub(r'<title>.*?</title>', f'<title>{title} · podkit · Quiet</title>', head)
	head = fix(head)
	head = head.replace(f'<link rel="stylesheet" href="{QP}mobile.css">', f'<link rel="stylesheet" href="canvas.css">\n<link rel="stylesheet" href="{QP}mobile.css">')
	head = head.replace(f'<script src="{QP}sim.js" defer></script>', f'<script src="{QP}sim.js" defer></script>' + ('\n<script src="canvas-demo.js" defer></script>' if body.count('id="cv-view"') and 'cv-view--empty' not in body else ''))
	pre = re.sub(r'<li aria-current="page"><button class="crumb-sw".*?</button></li>', '<li aria-current="page">Canvas</li>', pre, flags=re.S)
	pre = pre.replace('<a href="environment.html">storefront</a>', f'<a href="{QP}environment.html">storefront</a>')
	return (head + '\n<body data-screen="canvas"' + (f' data-demo="{demo}"' if demo else '') + '>\n<a class="skip" href="#main">Skip to content</a>\n' + sprite + '\n' + SPRITE + '\n' + fix(pre)
		+ '<main id="main" class="page page--canvas">' + body + fix(tail).replace('<body>', ''))


def page_head(lede, create=True):
	return ('<header class="page-head"><div class="page-title"><p class="eyebrow">Project</p><h1>storefront</h1>' f'<p class="lede">{lede}</p></div>'
		'<div class="page-actions"><a class="btn btn--ghost" href="' + QP + 'environment.html">' + ic('i-layout-grid') + '<span>Resource list</span></a>'
		'<button class="btn btn--primary" type="button" commandfor="create-menu" command="toggle-popover" aria-haspopup="menu">' + ic('i-plus') + '<span>Create resource</span></button></div></header>')


def toolbar(disabled=False):
	dis = ' disabled' if disabled else ''
	return ('<div class="cv-bar tabset" role="toolbar" aria-label="Canvas tools">'
		'<fieldset class="tabs tabs--range cv-views"><legend class="sr-only">View</legend>'
		'<input type="radio" name="cvv" id="cv-v-canvas" value="canvas" checked><label for="cv-v-canvas" class="tab">Canvas</label>'
		'<input type="radio" name="cvv" id="cv-v-list" value="list"><label for="cv-v-list" class="tab">List</label></fieldset>'
		'<span class="cv-tools">'
		f'<button class="chip chip--btn" type="button" aria-pressed="true" data-cv-toggle="labels"{dis}>Link labels</button>'
		f'<button class="chip chip--btn" type="button" aria-pressed="true" data-cv-toggle="nets"{dis}>Networks</button>'
		f'<button class="btn btn--sm btn--secondary" type="button" aria-pressed="false" data-cv-connect{dis}>{ic("c-plug")}<span>Connect</span></button>'
		'<span class="cv-sp"></span>'
		'<span class="cv-zoom" role="group" aria-label="Zoom">'
		f'<button class="btn btn--icon btn--sm btn--ghost" type="button" data-cv-zoom="out" aria-label="Zoom out"{dis}>{ic("c-minus")}</button>'
		f'<output id="cv-zoom-out" aria-label="Zoom level">100%</output>'
		f'<button class="btn btn--icon btn--sm btn--ghost" type="button" data-cv-zoom="in" aria-label="Zoom in"{dis}>{ic("i-plus")}</button>'
		f'<button class="btn btn--icon btn--sm btn--ghost" type="button" data-cv-zoom="fit" aria-label="Fit to screen"{dis}>{ic("c-fit")}</button></span></span></div>')


def world():
	o = [f'<div class="cv-world" id="cv-world" data-w="{WW}" data-h="{WH}" style="width:{WW}px;height:{WH}px;--cv-gap:{CG}px">']
	for L in LAY:
		n, s = counts(L['env'])
		o.append(f'<section class="cv-env" aria-label="{L["env"]} environment" style="--x:{L["x"]}px;--y:{L["y"]}px;--w:{L["w"]}px;--h:{L["h"]}px"><div class="cv-env-h"><h3 class="cv-env-t"><a href="{QP}environment.html">{L["env"]}</a></h3><span class="cv-env-s">{n} resources · {s}</span></div>'
			f'<button class="btn btn--icon btn--sm btn--ghost cv-env-zoom" type="button" aria-label="Zoom to {L["env"]}">{ic("c-zoom-focus")}</button></section>')
		for f in L['frames']:
			o.append(f'<div class="cv-net" data-net="{f["net"]}" style="--x:{f["x"]}px;--y:{f["y"]}px;--w:{f["w"]}px;--h:{f["h"]}px"><span class="cv-net-l">{ic("c-network")}{f["net"]}<small>{NETS[f["net"]][1]}</small></span></div>')
	o.append(f'<svg class="cv-edges" id="cv-edges" width="{WW}" height="{WH}" aria-hidden="true"><defs><marker id="cv-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path class="cv-arrow" d="M1 1L9 5L1 9" fill="none" stroke="context-stroke" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></marker></defs>')
	lab = []
	for a, b, k, v in EDGES:
		d, mx, my = edge_geom(byid[a], byid[b])
		o.append(f'<path class="cv-edge" d="{d}" data-from="{a}" data-to="{b}" data-kind="{k}" data-label="{v}"/>')
		g = ic('i-brain') if k == 'gateway' else ''
		lab.append(f'<button class="cv-elabel" type="button" data-from="{a}" data-to="{b}" aria-haspopup="menu" aria-label="{"Model gateway link" if k == "gateway" else v + " variable"}: {e(byid[a]["name"])} to {e(byid[b]["name"])}" style="--x:{mx:g}px;--y:{my:g}px">{g}{v}</button>')
	o.append('</svg><ul class="cv-nodes" aria-label="Resources">')
	for L in LAY:
		for r in RES:
			if r['env'] == L['env']: o.append(node(r))
	o.append('</ul><div class="cv-elabels">' + ''.join(lab) + '</div><div class="cv-tip" id="cv-tip" role="presentation"></div></div>')
	return ''.join(o)


def list_view():
	o = ['<div class="cv-list" id="cv-list">']
	for env in ENVS:
		n, s = counts(env)
		o.append(f'<section aria-labelledby="l-{env}"><h3 id="l-{env}">{env} <small>{n} resources · {s}</small></h3><div class="tbl-wrap"><table class="tbl"><caption class="sr-only">Resources in {env} and how they are connected</caption>'
			'<thead><tr><th scope="col">Resource</th><th scope="col">Status</th><th scope="col">Network</th><th scope="col">Connected to</th><th scope="col">Scan</th><th scope="col"><span class="sr-only">Actions</span></th></tr></thead><tbody>')
		for r in RES:
			if r['env'] != env: continue
			out = [(b, k, v) for a, b, k, v in EDGES if a == r['id']]
			inc = [(a, k, v) for a, b, k, v in EDGES if b == r['id']]
			links = ''.join([f'<li>to <b>{e(byid[b]["name"])}</b> <code>{v if k != "gateway" else "model gateway"}</code></li>' for b, k, v in out] + [f'<li>from <b>{e(byid[a]["name"])}</b></li>' for a, k, v in inc])
			nets = ''.join(f'<code>{x}</code>' for x in r['nets']) or '<span class="cv-none">none</span>'
			sv = sev_badge(r).replace('cv-sev-w', '').replace('cv-sev-n', '') if r.get('sev') else '<span class="cv-none">Clean</span>' if r['kind'] in ('git', 'image') else '<span class="cv-none">n/a</span>'
			act = '<span class="cv-none">n/a</span>' if r['kind'] == 'model' else f'<button class="btn btn--sm btn--secondary" type="button" data-cv-list-connect="{r["id"]}" aria-label="Connect {e(r["name"])} to…">{ic("c-plug")}<span>Connect…</span></button>'
			o.append(f'<tr id="r-{r["id"]}"><td data-label="Resource"><div class="cv-cellname"><span class="cv-ic">{ic(ICON[r["kind"]])}</span><div><b><a href="{QP}{r["href"]}">{e(r["name"])}</a></b><small>{KIND[r["kind"]]} · {e(r["sub"].split(" · ")[-1])}</small></div></div></td>'
				f'<td data-label="Status">{badge(r["status"])}</td><td data-label="Network"><span class="cv-nets">{nets}</span></td><td data-label="Connected to">{"<ul class=cv-links>" + links + "</ul>" if links else "<span class=cv-none>No links</span>"}</td>'
				f'<td data-label="Scan">{sv}</td><td class="tbl-act">{act}</td></tr>')
		o.append('</tbody></table></div></section>')
	o.append('</div>')
	return ''.join(o)


def dialogs():
	return ('<div id="cv-pop" class="dlg cv-confirm" popover role="dialog" aria-labelledby="cv-pop-t" aria-describedby="cv-pop-d"></div>'
		'<div id="cv-menu" class="menu" popover></div>'
		f'<dialog id="cv-connect" class="dlg dlg--confirm" aria-labelledby="cv-connect-t" closedby="any"><form method="dialog" class="dlg-form"><header class="dlg-head"><span class="dlg-ic dlg-ic--info">{ic("c-plug")}</span><div><h2 id="cv-connect-t">Connect</h2></div></header>'
		'<div class="dlg-body"><div class="field cv-pick"><label class="field-l" for="cv-target">Connect to</label><span class="select"><select id="cv-target" class="input"></select>' + ic('i-chevrons-up-down') + '</span></div><div class="cv-preview" aria-live="polite"></div></div>'
		'<footer class="dlg-foot"><button class="btn btn--secondary" type="submit" value="cancel">Cancel</button><button class="btn btn--primary" type="submit" value="ok">Connect</button></footer></form></dialog>')


def canvas_page(demo=''):
	n = len(RES)
	lede = f'{n} resources in 2 environments · 6 running · 1 building · 1 failed · 2 networks'
	b = page_head(lede) + '<section class="cv" id="cv" data-labels="on" data-nets="on" aria-labelledby="cv-h"><h2 class="sr-only" id="cv-h">Project map</h2>' + toolbar()
	b += ('<div class="cv-view" id="cv-view" tabindex="0" role="group" data-lod="full" aria-label="Project canvas, 8 resources. Tab visits resources, arrow keys follow links, C connects, plus and minus zoom, F fits, Shift and arrows pan." aria-describedby="cv-keys">'
		+ world()
		+ '<ul class="cv-legend" aria-label="Link types"><li><svg viewBox="0 0 28 8" aria-hidden="true"><path d="M0 4h28"/></svg>Variable</li><li><svg viewBox="0 0 28 8" aria-hidden="true"><path class="g" d="M0 4h28"/></svg>Model gateway</li><li><svg viewBox="0 0 28 8" aria-hidden="true"><path class="n" d="M0 4h28"/></svg>Network</li></ul>'
		'<p class="cv-hint" id="cv-keys">Drag from a handle to connect <kbd class="kbd">Ctrl</kbd> + scroll to zoom</p>'
		'<p class="sr-only" id="cv-live" role="status" aria-live="polite"></p></div>')
	b += list_view() + '</section>' + dialogs()
	return shell('storefront / canvas', b, demo)


def empty_page():
	g1 = dict(id='g1', name='Your app', kind='git', sub='Git, image or template', status='pending', x=0, y=0, nets=[], env='production', href='#', v=None)
	g2 = dict(id='g2', name='Your database', kind='db', sub='Postgres, MySQL, Redis', status='pending', x=W + CG, y=0, nets=[], env='production', href='#', v=None)
	def ghost(r):
		return f'<li class="cv-node cv-node--ghost" aria-hidden="true" style="--x:{r["x"]}px;--y:{r["y"]}px"><div class="cv-head"><span class="cv-ic">{ic(ICON[r["kind"]])}</span><span class="cv-open" style="position:static">{r["name"]}</span></div><span class="cv-sub">{r["sub"]}</span></li>'
	d, _, _ = edge_geom(g1, g2)
	b = page_head('No resources yet in this project') + '<section class="cv" id="cv" aria-labelledby="cv-h"><h2 class="sr-only" id="cv-h">Project map</h2>' + toolbar(True)
	b += ('<div class="cv-view cv-view--empty" id="cv-view" data-lod="full"><div class="cv-world" style="width:528px;height:92px">'
		f'<svg class="cv-edges" width="528" height="92" aria-hidden="true"><defs><marker id="cv-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M1 1L9 5L1 9" fill="none" stroke="context-stroke" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></marker></defs><path class="cv-edge" data-kind="network" d="{d}"/></svg>'
		f'<ul class="cv-nodes" aria-hidden="true">{ghost(g1)}{ghost(g2)}</ul></div>'
		'<div class="cv-empty"><section class="empty" role="status">' + f'<span class="empty-ic">{ic("c-network")}</span>'
		'<h2>Nothing to connect yet</h2><p>Resources in this project show up here as a map. Drag from one to another to put them on a shared network and inject the connection variables.</p>'
		f'<div class="empty-act"><a class="btn btn--primary" href="{QP}create.html">{ic("i-plus")}<span>Create resource</span></a><a class="btn btn--secondary" href="{QP}templates.html">{ic("i-package")}<span>Browse templates</span></a><a class="btn btn--ghost" href="{QP}create.html#stack">{ic("i-layers")}<span>Import compose file</span></a></div></section></div></div>')
	b += '</section>'
	return shell('storefront / canvas (empty)', b)


def states_page():
	ex = dict(RES[0]); ex.update(x=0, y=0)
	def cell(label, note, r=None, state=None, st=None):
		rr = dict(ex); rr.update(r or {}); rr.update(x=0, y=0)
		rr['id'] = rr['id'] + '-' + (state or rr['status'])
		return f'<figure><div class="cv-cell"><ul class="cv-nodes" style="position:static">{node(rr, state=state)}</ul></div><figcaption><b>{label}</b>{note}</figcaption></figure>'
	S = []
	S.append(cell('Running', 'Default. Status is a dot plus a word.', dict(status='running', sev=None)))
	S.append(cell('Building', 'Pulsing dot, hairline progress at the foot.', dict(status='building', progress=64, sev=('high', 2))))
	S.append(cell('Failed', 'Border and name take the error colour; the diamond dot and the word say it too.', dict(status='failed', sev=None)))
	S.append(cell('Deploying', 'After a wire is confirmed. Returns to Running by itself.', dict(status='deploying', sev=None)))
	S.append(cell('Stopped', 'Hollow dot.', dict(status='stopped', sev=None)))
	S.append(cell('Hover', 'Hairline strengthens, handles and the menu button appear, linked edges turn accent.', dict(status='running'), 'hover'))
	S.append(cell('Focus', 'Keyboard focus ring (2 px, --focus) around the whole node.', dict(status='running'), 'focus'))
	S.append(cell('Selected', 'Accent border and soft fill: current focus target, menu open, or link selected.', dict(status='running'), 'selected'))
	S.append(cell('Connecting: source', 'The node being wired. Handles stay lit.', dict(status='running'), 'source'))
	S.append(cell('Connecting: candidate', 'Dashed accent border: a valid target. Everything else dims.', dict(status='running', sev=None, name='postgres-16', kind='db', sub='PostgreSQL 16.4'), 'candidate'))
	S.append(cell('Dragging over: valid', 'Solid accent, halo, the in-handle fills. Release to open the confirm popover.', dict(status='running', sev=None, name='postgres-16', kind='db', sub='PostgreSQL 16.4'), 'valid'))
	S.append(cell('Dragging over: invalid', 'Error border and fill, not-allowed cursor, reason in the tip. Releasing does nothing.', dict(status='running', sev=None, name='postgres-16', kind='db', sub='PostgreSQL 16.4'), 'invalid'))
	S.append(cell('Connecting: not a target', 'Dimmed to 45 %, skipped by Tab (inert).', dict(status='running', sev=None, name='qwen3-4b', kind='model', sub='Model · llama.cpp'), 'off'))
	sevs = ''.join(f'<span class="sev" data-sev="{k}">{SEV[k]} {n}</span> ' for k, n in (('critical', 1), ('high', 2), ('medium', 3), ('low', 4)))
	def spec(path, kind, extra='', label=''):
		return f'<figure><div class="cv-spec"><svg viewBox="0 0 220 40" aria-hidden="true"><defs><marker id="m{kind}{extra}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M1 1L9 5L1 9" fill="none" stroke="context-stroke" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></marker></defs><g class="cv-edges"><path class="cv-edge {extra}" data-kind="{kind}" d="M10 20L210 20" style="marker-end:url(#m{kind}{extra})"/></g></svg></div><figcaption><b>{label[0]}</b>{label[1]}</figcaption></figure>'
	E = [spec('', 'env', '', ('Variable link', 'Solid, 1.5 px. The pill names the variable.')), spec('', 'gateway', '', ('Model gateway link', 'Dotted, 2.25 px, brain icon in the pill.')),
		spec('', 'network', '', ('Network tie', 'Dashed. Only drawn between frames; inside one frame the frame is the tie.')), spec('', 'env', 'is-hl', ('Hover or selected', 'Accent, 2 px.')),
		f'<figure><div class="cv-spec" style="display:grid;place-items:center"><span class="cv-elabel" style="position:static;translate:none;--cv-gap:200px">DATABASE_URL</span></div><figcaption><b>Link label</b>Mono pill on the canvas colour. Hidden below 75 % zoom until hover or focus.</figcaption></figure>']
	lod = ''.join(f'<figure><div class="cv-spec" data-lod="{k}" style="--z:{z};height:110px;display:grid;place-items:center"><div class="cv-cell" style="transform:scale({z});--x:0px;--y:0px"><ul class="cv-nodes" style="position:static">{node(dict(ex, id="lod-"+k, sev=ex["sev"]), state=None)}</ul></div></div><figcaption><b>{t}</b>{n}</figcaption></figure>'
		for k, z, t, n in (('full', 1, 'Full, 75 % and up', 'Everything: sub line, status word, scan level and count, link labels.'), ('compact', .6, 'Compact, 50 to 75 %', 'Sub line and link labels go; the scan count stays.'), ('overview', .4, 'Overview, under 50 %', 'Name and status dot only. Text is counter-scaled so it stays legible.')))
	b = ('<header class="page-head"><div class="page-title"><p class="eyebrow">Canvas</p><h1>Node and link states</h1><p class="lede">Every state the canvas can show, rendered from the real component CSS.</p></div></header>'
		'<section aria-labelledby="s1"><h2 id="s1">Node</h2><div class="cv cv--bare"><div class="cv-sheet">' + ''.join(S) + '</div></div></section>'
		f'<section aria-labelledby="s2"><h2 id="s2">Scan severity (S1)</h2><p>{sevs}</p></section>'
		'<section aria-labelledby="s3"><h2 id="s3">Links</h2><div class="cv cv--bare"><div class="cv-sheet">' + ''.join(E) + '</div></div></section>'
		'<section aria-labelledby="s4"><h2 id="s4">Level of detail</h2><div class="cv cv--bare"><div class="cv-sheet">' + lod + '</div></div></section>')
	return shell('Canvas states', b)


if __name__ == '__main__':
	(OUT / 'canvas.html').write_text(canvas_page())
	(OUT / 'canvas-wire.html').write_text(canvas_page('wire'))
	(OUT / 'canvas-empty.html').write_text(empty_page())
	(OUT / 'canvas-states.html').write_text(states_page())
	print('world', WW, WH, [(l['env'], l['w'], l['h']) for l in LAY])
