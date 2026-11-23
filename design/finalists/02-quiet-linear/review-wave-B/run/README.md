# Run guide: how the wave B review was run

Everything is isolated: its own data dir, its own Postgres database on the shared `pk-pg` container, ports that are not chiko's (8080, 18080, 18443). No agent, no containers, nothing deployed. All commands are from the repo root of `roadmap/DR-B`.

```sh
export PATH=<devenv>/bin:$PATH            # devenv 2.4.0 from the nix store
B=/tmp/pk-t_b6efe1ba                      # data dir, logs, shots
mkdir -p $B/data/server $B/shots

# 1. build (release, offline sqlx)
SQLX_OFFLINE=true devenv shell -- cargo build -p server --release
cp target/release/server $B/server-bin

# 2. own database on the shared container (never the `postgres` one)
podman exec pk-pg createdb -U postgres pk_t_b6efe1ba_live

# 3. an age key for the secrets vault (any 32 random bytes in age's bech32 form)
python3 <script that prints AGE-SECRET-KEY-1...> > $B/age.key

# 4. start (the script in this folder; ports 26411 http, 26412 ingress, 26413 https)
bash design/finalists/02-quiet-linear/review-wave-B/run/up.sh

# 5. seed: a user (dana@example.com), a team, two projects, two apps; then a Git app
python3 .../run/seed.py        # writes $B/ids.json
python3 .../run/seed2.py       # the Git application ("site")
```

The team and instance identity providers were added with one POST each to
`/teams/<team>/settings/authentication/{team,instance}/providers` (Datastar-style JSON of the `p_*` signals: `p_kind`, `p_name`, `p_slug`, `p_client_id`, `p_secret`, `p_scopes`, `p_jit`, `p_enabled`, ...), as a signed-in browser would.

**Chromium** (the harness the review used): `chromium_helpers.py` has `setup(width, theme)`, `shot(name, path, w, theme, outdir=...)`, `go(path)` (signs in again when it lands on `/login`) and `probe()`. `pages.json` is the list walked (32 pages). `audit.js` is the page audit (overflow, one `h1`, contrast, accessible names, third-party requests); its output for the first pass is `audit-chromium.json`.

**Firefox 160** has no webdriver here, so it is driven over Marionette by `ff.py`:

```sh
mkdir -p /tmp/pk-t_b6efe1ba-ff
printf 'user_pref("marionette.port", 28489);\n' > /tmp/pk-t_b6efe1ba-ff/user.js
firefox --headless --marionette --no-remote --profile /tmp/pk-t_b6efe1ba-ff about:blank &
python3 ff_run.py ff           # every page, 1440 and 500 px, dark and light: audit + a screenshot
python3 ff_fix.py ffafter      # the six fixed pages again (pages-fix.json)
```

**Clean up** (only things with the task id in the name): stop the server and Firefox, `podman exec pk-pg dropdb -U postgres pk_t_b6efe1ba_live`, remove `/tmp/pk-t_b6efe1ba*`. Nothing was left running by this review; the proof is in the hand-off.
