import json, math, random, re, os, html, shutil
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)                       # design/finalists
_sprite = open(f"{HERE}/icons.svg").read()
SYMS = {k: v for k, v in re.findall(r'<symbol id="([^"]+)" viewBox="0 0 24 24">(.*?)</symbol>', _sprite, re.S)}
SYMS.update(json.load(open(f"{HERE}/extra_icons.json")))
esc = html.escape
LOGO_SRC = f"{ROOT}/../concepts/_assets/logos"
LOGO_EXT = {os.path.splitext(f)[0]: f for f in os.listdir(LOGO_SRC)}
USED_LOGOS = set()

def icon(n, cls=""):
    return f'<svg class="ic {cls}" aria-hidden="true"><use href="#i-{n}"/></svg>'

def sprite(page_html):
    used = sorted(set(re.findall(r'#i-([a-z0-9-]+)', page_html)) | {"circle-check", "circle-alert", "x"})
    out = ['<svg width="0" height="0" style="position:absolute" aria-hidden="true" focusable="false"><defs>']
    for u in used:
        if u in SYMS: out.append(f'<symbol id="i-{u}" viewBox="0 0 24 24">{SYMS[u]}</symbol>')
        else: print("MISSING ICON", u)
    out.append('</defs></svg>')
    return "".join(out)

def logo(n, size=32, cls="logo"):
    """Brand logo as <img>; services without a logo get a two-letter monogram tile (deterministic tint 1-6)."""
    f = LOGO_EXT.get(n)
    if not f:
        return f'<span class="{cls} {cls}--glyph" data-h="{sum(map(ord, n)) % 6 + 1}" aria-hidden="true">{esc(n[:2].title())}</span>'
    USED_LOGOS.add(f)
    return f'<img class="{cls}" src="../.assets/logos/{f}" alt="" width="{size}" height="{size}" loading="lazy">'

def copy_logos():
    os.makedirs(f"{ROOT}/.assets/logos", exist_ok=True)
    for f in sorted(USED_LOGOS): shutil.copy(f"{LOGO_SRC}/{f}", f"{ROOT}/.assets/logos/{f}")

STATUS = {"running": "Running", "building": "Building", "deploying": "Deploying", "failed": "Failed", "stopped": "Stopped",
          "pending": "Pending", "unreachable": "Unreachable", "online": "Online"}
def badge(st, label=None, extra=""):
    key = "running" if st == "online" else st
    return f'<span class="badge" data-status="{key}" {extra}><span class="dot" aria-hidden="true"></span><span class="lbl">{label or STATUS[st]}</span></span>'

