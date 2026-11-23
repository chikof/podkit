import sys, json
sys.path.insert(0, "/tmp/pk-t_b6efe1ba")
import ff as F
ids = json.load(open("/tmp/pk-t_b6efe1ba/ids.json"))
x = F.FF(); x.login()
x.rect(1440, 900)
x.goto(F.BASE + "/teams/%s/servers" % ids["T"])
print(x.js("return [...document.querySelectorAll('button,a[href]')].filter(b=>!(b.innerText||b.getAttribute('aria-label')||b.title||'').trim()).map(b=>b.outerHTML.slice(0,200))"))
x.goto(F.BASE + "/teams/%s/settings/authentication" % ids["T"])
print(x.js("return [...document.querySelectorAll('button,a[href]')].filter(b=>!(b.innerText||b.getAttribute('aria-label')||b.title||'').trim()).map(b=>b.outerHTML.slice(0,200))"))
