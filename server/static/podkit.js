/* podkit.js: the only behaviour of the UI that neither the platform nor Datastar covers.
   No dependencies, no build step. Everything here is an enhancement: without it menus
   and tooltips still open and close, and dialogs still work.

   1. Menus: arrow keys, Home/End, roles, aria-expanded, close after choosing a command.
   2. Tooltips: Esc dismisses one without moving focus or pointer (WCAG 1.4.13).
   3. Dialogs: click outside closes a `closedby="any"` dialog where the browser lacks
      the attribute. */

(() => {
	/* Invoker commands (`command`/`commandfor`) where the browser lacks them. */
	if (!("commandForElement" in HTMLButtonElement.prototype)) {
		document.addEventListener("click", (e) => {
			const button = e.target instanceof Element ? e.target.closest("button[commandfor]") : null;
			const target = button && document.getElementById(button.getAttribute("commandfor"));
			if (!target) return;
			const command = button.getAttribute("command");
			if (command === "toggle-popover") target.togglePopover();
			else if (command === "show-modal") target.showModal();
			else if (command === "close") target.close();
		});
	}
	const items = (menu) =>
		menu ? [...menu.querySelectorAll(".ui-menu-item")].filter((el) => !el.disabled) : [];
	const triggerOf = (popover) =>
		document.querySelector(`[commandfor="${CSS.escape(popover.id)}"]`);

	/* focusgroup="menu" would infer these roles natively; no browser ships it everywhere. */
	if (!("focusgroup" in HTMLElement.prototype)) {
		for (const menu of document.querySelectorAll("[data-menu]")) {
			menu.setAttribute("role", "menu");
			for (const item of menu.querySelectorAll(".ui-menu-item")) {
				item.setAttribute("role", "menuitem");
			}
			for (const hr of menu.querySelectorAll("hr")) hr.setAttribute("role", "separator");
		}
	}

	/* `toggle` does not bubble: listen in the capture phase. */
	document.addEventListener(
		"toggle",
		(e) => {
			const popover = e.target;
			if (!(popover instanceof HTMLElement) || !popover.matches(".ui-menu")) return;
			const open = e.newState === "open";
			for (const trigger of document.querySelectorAll(
				`[commandfor="${CSS.escape(popover.id)}"]`,
			)) {
				trigger.setAttribute("aria-expanded", String(open));
			}
			/* Only a keyboard open moves focus into the menu; a click leaves it on the trigger. */
			if (open && popover.dataset.viaKeyboard !== undefined) {
				delete popover.dataset.viaKeyboard;
				items(popover.querySelector("[data-menu]"))[0]?.focus();
			}
		},
		true,
	);

	document.addEventListener("keydown", (e) => {
		const target = e.target;
		if (!(target instanceof HTMLElement)) return;

		if (target.matches(".ui-menu-trigger")) {
			const popover = document.getElementById(target.getAttribute("commandfor"));
			if (!popover) return;
			const closed = !popover.matches(":popover-open");
			if (e.key === "ArrowDown") {
				/* ArrowDown opens the menu and enters it. */
				e.preventDefault();
				if (closed) {
					popover.dataset.viaKeyboard = "";
					target.click();
				} else {
					items(popover.querySelector("[data-menu]"))[0]?.focus();
				}
			} else if ((e.key === "Enter" || e.key === " ") && closed) {
				/* Opens natively; remember it was the keyboard. */
				popover.dataset.viaKeyboard = "";
			}
			return;
		}

		const menu = target.closest("[data-menu]");
		if (menu) {
			const list = items(menu);
			const at = list.indexOf(target);
			const go = (i) => {
				e.preventDefault();
				list[(i + list.length) % list.length]?.focus();
			};
			if (e.key === "ArrowDown") go(at + 1);
			else if (e.key === "ArrowUp") go(at - 1);
			else if (e.key === "Home") go(0);
			else if (e.key === "End") go(list.length - 1);
			else if (e.key === "Tab") menu.closest("[popover]")?.hidePopover();
			return;
		}

		/* Esc on a tooltip's trigger hides the tooltip until pointer and focus leave. */
		if (e.key === "Escape") {
			const tip = target.closest(".ui-tooltip");
			if (tip) tip.dataset.dismissed = "";
		}
	});

	/* After a command, the menu closes and focus goes back to its trigger. */
	document.addEventListener("click", (e) => {
		const item = e.target instanceof Element ? e.target.closest(".ui-menu .ui-menu-item") : null;
		const popover = item?.closest(".ui-menu");
		if (!popover) return;
		const trigger = triggerOf(popover);
		popover.hidePopover();
		/* A click on a link navigates away; a command returns focus without a ring after a pointer. */
		if (item.tagName === "BUTTON") trigger?.focus({ focusVisible: e.detail === 0 });
	});

	for (const event of ["pointerleave", "focusout"]) {
		document.addEventListener(
			event,
			(e) => {
				if (e.target instanceof Element) {
					const tip = e.target.closest(".ui-tooltip");
					if (tip) delete tip.dataset.dismissed;
				}
			},
			true,
		);
	}

	/* closedby="any": click outside. Compare the pointer position, not event.target: the
	   <dialog> is also the target for its own padding. Press and release must both be
	   outside, so a text selection that ends on the backdrop does not dismiss. */
	if (!("closedBy" in HTMLDialogElement.prototype)) {
		const outside = (e, dialog) => {
			const r = dialog.getBoundingClientRect();
			return (
				e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom
			);
		};
		for (const dialog of document.querySelectorAll('dialog[closedby="any"]')) {
			let pressedOutside = false;
			dialog.addEventListener("pointerdown", (e) => {
				pressedOutside = outside(e, dialog);
			});
			dialog.addEventListener("pointerup", (e) => {
				if (pressedOutside && outside(e, dialog)) dialog.close();
			});
		}
	}
})();
