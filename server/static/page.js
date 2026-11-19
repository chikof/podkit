/* page.js: the interaction layer of the design (Quiet). Dialogs, menus, tabs and tooltips are
   native HTML (`commandfor`/`command`, `<dialog closedby>`, `[popover]`, `focusgroup`,
   `popover=hint` + `interestfor`, CSS anchor positioning, radio `:has()` tabs). This file adds
   only what a browser may still lack, each part feature-detected, plus a few touches that no
   browser does: Ctrl/Cmd+K, a menu closing after its command, focus returning to where a dialog
   was opened from, and a theme switch that does not reload the page.

   Delete it and every page renders; menus and dialogs still open where the browser has the
   features. In a debug build `?nofeature=invoker,closedby,interest,anchor,focusgroup` switches
   the named features off, so each fallback can be tested in a browser that has them. */
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

	/* Without CSS anchor positioning the browser centers a popover. Put it under its trigger
	   (above when there is no room), clamped to the viewport; the inline styles also switch the
	   stylesheet's anchor rules off, so this is the same where the feature is forced off. */
	const place = (pop, anchor, centered) => {
		if (native.anchor || !anchor) return;
		pop.style.cssText += ";position:fixed;position-area:none;position-try-fallbacks:none;inset:auto;margin:0";
		const a = anchor.getBoundingClientRect(), w = pop.offsetWidth, h = pop.offsetHeight;
		const left = centered ? a.left + a.width / 2 - w / 2 : a.left;
		const below = a.bottom + 6 + h <= innerHeight || a.top - 6 - h < 0;
		pop.style.left = `${Math.max(8, Math.min(left, innerWidth - w - 8))}px`;
		pop.style.top = `${Math.max(8, below ? a.bottom + 6 : a.top - 6 - h)}px`;
	};

	/* Invoker commands (Baseline since Dec 2025). The native activation is cancelled, so a
	   browser that has them cannot toggle twice. A popover that light dismissal just closed
	   (pointerdown on its own trigger) stays closed. */
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
		/* A dialog opened from a menu item returns focus to the menu's button: the item is gone by then. */
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

	/* `<dialog closedby="any">`: a click outside closes it. Press and release must both be
	   outside, so a text selection that ends on the backdrop does not dismiss. */
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
			if (pressed && outside(e, pressed)) pressed.close();
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
		/* A link navigates away; after a command focus returns, with no ring after a pointer. */
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

	/* Ctrl/Cmd+K opens the command palette, or closes it; another open dialog is left alone. */
	addEventListener("keydown", (e) => {
		const p = $("#palette");
		if (!p || !(e.ctrlKey || e.metaKey) || e.altKey || e.key.toLowerCase() !== "k") return;
		e.preventDefault();
		if (p.open) p.close();
		else if (!$("dialog[open]")) p.showModal();
	});

	/* Theme: the form posts the choice for the cookie; here the page changes at once, with no
	   reload, and the pressed state follows. Without script the form still works. */
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