# ---------------- data ----------------
PROJECTS = [
 dict(name="storefront", logos=["nextdotjs","postgres","redis","qwen"], res=6, envs=2, last="12 min ago", what="web", h=(3,1,1,1)),
 dict(name="internal-tools", logos=["n8n","nocodb","postgres"], res=4, envs=2, last="3 h ago", what="n8n", h=(4,0,0,0)),
 dict(name="analytics", logos=["umami","plausible","clickhouse","grafana"], res=5, envs=1, last="yesterday", what="umami", h=(4,0,0,1)),
 dict(name="ai-lab", logos=["ollama","open-webui","qwen","deepseek","vllm"], res=7, envs=2, last="2 d ago", what="open-webui", h=(5,1,0,1)),
 dict(name="docs-site", logos=["github","caddy"], res=2, envs=1, last="5 d ago", what="docs", h=(2,0,0,0)),
 dict(name="mailroom", logos=["listmonk","postgres","redis"], res=3, envs=1, last="2 w ago", what="listmonk", h=(2,0,1,0)),
]
RES = [
 dict(id="web", name="web", kind="Git app", sub="github.com/acme/storefront · main", logo="nextdotjs", st="running", meta=[("URL","shop.acme.dev"),("Deployed","12 min ago")], m=[("CPU","3.2%"),("Mem","412 MB")], href="app.html", t="git-branch"),
 dict(id="checkout", name="checkout-worker", kind="Image app", sub="ghcr.io/acme/checkout-worker:2.14.1", logo="docker", st="building", meta=[("Deploy","#31 · pulling layers"),("Started","40 s ago")], m=[("CPU","38%"),("Mem","1.1 GB")], href="app.html", t="box", prog=64),
 dict(id="pg", name="postgres-16", kind="Database", sub="PostgreSQL 16.4 · 2.1 GB", logo="postgres", st="running", meta=[("Backup","daily 03:00"),("Connections","18 / 100")], m=[("CPU","1.4%"),("Mem","640 MB")], href="database.html", t="database"),
 dict(id="redis", name="redis-cache", kind="Database", sub="Redis 7.4 · 38 MB", logo="redis", st="stopped", meta=[("Stopped","yesterday 18:02"),("By","chiko")], m=[("CPU","—"),("Mem","—")], href="database.html", t="database"),
 dict(id="obs", name="observability", kind="Compose stack", sub="4 services · prometheus, grafana, loki, alloy", logo="grafana", st="failed", meta=[("Error","prometheus exited 137"),("Failed","2 h ago")], m=[("CPU","—"),("Mem","—")], href="stack.html", t="layers", err="prometheus exited with code 137 (OOM-killed)"),
 dict(id="qwen", name="qwen3-4b", kind="AI model", sub="Qwen3-4B Q4_K_M · llama.cpp CUDA", logo="qwen", st="running", meta=[("Speed","175 tok/s"),("VRAM","3.4 GB")], m=[("GPU","31%"),("Ctx","32k")], href="playground.html", t="brain"),
]
STAGING = [
 dict(id="web-s", name="web", kind="Git app", sub="github.com/acme/storefront · develop", logo="nextdotjs", st="running", meta=[("URL","staging.shop.acme.dev"),("Deployed","2 h ago")], m=[("CPU","0.8%"),("Mem","288 MB")], href="app.html", t="git-branch"),
 dict(id="pg-s", name="postgres-16", kind="Database", sub="PostgreSQL 16.4 · 180 MB", logo="postgres", st="running", meta=[("Backup","weekly"),("Connections","3 / 100")], m=[("CPU","0.2%"),("Mem","210 MB")], href="database.html", t="database"),
]
TEMPLATES = [
 ("Gotify","gotify","Push notifications to your phone and desktop","other","2.9.1"),
 ("Umami","umami","Privacy-first web analytics","analytics","3.0"),
 ("n8n","n8n","Workflow automation with 400+ integrations","automation","1.97"),
 ("Excalidraw","excalidraw","Virtual whiteboard that looks hand-drawn","other","latest"),
 ("Uptime Kuma","uptime-kuma","Self-hosted uptime monitoring and status pages","monitoring","2.0"),
 ("Hermes Agent","hermes-agent","Self-improving AI agent with tools and memory","ai","0.8"),
 ("Plausible","plausible","Lightweight, cookie-free analytics","analytics","2.1"),
 ("Grafana","grafana","Dashboards for metrics, logs and traces","monitoring","12.0"),
 ("Vaultwarden","vaultwarden","Bitwarden-compatible password server","security","1.34"),
 ("Immich","immich","Photo and video library with ML search","media","1.135"),
 ("Ghost","ghost","Publishing and newsletters","cms","5.120"),
 ("Open WebUI","open-webui","Chat UI for Ollama and OpenAI APIs","ai","0.6"),
 ("Langfuse","langfuse","LLM tracing, evals and prompt management","ai","3.6"),
 ("PocketBase","pocketbase","Realtime backend in a single binary","database","0.28"),
 ("Penpot","penpot","Open-source design and prototyping","productivity","2.7"),
 ("Paperless-ngx","paperless","Scan, index and archive documents","productivity","2.17"),
 ("Flowise","flowise","Drag-and-drop LLM flows","ai","3.0"),
 ("Listmonk","listmonk","Newsletter and mailing-list manager","communication","5.0"),
 ("Jellyfin","jellyfin","Media server for movies, shows and music","media","10.10"),
 ("NocoDB","nocodb","Airtable-style UI on top of any database","database","0.263"),
]
CATS = [("all","All",484),("ai","AI",37),("analytics","Analytics",14),("automation","Automation",13),("database","Database",35),("monitoring","Monitoring",29),("productivity","Productivity",82),("security","Security",26),("media","Media",33),("cms","CMS",18)]
CMDK = [("layout-grid","Go to Projects","projects.html","G P"),("folder-open","storefront / production","environment.html","G E"),("rocket","Deploy web","app.html","D"),("database","postgres-16 · Connection","database.html",""),
        ("layers","observability · Services","stack.html",""),("package","Browse templates","templates.html","G T"),("server","Server atlas","server.html","G S"),("brain","Model Qwen3-4B playground","playground.html","G M"),
        ("users","Team members","settings.html",""),("plus","Create resource…","create.html","C")]
