/* podkit.js: behaviour of two pages that page.js (the interaction layer of the design: menus,
   tooltips, dialogs, Ctrl+K, theme) does not cover. No dependencies, no build step. Everything
   here is an enhancement.

   1. Command palette (see ui/shell/palette.rs).
   2. Arrow keys across a grid of cards. */

/* 1. Command palette (see ui/shell/palette.rs). The input is a combobox: focus stays in it,
   and the arrow keys move `aria-selected` over the listbox options and
   `aria-activedescendant` on the input. Enter opens the selected option. A fresh result
   list selects its first option; closing the palette puts the initial list back. */
(() => {
	const palette = document.getElementById("palette");
	const input = palette?.querySelector("[data-palette-input]");
	if (!palette || !input) return;
	const list = () => palette.querySelector("[data-palette-list]");
	const options = () => [...(list()?.querySelectorAll('[role="option"]:not([aria-disabled])') ?? [])];
	const initial = list()?.innerHTML;

	const select = (index) => {
		const all = options();
		if (!all.length) {
			input.removeAttribute("aria-activedescendant");
			return;
		}
		const at = ((index % all.length) + all.length) % all.length;
		all.forEach((option, i) => option.setAttribute("aria-selected", String(i === at)));
		input.setAttribute("aria-activedescendant", all[at].id);
		all[at].scrollIntoView({ block: "nearest" });
	};
	const selected = () => options().findIndex((o) => o.getAttribute("aria-selected") === "true");

	new MutationObserver((records) => {
		const box = list();
		if (box && records.some((r) => box.contains(r.target))) select(0);
	}).observe(palette, { childList: true, subtree: true });

	input.addEventListener("keydown", (e) => {
		if (e.isComposing || e.altKey || e.ctrlKey || e.metaKey) return;
		const moves = { ArrowDown: 1, ArrowUp: -1 };
		if (e.key in moves) {
			e.preventDefault();
			select(selected() + moves[e.key]);
		} else if (e.key === "Home" || e.key === "End") {
			if (input.value) return; /* let the text cursor move */
			e.preventDefault();
			select(e.key === "Home" ? 0 : -1);
		} else if (e.key === "Enter") {
			const option = options()[selected()];
			if (option) {
				e.preventDefault();
				option.click();
			}
		}
	});

	/* The pointer selects too, so the highlight is where Enter would go. */
	palette.addEventListener("pointermove", (e) => {
		const option = e.target instanceof Element ? e.target.closest('[role="option"]') : null;
		if (option && option.getAttribute("aria-selected") !== "true") {
			select(options().indexOf(option));
		}
	});

	palette.addEventListener("close", () => {
		input.value = "";
		if (initial !== undefined && list()) list().innerHTML = initial;
		select(0);
	});
})();

/* 2. Arrow keys across a grid of cards (`data-grid-nav`, the template gallery). Tab still
   visits every card's link in order; the arrows add a move to the nearest card in that
   direction by where the cards are drawn, so it follows the responsive columns. When there
   is no card that way the key is left alone, so the page scrolls as usual. */
(() => {
	const SKIP = ':read-write, select, input[type="range"], [role="slider"]';
	const gap = (from, to, key) =>
		key === "ArrowRight" ? to.left - from.right
		: key === "ArrowLeft" ? from.left - to.right
		: key === "ArrowDown" ? to.top - from.bottom
		: from.top - to.bottom;
	const horizontal = (key) => key === "ArrowLeft" || key === "ArrowRight";
	const overlap = (a, b, h) =>
		h ? Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top) > 0
			: Math.min(a.right, b.right) - Math.max(a.left, b.left) > 0;
	const centre = (r, h) => (h ? (r.top + r.bottom) / 2 : (r.left + r.right) / 2);

	document.addEventListener("keydown", (e) => {
		if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey) return;
		if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(e.key)) return;
		const current = e.target instanceof Element ? e.target.closest(".ui-link-card-link") : null;
		const grid = current?.closest("[data-grid-nav]");
		if (!grid || (e.composedPath()[0] instanceof Element && e.composedPath()[0].matches(SKIP))) return;
		const h = horizontal(e.key);
		const from = current.getBoundingClientRect();
		let best = null;
		for (const link of grid.querySelectorAll(".ui-link-card-link")) {
			if (link === current || !link.checkVisibility()) continue;
			const card = link.closest(".ui-link-card") ?? link;
			const to = card.getBoundingClientRect();
			const distance = gap(from, to, e.key);
			if (distance < -1) continue;
			const aligned = overlap(from, to, h);
			const score = aligned
				? distance
				: 10000 + distance + 2 * Math.abs(centre(to, h) - centre(from, h));
			if (!best || score < best.score) best = { link, score };
		}
		if (!best) return;
		e.preventDefault();
		best.link.focus();
		best.link.scrollIntoView({ block: "nearest", inline: "nearest" });
	});
})();
