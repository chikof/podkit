/* page.js: menus, tooltips, dialogs, Ctrl+K and theme on top of native HTML; each part feature-detected.
   Debug builds: ?nofeature=invoker,closedby,interest,anchor,focusgroup forces the fallbacks. */
(() => {
	const d = document;
	const $ = (s, r = d) => r.querySelector(s);
	const $$ = (s, r = d) => [...r.querySelectorAll(s)];
	const off = $('meta[name="podkit-debug"]')
		? (new URLSearchParams(location.search).get("nofeature") || "").split(",")
		: [];
	const has = (name, test) => test && !off.includes(name);
	const native = {
		invoker: has("invoker", "commandForElement" in HTMLButtonElement.prototype),
		closedby: has("closedby", "closedBy" in HTMLDialogElement.prototype),
		interest: has("interest", "interestForElement" in HTMLButtonElement.prototype),
		anchor: has("anchor", CSS.supports("position-area", "block-end")),
		focusgroup: has("focusgroup", "focusGroup" in HTMLElement.prototype),
	};
	const at = (e) => (e.target instanceof Element ? e.target : null);
	const triggerOf = (pop) => $(`[commandfor="${CSS.escape(pop.id)}"]`);

	/* No anchor positioning: put the popover under its trigger (above if no room), inside the viewport. */
	const place = (pop, anchor, centered) => {
		if (native.anchor || !anchor) return;
		pop.style.cssText += ";position:fixed;position-area:none;position-try-fallbacks:none;inset:auto;margin:0";
		const a = anchor.getBoundingClientRect(), w = pop.offsetWidth, h = pop.offsetHeight;
		const left = centered ? a.left + a.width / 2 - w / 2 : a.left;
		const below = a.bottom + 6 + h <= innerHeight || a.top - 6 - h < 0;
		pop.style.left = `${Math.max(8, Math.min(left, innerWidth - w - 8))}px`;
		pop.style.top = `${Math.max(8, below ? a.bottom + 6 : a.top - 6 - h)}px`;
	};

	/* Invoker commands, run here when the browser lacks them. A popover just light-dismissed stays closed. */
	let wasOpen = false, kb = false;
	const dialogBack = new WeakMap();
	d.addEventListener("pointerdown", (e) => {
		const b = at(e)?.closest("button[commandfor]");
		wasOpen = !!b && !!d.getElementById(b.getAttribute("commandfor"))?.matches(":popover-open");
	}, true);
	d.addEventListener("click", (e) => {
		const b = at(e)?.closest("button[commandfor]");
		if (!b) return;
		kb = e.detail === 0;
		const t = d.getElementById(b.getAttribute("commandfor")), c = b.getAttribute("command");
		/* A dialog opened from a menu item gives focus back to the menu button. */
		const menu = b.closest(".ui-menu");
		if (t && c === "show-modal" && menu) dialogBack.set(t, triggerOf(menu));
		if (native.invoker || !t) return;
		e.preventDefault();
		if (c === "show-modal") t.showModal();
		else if (c === "close") t.close();
		else if (c === "toggle-popover" && !wasOpen) t.togglePopover({ source: b });
	});
	d.addEventListener("close", (e) => {
		const back = dialogBack.get(e.target);
		if (back) setTimeout(() => d.activeElement === d.body && back.focus(), 0);
	}, true);

	/* A dialog with typed-in input asks before Esc or a click outside closes it (the palette never asks). */
	const vals = (r) => $$("input:not([type=hidden]),textarea,select", r).map((e) => (/^(checkbox|radio)$/.test(e.type) ? e.checked : e.value)).join("\0");
	const held = new WeakMap();
	const dirty = (x) => held.has(x) && !x.matches("#palette,#discard-confirm") && vals(x) !== held.get(x);
	new MutationObserver((ms) => ms.forEach((m) => m.target.open && held.set(m.target, vals(m.target))))
		.observe(d.body, { subtree: true, attributes: true, attributeFilter: ["open"] });
	const ask = () => new Promise((done) => {
		const c = $("#discard-confirm");
		c.returnValue = "";
		c.addEventListener("close", () => done(c.returnValue === "discard"), { once: true });
		c.showModal();
	});
	const leave = (x) => (dirty(x) ? ask().then((ok) => ok && x.close()) : x.close());
	d.addEventListener("cancel", (e) => {
		if (e.cancelable && at(e) && dirty(at(e))) {
			e.preventDefault();
			leave(at(e));
		}
	}, true);

	/* `closedby="any"`: a click outside closes the dialog (press and release both outside). */
	if (!native.closedby) {
		let pressed = null;
		const outside = (e, dlg) => {
			const r = dlg.getBoundingClientRect();
			return e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom;
		};
		d.addEventListener("pointerdown", (e) => {
			const dlg = at(e)?.closest('dialog[closedby="any"]') || (at(e)?.matches("dialog") && at(e));
			pressed = dlg && dlg.matches('[closedby="any"]') && outside(e, dlg) ? dlg : null;
		}, true);
		d.addEventListener("pointerup", (e) => {
			if (pressed && outside(e, pressed)) leave(pressed);
			pressed = null;
		}, true);
	}

	/* Menus: aria-expanded, placement, entering by keyboard, and closing after a command. */
	d.addEventListener("toggle", (e) => {
		const m = e.target;
		if (!(m instanceof HTMLElement) || !m.matches(".ui-menu")) return;
		const open = e.newState === "open", trigger = triggerOf(m);
		for (const t of $$(`[commandfor="${CSS.escape(m.id)}"]`)) t.setAttribute("aria-expanded", String(open));
		if (!open) return;
		place(m, trigger, false);
		if (kb) $(".ui-menu-item", m)?.focus();
		kb = false;
	}, true);
	d.addEventListener("keydown", (e) => {
		const t = at(e);
		if (!t) return;
		if (e.key === "ArrowDown" && t.matches('[commandfor][aria-haspopup="menu"]')) {
			const m = d.getElementById(t.getAttribute("commandfor"));
			e.preventDefault();
			kb = true;
			if (m.matches(":popover-open")) $(".ui-menu-item", m)?.focus();
			else m.showPopover({ source: t });
		} else if (e.key === "Tab" && t.closest(".ui-menu")) t.closest(".ui-menu").hidePopover();
	});
	d.addEventListener("click", (e) => {
		const item = at(e)?.closest(".ui-menu .ui-menu-item");
		const m = item?.closest(".ui-menu");
		if (!m) return;
		const trigger = triggerOf(m);
		m.hidePopover();
		/* After a command, focus returns to the trigger (no ring after a pointer). */
		if (item.tagName === "BUTTON" && !item.matches("[commandfor]")) trigger?.focus({ focusVisible: e.detail === 0 });
	});

	/* `focusgroup="menu"` gives the roles and Up, Down, Home and End natively. */
	const roving = () => {
		if (native.focusgroup) return;
		for (const g of $$('[focusgroup~="menu"]:not([data-roving])')) {
			g.dataset.roving = "";
			g.setAttribute("role", "menu");
			for (const i of $$(".ui-menu-item:not([role])", g)) i.setAttribute("role", "menuitem");
			for (const hr of $$("hr", g)) hr.setAttribute("role", "separator");
			g.addEventListener("keydown", (e) => {
				const items = $$(".ui-menu-item:not(:disabled)", g), i = items.indexOf(d.activeElement);
				if (i < 0 || !["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
				e.preventDefault();
				e.stopImmediatePropagation();
				const to = e.key === "Home" ? 0 : e.key === "End" ? items.length - 1 : (i + (e.key === "ArrowUp" ? -1 : 1) + items.length) % items.length;
				items[to].focus({ preventScroll: true });
			}, true);
		}
	};
	roving();
	new MutationObserver(roving).observe(d.body, { childList: true, subtree: true });

	/* Tooltips: `interestfor` opens the hint on hover and focus; without it, do the same. */
	if (!native.interest) {
		const hint = (e) => {
			const b = at(e)?.closest("[interestfor]");
			return b && [b, d.getElementById(b.getAttribute("interestfor"))];
		};
		const show = (e) => {
			const [b, t] = hint(e) || [];
			if (!t || t.matches(":popover-open")) return;
			t.showPopover();
			place(t, b, true);
		};
		const hide = (e) => {
			const [, t] = hint(e) || [];
			if (t?.matches(":popover-open")) t.hidePopover();
		};
		d.addEventListener("pointerover", show);
		d.addEventListener("focusin", show);
		d.addEventListener("pointerout", hide);
		d.addEventListener("focusout", hide);
		d.addEventListener("keydown", (e) => {
			if (e.key === "Escape") $$('[popover="hint"]:popover-open').forEach((t) => t.hidePopover());
		});
	}

	/* Ctrl/Cmd+K toggles the command palette; another open dialog is left alone. */
	addEventListener("keydown", (e) => {
		const p = $("#palette");
		if (!p || !(e.ctrlKey || e.metaKey) || e.altKey || e.key.toLowerCase() !== "k") return;
		e.preventDefault();
		if (p.open) p.close();
		else if (!$("dialog[open]")) p.showModal();
	});

	/* Charts: `data-points` (charts.rs) holds every reading; the pointer, or arrow keys on the focused
	   chart, put a crosshair, a dot per series and a card on the nearest one. */
	const C = (g) => (g._c ??= JSON.parse(g.dataset.points));
	const mk = (tag, props) => Object.assign(d.createElement(tag), props);
	const hideChart = (g) => $$(".ui-chart-cross,.ui-chart-dot,.ui-chart-tip", g).forEach((o) => (o.hidden = true));
	const showChart = (g, i, say) => {
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
		if (!native.anchor) { /* no anchor positioning: right of the crosshair, left when it would overflow */
			const px = (x / 100) * g.clientWidth, w = tip.offsetWidth;
			tip.style.left = `${px + 12 + w > g.clientWidth ? Math.max(0, px - 12 - w) : px + 12}px`;
		}
		if (say) $("[aria-live]", g).textContent = `${when}, ${text.map((r) => r.join(" ")).join(", ")}`;
	};
	d.addEventListener("pointermove", (e) => {
		const g = at(e)?.closest("[data-points]");
		if (!g) return;
		const c = C(g), r = g.getBoundingClientRect(), t = c.f + ((e.clientX - r.left) / r.width) * (c.t - c.f);
		showChart(g, c.p.reduce((b, p, i) => (Math.abs(p[0] - t) < Math.abs(c.p[b][0] - t) ? i : b), 0));
	});
	d.addEventListener("pointerout", (e) => {
		const g = at(e)?.closest("[data-points]");
		if (g && !g.contains(e.relatedTarget) && d.activeElement !== g) hideChart(g);
	});
	d.addEventListener("focusout", (e) => at(e)?.matches("[data-points]") && hideChart(at(e)));
	d.addEventListener("keydown", (e) => {
		const g = at(e);
		if (!g?.matches("[data-points]")) return;
		const n = C(g).p.length, i = g._i ?? (e.key === "ArrowRight" ? n - 2 : n);
		const to = { ArrowLeft: i - 1, ArrowRight: i + 1, Home: 0, End: n - 1 }[e.key];
		if (e.key === "Escape") hideChart(g);
		else if (to !== undefined) {
			e.preventDefault();
			showChart(g, Math.min(n - 1, Math.max(0, to)), true);
		}
	});

	/* `data-dirty` on a save bar (save_bar.rs) is "unsaved changes": a link asks first, a reload gets the browser's prompt. */
	let leaving = false;
	const unsaved = () => $(".ui-save-bar[data-dirty]");
	d.addEventListener("click", (e) => {
		const a = at(e)?.closest("a[href]");
		if (!a || !unsaved() || e.defaultPrevented || e.button || e.ctrlKey || e.metaKey || e.shiftKey || a.target || a.origin !== location.origin) return;
		if (a.hash && a.pathname + a.search === location.pathname + location.search) return;
		e.preventDefault();
		ask().then((ok) => ok && ((leaving = true), (location.href = a.href)));
	});
	addEventListener("beforeunload", (e) => !leaving && unsaved() && e.preventDefault());

	/* Theme: the form posts for the cookie; the page changes at once. Works without script too. */
	d.addEventListener("submit", (e) => {
		const form = at(e);
		if (!form?.matches('form[action="/theme"]')) return;
		const choice = e.submitter?.value;
		if (!["system", "light", "dark"].includes(choice)) return;
		e.preventDefault();
		const root = d.documentElement;
		root.classList.remove("light", "dark");
		if (choice !== "system") root.classList.add(choice);
		const meta = $('meta[name="color-scheme"]');
		if (meta) meta.content = choice === "system" ? "dark light" : choice;
		$$("[aria-pressed]", form).forEach((b) => b.setAttribute("aria-pressed", String(b === e.submitter)));
		fetch("/theme", { method: "POST", body: new URLSearchParams({ theme: choice }), redirect: "manual", credentials: "same-origin" });
	});
})();