LOG = [
 ("00:00","STEP","Deployment #148 queued · trigger: push to main (a3f9c21)"),
 ("00:01","INFO","Cloning github.com/acme/storefront @ main"),
 ("00:02","INFO","Detected Dockerfile · builder: podman 5.4"),
 ("00:03","STEP","[1/7] FROM docker.io/library/node:22-alpine"),
 ("00:05","INFO","Resolved node:22-alpine → sha256:7a1e…c90b (cached)"),
 ("00:05","STEP","[2/7] WORKDIR /app"),
 ("00:06","STEP","[3/7] COPY package.json pnpm-lock.yaml ./"),
 ("00:07","STEP","[4/7] RUN corepack enable && pnpm install --frozen-lockfile"),
 ("00:19","INFO","Packages: +912 · Done in 11.8s"),
 ("00:20","STEP","[5/7] COPY . ."),
 ("00:21","STEP","[6/7] RUN pnpm build"),
 ("00:22","INFO","▲ Next.js 15.3 · Creating an optimized production build"),
 ("00:31","INFO","✓ Compiled successfully in 8.7s"),
 ("00:32","WARN","Image `/hero.png` is 2.4 MB · consider compressing"),
 ("00:38","INFO","✓ Generating static pages (42/42)"),
 ("00:41","STEP","[7/7] CMD [\"node\", \"server.js\"]"),
 ("00:44","INFO","Image acme/storefront-web:a3f9c21 · 186 MB"),
 ("00:49","STEP","Starting new container web-148 · health check GET /healthz"),
 ("00:55","OK","Health check passed (3/3)"),
 ("00:57","STEP","Switching traffic · shop.acme.dev → web-148"),
 ("01:04","INFO","Stopping previous container web-147"),
 ("01:08","OK","Deployment #148 finished · live at shop.acme.dev"),
]
DEPLOYS = [
 ("#148","Deploy from main","a3f9c21 “fix: cart rounding on bundles”","chiko","now","building"),
 ("#147","Deploy from main","91be0d4 “feat: wishlist share links”","mara","12 min ago","running"),
 ("#146","Deploy from main","5c20a77 “chore: bump next to 15.3”","chiko","yesterday","failed"),
 ("#145","Rollback to #143","e8d4f10 “fix: stripe webhook retry”","chiko","yesterday","running"),
 ("#144","Deploy from main","7b3a9e2 “feat: new checkout step”","mara","3 d ago","stopped"),
 ("#143","Deploy from main","e8d4f10 “fix: stripe webhook retry”","chiko","4 d ago","running"),
]
BACKUPS = [
 ("2026-11-19 03:00","scheduled","412 MB","completed","running"),
 ("2026-11-18 03:00","scheduled","409 MB","completed","running"),
 ("2026-11-17 14:21","manual · chiko","407 MB","completed","running"),
 ("2026-11-17 03:00","scheduled","406 MB","failed","failed"),
 ("2026-11-16 03:00","scheduled","401 MB","completed","running"),
 ("2026-11-15 03:00","scheduled","398 MB","completed","running"),
]
SERVICES = [
 dict(name="prometheus", img="prom/prometheus:v3.4.1", st="failed", ports="9090", cpu="—", mem="—", rs="5", note="exited 137 (OOM-killed) · limit 512 MB", logo="prometheus"),
 dict(name="grafana", img="grafana/grafana:12.0", st="running", ports="3000", cpu="1.1%", mem="188 MB", rs="0", note="", logo="grafana"),
 dict(name="loki", img="grafana/loki:3.5", st="running", ports="3100", cpu="2.4%", mem="302 MB", rs="0", note="", logo="loki"),
 dict(name="alloy", img="grafana/alloy:v1.9", st="running", ports="12345", cpu="0.6%", mem="96 MB", rs="1", note="", logo="alloy"),
]
COMPOSE = """services:
  prometheus:
    image: prom/prometheus:v3.4.1
    volumes: [prom-data:/prometheus]
    deploy: { resources: { limits: { memory: 512M } } }
  grafana:
    image: grafana/grafana:12.0
    depends_on: [prometheus, loki]
  loki:
    image: grafana/loki:3.5
  alloy:
    image: grafana/alloy:v1.9
volumes: { prom-data: {} }"""
MEMBERS = [
 ("chiko","chiko@acme.dev","Owner","just now","C"),
 ("Mara Lindqvist","mara@acme.dev","Admin","12 min ago","M"),
 ("Tomás Ortega","tomas@acme.dev","Developer","yesterday","T"),
 ("Priya Nair","priya@acme.dev","Developer","3 d ago","P"),
 ("CI bot","ci@acme.dev","Viewer","token · 2 h ago","B"),
]
INVITES = [("jonas@acme.dev","Developer","sent 2 d ago"),("ines@partner.io","Viewer","sent today")]
RTYPES = [
 ("git-branch","Git repository","GitHub, GitLab or any Git URL","Builds a Dockerfile or Nixpacks plan on every push and deploys behind a domain.",[("Repository","github.com/acme/storefront"),("Branch","main")],"~1 min build"),
 ("box","Docker image","From a registry, with auth","Runs an existing image. Pulls on deploy; supports private registries.",[("Image","ghcr.io/acme/checkout-worker:2.14"),("Port","8080")],"~10 s pull"),
 ("database","Database","Postgres, MySQL, MongoDB, Redis","Managed container with a volume, scheduled backups and a connection string.",[("Engine","PostgreSQL 16"),("Name","storefront")],"~5 s start"),
 ("layers","Compose stack","Paste or import docker-compose.yml","Runs several services as one unit with shared env and a per-service status.",[("Compose file","docker-compose.yml"),("Project name","observability")],"depends on services"),
 ("package","Template","484 one-click apps","Curated apps with generated secrets and a ready domain. Pick one from the catalog.",[("Template","Gotify 2.9.1"),("Domain","gotify.acme.dev")],"~30 s"),
 ("brain","AI model","llama.cpp, Ollama, vLLM, colibri","Downloads weights, reserves VRAM and exposes an OpenAI-compatible endpoint.",[("Model","Qwen3-4B Q4_K_M"),("Context","32768")],"~2 min download"),
]
THINK = ("The user wants a Containerfile for a Rust axum service with a small runtime image. Build in a rust:1.88 stage and use cargo-chef so dependency layers cache. "
         "A distroless cc image is enough because axum links glibc dynamically. Run as nonroot, expose 8080, and copy only the release binary. A .dockerignore for target/ is worth mentioning.")
