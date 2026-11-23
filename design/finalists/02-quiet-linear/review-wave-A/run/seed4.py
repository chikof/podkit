import json, urllib.request, urllib.error
ids = json.load(open("/tmp/pk-t_87d44fd2/ids.json"))
t = ids["token"]
def call(m, p, b=None):
    r = urllib.request.Request("http://127.0.0.1:24731" + p, method=m, data=json.dumps(b).encode() if b is not None else None,
                               headers={"Content-Type": "application/json", "Authorization": "Bearer " + t})
    with urllib.request.urlopen(r) as f:
        d = f.read(); return json.loads(d) if d else None
team = call("POST", "/api/v1/teams", {"name": "Empty", "slug": "empty"})
T2 = team["id"]
P2 = call("POST", f"/api/v1/teams/{T2}/projects", {"name": "Blank", "slug": "blank"})["id"]
A2 = call("POST", f"/api/v1/teams/{T2}/projects/{P2}/applications", {"name": "site", "slug": "site", "server_id": call("GET", f"/api/v1/teams/{T2}/servers")[0]["id"], "image": "docker.io/library/nginx", "container_port": 80})["id"]
ids.update(T2=T2, P2b=P2, A2=A2)
json.dump(ids, open("/tmp/pk-t_87d44fd2/ids.json", "w"))
print(ids)
