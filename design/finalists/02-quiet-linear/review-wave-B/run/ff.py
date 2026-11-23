"""Tiny Marionette client for Firefox (no geckodriver on this host)."""
import base64, json, socket, sys, time

SHOTS = "/tmp/pk-t_b6efe1ba/shots/ff"
BASE = "http://127.0.0.1:26411"


class FF:
	def __init__(self, port=28489):
		self.s = socket.create_connection(("127.0.0.1", port), timeout=60)
		self.buf = b""
		self.n = 0
		self.read()  # hello
		self.cmd("WebDriver:NewSession", {"capabilities": {}})

	def read(self):
		while b":" not in self.buf:
			self.buf += self.s.recv(65536)
		ln, rest = self.buf.split(b":", 1)
		ln = int(ln)
		while len(rest) < ln:
			rest += self.s.recv(65536)
		self.buf = rest[ln:]
		return json.loads(rest[:ln])

	def cmd(self, name, params=None):
		self.n += 1
		m = json.dumps([0, self.n, name, params or {}]).encode()
		self.s.sendall(str(len(m)).encode() + b":" + m)
		while True:
			r = self.read()
			if r[0] == 1 and r[1] == self.n:
				if r[2]:
					raise RuntimeError(f"{name}: {r[2]}")
				return r[3]

	def js(self, script, args=None):
		r = self.cmd("WebDriver:ExecuteScript", {"script": script, "args": args or []})
		return r.get("value") if isinstance(r, dict) else r

	def goto(self, url):
		self.cmd("WebDriver:Navigate", {"url": url})
		for _ in range(50):
			if self.js("return document.readyState") == "complete":
				break
			time.sleep(0.1)
		time.sleep(0.6)

	def rect(self, w, h):
		self.cmd("WebDriver:SetWindowRect", {"width": w, "height": h})

	def shot(self, name):
		r = self.cmd("WebDriver:TakeScreenshot", {"full": False})
		open(f"{SHOTS}/{name}.png", "wb").write(base64.b64decode(r["value"]))

	def theme(self, t):
		self.js(f"document.cookie='theme={t}; path=/'")

	def login(self):
		self.goto(BASE + "/login")
		self.cmd("WebDriver:ExecuteAsyncScript", {"script": """const done=arguments[arguments.length-1];fetch('/login',{method:'POST',headers:{'Content-Type':'application/json','Datastar-Request':'true'},body:JSON.stringify({email:'dana@example.com',password:'correct horse battery'})}).then(r=>done(r.status))""", "args": []})


def flags(k, v, w):
	f = []
	if v["h1"] != 1:
		f.append(f"h1={v['h1']}")
	if v["overflowX"]:
		f.append(f"overflow {v['sw']}>{v['cw']}")
	if v["nfails"]:
		f.append(f"contrast {v['nfails']}: {v['contrastFails'][:2]}")
	if v["thirdPartyRes"]:
		f.append(f"3p {v['thirdPartyRes']}")
	if v["errs"]:
		f.append(f"errs {v['errs'][:2]}")
	if v["btnNoName"]:
		f.append(f"unnamed {v['btnNoName']}")
	return f or ["ok"]


if __name__ == "__main__":
	T = "77378636181655552"
	B = f"/teams/{T}"
	S = "77378636185849856"
	P = "77381338953404416"
	pages = json.load(open(sys.argv[1]))
	ff = FF()
	ff.login()
	audit_js = open("/tmp/pk-t_b6efe1ba/audit.js").read()
	out = {}
	for name, path in pages:
		for w in (1440, 765, 500):
			for th in ("dark", "light"):
				ff.rect(w, 900)
				ff.goto(BASE + "/")
				ff.theme(th)
				ff.goto(BASE + path)
				v = ff.js(audit_js)
				tag = f"{name}-{w}-{th}"
				ff.shot(tag)
				out[tag] = v
				print(tag, v["contrastChecked"], flags(tag, v, w), flush=True)
	json.dump(out, open("/tmp/pk-t_b6efe1ba/audit-ff-2.json", "w"))
