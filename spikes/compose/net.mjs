// Network design check through the Docker-compatible API (what the agent's bollard client uses):
// one network per stack, service names as per-network aliases, only the routed service joins ingress.
import http from "node:http";
import { execFileSync } from "node:child_process";
const sock = process.argv[2];
const IMG = "docker.io/library/busybox:1.37";
const PODMAN = "/run/current-system/sw/bin/podman";
const call = (method, path, body) => new Promise((resolve, reject) => {
  const data = body ? JSON.stringify(body) : undefined;
  const req = http.request({ socketPath: sock, method, path, headers: data ? { "content-type": "application/json", "content-length": Buffer.byteLength(data) } : {} }, (res) => {
    let b = ""; res.on("data", (c) => (b += c)); res.on("end", () => resolve({ status: res.statusCode, body: b }));
  });
  req.on("error", reject); if (data) req.write(data); req.end();
});
const must = (r, what) => { if (r.status >= 300) throw new Error(`${what}: ${r.status} ${r.body}`); return r; };
const names = { nets: ["pk-n-ingress", "pk-n-s1", "pk-n-s2"], ctrs: ["pk-n-db1", "pk-n-web1", "pk-n-db2", "pk-n-web2", "pk-n-edge"] };
const cleanup = async () => { for (const c of names.ctrs) await call("DELETE", `/containers/${c}?force=true`); for (const n of names.nets) await call("DELETE", `/networks/${n}`); };
await cleanup();
for (const n of names.nets) must(await call("POST", "/networks/create", { Name: n, Driver: "bridge" }), `net ${n}`);
async function ctr(name, net, aliases, extraNet) {
  must(await call("POST", `/containers/create?name=${name}`, { Image: IMG, Cmd: ["sleep", "120"], HostConfig: { NetworkMode: net }, NetworkingConfig: { EndpointsConfig: { [net]: { Aliases: aliases } } } }), `create ${name}`);
  must(await call("POST", `/containers/${name}/start`), `start ${name}`);
  if (extraNet) must(await call("POST", `/networks/${extraNet.net}/connect`, { Container: name, EndpointConfig: { Aliases: extraNet.aliases } }), `connect ${name}`);
}
await ctr("pk-n-db1", "pk-n-s1", ["db"]);
await ctr("pk-n-web1", "pk-n-s1", ["web"], { net: "pk-n-ingress", aliases: ["stack1-web"] });
await ctr("pk-n-db2", "pk-n-s2", ["db"]);
await ctr("pk-n-web2", "pk-n-s2", ["web"], { net: "pk-n-ingress", aliases: ["stack2-web"] });
await ctr("pk-n-edge", "pk-n-ingress", ["edge"]); // stands in for Traefik
const ip = (from, name) => {
  let o = "";
  try { o = execFileSync(PODMAN, ["exec", from, "nslookup", name], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }); } catch (e) { o = String(e.stdout || ""); }
  const m = o.match(/Name:\s*\S+\s*\nAddress:\s*(\d+\.\d+\.\d+\.\d+)/);
  return m ? m[1] : "(nxdomain)";
};
const inspect = async (c) => JSON.parse((await call("GET", `/containers/${c}/json`)).body).NetworkSettings.Networks;
const ipOf = async (c, n) => (await inspect(c))[n].IPAddress;
const rows = [
  ["web1 -> db", "pk-n-web1", "db", `db1 = ${await ipOf("pk-n-db1", "pk-n-s1")}`],
  ["web2 -> db", "pk-n-web2", "db", `db2 = ${await ipOf("pk-n-db2", "pk-n-s2")}`],
  ["edge -> stack1-web", "pk-n-edge", "stack1-web", `web1 on ingress = ${await ipOf("pk-n-web1", "pk-n-ingress")}`],
  ["edge -> stack2-web", "pk-n-edge", "stack2-web", `web2 on ingress = ${await ipOf("pk-n-web2", "pk-n-ingress")}`],
  ["edge -> db", "pk-n-edge", "db", "must NOT resolve"],
  ["edge -> web", "pk-n-edge", "web", "must NOT resolve (stack alias stays off ingress)"],
  ["db1 -> stack2-web", "pk-n-db1", "stack2-web", "must NOT resolve (db1 not on ingress)"],
  ["db1 -> db2's net", "pk-n-db1", "pk-n-db2", "other stack's container name must NOT resolve"],
];
for (const [label, from, name, expect] of rows) console.log(label.padEnd(20), "->", ip(from, name).padEnd(12), "|", expect);
console.log("web1 networks:", Object.keys(await inspect("pk-n-web1")).join(", "), "| db1 networks:", Object.keys(await inspect("pk-n-db1")).join(", "));
if (!process.env.KEEP) await cleanup();
