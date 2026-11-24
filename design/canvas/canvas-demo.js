/* canvas-demo.js: behaviour prototype for the project canvas (D1). It is the reference for crates/ui/static/canvas.js (I2).
   In podkit the server renders the world (frames, edges, nodes, labels) and validates every wire; this file only does pan, zoom,
   level of detail, keyboard focus along edges, and the connect gesture (drag, click-click, or menu). Anything that changes data
   is a Datastar action; here ask() and commit() stand in for the preview GET and the wire POST. ?still=1 skips animation. */
(() => {
	const d = document, $ = (s, r = d) => r.querySelector(s), $$ = (s, r = d) => [...r.querySelectorAll(s)];
	const cv = $('#cv'), view = $('#cv-view'), world = $('#cv-world'), svg = $('#cv-edges'), live = $('#cv-live'), pop = $('#cv-pop'), menu = $('#cv-menu'), tip = $('#cv-tip');
	if (!view) return;
	const still = new URLSearchParams(location.search).has('still') || matchMedia('(prefers-reduced-motion: reduce)').matches;
	const WW = +world.dataset.w, WH = +world.dataset.h, GAP = 112, NW = 208, NH = 92;
	const CONSUMER = ['git', 'image', 'stack', 'template'];
	let z = 1, tx = 0, ty = 0, conn = null, hover = null, pan = null, src = null;
	const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
	const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
	const say = m => { live.textContent = ''; setTimeout(() => live.textContent = m, 30); };
	const N = id => $('#n-' + id), nodes = () => $$('.cv-node[data-id]', world);
	const info = el => ({ el, id: el.dataset.id, name: el.dataset.name, kind: el.dataset.kind, env: el.dataset.env, nets: (el.dataset.nets || '').split(' ').filter(Boolean), v: el.dataset.var, x: +el.dataset.x, y: +el.dataset.y });
	const edges = () => $$('.cv-edge[data-from]', svg).map(p => ({ el: p, from: p.dataset.from, to: p.dataset.to, kind: p.dataset.kind, v: p.dataset.label }));
	const toast = m => { const b = d.createElement('button'); b.dataset.toast = m; d.body.append(b); b.click(); b.remove(); };

	/* ---- viewport: pan, zoom, fit, level of detail ---- */
	const apply = anim => {
		view.classList.toggle('is-anim', !!anim && !still);
		view.style.setProperty('--tx', tx + 'px'); view.style.setProperty('--ty', ty + 'px'); view.style.setProperty('--z', z);
		view.dataset.lod = z >= .75 ? 'full' : z >= .5 ? 'compact' : 'overview';
		const o = $('#cv-zoom-out'); if (o) o.textContent = Math.round(z * 100) + '%';
	};
	const set = (nz, nx, ny, anim) => { z = clamp(nz, .35, 2); tx = nx; ty = ny; apply(anim); };
	const zoomAt = (f, cx, cy, anim) => { const nz = clamp(z * f, .35, 2), k = nz / z; set(nz, cx - (cx - tx) * k, cy - (cy - ty) * k, anim); };
	const fit = (b, anim) => {
		const r = view.getBoundingClientRect(), p = 24; b = b || { x: 0, y: 0, w: WW, h: WH };
		const nz = clamp(Math.min((r.width - 2 * p) / b.w, (r.height - 2 * p) / b.h), .35, 1);
		set(nz, (r.width - b.w * nz) / 2 - b.x * nz, (r.height - b.h * nz) / 2 - b.y * nz, anim);
	};
	const reveal = el => {
		const v = view.getBoundingClientRect(), r = el.getBoundingClientRect(), m = 32; let dx = 0, dy = 0;
		if (r.left < v.left + m) dx = v.left + m - r.left; else if (r.right > v.right - m) dx = v.right - m - r.right;
		if (r.top < v.top + m) dy = v.top + m - r.top; else if (r.bottom > v.bottom - m) dy = v.bottom - m - r.bottom;
		if (dx || dy) set(z, tx + dx, ty + dy, true);
	};
	const toWorld = e => { const r = view.getBoundingClientRect(); return { x: (e.clientX - r.left - tx) / z, y: (e.clientY - r.top - ty) / z }; };

	d.addEventListener('click', e => {
		const b = e.target.closest('[data-cv-zoom]'); if (!b) return; const r = view.getBoundingClientRect(), a = b.dataset.cvZoom;
		if (a === 'in') zoomAt(1.25, r.width / 2, r.height / 2, true); else if (a === 'out') zoomAt(.8, r.width / 2, r.height / 2, true); else if (a === 'fit') fit(null, true);
		else if (a === 'reset') set(1, tx, ty, true);
	});
	d.addEventListener('click', e => {
		const b = e.target.closest('[data-cv-toggle]'); if (!b) return; const on = b.getAttribute('aria-pressed') !== 'true';
		b.setAttribute('aria-pressed', on); cv.dataset[b.dataset.cvToggle] = on ? 'on' : 'off';
	});
	d.addEventListener('click', e => { const b = e.target.closest('.cv-env-zoom'); if (b) { const f = b.closest('.cv-env'); fit({ x: f.offsetLeft, y: f.offsetTop, w: f.offsetWidth, h: f.offsetHeight }, true); } });
	view.addEventListener('wheel', e => {
		e.preventDefault();
		if (e.ctrlKey || e.metaKey) { const r = view.getBoundingClientRect(); zoomAt(Math.exp(-e.deltaY * .01), e.clientX - r.left, e.clientY - r.top); } else set(z, tx - e.deltaX, ty - e.deltaY);
	}, { passive: false });

	/* ---- edges between two nodes: same cubic the server draws, port to port ---- */
	const path = (x0, y0, x1, y1) => { const dx = Math.max(40, (x1 - x0) / 2); return y0 === y1 ? `M${x0} ${y0}L${x1} ${y1}` : `M${x0} ${y0}C${x0 + dx} ${y0} ${x1 - dx} ${y1} ${x1} ${y1}`; };
	const portOut = n => [n.x + NW, n.y + NH / 2], portIn = n => [n.x, n.y + NH / 2];

	/* ---- what may be wired: the server decides for real (and re-checks on POST); this mirrors its rules so the UI can light targets without a round trip ---- */
	const can = (a, b) => {
		if (a.id === b.id) return { reason: 'Pick a different resource' };
		if (a.env !== b.env) return { reason: `${b.name} is in ${b.env}. Wiring stays inside one environment` };
		if (!CONSUMER.includes(a.kind) && CONSUMER.includes(b.kind)) [a, b] = [b, a];
		if (a.kind === 'model') return { reason: 'A model can only be linked from an app' };
		if (b.kind === 'model' && !CONSUMER.includes(a.kind)) return { reason: 'Only apps and stacks can use a model' };
		const kind = b.kind === 'model' ? 'gateway' : b.kind === 'db' && CONSUMER.includes(a.kind) ? 'env' : 'network';
		const dup = edges().find(e => e.from === a.id && e.to === b.id && e.kind !== 'network');
		if (dup && kind !== 'network') return { reason: `Already connected through ${dup.v}` };
		const shared = a.nets.find(n => b.nets.includes(n));
		if (kind === 'network' && shared) return { reason: `Already on network ${shared}` };
		return { ok: true, kind, a, b };
	};
	const describe = r => {
		const { a, b, kind } = r, A = `<code>${esc(a.name)}</code>`, B = `<code>${esc(b.name)}</code>`, net = b.nets[0], join = net && !a.nets.includes(net);
		if (kind === 'env') return { title: `Connect ${esc(a.name)} to ${esc(b.name)}`, ok: 'Connect and redeploy',
			lead: `${join ? `Add ${A} to network <code>${esc(net)}</code>, ` : ''}inject <code>${esc(b.v)}</code>, redeploy?`,
			rows: [join && ['Network', `Add ${A} to <code>${esc(net)}</code>`], ['Env var', `Inject <code>${esc(b.v)}</code> into ${A}<small>Secret, write-only. The value is never shown.</small>`], ['Redeploy', `Rolling redeploy of ${A}, about 40 s`]] };
		if (kind === 'gateway') return { title: `Link ${esc(a.name)} to ${esc(b.name)}`, ok: 'Link and redeploy',
			lead: `Route ${A} through the model gateway to ${B}, inject <code>OPENAI_BASE_URL</code>, redeploy?`,
			rows: [['Gateway', `${A} calls ${B} through the podkit gateway`], ['Env vars', `Inject <code>OPENAI_BASE_URL</code> and <code>OPENAI_API_KEY</code><small>New key, scoped to ${esc(b.name)}. Write-only.</small>`], ['Redeploy', `Rolling redeploy of ${A}`]] };
		const n = a.nets[0] || b.nets[0] || `${a.name}-net`, who = a.nets[0] ? B : A;
		return { title: `Put ${esc(a.name)} and ${esc(b.name)} on one network`, ok: 'Join network',
			lead: `Add ${who} to network <code>${esc(n)}</code>?`,
			rows: [['Network', `Add ${who} to <code>${esc(n)}</code>${a.nets[0] || b.nets[0] ? '' : '<small>New user network, created for you.</small>'}`], ['Attach', 'Attached live. No redeploy.']] };
	};
	const body = (r, dlg) => { const t = describe(r); return { t, html: `<ul class="cv-change">${t.rows.filter(Boolean).map(([k, v]) => `<li><b>${k}</b><span>${v}</span></li>`).join('')}</ul>` }; };

	/* ---- connect: one state machine, three ways in (drag a port, click source then target, menu or the C key) ---- */
	const begin = (s, via) => {
		conn = { s: info(s), via }; src = s; cv.classList.add('is-connecting'); view.classList.add('is-dragging');
		nodes().forEach(n => { const r = n === s ? null : can(conn.s, info(n)); n.dataset.drop = n === s ? 'source' : r.ok ? 'candidate' : 'off'; if (via !== 'drag') n.toggleAttribute('inert', n !== s && !r?.ok); });
		if (via === 'drag') { const p = d.createElementNS('http://www.w3.org/2000/svg', 'path'); p.setAttribute('class', 'cv-edge cv-edge--band'); p.id = 'cv-band'; svg.append(p); band(...portOut(conn.s)); }
		const ok = nodes().filter(n => n.dataset.drop === 'candidate').map(n => n.dataset.name);
		say(ok.length ? `Connecting ${conn.s.name}. Valid targets: ${ok.join(', ')}. ${via === 'drag' ? 'Release on one.' : 'Tab or arrow keys to choose, Enter to connect, Escape to cancel.'}` : `${conn.s.name} has nothing it can connect to.`);
		if (via !== 'drag') { const f = $('.cv-node[data-drop="candidate"] .cv-open', world); f?.focus(); }
	};
	const band = (x, y, ok = true) => { const p = $('#cv-band'); if (!p) return; const [x0, y0] = portOut(conn.s); p.setAttribute('d', path(x0, y0, x, y)); p.dataset.ok = ok; };
	const over = n => {
		if (hover && hover !== n && hover.dataset.drop && hover.dataset.drop !== 'source') hover.dataset.drop = hover.dataset.prev;
		hover = n && n !== src ? n : null; if (!hover) { tip.removeAttribute('data-on'); return null; }
		hover.dataset.prev = hover.dataset.drop === 'valid' || hover.dataset.drop === 'invalid' ? hover.dataset.prev : hover.dataset.drop;
		const r = can(conn.s, info(hover)); hover.dataset.drop = r.ok ? 'valid' : 'invalid';
		const t = r.ok ? describe(r) : null; tip.dataset.ok = !!r.ok; tip.dataset.on = '';
		tip.innerHTML = `<svg class="ic" aria-hidden="true"><use href="#${r.ok ? 'c-plug' : 'c-ban'}"/></svg><span>${r.ok ? esc(hover.dataset.name) + ': ' + (r.kind === 'env' ? esc(r.b.v) : r.kind === 'gateway' ? 'model gateway' : 'shared network') : esc(r.reason)}</span>`;
		const x = +hover.dataset.x, y = +hover.dataset.y; tip.style.setProperty('--x', x + 'px'); tip.style.setProperty('--y', y - 8 + 'px');
		return r;
	};
	const clear = () => {
		nodes().forEach(n => { n.removeAttribute('data-drop'); n.removeAttribute('data-prev'); n.removeAttribute('inert'); });
		$('#cv-band')?.remove(); tip.removeAttribute('data-on'); cv.classList.remove('is-connecting'); view.classList.remove('is-dragging'); conn = hover = null;
	};
	const cancel = (msg = 'Connection cancelled') => { const s = src; clear(); say(msg); s?.querySelector('.cv-open')?.focus({ preventScroll: true }); };
	const ask = (r, at) => {
		const { t, html } = body(r); const m = r.kind;
		pop.innerHTML = `<form method="dialog" class="dlg-form"><header class="dlg-head"><span class="dlg-ic dlg-ic--info"><svg class="ic" aria-hidden="true"><use href="#c-plug"/></svg></span><div><h2 id="cv-pop-t">${t.title}</h2><p id="cv-pop-d">${t.lead}</p></div></header><div class="dlg-body">${html}<p class="cv-note">Nothing changes until you confirm. Secret values stay write-only.</p></div><footer class="dlg-foot"><button class="btn btn--secondary" type="submit" value="cancel">Cancel</button><button class="btn btn--primary" type="submit" value="ok" autofocus>${t.ok}</button></footer></form>`;
		pop.dataset.kind = m; pop.returnValue = ''; pop._r = r;
		const q = at.getBoundingClientRect(); pop.showPopover();
		const w = pop.offsetWidth, h = pop.offsetHeight, vw = innerWidth, vh = innerHeight;
		if (matchMedia('(max-width:760px)').matches) return $('[autofocus]', pop).focus();
		let l = q.right + 12; if (l + w > vw - 8) l = Math.max(8, q.left - w - 12);
		pop.style.left = l + 'px'; pop.style.top = clamp(q.top - 8, 8, vh - h - 8) + 'px';
		$('[autofocus]', pop).focus();
	};
	const commit = r => {
		const { a, b, kind } = r, A = N(a.id), B = N(b.id);
		const p = d.createElementNS('http://www.w3.org/2000/svg', 'path'); const [x0, y0] = portOut(a), [x1, y1] = portIn(b);
		p.setAttribute('class', 'cv-edge is-new'); p.setAttribute('d', path(x0, y0, x1, y1)); Object.assign(p.dataset, { from: a.id, to: b.id, kind, label: kind === 'env' ? b.v : kind === 'gateway' ? 'gateway' : 'network' }); svg.append(p);
		const l = d.createElement('button'); l.className = 'cv-elabel is-new'; l.type = 'button'; l.dataset.from = a.id; l.dataset.to = b.id; l.textContent = p.dataset.label;
		l.style.setProperty('--x', (x0 + x1) / 2 + 'px'); l.style.setProperty('--y', (y0 + y1) / 2 + 'px'); $('.cv-elabels', world).append(l);
		if (kind === 'env' && b.nets[0] && !a.nets.includes(b.nets[0])) A.dataset.nets = (A.dataset.nets + ' ' + b.nets[0]).trim();
		const msg = kind === 'network' ? `${a.name} and ${b.name} are on one network` : `${a.name} linked to ${b.name}. Redeploying`;
		toast(msg); say(msg);
		if (kind !== 'network') { const bd = $('.badge', A), set = (s, t) => { A.dataset.status = s; bd.dataset.status = s; $('.lbl', bd).textContent = t; }; set('deploying', 'Deploying'); setTimeout(() => set('running', 'Running'), still ? 0 : 2200); }
	};
	pop.addEventListener('submit', e => { e.preventDefault(); const ok = e.submitter?.value === 'ok', r = pop._r; pop.hidePopover(); if (ok) { const s = src; clear(); commit(r); s?.querySelector('.cv-open')?.focus({ preventScroll: true }); } else cancel(); });
	pop.addEventListener('toggle', e => { if (e.newState === 'closed' && conn) cancel(); });

	/* ---- pointer: pan on empty canvas, connect from a port ---- */
	view.addEventListener('pointerdown', e => {
		if (e.button !== 0) return;
		const port = e.target.closest('.cv-port');
		if (port) { e.preventDefault(); begin(port.closest('.cv-node'), 'drag'); view.setPointerCapture(e.pointerId); return; }
		if (conn && conn.via !== 'drag') { return; }
		if (e.target.closest('.cv-node,.cv-elabel,button,a')) return;
		pan = { x: e.clientX, y: e.clientY, tx, ty, moved: false }; view.setPointerCapture(e.pointerId);
	});
	let raf = 0, last = null;
	view.addEventListener('pointermove', e => {
		last = e; if (raf) return; raf = requestAnimationFrame(() => { raf = 0; e = last;
			if (conn?.via === 'drag' && view.hasPointerCapture(e.pointerId)) {
				const n = d.elementFromPoint(e.clientX, e.clientY)?.closest('.cv-node[data-id]'), r = over(n), p = toWorld(e);
				if (r?.ok) band(...portIn(r.b), true); else band(p.x, p.y, !n);
			} else if (pan) { const dx = e.clientX - pan.x, dy = e.clientY - pan.y; if (!pan.moved && Math.hypot(dx, dy) < 4) return; pan.moved = true; view.classList.add('is-panning'); set(z, pan.tx + dx, pan.ty + dy); }
		});
	});
	const up = e => {
		view.classList.remove('is-panning');
		if (conn?.via === 'drag') { const n = d.elementFromPoint(e.clientX, e.clientY)?.closest('.cv-node[data-id]'), r = n && n !== src ? can(conn.s, info(n)) : null; const s = src;
			if (r?.ok) { const target = n; band(...portIn(r.b), true); setTimeout(() => ask(r, target), 0); } else { cancel(r ? r.reason : 'Connection cancelled'); if (r) toast(r.reason); } }
		else if (pan && !pan.moved) $$('.cv-node[data-selected]').forEach(n => n.removeAttribute('data-selected'));
		pan = null;
	};
	view.addEventListener('pointerup', up); view.addEventListener('pointercancel', () => { if (conn?.via === 'drag') cancel(); pan = null; view.classList.remove('is-panning'); });

	/* ---- click-click and keyboard connect: while connecting, a click or Enter on a node picks it instead of opening it ---- */
	view.addEventListener('click', e => {
		const a = e.target.closest('.cv-open'); if (!a || !conn || conn.via === 'drag') return; e.preventDefault();
		const n = a.closest('.cv-node'), r = n === src ? null : can(conn.s, info(n));
		if (r?.ok) ask(r, n); else if (n === src) cancel(); else toast(r.reason);
	});
	d.addEventListener('click', e => { const b = e.target.closest('[data-cv-connect]'); if (!b) return; if (conn) cancel(); else say('Pick the first resource, then the one to connect it to.'), b.setAttribute('aria-pressed', 'true'), cv.classList.add('is-picking'); });
	view.addEventListener('click', e => {
		if (!cv.classList.contains('is-picking')) return; const a = e.target.closest('.cv-open'); if (!a) return; e.preventDefault();
		cv.classList.remove('is-picking'); $('[data-cv-connect]')?.setAttribute('aria-pressed', 'false'); begin(a.closest('.cv-node'), 'click');
	}, true);

	/* ---- hover/focus highlight along edges ---- */
	const hl = id => {
		$$('.cv-edge[data-from],.cv-elabel', world).forEach(x => x.classList.toggle('is-hl', !!id && (x.dataset.from === id || x.dataset.to === id)));
		const ends = id ? edges().filter(e => e.from === id || e.to === id).flatMap(e => [e.from, e.to]) : [];
		nodes().forEach(n => n.toggleAttribute('data-hl', ends.includes(n.dataset.id) && n.dataset.id !== id));
	};
	view.addEventListener('pointerover', e => { if (!conn) hl(e.target.closest('.cv-node[data-id]')?.dataset.id); });
	view.addEventListener('focusin', e => { const n = e.target.closest('.cv-node[data-id]'); if (n) { hl(n.dataset.id); reveal(n); nodes().forEach(x => x.toggleAttribute('data-selected', x === n)); } });
	view.addEventListener('pointerleave', () => { if (!conn) hl(null); });

	/* ---- keyboard: Tab visits nodes in reading order, arrows follow edges ---- */
	const focusNode = n => { n?.querySelector('.cv-open')?.focus(); return !!n; };
	const follow = (id, dir) => edges().filter(e => (dir > 0 ? e.from : e.to) === id).map(e => N(dir > 0 ? e.to : e.from)).filter(n => n && !n.hasAttribute('inert'));
	view.addEventListener('keydown', e => {
		const n = e.target.closest?.('.cv-node[data-id]'), k = e.key;
		if (k === 'Escape' && conn) { e.preventDefault(); return cancel(); }
		if (n && !e.altKey && !e.ctrlKey && !e.metaKey) {
			const id = n.dataset.id;
			if (k === 'c' || k === 'C') { e.preventDefault(); return conn ? cancel() : begin(n, 'key'); }
			if (!conn && (k === 'ContextMenu' || k === ' ' || (k === 'F10' && e.shiftKey))) { e.preventDefault(); return openMenu(n); }
			if (k.startsWith('Arrow') && !e.shiftKey) {
				e.preventDefault(); let to;
				if (conn && conn.via !== 'drag') { const c = $$('.cv-node:not([inert])', world), i = c.indexOf(n); to = c[(i + (k === 'ArrowRight' || k === 'ArrowDown' ? 1 : -1) + c.length) % c.length]; }
				else if (k === 'ArrowRight') to = follow(id, 1)[0]; else if (k === 'ArrowLeft') to = follow(id, -1)[0];
				else { const col = nodes().filter(x => x.dataset.env === n.dataset.env && x.dataset.x === n.dataset.x).sort((a, b) => a.dataset.y - b.dataset.y), i = col.indexOf(n); to = col[i + (k === 'ArrowDown' ? 1 : -1)]; }
				if (!focusNode(to)) say(k === 'ArrowRight' ? `${n.dataset.name} has no outgoing links` : k === 'ArrowLeft' ? `${n.dataset.name} has no incoming links` : 'No more resources in this column');
				else say(`${to.dataset.name}, ${k === 'ArrowRight' ? 'linked from here' : k === 'ArrowLeft' ? 'links to here' : 'same column'}`);
				return;
			}
		}
		if (e.target === view || n) {
			const r = view.getBoundingClientRect(), cx = r.width / 2, cy = r.height / 2;
			if (k === '+' || k === '=') zoomAt(1.25, cx, cy, true); else if (k === '-') zoomAt(.8, cx, cy, true); else if (k === '0') set(1, tx, ty, true); else if (k === 'f' || k === 'F') fit(null, true);
			else if (e.shiftKey && k.startsWith('Arrow')) { e.preventDefault(); set(z, tx + (k === 'ArrowLeft' ? 80 : k === 'ArrowRight' ? -80 : 0), ty + (k === 'ArrowUp' ? 80 : k === 'ArrowDown' ? -80 : 0), true); }
		}
	});

	/* ---- shared menu: node actions and link actions ---- */
	const item = (ic, t, a, x = '') => `<button class="menu-item ${x}" type="button" data-act="${a}"><svg class="ic" aria-hidden="true"><use href="#${ic}"/></svg><span>${t}</span></button>`;
	const openMenu = (n, btn) => {
		const b = btn || $('.cv-more', n), id = n.dataset.id;
		menu.innerHTML = `<div focusgroup="menu nomemory" aria-label="Actions for ${esc(n.dataset.name)}"><a class="menu-item" href="${esc(n.dataset.href)}"><svg class="ic" aria-hidden="true"><use href="#i-arrow-right"/></svg><span>Open</span></a>${item('c-plug', 'Connect to…', 'connect')}${item('c-list', 'Show in list', 'list')}</div>`;
		menu._n = n; place(menu, b); $('.menu-item', menu).focus();
	};
	const place = (m, b) => { m.showPopover(); const r = b.getBoundingClientRect(); m.style.cssText += `;position:fixed;inset:auto;margin:0;top:${r.bottom + 6}px;left:${clamp(r.right - m.offsetWidth, 8, innerWidth - m.offsetWidth - 8)}px`; };
	view.addEventListener('click', e => {
		const m = e.target.closest('.cv-more'); if (m && !conn) { e.preventDefault(); return openMenu(m.closest('.cv-node'), m); }
		const l = e.target.closest('.cv-elabel'); if (!l) return;
		const f = N(l.dataset.from), t = N(l.dataset.to); $$('.cv-edge.is-sel', svg).forEach(p => p.classList.remove('is-sel'));
		$$('.cv-edge', svg).find(p => p.dataset.from === l.dataset.from && p.dataset.to === l.dataset.to)?.classList.add('is-sel');
		menu.innerHTML = `<div focusgroup="menu nomemory" aria-label="Link ${esc(l.textContent)}"><p class="menu-h" role="presentation">${esc(f.dataset.name)} → ${esc(t.dataset.name)}</p><a class="menu-item" href="${esc(f.dataset.href)}#variables"><svg class="ic" aria-hidden="true"><use href="#i-arrow-right"/></svg><span>Open variables</span></a><hr>${item('c-unlink', 'Unlink…', 'unlink', 'menu-item--danger')}</div>`;
		menu._n = f; menu._l = l; place(menu, l); $('.menu-item', menu).focus();
	});
	menu.addEventListener('click', e => {
		const b = e.target.closest('[data-act]'); if (!b) return; const n = menu._n; menu.hidePopover();
		if (b.dataset.act === 'connect') begin(n, 'key'); else if (b.dataset.act === 'unlink') toast(`Unlink ${menu._l.textContent}: confirm dialog in the real UI`); else { $('#cv-v-list').click(); $(`#r-${n.dataset.id} button`)?.focus(); }
	});
	menu.addEventListener('keydown', e => { if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return; const i = $$('.menu-item', menu), j = i.indexOf(d.activeElement); e.preventDefault(); i[(j + (e.key === 'ArrowDown' ? 1 : -1) + i.length) % i.length].focus(); });

	/* ---- list view: same wiring through a dialog, for screen readers, phones and anyone who cannot drag ---- */
	const dlg = $('#cv-connect');
	if (dlg) {
		const fill = () => { const s = info(N(dlg.dataset.src)), sel = $('select', dlg); const r = can(s, info(N(sel.value))); const out = $('.cv-preview', dlg), go = $('[value=ok]', dlg);
			if (r.ok) { const { html } = body(r); out.innerHTML = describe(r).lead.replace(/^/, '<p class="cv-note">') + '</p>' + html; go.disabled = false; go.textContent = describe(r).ok; dlg._r = r; } else { out.innerHTML = `<p class="cv-note">${esc(r.reason)}</p>`; go.disabled = true; } };
		d.addEventListener('click', e => { const b = e.target.closest('[data-cv-list-connect]'); if (!b) return; const s = info(N(b.dataset.cvListConnect)); dlg.dataset.src = s.id;
			$('h2', dlg).textContent = `Connect ${s.name}`; const sel = $('select', dlg); sel.innerHTML = nodes().filter(n => n.dataset.id !== s.id && n.dataset.env === s.env).map(n => `<option value="${esc(n.dataset.id)}" ${can(s, info(n)).ok ? '' : 'disabled'}>${esc(n.dataset.name)}${can(s, info(n)).ok ? '' : ' (not possible)'}</option>`).join('');
			sel.selectedIndex = [...sel.options].findIndex(o => !o.disabled); fill(); dlg.showModal(); });
		$('select', dlg).addEventListener('change', fill);
		dlg.addEventListener('close', () => { if (dlg.returnValue === 'ok' && dlg._r) { commit(dlg._r); } dlg.returnValue = ''; });
	}

	/* ---- boot ---- */
	fit(null, false);
	view.addEventListener('dblclick', e => { if (!e.target.closest('.cv-node,.cv-elabel,button,a')) fit(null, true); });
	const stage = new URLSearchParams(location.search).get('stage') || (d.body.dataset.demo === 'wire' ? 'confirm' : '');
	if (stage && view.offsetParent) requestAnimationFrame(() => requestAnimationFrame(() => {
		const a = N('api'), t = N(stage === 'invalid' ? 'sapi' : stage === 'model' ? 'qwen' : 'pg'); begin(a, 'drag'); const r = over(t);
		if (r?.ok) band(...portIn(r.b), true); else band(+t.dataset.x + 40, +t.dataset.y + 40, false);
		if (stage === 'confirm' && r?.ok) ask(r, t);
	}));
})();
