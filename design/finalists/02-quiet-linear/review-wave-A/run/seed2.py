import json, sys
sys.path.insert(0, "/tmp/pk-t_87d44fd2")
import urllib.request, urllib.error
ids = json.load(open("/tmp/pk-t_87d44fd2/ids.json"))
T, S, P, P2, A, t = ids["T"], ids["S"], ids["P"], ids["P2"], ids["A"], ids["token"]
U = "http://127.0.0.1:24731"
def api(m, p, b=None):
    r = urllib.request.Request(f"{U}/api/v1/teams/{T}{p}", method=m, data=json.dumps(b).encode() if b is not None else None,
                               headers={"Content-Type": "application/json", "Authorization": "Bearer " + t})
    try:
        with urllib.request.urlopen(r) as f:
            d = f.read(); out = json.loads(d) if d else None; s = f.status
    except urllib.error.HTTPError as e:
        out = e.read().decode()[:300]; s = e.code
    print(m, p, s, str(out)[:150]); return out
ch = api("POST", "/notifications/channels", {"name": "Local hook", "kind": "discord", "url": "http://127.0.0.1:9/api/webhooks/1/token"})
ch2 = api("POST", "/notifications/channels", {"name": "Ops mail", "kind": "discord", "url": "http://127.0.0.1:9/api/webhooks/2/token"})
if isinstance(ch, dict):
    api("POST", "/notifications/rules", {"name": "Everything important", "kinds": ["deploy_failed", "uptime_down", "uptime_up", "cron_failed", "rollback"], "channel_ids": [ch["id"]]})
api("POST", f"/projects/{P}/cron", {"environment_id": P, "name": "clock", "kind": "container", "server_id": S, "image": "docker.io/library/busybox", "command": "date -u", "schedule": "* * * * *"})
api("POST", f"/projects/{P}/cron", {"environment_id": P, "name": "nightly-report", "kind": "container", "server_id": S, "image": "docker.io/library/busybox", "command": "echo report", "schedule": "0 3 * * *", "max_retries": 2})
api("POST", "/uptime/checks", {"name": "Ingress", "kind": "tcp", "host": "127.0.0.1", "port": 24732, "interval_secs": 30, "status_visible": True, "status_name": "Public ingress"})
api("POST", "/uptime/checks", {"name": "Shop site", "kind": "http", "url": "http://127.0.0.1:24731/", "interval_secs": 60, "status_visible": True, "status_name": "Shop"})
api("PUT", "/uptime/status-page", {"enabled": True, "title": "Homelab status"})
api("POST", f"/projects/{P}/mcp", {"name": "Memory", "server_id": S, "image": "docker.io/mcp/memory", "container_port": 8000})
api("GET", "/uptime/checks")
