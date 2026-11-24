#!/usr/bin/env python3
"""Crawls a running podkit in Chromium and Firefox and reports, per page x width x theme:

  unnamed   controls without an accessible name (links, buttons, fields, the select's own button)
  dup_ids   ids that occur more than once in the document
  overflow  the page scrolls sideways

Standard library only: it logs in over HTTP, finds pages by following the links of the team
it signs in to (up to --per-template pages of each URL shape), starts the browsers itself
(Chromium over the DevTools protocol, Firefox over Marionette) and stops them again.

  scripts/a11y-crawl.py --base http://127.0.0.1:8080 --email you@example.com --password ... \
      --chromium /path/to/chromium --firefox /path/to/firefox --out crawl.json

Exit status 1 when anything is found. The last line is `unnamed=N dup_ids=N overflow=N`.
"""

import argparse
import base64
import http.cookiejar
import json
import os
import re
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request

# Runs in the page. Returns what is wrong, nothing else.
AUDIT_JS = r"""(() => {
  const vis = (e) => e.checkVisibility({ checkVisibilityCSS: true, checkOpacity: false });
  const text = (n, seen) => {
    if (n.nodeType === 3) return n.textContent;
    if (n.nodeType !== 1 || seen.has(n)) return '';
    const cs = getComputedStyle(n);
    if (n.getAttribute('aria-hidden') === 'true' || n.hidden || cs.display === 'none' || cs.visibility === 'hidden') return '';
    if (n.tagName === 'IMG') return n.getAttribute('alt') || '';
    if (n.tagName === 'svg') return n.getAttribute('aria-label') || n.querySelector(':scope > title')?.textContent || '';
    return [...n.childNodes].map((c) => text(c, seen)).join(' ');
  };
  const byId = (ids) => ids.split(/\s+/).map((i) => document.getElementById(i)).filter(Boolean);
  const name = (e) => {
    const lb = e.getAttribute('aria-labelledby');
    if (lb) { const t = byId(lb).map((x) => text(x, new Set())).join(' ').trim(); if (t) return t; }
    const al = (e.getAttribute('aria-label') || '').trim();
    if (al) return al;
    if (e.labels && e.labels.length) { const t = [...e.labels].map((l) => text(l, new Set([e]))).join(' ').trim(); if (t) return t; }
    if (e.tagName === 'INPUT' && /^(submit|button|reset)$/.test(e.type)) return (e.value || '').trim();
    if (e.tagName === 'INPUT' && e.type === 'image') return (e.alt || '').trim();
    if (!/^(INPUT|SELECT|TEXTAREA)$/.test(e.tagName)) { const t = text(e, new Set()).trim(); if (t) return t; }
    return (e.getAttribute('title') || '').trim();
  };
  const sel = 'a[href],button,input:not([type=hidden]),select,textarea,summary,[role=button],[role=link],[role=tab],[role=menuitem],[role=checkbox],[role=switch],[role=combobox],[role=textbox]';
  const unnamed = [...document.querySelectorAll(sel)].filter((e) => (vis(e) || e.matches('select > button')) && !e.closest('[inert]') && !name(e))
    .map((e) => e.tagName.toLowerCase() + (e.id ? '#' + e.id : '') + (e.className && typeof e.className === 'string' ? '.' + e.className.split(/\s+/)[0] : '') + (e.parentElement ? ' in ' + e.parentElement.tagName.toLowerCase() + (typeof e.parentElement.className === 'string' && e.parentElement.className ? '.' + e.parentElement.className.split(/\s+/)[0] : '') : ''));
  const count = {};
  for (const e of document.querySelectorAll('[id]')) count[e.id] = (count[e.id] || 0) + 1;
  const dup = Object.keys(count).filter((k) => count[k] > 1);
  const de = document.documentElement;
  const wide = [...document.querySelectorAll('body *')].filter((e) => { const r = e.getBoundingClientRect(); return r.width > 0 && r.right > de.clientWidth + 1 && vis(e) && !e.closest('[popover],dialog:not([open]),.ui-visually-hidden,pre,.ui-log,[data-scroll]'); }).slice(0, 3)
    .map((e) => e.tagName.toLowerCase() + (typeof e.className === 'string' && e.className ? '.' + e.className.split(/\s+/)[0] : ''));
  return { unnamed, dup, overflow: de.scrollWidth > de.clientWidth + 1, sw: de.scrollWidth, cw: de.clientWidth, wide, h1: document.querySelectorAll('h1').length };
})()"""


