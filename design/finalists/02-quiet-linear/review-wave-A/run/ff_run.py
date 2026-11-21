import json, sys, time
sys.path.insert(0, "/tmp/pk-t_87d44fd2")
import ff as F

tag = sys.argv[1] if len(sys.argv) > 1 else "ff"
pages = json.load(open("/tmp/pk-t_87d44fd2/pages.json"))
x = F.FF()
x.login()
audit_js = open("/tmp/pk-t_87d44fd2/audit.js").read()
out = {}
for name, path in pages:
    for w in (1440, 500):
        for th in ("dark", "light"):
            x.rect(w, 900)
            x.goto(F.BASE + "/")
            x.theme(th)
            x.goto(F.BASE + path)
            v = x.js(audit_js)
            t = f"{name}-{w}-{th}"
            x.shot(f"{tag}-{t}")
            out[t] = v
            print(t, F.flags(t, v, w), flush=True)
json.dump(out, open(f"/tmp/pk-t_87d44fd2/audit-{tag}.json", "w"))
