"""Backs scripts/catalog-verify.sh. See that file for what it does."""
import fnmatch
import json
import os
import re
import sys
import tomllib
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CATALOG = os.path.join(ROOT, "crates", "core", "catalog", "models.toml")
ENDPOINT = os.environ.get("HF_ENDPOINT", "https://huggingface.co").rstrip("/")
TOKEN = os.environ.get("HF_TOKEN")
MIB = 1 << 20
# Must match crates/core/src/models/service.rs::estimate and catalog::CONTEXT.
CONTEXT = 8192
KV_KB_PER_TOKEN = 128
BASE_OVERHEAD_MB = 512
BUFFER_PERCENT = 15


def estimate_mb(total_bytes):
    weights = -(-total_bytes // MIB)
    return weights + CONTEXT * KV_KB_PER_TOKEN // 1024 + BASE_OVERHEAD_MB + weights * BUFFER_PERCENT // 100


class Unresolved(Exception):
    pass


def get(url):
    req = urllib.request.Request(url)
    if TOKEN:
        req.add_header("Authorization", f"Bearer {TOKEN}")
    last = None
    for _ in range(3):
        try:
            return urllib.request.urlopen(req, timeout=60)
        except urllib.error.HTTPError as e:
            if e.code in (401, 403, 404):
                raise Unresolved(f"{url}: HTTP {e.code}")
            last = e
        except (urllib.error.URLError, TimeoutError) as e:
            last = e
    print(f"catalog-verify: network failure, nothing was changed: {last}", file=sys.stderr)
    sys.exit(2)


_cache = {}


def sha_of(repo, revision):
    key = ("sha", repo, revision)
    if key not in _cache:
        rev = revision.replace("/", "%2F")
        with get(f"{ENDPOINT}/api/models/{repo}/revision/{rev}") as r:
            _cache[key] = json.load(r)["sha"]
    return _cache[key]


def tree(repo, revision):
    key = ("tree", repo, revision)
    if key in _cache:
        return _cache[key]
    url = f"{ENDPOINT}/api/models/{repo}/tree/{revision.replace('/', '%2F')}?recursive=true"
    out = []
    while url:
        with get(url) as r:
            out += json.load(r)
            m = re.search(r'<([^>]+)>;\s*rel="next"', r.headers.get("Link", ""))
            url = m.group(1) if m and m.group(1).startswith(ENDPOINT) else None
    _cache[key] = out
    return out


def select(entries, include, files):
    patterns = list(include) + list(files)
    chosen = []
    for e in entries:
        if e["type"] != "file":
            continue
        if patterns and not any(fnmatch.fnmatchcase(e["path"], p) for p in patterns):
            continue
        chosen.append((e["path"], e.get("lfs", {}).get("size", e["size"])))
    return chosen


def resolve(recipe):
    """Returns (revision_sha, total_bytes) or raises Unresolved."""
    repo = recipe["repo"]
    sha = sha_of(repo, recipe["revision"])
    entries = tree(repo, sha)
    chosen = select(entries, recipe.get("include", []), recipe.get("files", []))
    paths = {p for p, _ in chosen}
    for f in recipe.get("files", []):
        if f not in paths:
            raise Unresolved(f"{repo}: file {f!r} does not exist")
    if not chosen:
        raise Unresolved(f"{repo}: the selection matches no file")
    mp = recipe["model_path"]
    if mp != "." and mp not in paths:
        raise Unresolved(f"{repo}: model_path {mp!r} is not among the selected files")
    return sha, sum(s for _, s in chosen)


def block_id(lines):
    for line in lines:
        m = re.match(r'id\s*=\s*"([^"]+)"', line)
        if m:
            return m.group(1)
    raise SystemExit("block without an id line")


def blocks(text):
    """Splits the file into (kind, lines): kind is 'head', 'model' or 'recipe'."""
    out = [("head", [])]
    for line in text.splitlines(keepends=True):
        s = line.strip()
        if s == "[[model]]":
            out.append(("model", [line]))
        elif s == "[[model.recipe]]":
            out.append(("recipe", [line]))
        else:
            out[-1][1].append(line)
    return out


def set_key(lines, key, value):
    pat = re.compile(rf"^{key}\s*=")
    for i, line in enumerate(lines):
        if pat.match(line):
            lines[i] = f"{key} = {value}\n"
            return
    raise SystemExit(f"recipe block has no {key} line")


def rewrite(text, results):
    """Fills the resolved values in and drops what did not resolve."""
    bl = blocks(text)
    kept, mid = {}, None
    for kind, lines in bl:
        if kind == "model":
            mid = block_id(lines)
            kept[mid] = 0
        elif kind == "recipe" and f"{mid}/{block_id(lines)}" in results:
            kept[mid] += 1
    out, dropped, mid, skip = [], [], None, False
    for kind, lines in bl:
        if kind == "head":
            out += lines
        elif kind == "model":
            mid = block_id(lines)
            skip = kept[mid] == 0
            if skip:
                dropped.append(f"{mid} (no recipe left)")
            else:
                out += lines
        elif not skip:
            want = results.get(f"{mid}/{block_id(lines)}")
            if want is None:
                dropped.append(f"{mid}/{block_id(lines)}")
                continue
            for k, v in want.items():
                set_key(lines, k, json.dumps(v) if isinstance(v, str) else str(v))
            out += lines
        else:
            dropped.append(f"{mid}/{block_id(lines)}")
    return "".join(out), dropped


def main():
    fix = "--fix" in sys.argv[1:]
    with open(CATALOG, "rb") as f:
        text = f.read().decode()
    data = tomllib.loads(text)
    problems, results = [], {}
    for m in data["model"]:
        for r in m["recipe"]:
            tag = f"{m['id']}/{r['id']}"
            try:
                sha, total = resolve(r)
            except Unresolved as e:
                problems.append(f"{tag}: unresolved: {e}")
                continue
            est = estimate_mb(total)
            want = {"revision": sha, "total_bytes": total, "est_ram_mb": est, "est_vram_mb": est}
            results[tag] = want
            for k, v in want.items():
                if r.get(k) != v:
                    problems.append(f"{tag}: {k} is {r.get(k)!r}, Hugging Face says {v!r}")
    if fix:
        new, dropped = rewrite(text, results)
        if new != text:
            with open(CATALOG, "w") as f:
                f.write(new)
        print(f"catalog-verify: {'rewrote' if new != text else 'left'} {os.path.relpath(CATALOG, ROOT)}")
        for d in dropped:
            print(f"  dropped: {d}")
        for p in problems:
            if "unresolved" in p:
                print(f"  {p}")
        return 0
    n = sum(len(m["recipe"]) for m in data["model"])
    for p in problems:
        print(f"  {p}", file=sys.stderr)
    if problems:
        print(f"catalog-verify: {len(problems)} problem(s) in {n} recipes; run with --fix", file=sys.stderr)
        return 1
    print(f"catalog-verify: {n} recipes of {len(data['model'])} models resolve and match Hugging Face")
    return 0


if __name__ == "__main__":
    sys.exit(main())