class Site:
	"""Logs in over HTTP and lists the pages worth visiting."""

	def __init__(self, base, email, password):
		self.base = base.rstrip("/")
		self.jar = http.cookiejar.CookieJar()
		self.op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))
		body = json.dumps({"email": email, "password": password}).encode()
		req = urllib.request.Request(
			self.base + "/login", body, {"Content-Type": "application/json", "Datastar-Request": "true"}
		)
		self.op.open(req).read()
		self.cookies = {c.name: c.value for c in self.jar}
		if "session" not in self.cookies:
			sys.exit("login failed: no session cookie")

	def get(self, path):
		try:
			with self.op.open(self.base + path) as r:
				return r.read().decode("utf-8", "replace") if "html" in r.headers.get("content-type", "") else ""
		except Exception:
			return ""

	def pages(self, per_template, limit):
		shape = lambda p: re.sub(r"/templates/[^/]+", "/templates/{slug}", re.sub(r"\d{6,}", "{id}", p.split("?")[0]))
		seen, per, queue, out = set(), {}, ["/"], []
		skip = re.compile(r"^/(logout|static|api|ws|theme|assets|status/|events|sse)|/(delete|export|download)$|/ws/")
		while queue and len(out) < limit:
			path = queue.pop(0)
			if path in seen:
				continue
			seen.add(path)
			key = shape(path)
			if skip.search(path) or per.get(key, 0) >= per_template:
				continue
			html = self.get(path)
			if not html:
				continue
			per[key] = per.get(key, 0) + 1
			out.append(path)
			for h in re.findall(r'href="(/[^"#]*)"', html):
				h = h.replace("&amp;", "&")
				if h not in seen and not skip.search(h):
					queue.append(h)
		return out


def ws_connect(url):
	u = urllib.parse.urlparse(url)
	s = socket.create_connection((u.hostname, u.port), timeout=60)
	key = base64.b64encode(os.urandom(16)).decode()
	s.sendall(
		(f"GET {u.path} HTTP/1.1\r\nHost: {u.netloc}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
		 f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode()
	)
	head = b""
	while b"\r\n\r\n" not in head:
		head += s.recv(1)
	return s


def ws_send(s, text):
	data = text.encode()
	n = len(data)
	hdr = bytes([0x81])
	hdr += bytes([0x80 | n]) if n < 126 else bytes([0x80 | 126]) + struct.pack(">H", n) if n < 65536 else bytes([0x80 | 127]) + struct.pack(">Q", n)
	mask = os.urandom(4)
	s.sendall(hdr + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))


def ws_recv(s):
	def need(n):
		b = b""
		while len(b) < n:
			c = s.recv(n - len(b))
			if not c:
				raise ConnectionError("closed")
			b += c
		return b

	msg = b""
	while True:
		b1, b2 = need(2)
		n = b2 & 0x7F
		if n == 126:
			n = struct.unpack(">H", need(2))[0]
		elif n == 127:
			n = struct.unpack(">Q", need(8))[0]
		payload = need(n)
		if b1 & 0x0F in (1, 0, 2):
			msg += payload
		if b1 & 0x80:
			return msg.decode()


