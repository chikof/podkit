/* passkey.js: the browser half of passkeys (WebAuthn); the server half is two_factor::passkeys. Loaded only by
   the pages that use them: sign in, the second step of a sign-in, and Account > Security. Plain DOM, no libraries.
   A button asks for a ceremony with data-passkey="register|login|second"; messages go to [data-passkey-status].
   parse*OptionsFromJSON() and toJSON() are Baseline 2025 ("newly available"); the small converters below stand in
   for them in older browsers. Passkeys need a secure context (HTTPS or localhost), so the buttons stay hidden
   without one and [data-passkey-note] explains why. */
const d = document;
const $ = (s, r = d) => r.querySelector(s);
const PKC = window.PublicKeyCredential;
const usable = !!PKC && window.isSecureContext && !!navigator.credentials;

const dec = (s) => Uint8Array.from(atob(s.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0)).buffer;
const enc = (b) => btoa(String.fromCharCode(...new Uint8Array(b))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const ids = (list) => (list || []).map((c) => ({ ...c, id: dec(c.id) }));

const forCreate = (j) => PKC.parseCreationOptionsFromJSON ? PKC.parseCreationOptionsFromJSON(j)
	: { ...j, challenge: dec(j.challenge), user: { ...j.user, id: dec(j.user.id) }, excludeCredentials: ids(j.excludeCredentials) };
const forGet = (j) => PKC.parseRequestOptionsFromJSON ? PKC.parseRequestOptionsFromJSON(j)
	: { ...j, challenge: dec(j.challenge), allowCredentials: ids(j.allowCredentials) };
const plain = (c) => {
	if (c.toJSON) return c.toJSON();
	const r = c.response, out = { id: c.id, rawId: enc(c.rawId), type: c.type, clientExtensionResults: c.getClientExtensionResults() };
	out.response = { clientDataJSON: enc(r.clientDataJSON) };
	if (r.attestationObject) Object.assign(out.response, { attestationObject: enc(r.attestationObject), transports: r.getTransports?.() || [] });
	else Object.assign(out.response, { authenticatorData: enc(r.authenticatorData), signature: enc(r.signature), userHandle: r.userHandle && enc(r.userHandle) });
	return out;
};

const post = async (url, body) => {
	const res = await fetch(url, { method: "POST", headers: { "content-type": "application/json" }, credentials: "same-origin", body: JSON.stringify(body || {}) });
	const json = await res.json().catch(() => ({}));
	if (!res.ok) throw Object.assign(new Error(json.error || "That did not work. Try again."), { server: true });
	return json;
};

const say = (btn, text) => {
	const out = btn.closest("[data-passkey-scope]")?.querySelector("[data-passkey-status]") || $("[data-passkey-status]");
	if (out) out.textContent = text;
};

const explain = (e) => ({
	NotAllowedError: "Cancelled, or it timed out.",
	InvalidStateError: "This device already has a passkey for your account.",
	SecurityError: "Passkeys need HTTPS (or localhost), and the address you use must stay the same.",
	AbortError: "Cancelled.",
})[e.name] || (e.server ? e.message : "The passkey could not be used.");

const reveal = (codes) => {
	const box = $("[data-passkey-codes]");
	if (!box || !codes?.length) return false;
	const list = $("ol", box);
	list.replaceChildren(...codes.map((c) => Object.assign(d.createElement("li"), { textContent: c })));
	box.hidden = false;
	$("[data-passkey-register-form]")?.setAttribute("hidden", "");
	box.querySelector("a,button")?.focus();
	return true;
};

const run = {
	async register() {
		const name = $("#passkey-name")?.value || "";
		const { flow, options } = await post("/account/passkeys/start");
		const publicKey = forCreate(options.publicKey);
		const made = await navigator.credentials.create({ publicKey });
		const credential = plain(made);
		try {
			const { codes } = await post("/account/passkeys/finish", { flow, name, credential });
			if (!reveal(codes)) location.reload();
		} catch (e) {
			// The authenticator holds a passkey the server refused: tell the password manager it is orphaned.
			await PKC.signalUnknownCredential?.({ rpId: publicKey.rp.id, credentialId: credential.id }).catch(() => {});
			throw e;
		}
	},
	async login() {
		const email = $("#si-email")?.value.trim();
		if (!email) throw Object.assign(new Error("Enter your email first, then use your passkey."), { server: true });
		const { flow, options } = await post("/login/passkey/start", { email });
		const got = await navigator.credentials.get({ publicKey: forGet(options.publicKey) });
		const { next } = await post("/login/passkey/finish", { flow, credential: plain(got) });
		const to = new URLSearchParams(location.search).get("next");
		location.assign(to && to.startsWith("/") && !to.startsWith("//") ? to : next);
	},
	async second() {
		const { flow, options } = await post("/login/two-step/passkey/start");
		const got = await navigator.credentials.get({ publicKey: forGet(options.publicKey) });
		const { next } = await post("/login/two-step/passkey/finish", { flow, credential: plain(got) });
		location.assign(next);
	},
};

const buttons = [...d.querySelectorAll("[data-passkey]")];
buttons.forEach((btn) => {
	btn.hidden = !usable;
	btn.addEventListener("click", async () => {
		btn.disabled = true;
		say(btn, "");
		try { await run[btn.dataset.passkey](btn); } catch (e) { say(btn, explain(e)); } finally { btn.disabled = false; }
	});
});
if (!usable) d.querySelectorAll("[data-passkey-note]").forEach((n) => (n.hidden = false));

/* Account > Security: keep the password manager's list in step with the server's (Signal API, Chrome and Safari). */
const sync = $("[data-passkey-sync]");
if (usable && sync && PKC.signalAllAcceptedCredentials) {
	const ok = (v) => /^[A-Za-z0-9_-]+$/.test(v) && v.length % 4 !== 1;
	const list = (sync.dataset.credentialIds || "").split(" ").filter(Boolean);
	if (sync.dataset.rpId && ok(sync.dataset.userId || "") && list.every(ok)) {
		PKC.signalAllAcceptedCredentials({ rpId: sync.dataset.rpId, userId: sync.dataset.userId, allAcceptedCredentialIds: list })
			.catch((e) => console.warn("signalAllAcceptedCredentials:", e));
	}
}
