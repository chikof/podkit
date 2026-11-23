import sys, json
sys.path.insert(0, "/tmp/pk-t_b6efe1ba")
import ff as F
ids = json.load(open("/tmp/pk-t_b6efe1ba/ids.json"))
T, P, A = ids["T"], ids["P"], ids["A"]
x = F.FF(); x.login(); x.rect(1280, 900)
JS = """return [...document.querySelectorAll('button,a,select,input,textarea,[role=button]')].filter(e=>{
 if(e.type==='hidden')return false; const cs=getComputedStyle(e); if(cs.display==='none'||cs.visibility==='hidden')return false;
 const name=(e.getAttribute('aria-label')||e.getAttribute('title')||e.innerText||e.value||'').trim();
 const lab=e.labels&&e.labels.length; const lb=e.getAttribute('aria-labelledby');
 return !name && !lab && !lb;}).map(e=>e.tagName+'.'+e.className+'#'+e.id+' '+e.outerHTML.slice(0,140))"""
for n, p in [("cron-task", f"/teams/{T}/projects/{P}/cron/77679348296445952"), ("app-volumes", f"/teams/{T}/projects/{P}/apps/{A}/volumes"), ("env", f"/teams/{T}/projects/{P}/production"), ("app-health", f"/teams/{T}/projects/{P}/apps/{A}/settings")]:
    x.goto(F.BASE + p)
    print(n, json.dumps(x.js(JS), indent=1))