class Chromium:
	name = "chromium"

	def __init__(self, binary, base, port=int(os.environ.get("PK_PORT", "29330")) + 1):
		self.dir = tempfile.mkdtemp(prefix="pk-a11y-cr-")
		self.proc = subprocess.Popen(
			[binary, "--headless=new", "--no-sandbox", "--disable-gpu", f"--remote-debugging-port={port}",
			 f"--user-data-dir={self.dir}", "about:blank"],
			stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
		)
		for _ in range(100):
			try:
				tabs = json.load(urllib.request.urlopen(f"http://127.0.0.1:{port}/json"))
				tab = next(t for t in tabs if t["type"] == "page")
				break
			except Exception:
				time.sleep(0.2)
		else:
			raise RuntimeError("chromium did not start")
		self.s = ws_connect(tab["webSocketDebuggerUrl"])
		self.n = 0
		self.cmd("Page.enable")

	def cmd(self, method, **params):
		self.n += 1
		ws_send(self.s, json.dumps({"id": self.n, "method": method, "params": params}))
		while True:
			r = json.loads(ws_recv(self.s))
			if r.get("id") == self.n:
				if "error" in r:
					raise RuntimeError(f"{method}: {r['error']}")
				return r["result"]

	def eval(self, expr):
		r = self.cmd("Runtime.evaluate", expression=expr, returnByValue=True, awaitPromise=True)
		if "exceptionDetails" in r:
			raise RuntimeError(str(r["exceptionDetails"])[:300])
		return r["result"].get("value")

	def resize(self, w, h=900):
		self.cmd("Emulation.setDeviceMetricsOverride", width=w, height=h, deviceScaleFactor=1, mobile=False)

	def goto(self, url):
		self.cmd("Page.navigate", url=url)
		self.settle()

	def settle(self):
		for _ in range(100):
			if self.eval("document.readyState") == "complete":
				break
			time.sleep(0.1)
		time.sleep(0.5)

	def shot(self, path):
		open(path, "wb").write(base64.b64decode(self.cmd("Page.captureScreenshot", format="png")["data"]))

	def key(self, key, code, vk):
		for t in ("rawKeyDown", "keyUp"):
			self.cmd("Input.dispatchKeyEvent", type=t, key=key, code=code, windowsVirtualKeyCode=vk)

	def click(self, x, y):
		for t in ("mousePressed", "mouseReleased"):
			self.cmd("Input.dispatchMouseEvent", type=t, x=x, y=y, button="left", clickCount=1)

	def close(self):
		self.proc.terminate()


class Firefox:
	"""Firefox over WebDriver BiDi (its own `--remote-debugging-port`): unlike a window, the viewport of
	a page can be narrower than 500 px, so 390 px is a real 390."""

	name = "firefox"

	def __init__(self, binary, base, port=int(os.environ.get("PK_PORT", "29330")) + 12):
		self.dir = tempfile.mkdtemp(prefix="pk-a11y-ff-")
		open(os.path.join(self.dir, "user.js"), "w").write(
			'user_pref("browser.shell.checkDefaultBrowser", false);\n'
			'user_pref("datareporting.policy.dataSubmissionEnabled", false);\n'
		)
		self.proc = subprocess.Popen(
			[binary, "--headless", f"--remote-debugging-port={port}", "--profile", self.dir, "--no-remote", "--window-size=1440,900"],
			stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
		)
		for _ in range(150):
			try:
				self.s = ws_connect(f"ws://127.0.0.1:{port}/session")
				break
			except OSError:
				time.sleep(0.2)
		else:
			raise RuntimeError("firefox did not start")
		self.n = 0
		self.cmd("session.new", {"capabilities": {}})
		self.ctx = self.cmd("browsingContext.getTree", {})["contexts"][0]["context"]

	def cmd(self, method, params):
		self.n += 1
		ws_send(self.s, json.dumps({"id": self.n, "method": method, "params": params}))
		while True:
			r = json.loads(ws_recv(self.s))
			if r.get("id") == self.n:
				if r.get("type") == "error":
					raise RuntimeError(f"{method}: {r.get('message')}")
				return r["result"]

	def eval(self, expr):
		r = self.cmd("script.evaluate", {
			"expression": "JSON.stringify((" + expr + "))", "target": {"context": self.ctx}, "awaitPromise": True,
		})
		if r["type"] == "exception":
			raise RuntimeError(str(r["exceptionDetails"])[:300])
		v = r["result"].get("value")
		return None if v is None else json.loads(v)

	def resize(self, w, h=900):
		self.cmd("browsingContext.setViewport", {"context": self.ctx, "viewport": {"width": w, "height": h}, "devicePixelRatio": 1})

	def goto(self, url):
		self.cmd("browsingContext.navigate", {"context": self.ctx, "url": url, "wait": "complete"})
		self.settle()

	def settle(self):
		time.sleep(0.5)

	def shot(self, path):
		open(path, "wb").write(base64.b64decode(self.cmd("browsingContext.captureScreenshot", {"context": self.ctx})["data"]))

	def key(self, key, code, vk):
		k = {"Tab": "\ue004", "Escape": "\ue00c", "Enter": "\ue007"}[key]
		self.cmd("input.performActions", {"context": self.ctx, "actions": [{"type": "key", "id": "k", "actions": [
			{"type": "keyDown", "value": k}, {"type": "keyUp", "value": k}]}]})

	def click(self, x, y):
		self.cmd("input.performActions", {"context": self.ctx, "actions": [{"type": "pointer", "id": "m", "parameters": {"pointerType": "mouse"}, "actions": [
			{"type": "pointerMove", "x": x, "y": y}, {"type": "pointerDown", "button": 0}, {"type": "pointerUp", "button": 0}]}]})

	def close(self):
		self.proc.terminate()


