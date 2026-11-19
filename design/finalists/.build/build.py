"""Build the three finalists. python3 design/finalists/.build/build.py   (python3 stdlib only)"""
import os, sys, re, shutil, gzip, json
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
from lib import *
import screens as S
import components as C

OFL = "SIL OFL 1.1"
FINALISTS = [
 dict(id="01-terminal-ops", name="Cockpit", theme="dark", nav="sidebar", tree=True, status=True, charts="step", structure=False),
 dict(id="02-quiet-linear", name="Quiet", theme="dark", nav="sidebar", tree=False, status=False, charts="line"),
 dict(id="09-data-dashboard", name="Panelboard", theme="dark", nav="topbar", tree=False, status=True, varbar=True, charts="area"),
]
SCREENS = [  # (file, key, title, builder returns)
 ("projects", "Projects"), ("environment", "storefront / production"), ("app", "web · storefront"), ("templates", "Templates"), ("server", "atlas · Server"), ("playground", "Qwen3-4B Playground"),
 ("signin", "Sign in"), ("create", "Create resource"), ("database", "postgres-16"), ("stack", "observability"), ("settings", "Settings · Members"), ("empty", "sandbox / production"), ("404", "Not found"),
]
def panels_css():
    return "\n".join(f'.tabset:has(input[value="{k}"]:checked) [data-panel="{k}"]{{display:block}}' for k in S.PANEL_KEYS)

def split_top(css):
    """yield (prelude, body) for top-level blocks"""
    i, n = 0, len(css)
    while i < n:
        j = css.find("{", i)
        if j < 0: break
        depth, k = 1, j + 1
        while depth and k < n:
            c = css[k]; depth += (c == "{") - (c == "}"); k += 1
        yield css[i:j].strip(), css[j + 1:k - 1]; i = k

def states_css(css):
    """Mirror :hover / :focus-visible / :active rules onto [data-state] so the component sheet can show them statically."""
    css = re.sub(r"/\*.*?\*/", "", css, flags=re.S); out = []
    for pre, body in split_top(css):
        if pre.startswith("@") or not re.search(r":(hover|focus-visible|active)", pre): continue
        sels = []
        for s in pre.split(","):
            if re.search(r":(hover|focus-visible|active)", s):
                s2 = re.sub(r":hover", '[data-state~="hover"]', s); s2 = re.sub(r":focus-visible", '[data-state~="focus"]', s2); s2 = re.sub(r":active", '[data-state~="active"]', s2); sels.append(s2.strip())
        if sels: out.append(",".join(sels) + "{" + body + "}")
    return "/* generated: forced states for components.html (not shipped) */\n" + "\n".join(out) + "\n"

def build():
    stats = {}
    for cfg in FINALISTS:
        d = f"{ROOT}/{cfg['id']}"
        shutil.copy(f"{HERE}/base.css", d); shutil.copy(f"{HERE}/mobile.css", d)
        shutil.copy(f"{HERE}/page.js", d); shutil.copy(f"{HERE}/sim.js", d)
        shutil.copy(f"{HERE}/contrast_core.py", d); shutil.copy(f"{HERE}/contrast_stub.py", f"{d}/contrast.py")
        if cfg.get("structure", True): shutil.copy(f"{HERE}/structure.css", d)
        elif os.path.exists(f"{d}/structure.css"): os.remove(f"{d}/structure.css")
        open(f"{d}/extra.css", "w").write(open(f"{HERE}/extra.css").read().replace("/*@PANELS@*/", panels_css()))
        fn = {"projects": S.projects, "environment": S.environment, "app": S.app, "templates": S.templates, "create": S.create, "database": S.database, "stack": S.stack, "settings": S.settings, "empty": S.empty, "404": S.notfound}
        for name, title in SCREENS:
            if name == "signin": html = S.signin(cfg)
            elif name == "server": html = S.page(cfg, "server", title, S.server(cfg))
            elif name == "playground": html = S.page(cfg, "playground", title, S.playground(cfg))
            else:
                b, ex = fn[name](cfg); key = "notfound" if name == "404" else name
                html = S.page(cfg, key, title, b, ex)
            open(f"{d}/{name}.html", "w").write(html)
        open(f"{d}/components.html", "w").write(C.components(cfg))
        allcss = "".join(open(f"{d}/{f}").read() for f in ("base.css", "extra.css", "concept.css") + (("structure.css",) if cfg.get("structure", True) else ()))
        open(f"{d}/states.css", "w").write(states_css(allcss))
        sh = [f for f in ("tokens.css", "base.css", "structure.css", "extra.css", "concept.css", "mobile.css") if os.path.exists(f"{d}/{f}")]
        raw = sum(os.path.getsize(f"{d}/{f}") for f in sh); gz = sum(len(gzip.compress(open(f"{d}/{f}", "rb").read(), 9)) for f in sh)
        fonts = sum(os.path.getsize(f"{d}/fonts/{f}") for f in os.listdir(f"{d}/fonts"))
        js = sum(os.path.getsize(f"{d}/{f}") for f in ("page.js", "sim.js")); jsgz = sum(len(gzip.compress(open(f"{d}/{f}", "rb").read(), 9)) for f in ("page.js", "sim.js"))
        stats[cfg["id"]] = dict(css_files={f: os.path.getsize(f"{d}/{f}") for f in sh}, css_raw=raw, css_gzip=gz, fonts=fonts, js_raw=js, js_gzip=jsgz, page_js=os.path.getsize(f"{d}/page.js"), sim_js=os.path.getsize(f"{d}/sim.js"))
        print("built", cfg["id"], f"css {raw/1024:.1f} KB ({gz/1024:.1f} gz) fonts {fonts/1024:.0f} KB")
    copy_logos()
    json.dump(stats, open(f"{HERE}/stats.json", "w"), indent=1)
    import gallery; gallery.build()

if __name__ == "__main__": build()