ANSWER = ('<p>Here is a two-stage <code>Containerfile</code>. Dependencies are cached in their own layer with <strong>cargo-chef</strong>, and the final image is distroless, so there is no shell and no package manager to patch.</p>'
 '<pre class="code"><code>FROM docker.io/library/rust:1.88 AS chef\nRUN cargo install cargo-chef --locked\nWORKDIR /src\n\nFROM chef AS plan\nCOPY . .\nRUN cargo chef prepare --recipe-path recipe.json\n\nFROM chef AS build\nCOPY --from=plan /src/recipe.json .\nRUN cargo chef cook --release --recipe-path recipe.json\nCOPY . .\nRUN cargo build --release --bin api\n\nFROM gcr.io/distroless/cc-debian12:nonroot\nCOPY --from=build /src/target/release/api /api\nEXPOSE 8080\nENTRYPOINT ["/api"]</code></pre>'
 '<p>Add <code>target/</code> and <code>.git</code> to <code>.dockerignore</code> so the build context stays small. With Podman, <code>podman build -t api .</code> works unchanged.</p>')

# -------- charts --------
def _series(seed, n, base, amp, noise, lo, hi, phase=0.0, cycles=2.0, spike=0):
    r = random.Random(seed); out = []; x = base
    for i in range(n):
        t = i / (n - 1)
        target = base + amp * math.sin(t * math.pi * 2 * cycles + phase) + amp * .4 * math.sin(t * 17 + phase)
        x += (target - x) * .35 + r.gauss(0, noise)
        v = x
        if spike and r.random() < .04: v += spike * r.random()
        out.append(max(lo, min(hi, v)))
    return out