def login(b, site):
	b.goto(site.base + "/login")
	js = ("fetch('/login',{method:'POST',headers:{'Content-Type':'application/json','Datastar-Request':'true'},"
	      "body:JSON.stringify({email:%s,password:%s})}).then(r=>r.status)")
	# The session cookie is the one the HTTP login got: set it on the browser instead of typing the password again.
	b.eval("(() => { document.cookie = %s; return 1 })()" % json.dumps(f"session={site.cookies['session']}; path=/"))
	del js


# Runs in the page after each Tab: where focus is and whether anything shows it.
FOCUS_JS = r"""(() => {
  const e = document.activeElement;
  if (!e || e === document.body) return null;
  const shows = (x) => {
    const cs = getComputedStyle(x);
    const ring = cs.outlineStyle !== 'none' && parseFloat(cs.outlineWidth) > 0;
    const shadow = cs.boxShadow !== 'none' && /\d+px/.test(cs.boxShadow.replace(/rgba?\([^)]*\)/g, ''));
    return ring || shadow;
  };
  let ring = shows(e);
  for (let x = e.parentElement, i = 0; !ring && x && i < 4; x = x.parentElement, i++) ring = shows(x) && x.matches(':has(:focus-visible)');
  if (!ring) { const a = getComputedStyle(e, '::after'); ring = a.outlineStyle !== 'none' && parseFloat(a.outlineWidth) > 0; }
  if (!ring && e.matches('.ui-menu-item,option')) ring = true; // a menu item is shown by its wash
  const r = e.getBoundingClientRect();
  const label = (e.getAttribute('aria-label') || e.innerText || e.value || e.getAttribute('title') || e.name || '').trim().replace(/\s+/g, ' ').slice(0, 40);
  return { tag: e.tagName.toLowerCase() + (e.id ? '#' + e.id : ''), label, ring, inMain: !!e.closest('main'), dlg: !!e.closest('dialog[open]'),
    visible: r.width > 0 && r.height > 0 && r.bottom > 0 && r.top < innerHeight, trap: false };
})()"""


def walk(b, site, path, stops, shots=None, tag=""):
	"""Tabs through a page like a person: records each stop, flags the ones with no focus ring
	or that are off screen, and stops when focus comes back to where it began."""
	b.goto(site.base + path)
	b.eval("(() => { document.activeElement && document.activeElement.blur(); window.scrollTo(0, 0); return 1 })()")
	seen, out, shot_at = [], [], None
	for i in range(stops):
		b.key("Tab", "Tab", 9)
		time.sleep(0.04)
		f = b.eval(FOCUS_JS)
		if f is None:
			out.append({"i": i, "tag": "body"})
			break
		key = f["tag"] + f["label"]
		if seen and key == seen[0]:
			break
		seen.append(key)
		out.append({"i": i, **f})
		if shots and f["inMain"] and shot_at is None:
			shot_at = i
			b.shot(os.path.join(shots, f"{tag}-{b.name}-first-in-main.png"))
	bad = [o for o in out if o.get("tag") != "body" and (not o["ring"] or not o["visible"] and not o["dlg"])]
	return out, bad


