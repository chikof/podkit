"""Fetch permissively-licensed fonts (fontsource, latin subset) + Lucide extras and copy logos. Run once."""
import urllib.request, os, shutil, re, json
ROOT=os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CDN="https://cdn.jsdelivr.net/npm/"
SRC="/home/chiko/Projects/podkit/.worktrees/t_e204fafb/server"
def get(u):
    return urllib.request.urlopen(u,timeout=30).read()
# (folder, [(file-out, package, file-in)])
def var(slug,axis="wght",style="normal"): return (f"{slug}-var{'-i' if style=='italic' else ''}.woff2", f"@fontsource-variable/{slug}", f"{slug}-latin-{axis}-{style}.woff2")
def sta(slug,w): return (f"{slug}-{w}.woff2", f"@fontsource/{slug}", f"{slug}-latin-{w}-normal.woff2")
FONTS={
 "01-terminal-ops":[var("jetbrains-mono")],
 "02-quiet-linear":[var("hanken-grotesk"),var("spline-sans-mono")],
 "03-mono-precision":[var("geist"),var("geist-mono")],
 "04-tinted-dark":[var("dm-sans"),var("source-code-pro")],
 "05-editorial":[var("fraunces"),var("newsreader"),sta("courier-prime",400),sta("courier-prime",700)],
 "06-playful":[var("bricolage-grotesque"),var("martian-mono")],
 "07-glass-depth":[var("sora"),var("fira-code")],
 "08-brutalist":[var("archivo"),sta("space-mono",400),sta("space-mono",700)],
 "09-data-dashboard":[var("ibm-plex-sans"),sta("ibm-plex-mono",400),sta("ibm-plex-mono",500)],
 "10-calm-soft":[var("figtree"),sta("dm-mono",400),sta("dm-mono",500)],
}
for d,items in FONTS.items():
    os.makedirs(f"{ROOT}/{d}/fonts",exist_ok=True)
    for out,pkg,fin in items:
        try:
            b=get(f"{CDN}{pkg}/files/{fin}")
            open(f"{ROOT}/{d}/fonts/{out}","wb").write(b); print("ok",d,out,len(b))
        except Exception as e: print("FAIL",d,out,pkg,fin,e)
# licenses from package metadata
pk=set()
for items in FONTS.values():
    for _,pkg,_ in items: pk.add(pkg)
lic={}
for p in sorted(pk):
    try:
        j=json.loads(get(f"{CDN}{p}/package.json")); lic[p]=(j.get("license"),j.get("version"))
    except Exception as e: lic[p]=("?",str(e))
json.dump(lic,open(f"{ROOT}/_build/font_licenses.json","w"),indent=1)
# logos
os.makedirs(f"{ROOT}/_assets/logos",exist_ok=True)
for f in os.listdir(f"{SRC}/static/logos"):
    if f.endswith(".svg") and not f.startswith("mono-"): shutil.copy(f"{SRC}/static/logos/{f}",f"{ROOT}/_assets/logos/{f}")
for t,out in [("gotify","gotify.svg"),("umami","umami.png"),("plausible","plausible.svg"),("grafana","grafana.svg"),("vaultwarden","vaultwarden.svg"),("immich","immich.svg"),("pocketbase","pocketbase.svg"),("paperless-ngx","paperless.svg"),("penpot","penpot.svg"),("langfuse","langfuse.svg"),("strapi","strapi.svg"),("jellyfin","jellyfin.svg"),("uptime-kotlin","uptime-kuma.svg"),("listmonk","listmonk.png"),("flowise","flowise.png"),("homepage","homepage.png"),("appsmith","appsmith.png")]:
    d=f"{SRC}/templates/{t}"
    for ext in (".svg",".png"):
        if os.path.exists(f"{d}/logo{ext}"):
            shutil.copy(f"{d}/logo{ext}",f"{ROOT}/_assets/logos/{os.path.splitext(out)[0]}{ext}"); print("logo",t,ext); break
    else: print("nologo",t)
# icons: sprite from podkit + extras from lucide-static
sp=open(f"{SRC}/static/icons.svg").read()
syms=dict(re.findall(r'<symbol id="([^"]+)"(.*?)</symbol>',sp,re.S))
print(sp[:400])