def chart(kind, series_list, ymax, fmt, xlabels, ylabels=3, label=None):
    W, H = 600, 160; n = len(series_list[0])
    def pts(vs): return [(i * W / (n - 1), H - (v / ymax) * H) for i, v in enumerate(vs)]
    g = "".join(f'<line class="gridline" x1="0" x2="{W}" y1="{H*i/(ylabels-1):.1f}" y2="{H*i/(ylabels-1):.1f}"/>' for i in range(ylabels))
    shapes = ""
    for si, vs in enumerate(series_list):
        P = pts(vs); cls = "s2" if si else ""
        if kind == "bars":
            bw = W / n * .66
            shapes += "".join(f'<rect class="bar {cls}" x="{x-bw/2 + (bw/len(series_list))*si:.1f}" y="{y:.1f}" width="{bw/len(series_list):.1f}" height="{H-y:.1f}"/>' for x, y in P)
            continue
        if kind == "step":
            d = f"M{P[0][0]:.1f},{P[0][1]:.1f}" + "".join(f"H{x:.1f}V{y:.1f}" for x, y in P[1:])
        elif kind == "smooth":
            d = f"M{P[0][0]:.1f},{P[0][1]:.1f}" + "".join(f"Q{P[i][0]:.1f},{P[i][1]:.1f} {(P[i][0]+P[i+1][0])/2:.1f},{(P[i][1]+P[i+1][1])/2:.1f}" for i in range(len(P)-1)) + f"T{P[-1][0]:.1f},{P[-1][1]:.1f}"
        else:
            d = f"M{P[0][0]:.1f},{P[0][1]:.1f}" + "".join(f"L{x:.1f},{y:.1f}" for x, y in P[1:])
        if kind in ("area", "step", "smooth") and si == 0:
            shapes += f'<path class="area" d="{d}L{W},{H}L0,{H}Z"/>'
        shapes += f'<path class="line {cls}" d="{d}"/>'
    yl = "".join(f'<span class="yl" style="--p:{100*i/(ylabels-1):.0f}">{fmt(ymax*(1-i/(ylabels-1)))}</span>' for i in range(ylabels))
    xl = "".join(f"<span>{x}</span>" for x in xlabels)
    lab = f' role="img" aria-label="{esc(label)}"' if label else ' aria-hidden="true"'
    return f'<div class="chart"><div class="chart-plot"><svg viewBox="0 0 {W} {H}" preserveAspectRatio="none"{lab}>{g}{shapes}</svg>{yl}</div><div class="xl" aria-hidden="true">{xl}</div></div>'

