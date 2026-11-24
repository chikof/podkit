/* terminal.js: the web terminal's page script. Loaded only by pages that show a terminal (see
   components/terminal.rs); it loads xterm.js and its fit add-on (vendored, MIT) on demand.

   Wire format, both ways (see crates/proto, TerminalControl / TerminalNotice): binary frames are
   raw terminal bytes, text frames are JSON: {type:"resize",cols,rows} to the server,
   {type:"exit",code} or {type:"error",message} from it. Colors come from the page's CSS tokens
   (the --term-* properties of .ui-terminal), so the terminal follows the light and dark theme. */
const enc = new TextEncoder();
const dec = new TextDecoder();

/* A CSS color of any syntax (oklch, light-dark() already resolved by the browser) as an #rrggbb
   or rgba() string, which is all xterm.js reads reliably. */
const ctx = document.createElement("canvas").getContext("2d", { willReadFrequently: true });
const rgb = (color) => {
	ctx.clearRect(0, 0, 1, 1);
	ctx.fillStyle = "#000";
	ctx.fillStyle = color;
	ctx.fillRect(0, 0, 1, 1);
	const [r, g, b, a] = ctx.getImageData(0, 0, 1, 1).data;
	const hex = (n) => n.toString(16).padStart(2, "0");
	return a === 255 ? `#${hex(r)}${hex(g)}${hex(b)}` : `rgba(${r},${g},${b},${(a / 255).toFixed(2)})`;
};

const COLORS = {
	background: "bg", foreground: "fg", cursor: "cursor", cursorAccent: "bg", selectionBackground: "selection",
	black: "black", red: "red", green: "green", yellow: "yellow", blue: "blue", magenta: "magenta", cyan: "cyan", white: "white",
	brightBlack: "bright-black", brightRed: "red", brightGreen: "green", brightYellow: "yellow",
	brightBlue: "blue", brightMagenta: "magenta", brightCyan: "cyan", brightWhite: "fg",
};

/* Reads every --term-* token through a probe element: `color` is resolved by the browser, a custom
   property holding light-dark() is not. */
const themeOf = (root) => {
	const probe = document.createElement("span");
	probe.hidden = true;
	root.append(probe);
	const theme = {};
	for (const [key, name] of Object.entries(COLORS)) {
		probe.style.color = `var(--term-${name})`;
		theme[key] = rgb(getComputedStyle(probe).color);
	}
	probe.remove();
	return theme;
};

const legacyCopy = (text) => {
	const field = Object.assign(document.createElement("textarea"), { value: text });
	field.setAttribute("readonly", "");
	field.style.cssText = "position:fixed;inset-block-start:0;opacity:0";
	document.body.append(field);
	field.select();
	try { return document.execCommand("copy"); } finally { field.remove(); }
};

