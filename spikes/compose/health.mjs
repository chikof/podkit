// Health status through the Docker-compatible API (compose `healthcheck` + depends_on: service_healthy).
import http from "node:http";
const sock = process.argv[2];
const IMG = "docker.io/library/busybox:1.37";
function call(method, path, body) {
  return new Promise((resolve, reject) => {
    const data = body ? JSON.stringify(body) : undefined;
    const req = http.request({ socketPath: sock, method, path, headers: data ? { "content-type": "application/json", "content-length": Buffer.byteLength(data) } : {} }, (res) => {
      let b = ""; res.on("data", (c) => (b += c)); res.on("end", () => resolve({ status: res.statusCode, body: b }));
    });
    req.on("error", reject); if (data) req.write(data); req.end();
  });
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const ver = JSON.parse((await call("GET", "/version")).body);
console.log("api:", ver.ApiVersion, "engine:", ver.Components?.[0]?.Name, ver.Components?.[0]?.Version);

async function run(name, healthcheck) {
  await call("DELETE", `/containers/${name}?force=true`);
  const cfg = { Image: IMG, Cmd: ["sleep", "300"] };
  if (healthcheck) cfg.Healthcheck = healthcheck;
  const c = await call("POST", `/containers/create?name=${name}`, cfg);
  if (c.status !== 201) throw new Error(`create ${name}: ${c.status} ${c.body}`);
  const id = JSON.parse(c.body).Id;
  const s = await call("POST", `/containers/${id}/start`);
  if (s.status >= 300) throw new Error(`start: ${s.status} ${s.body}`);
  return id;
}
async function state(id) {
  const j = JSON.parse((await call("GET", `/containers/${id}/json`)).body);
  return { running: j.State.Running, health: j.State.Health ? j.State.Health.Status : "(no State.Health)", failing: j.State.Health?.FailingStreak, healthcheck: j.Config.Healthcheck?.Test };
}
const NS = 1e9;
// 1. no healthcheck: State.Health is absent
const plain = await run("pk-hc-none", null);
console.log("no healthcheck ->", JSON.stringify(await state(plain)));
// 2. healthcheck that fails until /tmp/ok exists
const id = await run("pk-hc-1", { Test: ["CMD-SHELL", "test -f /tmp/ok"], Interval: 2 * NS, Timeout: 1 * NS, Retries: 1, StartPeriod: 0 });
const t0 = Date.now();
const seen = [];
let touched = false;
for (let i = 0; i < 40; i++) {
  const st = await state(id);
  const label = `${st.health}`;
  if (seen[seen.length - 1]?.label !== label) seen.push({ label, t: ((Date.now() - t0) / 1000).toFixed(1) });
  if (!touched && st.health === "unhealthy") {
    // make the check pass: exec touch /tmp/ok
    const e = JSON.parse((await call("POST", `/containers/${id}/exec`, { Cmd: ["touch", "/tmp/ok"], AttachStdout: false })).body);
    await call("POST", `/exec/${e.Id}/start`, { Detach: true });
    touched = true;
  }
  if (touched && st.health === "healthy") break;
  await sleep(1000);
}
console.log("health transitions (status @ seconds):", seen.map((s) => `${s.label}@${s.t}s`).join(" -> "));
console.log("final:", JSON.stringify(await state(id)));
// 3. HEALTHCHECK disabled via NONE
const none = await run("pk-hc-off", { Test: ["NONE"] });
console.log("Test=[NONE] ->", JSON.stringify(await state(none)));
for (const n of ["pk-hc-none", "pk-hc-1", "pk-hc-off"]) await call("DELETE", `/containers/${n}?force=true`);
