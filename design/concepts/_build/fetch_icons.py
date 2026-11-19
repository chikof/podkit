import urllib.request, json, re, os
ROOT=os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
names="send sparkles gauge layout-grid message-square chart-no-axes-combined paperclip thermometer git-commit-horizontal wifi arrow-down arrow-up flame gpu microchip chevron-right store".split()
out={}
for n in names:
    try:
        s=urllib.request.urlopen(f"https://cdn.jsdelivr.net/npm/lucide-static/icons/{n}.svg",timeout=20).read().decode()
        inner=re.search(r'<svg[^>]*>(.*)</svg>',s,re.S).group(1).strip()
        out[n]=re.sub(r'\s+',' ',inner)
    except Exception as e: print("skip",n,e)
json.dump(out,open(f"{ROOT}/_build/extra_icons.json","w"))
print(list(out))