const start = async (root) => {
	const $ = (s) => root.querySelector(s);
	const screen = $("[data-screen]");
	const state = $("[data-state]");
	const label = $("[data-label]");
	const note = $("[data-note]");
	const buttons = Object.fromEntries([...root.querySelectorAll("[data-act]")].map((b) => [b.dataset.act, b]));
	const say = (text) => {
		note.textContent = "";
		requestAnimationFrame(() => (note.textContent = text));
		clearTimeout(say.t);
		say.t = setTimeout(() => (note.textContent = ""), 3000);
	};
	const show = (name, text) => {
		state.dataset.state = name;
		label.textContent = text;
		buttons.reconnect.hidden = name === "open" || name === "connecting";
	};

	let Terminal, FitAddon;
	try {
		[{ Terminal }, { FitAddon }] = await Promise.all([import(root.dataset.xterm), import(root.dataset.fit)]);
	} catch {
		show("error", "The terminal could not load");
		return;
	}
	const term = new Terminal({
		fontFamily: getComputedStyle(root).getPropertyValue("--font-mono"),
		fontSize: 13,
		cursorBlink: true,
		scrollback: 5000,
		theme: themeOf(root),
	});
	const fit = new FitAddon();
	term.loadAddon(fit);
	term.open(screen);
	const refit = () => { try { fit.fit(); } catch { /* hidden: nothing to measure */ } };
	refit();

	/* Light/dark can change under us (the toggle, or the system). */
	const retheme = () => (term.options.theme = themeOf(root));
	new MutationObserver(retheme).observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
	matchMedia("(prefers-color-scheme: dark)").addEventListener("change", retheme);

	let ws = null;
	let gen = 0;
	const send = (data) => ws?.readyState === WebSocket.OPEN && ws.send(data);
	term.onData((s) => send(enc.encode(s)));
	term.onBinary((s) => send(Uint8Array.from(s, (c) => c.charCodeAt(0))));
	term.onResize(({ cols, rows }) => send(JSON.stringify({ type: "resize", cols, rows })));

	const connect = () => {
		const mine = ++gen;
		const url = new URL(root.dataset.socket, location.href);
		url.protocol = location.protocol === "https:" ? "wss:" : "ws:";
		url.searchParams.set("cols", term.cols);
		url.searchParams.set("rows", term.rows);
		show("connecting", "Connecting…");
		const socket = (ws = new WebSocket(url));
		socket.binaryType = "arraybuffer";
		let ended = false;
		socket.onopen = () => {
			show("open", "Connected");
			term.focus();
		};
		socket.onmessage = (e) => {
			if (typeof e.data !== "string") return term.write(new Uint8Array(e.data));
			let notice;
			try { notice = JSON.parse(e.data); } catch { return; }
			ended = true;
			if (notice.type === "exit") {
				term.write(`\r\n\x1b[2m[the shell ended${notice.code == null ? "" : ` with status ${notice.code}`}]\x1b[0m\r\n`);
				show("closed", "The shell ended");
			} else if (notice.type === "error") {
				term.write(`\r\n\x1b[2m[${String(notice.message).replace(/[\x00-\x1f\x7f]/g, " ")}]\x1b[0m\r\n`);
				show("error", "Disconnected");
			}
		};
		socket.onclose = () => {
			if (mine !== gen || ended) return;
			show("closed", "Disconnected");
			term.write("\r\n\x1b[2m[disconnected]\x1b[0m\r\n");
		};
	};

	/* Copy and paste. The Clipboard API is withheld over plain HTTP, so copy falls back to a
	   selected text area and paste to the browser's own shortcut (xterm handles the paste event). */
	const copy = async () => {
		const text = term.getSelection();
		if (!text) return say("Select some text first");
		try {
			await navigator.clipboard.writeText(text);
			say("Copied");
		} catch {
			say(legacyCopy(text) ? "Copied" : "Copy failed");
		}
	};
	const paste = async () => {
		try {
			term.paste(await navigator.clipboard.readText());
			term.focus();
		} catch {
			say("Press Ctrl+Shift+V (or ⌘V) to paste");
			term.focus();
		}
	};
	buttons.copy.onclick = copy;
	buttons.paste.onclick = paste;
	buttons.reconnect.onclick = () => { term.write("\r\n"); connect(); };
	/* Ctrl+Shift+C copies the selection; a plain Ctrl+C still goes to the shell. */
	term.attachCustomKeyEventHandler((e) => {
		if (e.type === "keydown" && e.ctrlKey && e.shiftKey && e.code === "KeyC") {
			copy();
			return false;
		}
		return true;
	});

	/* Full screen: the element itself where the browser can, else the same box pinned to the
	   viewport by CSS. */
	const full = buttons.full;
	const setFull = (on) => {
		full.setAttribute("aria-pressed", String(on));
		full.querySelector("span").textContent = on ? "Exit full screen" : "Full screen";
		root.toggleAttribute("data-full", on && !document.fullscreenElement);
		requestAnimationFrame(refit);
	};
	full.onclick = async () => {
		const on = full.getAttribute("aria-pressed") !== "true";
		if (root.requestFullscreen) {
			try {
				if (on) await root.requestFullscreen();
				else if (document.fullscreenElement) await document.exitFullscreen();
				else setFull(false);
				return;
			} catch { /* fall through to the CSS version */ }
		}
		setFull(on);
	};
	document.addEventListener("fullscreenchange", () => setFull(document.fullscreenElement === root));
	root.addEventListener("keydown", (e) => {
		if (e.key === "Escape" && root.hasAttribute("data-full")) setFull(false);
	});

	let frame = 0;
	new ResizeObserver(() => {
		cancelAnimationFrame(frame);
		frame = requestAnimationFrame(refit);
	}).observe(screen);

	root.dataset.ready = "";
	connect();
};

for (const root of document.querySelectorAll("[data-terminal]")) start(root);
