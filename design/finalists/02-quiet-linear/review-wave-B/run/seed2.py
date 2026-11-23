import json, urllib.request, urllib.error
ids = json.load(open("/tmp/pk-t_b6efe1ba/ids.json"))
T, S, P, t = ids["T"], ids["S"], ids["P"], ids["token"]
U = "http://127.0.0.1:26411"
def api(m, p, b=None):
    r = urllib.request.Request(f"{U}/api/v1/teams/{T}{p}", method=m, data=json.dumps(b).encode() if b is not None else None,
                               headers={"Content-Type": "application/json", "Authorization": "Bearer " + t})
    try:
        with urllib.request.urlopen(r) as f:
            d = f.read(); out = json.loads(d) if d else None; s = f.status
    except urllib.error.HTTPError as e:
        out = e.read().decode()[:300]; s = e.code
    print(m, p, s, str(out)[:150]); return out
g = api("POST", f"/projects/{P}/applications", {"name": "site", "slug": "site", "server_id": S, "repo_url": "https://github.com/example/site.git", "git_ref": "main", "container_port": 3000})
if isinstance(g, dict):
    ids["G"] = g["id"]
    json.dump(ids, open("/tmp/pk-t_b6efe1ba/ids.json", "w"))