RANGES = {"1h": (60, ["-60m", "-45m", "-30m", "-15m", "now"]), "24h": (96, ["-24h", "-18h", "-12h", "-6h", "now"]), "7d": (84, ["-7d", "-5d", "-3d", "-1d", "now"])}
METRICS = [
 dict(id="cpu", name="CPU", unit="%", cur="14", sub="Ryzen 7 9850X3D · 8c / 16t", ymax=100, base=(16, 24, 21), amp=(8, 14, 11), noise=(3, 4, 3.5), fmt=lambda v: f"{v:.0f}%", stats=("avg 17%", "max 62%"), icon="cpu"),
 dict(id="mem", name="Memory", unit="GB", cur="9.2", sub="of 30 GB · 31%", ymax=30, base=(9, 9.6, 10.4), amp=(.5, 1.2, 1.8), noise=(.12, .2, .25), fmt=lambda v: f"{v:.0f} GB", stats=("avg 9.4 GB", "max 14.8 GB"), icon="memory-stick"),
 dict(id="disk", name="Disk write", unit="MB/s", cur="42", sub="NVMe 2 TB · 612 / 1862 GB used", ymax=200, base=(30, 44, 38), amp=(18, 30, 26), noise=(8, 10, 9), fmt=lambda v: f"{v:.0f}", stats=("avg 38 MB/s", "max 188 MB/s"), icon="hard-drive", spike=120),
 dict(id="net", name="Network", unit="Mbit/s", cur="86 ↓ 14 ↑", sub="enp5s0 · 2.5 GbE", ymax=200, base=(70, 90, 80), amp=(30, 40, 36), noise=(10, 12, 11), fmt=lambda v: f"{v:.0f}", stats=("rx avg 74", "tx avg 12"), icon="network", two=True),
 dict(id="gpu", name="GPU", unit="%", cur="31", sub="RTX 5070 · 58 °C · 142 W", ymax=100, base=(28, 18, 22), amp=(24, 20, 18), noise=(5, 6, 6), fmt=lambda v: f"{v:.0f}%", stats=("avg 24%", "max 97%"), icon="zap"),
 dict(id="vram", name="VRAM", unit="GB", cur="3.4", sub="of 12 GB · qwen3-4b + embeddings", ymax=12, base=(3.4, 3.4, 3.6), amp=(.1, .6, 1.2), noise=(.02, .08, .1), fmt=lambda v: f"{v:.0f} GB", stats=("avg 3.5 GB", "max 7.9 GB"), icon="microchip"),
]
def metric_series(m, rng, i):
    n, _ = RANGES[rng]; k = list(RANGES).index(rng)
    a = _series(i * 10 + k, n, m["base"][k], m["amp"][k], m["noise"][k], 0, m["ymax"] * .98, phase=i, cycles=[2, 2, 3.5][k], spike=m.get("spike", 0) if k else 0)
    out = [a]
    if m.get("two"): out.append(_series(i * 10 + k + 5, n, m["base"][k] * .2, m["amp"][k] * .25, m["noise"][k] * .3, 0, m["ymax"] * .5, phase=i + 1, cycles=[2, 2, 3.5][k]))
    return out

def spark(seed, w=96, h=28, n=24, base=.5, amp=.25):
    vs = _series(seed, n, base * 100, amp * 100, 6, 4, 96)
    d = "M" + "L".join(f"{i*w/(n-1):.1f},{h-(v/100)*h:.1f}" for i, v in enumerate(vs))
    return f'<svg class="spark" viewBox="0 0 {w} {h}" preserveAspectRatio="none" aria-hidden="true"><path class="spark-area" d="{d}L{w},{h}L0,{h}Z"/><path class="spark-line" d="{d}"/></svg>'
