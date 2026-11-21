import json, urllib.request, urllib.error
U = "http://127.0.0.1:24731"
tok = None

def call(m, p, body=None, t=None):
    r = urllib.request.Request(U + p, method=m, data=json.dumps(body).encode() if body is not None else None,
                               headers={"Content-Type": "application/json", **({"Authorization": "Bearer " + t} if t else {})})
    try:
        with urllib.request.urlopen(r) as f:
            d = f.read()
            return f.status, (json.loads(d) if d else None)
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()[:300]

creds = {"name": "Dana", "email": "dana@example.com", "password": "correct horse battery"}
print(call("POST", "/api/v1/auth/register", creds))
s, l = call("POST", "/api/v1/auth/login", creds)
t = l["token"]
print(call("POST", "/api/v1/teams", {"name": "Homelab", "slug": "homelab"}, t))
s, teams = call("GET", "/api/v1/teams", None, t)
T = teams[0]["id"]
def api(m, p, b=None):
    s, d = call(m, f"/api/v1/teams/{T}{p}", b, t)
    print(m, p, s, str(d)[:140])
    return d
S = api("GET", "/servers")[0]["id"]
P = api("POST", "/projects", {"name": "Shop", "slug": "shop"})["id"]
P2 = api("POST", "/projects", {"name": "Blog", "slug": "blog"})["id"]
A = api("POST", f"/projects/{P}/applications", {"name": "web", "slug": "web", "server_id": S, "image": "docker.io/traefik/whoami", "container_port": 80})["id"]
api("POST", f"/projects/{P}/applications", {"name": "api", "slug": "api", "server_id": S, "image": "docker.io/library/nginx", "container_port": 80})
print("IDS", T, S, P, P2, A)
open("/tmp/pk-t_87d44fd2/ids.json", "w").write(json.dumps({"T": T, "S": S, "P": P, "P2": P2, "A": A, "token": t}))
