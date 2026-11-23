import json, urllib.request, urllib.error
ids = json.load(open("/tmp/pk-t_87d44fd2/ids.json"))
T, t = ids["T"], ids["token"]
def api(m, p, b=None):
    r = urllib.request.Request(f"http://127.0.0.1:24731/api/v1/teams/{T}{p}", method=m, data=json.dumps(b).encode() if b is not None else None,
                               headers={"Content-Type": "application/json", "Authorization": "Bearer " + t})
    try:
        with urllib.request.urlopen(r) as f:
            d = f.read(); print(m, p, f.status, d[:200]); return json.loads(d) if d else None
    except urllib.error.HTTPError as e:
        print(m, p, e.code, e.read()[:300])
api("POST", "/backup-targets", {"name": "offsite", "endpoint": "http://127.0.0.1:9", "region": "us-east-1", "bucket": "podkit", "path_style": True, "prefix": "pk", "access_key_id": "AKIA", "secret_access_key": "secret"})
