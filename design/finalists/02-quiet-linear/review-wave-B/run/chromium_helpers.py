from browser_harness.helpers import cdp, goto_url, wait_for_load, js
import base64, json, os, time
SH = "/tmp/pk-t_b6efe1ba/shots"
BASE = "http://127.0.0.1:26411"
IDS = json.load(open("/tmp/pk-t_b6efe1ba/ids.json"))
T = IDS["T"]; P = IDS["P"]; A = IDS["A"]

def setup(w, theme, h=900):
    mobile = w < 500
    cdp("Emulation.setDeviceMetricsOverride", width=w, height=h, deviceScaleFactor=1, mobile=mobile)
    cdp("Emulation.setEmulatedMedia", features=[{"name": "prefers-color-scheme", "value": theme}])

def shot(name, path, w=1440, theme="dark", full=True, wait=1.2, outdir=None):
    setup(w, theme)
    go(path, wait)
    args = {"format": "png"}
    if full:
        m = cdp("Page.getLayoutMetrics")
        cs = m.get("cssContentSize") or m["contentSize"]
        h = min(int(cs["height"]), 3000)
        args.update(captureBeyondViewport=True, clip={"x": 0, "y": 0, "width": w, "height": h, "scale": 1})
    r = cdp("Page.captureScreenshot", **args)
    d = outdir or SH
    os.makedirs(d, exist_ok=True)
    fn = f"{d}/{name}-{w}-{theme}.png"
    open(fn, "wb").write(base64.b64decode(r["data"]))
    return fn

def probe():
    return js("""(()=>{const r={};const de=document.documentElement;r.overflowX=de.scrollWidth-de.clientWidth;r.h1=document.querySelectorAll('h1').length;
    r.title=document.title;return JSON.stringify(r)})()""")

S = IDS["S"]
PP = f"/teams/{T}/projects/{P}"
AP = f"{PP}/apps/{A}"

def login():
    if "/login" not in js("location.pathname"):
        return False
    js("""(()=>{const s=(id,v)=>{const e=document.getElementById(id);e.focus();e.value=v;e.dispatchEvent(new Event('input',{bubbles:true}));e.dispatchEvent(new Event('change',{bubbles:true}))};s('si-email','dana@example.com');s('si-pw','correct horse battery');document.querySelector('form').requestSubmit()})()""")
    time.sleep(2.5)
    return True

def go(path, wait=0.8):
    goto_url(BASE + path if path.startswith("/") else path)
    wait_for_load()
    if login():
        goto_url(BASE + path); wait_for_load()
        print("RELOGIN needed at", path)
    time.sleep(wait)
