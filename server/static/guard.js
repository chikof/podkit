/* guard.js: asks before unsaved input is lost. Pages with a dialog or a save bar load it (the components emit
   the tag; a module loads once per page). Both ask through the shell's `#discard-confirm` dialog. */
(() => {
	const d = document;
	const $ = (s, r = d) => r.querySelector(s);
	const $$ = (s, r = d) => [...r.querySelectorAll(s)];
	const at = (e) => (e.target instanceof Element ? e.target : null);
	const ask = () => new Promise((done) => {
		const c = $("#discard-confirm");
		c.returnValue = "";
		c.addEventListener("close", () => done(c.returnValue === "discard"), { once: true });
		c.showModal();
	});

	/* A dialog with typed-in input asks before Esc or a click outside closes it (the palette never asks).
	   Both arrive as a `cancel` event (page.js sends one where `closedby` is missing). */
	const vals = (r) => $$("input:not([type=hidden]),textarea,select", r).map((e) => (/^(checkbox|radio)$/.test(e.type) ? e.checked : e.value)).join("\0");
	const held = new WeakMap();
	const dirty = (x) => held.has(x) && !x.matches("#palette,#discard-confirm") && vals(x) !== held.get(x);
	/* Discarding gives the fields back their saved values, so the dialog does not reopen with what was thrown away.
	   The events keep Datastar's bound signals in step with the fields. */
	const blank = (x) => $$("form", x).forEach((f) => {
		f.reset();
		$$("input:not([type=hidden]),textarea,select", f).forEach((e) => ["input", "change"].forEach((t) => e.dispatchEvent(new Event(t, { bubbles: true }))));
	});
	const note = (x) => x.open && held.set(x, vals(x));
	$$("dialog[open]").forEach(note);
	new MutationObserver((ms) => ms.forEach((m) => note(m.target)))
		.observe(d.body, { subtree: true, attributes: true, attributeFilter: ["open"] });
	d.addEventListener("cancel", (e) => {
		const x = at(e);
		if (!e.cancelable || !x || !dirty(x)) return;
		e.preventDefault();
		ask().then((ok) => ok && (blank(x), x.close()));
	}, true);

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
})();