def main():
	ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
	ap.add_argument("--base", required=True)
	ap.add_argument("--email", required=True)
	ap.add_argument("--password", required=True)
	ap.add_argument("--chromium")
	ap.add_argument("--firefox")
	ap.add_argument("--widths", default="1440,765,390")
	ap.add_argument("--themes", default="dark,light")
	ap.add_argument("--per-template", type=int, default=3)
	ap.add_argument("--limit", type=int, default=400)
	ap.add_argument("--out", default="crawl.json")
	ap.add_argument("--walk", type=int, metavar="STOPS", help="walk the keyboard (Tab) through each page instead of the audit")
	ap.add_argument("--shots", help="with --walk: a directory for a focus screenshot per page")
	ap.add_argument("--only", help="comma separated substrings: crawl only pages whose path contains one")
	args = ap.parse_args()

	site = Site(args.base, args.email, args.password)
	pages = site.pages(args.per_template, args.limit)
	if args.only:
		pages = [p for p in pages if any(o in p for o in args.only.split(","))]
	print(f"{len(pages)} pages", flush=True)
	widths = [int(w) for w in args.widths.split(",")]
	themes = args.themes.split(",")
	report, totals = {"pages": pages, "runs": []}, {"unnamed": 0, "dup_ids": 0, "overflow": 0}
	for cls, binary in ((Chromium, args.chromium), (Firefox, args.firefox)):
		if not binary:
			continue
		b = cls(binary, args.base)
		try:
			b.goto(site.base + "/login")
			b.eval("(() => { document.cookie = %s; return 1 })()" % json.dumps(f"session={site.cookies['session']}; path=/"))
			if args.walk:
				w = widths[0]
				b.resize(w)
				for path in pages:
					out, bad = walk(b, site, path, args.walk, args.shots, re.sub(r"\W+", "_", path)[-60:])
					report["runs"].append({"browser": b.name, "width": w, "path": path, "stops": len(out), "bad": bad})
					totals["unnamed"] += 0
					totals["overflow"] += len(bad)
					for o in bad:
						print(f"{b.name} {w} {path}: stop {o['i']} {o['tag']} {o['label']!r} ring={o['ring']} visible={o['visible']}", flush=True)
				continue
			for theme in themes:
				for w in widths:
					b.resize(w)
					for path in pages:
						b.eval("(() => { document.cookie = 'theme=%s; path=/'; return 1 })()" % theme)
						b.goto(site.base + path)
						v = b.eval(AUDIT_JS)
						row = {"browser": b.name, "theme": theme, "width": w, "inner": b.eval("innerWidth"), "path": path, **v}
						report["runs"].append(row)
						bad = []
						if v["unnamed"]:
							totals["unnamed"] += len(v["unnamed"])
							bad.append(f"unnamed {v['unnamed']}")
						if v["dup"]:
							totals["dup_ids"] += len(v["dup"])
							bad.append(f"dup {v['dup']}")
						if v["overflow"]:
							totals["overflow"] += 1
							bad.append(f"overflow {v['sw']}>{v['cw']} {v['wide']}")
						if v["h1"] != 1:
							bad.append(f"h1={v['h1']}")
						if bad:
							print(f"{b.name} {theme} {w} {path}: " + "; ".join(bad), flush=True)
		finally:
			b.close()
	json.dump(report, open(args.out, "w"), indent=1)
	print(f"{len(report['runs'])} page loads")
	if args.walk:
		print("tab stops without a ring or off screen: %d" % totals["overflow"])
		sys.exit(1 if totals["overflow"] else 0)
	print("unnamed={unnamed} dup_ids={dup_ids} overflow={overflow}".format(**totals))
	sys.exit(1 if any(totals.values()) else 0)


if __name__ == "__main__":
	main()
