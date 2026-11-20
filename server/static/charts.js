/* charts.js: the hover and keyboard layer of a chart. Only pages that draw a chart load it (charts.rs emits the
   tag; a module loads once per page). `data-points` holds every reading; the pointer, or the arrow keys on
   the focused chart, put a crosshair, a dot per series and a card on the nearest one. */
(() => {
	const d = document;
	const $ = (s, r = d) => r.querySelector(s);
	const $$ = (s, r = d) => [...r.querySelectorAll(s)];
	const at = (e) => (e.target instanceof Element ? e.target : null);
	const anchored = CSS.supports("position-area", "block-end");
	const C = (g) => (g._c ??= JSON.parse(g.dataset.points));
	const mk = (tag, props) => Object.assign(d.createElement(tag), props);
	const hide = (g) => $$(".ui-chart-cross,.ui-chart-dot,.ui-chart-tip", g).forEach((o) => (o.hidden = true));
	const show = (g, i, say) => {
		const c = C(g), [t, vs] = c.p[i], tip = $(".ui-chart-tip", g);
		const x = Math.min(1, Math.max(0, (t - c.f) / (c.t - c.f || 1))) * 100;
		const when = `${new Date(t * 1000).toISOString().slice(0, 19).replace("T", " ")} UTC`;
		const text = c.s.map((l, k) => [l, vs[k] ? vs[k][1] : "no reading"]);
		g._i = i;
		$(".ui-chart-cross", g).style.left = `${x}%`;
		$$(".ui-chart-dot", g).forEach((o, k) => {
			o.hidden = !vs[k];
			if (vs[k]) o.style.cssText = `left:${x}%;top:${vs[k][0] * 100}%`;
		});
		tip.replaceChildren(mk("b", { textContent: when }), ...text.map(([l, v], k) => {
			const row = mk("div", { className: "ui-chart-row", textContent: l });
			row.dataset.c = c.c[k];
			return row.append(mk("span", { textContent: v })) || row;
		}));
		$$(".ui-chart-cross,.ui-chart-tip", g).forEach((o) => (o.hidden = false));
		if (!anchored) { /* no anchor positioning: right of the crosshair, left when it would overflow */
			const px = (x / 100) * g.clientWidth, w = tip.offsetWidth;
			tip.style.left = `${px + 12 + w > g.clientWidth ? Math.max(0, px - 12 - w) : px + 12}px`;
		}
		if (say) $("[aria-live]", g).textContent = `${when}, ${text.map((r) => r.join(" ")).join(", ")}`;
	};
	d.addEventListener("pointermove", (e) => {
		const g = at(e)?.closest("[data-points]");
		if (!g) return;
		const c = C(g), r = g.getBoundingClientRect(), t = c.f + ((e.clientX - r.left) / r.width) * (c.t - c.f);
		show(g, c.p.reduce((b, p, i) => (Math.abs(p[0] - t) < Math.abs(c.p[b][0] - t) ? i : b), 0));
	});
	d.addEventListener("pointerout", (e) => {
		const g = at(e)?.closest("[data-points]");
		if (g && !g.contains(e.relatedTarget) && d.activeElement !== g) hide(g);
	});
	d.addEventListener("focusout", (e) => at(e)?.matches("[data-points]") && hide(at(e)));
	d.addEventListener("keydown", (e) => {
		const g = at(e);
		if (!g?.matches("[data-points]")) return;
		const n = C(g).p.length, i = g._i ?? (e.key === "ArrowRight" ? n - 2 : n);
		const to = { ArrowLeft: i - 1, ArrowRight: i + 1, Home: 0, End: n - 1 }[e.key];
		if (e.key === "Escape") hide(g);
		else if (to !== undefined) {
			e.preventDefault();
			show(g, Math.min(n - 1, Math.max(0, to)), true);
		}
	});
})();
