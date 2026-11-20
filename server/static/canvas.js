/* canvas.js: the project canvas. The server renders the world (frames, links, nodes) and
   decides every wire; this file only does what HTML cannot: pan, zoom, level of detail, the
   connect gesture (drag a handle, click then click, or the C key), keyboard focus along
   links, and menu and popover placement. A wire is two events on #cv that Datastar turns
   into server requests (cv-wire, cv-unlink); the server answers with the popover to
   confirm. Vanilla, no dependencies, loaded only on the canvas page. */
(() => {
	const d = document, $ = (s, r = d) => r.querySelector(s), $$ = (s, r = d) => [...r.querySelectorAll(s)];
	const cv = $('#cv'), view = $('#cv-view'), world = $('#cv-world'), live = $('#cv-live'), pop = $('#cv-pop'), menu = $('#cv-menu'), tip = $('#cv-tip');
	if (!cv || !view || !world) return;
	const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
	const NW = 208, NH = 92, ZMIN = 0.35, ZMAX = 2;
	let z = 1, tx = 0, ty = 0, conn = null, hover = null, pan = null, src = null, lastTo = null, fitted = false;
	const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
	const say = (m) => { live.textContent = ''; setTimeout(() => (live.textContent = m), 30); };
	const nodes = () => $$('.ui-cv-node[data-id]', world);
	const N = (id) => nodes().find((n) => n.dataset.id === id);
	const edges = () => $$('.ui-cv-edge[data-from]', world);
	const targets = (n) => (n.dataset.targets || '').split(' ').filter(Boolean);
	const open = (n) => n.querySelector('.ui-cv-open');
	const svg = (name) => `<svg class="ui-icon" aria-hidden="true"><use href="#cv-${name}"/></svg>`;
	const fire = (name, detail) => cv.dispatchEvent(new CustomEvent(name, { detail }));

	/* viewport: pan, zoom, fit, level of detail */
	const apply = (anim) => {
		view.classList.toggle('is-anim', !!anim && !still);
		view.style.setProperty('--tx', tx + 'px'); view.style.setProperty('--ty', ty + 'px'); view.style.setProperty('--z', z);
		view.dataset.lod = z >= 0.75 ? 'full' : z >= 0.5 ? 'compact' : 'overview';
		const o = $('#cv-zoom-out'); if (o) o.textContent = Math.round(z * 100) + '%';
	};
	const set = (nz, nx, ny, anim) => { z = clamp(nz, ZMIN, ZMAX); tx = nx; ty = ny; apply(anim); };
	const zoomAt = (f, cx, cy, anim) => { const nz = clamp(z * f, ZMIN, ZMAX), k = nz / z; set(nz, cx - (cx - tx) * k, cy - (cy - ty) * k, anim); };
	const fit = (b, anim) => {
		const r = view.getBoundingClientRect(), p = 24;
		b = b || { x: 0, y: 0, w: +world.dataset.w || NW, h: +world.dataset.h || NH };
		const nz = clamp(Math.min((r.width - 2 * p) / b.w, (r.height - 2 * p) / b.h), ZMIN, 1);
		set(nz, (r.width - b.w * nz) / 2 - b.x * nz, (r.height - b.h * nz) / 2 - b.y * nz, anim);
	};
	const reveal = (el) => {
		const v = view.getBoundingClientRect(), r = el.getBoundingClientRect(), m = 32; let dx = 0, dy = 0;
		if (r.left < v.left + m) dx = v.left + m - r.left; else if (r.right > v.right - m) dx = v.right - m - r.right;
		if (r.top < v.top + m) dy = v.top + m - r.top; else if (r.bottom > v.bottom - m) dy = v.bottom - m - r.bottom;
		if (dx || dy) set(z, tx + dx, ty + dy, true);
	};
	const toWorld = (e) => { const r = view.getBoundingClientRect(); return { x: (e.clientX - r.left - tx) / z, y: (e.clientY - r.top - ty) / z }; };
	const centre = (a) => { const r = view.getBoundingClientRect(); zoomAt(a, r.width / 2, r.height / 2, true); };

	d.addEventListener('click', (e) => {
		const t = e.target.closest('[data-cv-zoom],[data-cv-toggle],.ui-cv-env-zoom'); if (!t || !cv.contains(t)) return;
		const a = t.dataset.cvZoom;
		if (a === 'in') centre(1.25); else if (a === 'out') centre(0.8); else if (a === 'fit') fit(null, true); else if (a === 'reset') set(1, tx, ty, true);
		else if (t.dataset.cvToggle) { const on = t.getAttribute('aria-pressed') !== 'true'; t.setAttribute('aria-pressed', on); cv.dataset[t.dataset.cvToggle] = on ? 'on' : 'off'; }
		else { const f = t.closest('.ui-cv-env'); fit({ x: f.offsetLeft, y: f.offsetTop, w: f.offsetWidth, h: f.offsetHeight }, true); }
	});
	view.addEventListener('wheel', (e) => {
		e.preventDefault();
		if (e.ctrlKey || e.metaKey) { const r = view.getBoundingClientRect(); zoomAt(Math.exp(-e.deltaY * 0.01), e.clientX - r.left, e.clientY - r.top); } else set(z, tx - e.deltaX, ty - e.deltaY);
	}, { passive: false });
	view.addEventListener('dblclick', (e) => { if (!e.target.closest('.ui-cv-node,.ui-cv-elabel,button,a')) fit(null, true); });

	/* links between two points: the cubic the server draws, handle to handle */
	const path = (x0, y0, x1, y1) => { const dx = Math.max(40, (x1 - x0) / 2); return y0 === y1 ? `M${x0} ${y0}L${x1} ${y1}` : `M${x0} ${y0}C${x0 + dx} ${y0} ${x1 - dx} ${y1} ${x1} ${y1}`; };
	const portOut = (n) => [+n.dataset.x + NW, +n.dataset.y + NH / 2], portIn = (n) => [+n.dataset.x, +n.dataset.y + NH / 2];

	/* connect: one state machine, three ways in. The server's data-targets say what is valid;
	   it checks again when the popover is confirmed. */
	const ok = (a, b) => a !== b && targets(a).includes(b.dataset.id);
	const reason = (a, b) => a === b ? 'Pick a different resource' : a.dataset.env !== b.dataset.env ? `${b.dataset.name} is in ${b.dataset.envName}. Wiring stays inside one environment` : 'These two cannot be wired. Release to see why';
	const band = (x, y, good = true) => { const p = $('#cv-band'); if (!p) return; const [x0, y0] = portOut(src); p.setAttribute('d', path(x0, y0, x, y)); p.dataset.ok = good; };
	const begin = (s, via) => {
		conn = { via }; src = s; cv.classList.add('is-connecting'); view.classList.add('is-dragging');
		nodes().forEach((n) => { n.dataset.drop = n === s ? 'source' : ok(s, n) ? 'candidate' : 'off'; if (via !== 'drag') n.toggleAttribute('inert', n !== s && !ok(s, n)); });
		if (via === 'drag') { const p = d.createElementNS('http://www.w3.org/2000/svg', 'path'); p.setAttribute('class', 'ui-cv-edge ui-cv-edge-band'); p.id = 'cv-band'; $('#cv-edges').append(p); band(...portOut(s)); }
		const names = targets(s).map((id) => N(id)?.dataset.name).filter(Boolean);
		say(names.length ? `Connecting ${s.dataset.name}. Valid targets: ${names.join(', ')}. ${via === 'drag' ? 'Release on one.' : 'Tab or arrow keys to choose, Enter to connect, Escape to cancel.'}` : `${s.dataset.name} has nothing it can connect to.`);
		if (via !== 'drag') open($('.ui-cv-node[data-drop="candidate"]', world) || s)?.focus();
	};
	const over = (n) => {
		if (hover && hover !== n && hover.dataset.prev !== undefined) { hover.dataset.drop = hover.dataset.prev; delete hover.dataset.prev; }
		hover = n && n !== src ? n : null;
		if (!hover) { tip.removeAttribute('data-on'); return false; }
		const good = ok(src, hover);
		if (hover.dataset.prev === undefined) hover.dataset.prev = hover.dataset.drop;
		hover.dataset.drop = good ? 'valid' : 'invalid';
		tip.dataset.ok = good; tip.dataset.on = '';
		tip.innerHTML = svg(good ? 'plug' : 'ban') + '<span></span>';
		tip.lastChild.textContent = good ? hover.dataset.name : reason(src, hover);
		tip.style.setProperty('--x', hover.dataset.x + 'px'); tip.style.setProperty('--y', +hover.dataset.y - 8 + 'px');
		return good;
	};
	const clear = () => {
		nodes().forEach((n) => { n.removeAttribute('data-drop'); n.removeAttribute('data-prev'); n.removeAttribute('inert'); });
		$('#cv-band')?.remove(); tip.removeAttribute('data-on'); cv.classList.remove('is-connecting', 'is-picking'); view.classList.remove('is-dragging');
		$('[data-cv-connect]')?.setAttribute('aria-pressed', 'false'); conn = hover = null;
	};
	const cancel = (msg) => { const s = src; clear(); if (msg) say(msg); if (s) open(s)?.focus({ preventScroll: true }); };
	/* hands a drop to the server: it answers with the confirm popover, or says why not */
	const drop = (to) => { const from = src, s = to; lastTo = s; clear(); fire('cv-wire', { from: from.dataset.id, to: s.dataset.id }); };

	view.addEventListener('pointerdown', (e) => {
		if (e.button !== 0) return;
		const port = e.target.closest('.ui-cv-port-out');
		if (port && cv.hasAttribute('data-wire')) { e.preventDefault(); begin(port.closest('.ui-cv-node'), 'drag'); view.setPointerCapture(e.pointerId); return; }
		if (conn || e.target.closest('.ui-cv-node,.ui-cv-elabel,button,a')) return;
		pan = { x: e.clientX, y: e.clientY, tx, ty, moved: false }; view.setPointerCapture(e.pointerId);
	});
	let raf = 0, last = null;
	view.addEventListener('pointermove', (e) => {
		last = e; if (raf) return;
		raf = requestAnimationFrame(() => {
			raf = 0; e = last;
			if (conn?.via === 'drag' && view.hasPointerCapture(e.pointerId)) {
				const n = d.elementFromPoint(e.clientX, e.clientY)?.closest('.ui-cv-node[data-id]'), good = over(n);
				if (good) band(...portIn(n), true); else { const p = toWorld(e); band(p.x, p.y, !n); }
			} else if (pan) {
				const dx = e.clientX - pan.x, dy = e.clientY - pan.y;
				if (!pan.moved && Math.hypot(dx, dy) < 4) return;
				pan.moved = true; view.classList.add('is-panning'); set(z, pan.tx + dx, pan.ty + dy);
			}
		});
	});
	view.addEventListener('pointerup', (e) => {
		view.classList.remove('is-panning');
		if (conn?.via === 'drag') {
			const n = d.elementFromPoint(e.clientX, e.clientY)?.closest('.ui-cv-node[data-id]');
			if (n && n !== src) drop(n); else cancel('Connection cancelled');
		} else if (pan && !pan.moved) $$('.ui-cv-node[data-selected]').forEach((n) => n.removeAttribute('data-selected'));
		pan = null;
	});
	view.addEventListener('pointercancel', () => { if (conn?.via === 'drag') cancel(); pan = null; view.classList.remove('is-panning'); });

	/* click then click, and keyboard: while connecting, a click or Enter on a node picks it */
	d.addEventListener('click', (e) => {
		const b = e.target.closest('[data-cv-connect]'); if (!b || !cv.contains(b)) return;
		if (conn || cv.classList.contains('is-picking')) return cancel('Connection cancelled');
		say('Pick the first resource, then the one to connect it to.'); b.setAttribute('aria-pressed', 'true'); cv.classList.add('is-picking');
	});
	view.addEventListener('click', (e) => {
		const a = e.target.closest('.ui-cv-open'); if (!a) return;
		const n = a.closest('.ui-cv-node');
		if (cv.classList.contains('is-picking')) { e.preventDefault(); cv.classList.remove('is-picking'); begin(n, 'click'); return; }
		if (!conn || conn.via === 'drag') return;
		e.preventDefault();
		if (n === src) cancel('Connection cancelled'); else if (ok(src, n)) drop(n); else say(reason(src, n));
	});
	/* the popover closing ends the gesture and returns focus to where it began */
	d.addEventListener('toggle', (e) => {
		if (e.target !== pop) return;
		if (e.newState === 'open') {
			const q = lastTo?.getBoundingClientRect();
			if (q && q.width && !matchMedia('(max-width: 760px)').matches) {
				const w = pop.offsetWidth, h = pop.offsetHeight; let l = q.right + 12; if (l + w > innerWidth - 8) l = Math.max(8, q.left - w - 12);
				pop.style.inset = 'auto'; pop.style.left = l + 'px'; pop.style.top = clamp(q.top - 8, 8, innerHeight - h - 8) + 'px';
			} else { pop.style.inset = ''; pop.style.left = pop.style.top = ''; }
			(pop.querySelector('[autofocus]') || pop.querySelector('button,select'))?.focus();
		} else { const s = src; src = null; if (s && s.isConnected) open(s)?.focus({ preventScroll: true }); lastTo = null; }
	}, true);

	/* highlight along links, on hover and on focus */
	const hl = (id) => {
		$$('.ui-cv-edge[data-from],.ui-cv-elabel', world).forEach((x) => x.classList.toggle('is-hl', !!id && (x.dataset.from === id || x.dataset.to === id)));
		const ends = id ? edges().filter((x) => x.dataset.from === id || x.dataset.to === id).flatMap((x) => [x.dataset.from, x.dataset.to]) : [];
		nodes().forEach((n) => n.toggleAttribute('data-hl', ends.includes(n.dataset.id) && n.dataset.id !== id));
	};
	view.addEventListener('pointerover', (e) => { if (!conn) hl(e.target.closest('.ui-cv-node[data-id]')?.dataset.id); });
	view.addEventListener('pointerleave', () => { if (!conn) hl(null); });
	view.addEventListener('focusin', (e) => { const n = e.target.closest('.ui-cv-node[data-id]'); if (n) { hl(n.dataset.id); reveal(n); nodes().forEach((x) => x.toggleAttribute('data-selected', x === n)); } });

	/* keyboard: Tab visits nodes in reading order, arrows follow links, C connects */
	const follow = (id, dir) => edges().filter((x) => (dir > 0 ? x.dataset.from : x.dataset.to) === id).map((x) => N(dir > 0 ? x.dataset.to : x.dataset.from)).filter((n) => n && !n.hasAttribute('inert'));
	view.addEventListener('keydown', (e) => {
		const n = e.target.closest?.('.ui-cv-node[data-id]'), k = e.key;
		if (k === 'Escape' && conn) { e.preventDefault(); return cancel('Connection cancelled'); }
		if (n && !e.altKey && !e.ctrlKey && !e.metaKey) {
			if ((k === 'c' || k === 'C') && cv.hasAttribute('data-wire')) { e.preventDefault(); return conn ? cancel('Connection cancelled') : begin(n, 'key'); }
			if (!conn && (k === 'ContextMenu' || k === ' ' || (k === 'F10' && e.shiftKey))) { e.preventDefault(); return openMenu(n); }
			if (k.startsWith('Arrow') && !e.shiftKey) {
				e.preventDefault(); let to; const fwd = k === 'ArrowRight' || k === 'ArrowDown';
				if (conn) { const c = $$('.ui-cv-node:not([inert])', world), i = c.indexOf(n); to = c[(i + (fwd ? 1 : -1) + c.length) % c.length]; }
				else if (k === 'ArrowRight') to = follow(n.dataset.id, 1)[0]; else if (k === 'ArrowLeft') to = follow(n.dataset.id, -1)[0];
				else { const col = nodes().filter((x) => x.dataset.env === n.dataset.env && x.dataset.x === n.dataset.x).sort((a, b) => a.dataset.y - b.dataset.y); to = col[col.indexOf(n) + (k === 'ArrowDown' ? 1 : -1)]; }
				if (to) { open(to).focus(); say(`${to.dataset.name}, ${conn ? 'valid target' : k === 'ArrowRight' ? 'linked from here' : k === 'ArrowLeft' ? 'links to here' : 'same column'}`); }
				else say(k === 'ArrowRight' ? `${n.dataset.name} has no outgoing links` : k === 'ArrowLeft' ? `${n.dataset.name} has no incoming links` : 'No more resources in this column');
				return;
			}
		}
		if (e.target === view || n) {
			if (k === '+' || k === '=') centre(1.25); else if (k === '-') centre(0.8); else if (k === '0') set(1, tx, ty, true); else if (k === 'f' || k === 'F') fit(null, true);
			else if (e.shiftKey && k.startsWith('Arrow')) { e.preventDefault(); set(z, tx + (k === 'ArrowLeft' ? 80 : k === 'ArrowRight' ? -80 : 0), ty + (k === 'ArrowUp' ? 80 : k === 'ArrowDown' ? -80 : 0), true); }
		}
	});

	/* one shared menu: node actions and link actions. Built with the DOM, never with markup. */
	const item = (icon, text, act, danger) => { const b = d.createElement('button'); b.type = 'button'; b.className = 'ui-menu-item' + (danger ? ' ui-menu-item-danger' : ''); b.dataset.act = act; b.innerHTML = svg(icon); const s = d.createElement('span'); s.textContent = text; b.append(s); return b; };
	const link = (text, href) => { const a = d.createElement('a'); a.className = 'ui-menu-item'; a.href = href; a.innerHTML = svg('arrow'); const s = d.createElement('span'); s.textContent = text; a.append(s); return a; };
	const showMenu = (label, items, at, owner) => {
		const g = d.createElement('div'); g.dataset.menu = ''; g.setAttribute('focusgroup', 'menu nomemory'); g.setAttribute('aria-label', label); g.append(...items);
		menu.replaceChildren(g); menu._o = owner; menu.showPopover();
		const r = at.getBoundingClientRect();
		menu.style.cssText += `;position:fixed;position-area:none;inset:auto;margin:0;top:${r.bottom + 6}px;left:${clamp(r.right - menu.offsetWidth, 8, innerWidth - menu.offsetWidth - 8)}px`;
		items[0].focus();
	};
	const openMenu = (n, btn) => {
		const items = [link('Open', n.dataset.href)];
		if (cv.hasAttribute('data-wire') && targets(n).length) items.push(item('plug', 'Connect to…', 'connect'));
		items.push(item('list', 'Show in list', 'list'));
		showMenu('Actions for ' + n.dataset.name, items, btn || $('.ui-cv-more', n) || n, n);
	};
	view.addEventListener('click', (e) => {
		const m = e.target.closest('.ui-cv-more'); if (m && !conn) { e.preventDefault(); return openMenu(m.closest('.ui-cv-node'), m); }
		const l = e.target.closest('.ui-cv-elabel'); if (!l) return;
		$$('.ui-cv-edge.is-sel', world).forEach((p) => p.classList.remove('is-sel'));
		edges().find((p) => p.dataset.from === l.dataset.from && p.dataset.to === l.dataset.to && p.dataset.kind === l.dataset.kind)?.classList.add('is-sel');
		const items = [link(l.dataset.kind === 'env' ? 'Open variables' : l.dataset.kind === 'gateway' ? 'Open the model' : 'Open networks', l.dataset.href)];
		if (cv.hasAttribute('data-wire')) items.push(item('unlink', 'Unlink…', 'unlink', true));
		menu._l = l; showMenu(`Link ${l.dataset.label}`, items, l, N(l.dataset.from));
	});
	menu.addEventListener('click', (e) => {
		const b = e.target.closest('[data-act]'); if (!b) return;
		const n = menu._o, l = menu._l; menu._l = null; menu.hidePopover();
		if (b.dataset.act === 'connect') begin(n, 'key');
		else if (b.dataset.act === 'unlink') { src = n; lastTo = N(l.dataset.to); fire('cv-unlink', { from: l.dataset.from, to: l.dataset.to, kind: l.dataset.kind, label: l.dataset.label }); }
		else { $('#cv-v-list').click(); $(`[data-row="${CSS.escape(n.dataset.id)}"] button`)?.focus(); }
	});
	menu.addEventListener('keydown', (e) => {
		if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return;
		const i = $$('.ui-menu-item', menu), j = i.indexOf(d.activeElement); e.preventDefault(); i[(j + (e.key === 'ArrowDown' ? 1 : -1) + i.length) % i.length].focus();
	});

	/* boot: fit once the viewport has a size (it has none while the list view is shown) */
	apply(false);
	new ResizeObserver(() => { if (!fitted && view.clientWidth) { fitted = true; fit(null, false); } }).observe(view);
})();
