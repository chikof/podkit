#!/usr/bin/env python3
"""Rebuilds the vendored UI assets under crates/ui/static from npm tarballs.

    npm pack @fontsource-variable/inter @fontsource-variable/jetbrains-mono \
        @fontsource-variable/sora lucide-static simple-icons   # in an empty directory
    scripts/vendor-ui-assets.py <that directory>

Writes:
  crates/ui/static/fonts/{inter,jetbrains-mono,sora}-latin-wght-normal.woff2   (OFL-1.1)
  crates/ui/static/icons.svg   sprite of the Lucide icons listed in ICONS   (ISC)
  crates/ui/static/logos.svg   sprite of the brand marks listed in LOGOS    (CC0)

Nothing here runs at build time: the outputs are committed, so building podkit
needs no network. Add a name to a list, rerun, commit the result.
"""

import re
import shutil
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
STATIC = ROOT / "crates" / "ui" / "static"

ICONS = """
activity archive arrow-left arrow-right arrow-up-right bell book-open bot box boxes brain
calendar check chevron-down chevron-left chevron-right chevron-up chevrons-up-down
circle circle-alert circle-check circle-help circle-x clock cloud command container copy
cpu database download ellipsis ellipsis-vertical external-link eye eye-off file-text filter
folder folder-open git-branch globe grip-vertical hard-drive history house inbox info key-round
layers layout-dashboard layout-grid link loader-circle lock log-out memory-stick menu minus monitor moon
network package pause pencil play plug plus power refresh-cw rocket rotate-ccw save scroll-text
search server settings shield square sun tag terminal trash-2 triangle-alert undo-2 upload
user users webhook wrench x zap panel-left life-buoy
""".split()

LOGOS = """
postgresql mysql mariadb mongodb redis minio nginx docker github gitlab nodedotjs python rust
go php ruby bun deno nextdotjs react vuedotjs svelte astro nuxt django flask laravel
springboot dotnet openjdk elixir grafana prometheus traefik caddy rabbitmq apachekafka
clickhouse sqlite memcached ollama huggingface openai n8n wordpress ghost nextcloud
vaultwarden plausibleanalytics umami metabase directus supabase gitea forgejo jellyfin
plex homeassistant pihole wireguard tailscale cloudflare letsencrypt nodered portainer
syncthing immich paperlessngx
""".split()


def unpack(src: Path) -> dict[str, Path]:
    out = {}
    for tgz in src.glob("*.tgz"):
        name = re.sub(r"-\d.*$", "", tgz.name)
        dest = src / f"_{name}"
        with tarfile.open(tgz) as t:
            t.extractall(dest)
        out[name] = dest / "package"
    return out


def inner(svg: str) -> str:
    body = re.search(r"<svg[^>]*>(.*)</svg>", svg, re.S).group(1)
    body = re.sub(r"<title>.*?</title>", "", body)
    return re.sub(r"\s*\n\s*", "", body.strip())


def sprite(symbols: list[tuple[str, str, str]], header: str) -> str:
    lines = [f"<!-- {header} -->", '<svg xmlns="http://www.w3.org/2000/svg">']
    for ident, view_box, body in symbols:
        lines.append(f'<symbol id="{ident}" viewBox="{view_box}">{body}</symbol>')
    lines.append("</svg>")
    return "\n".join(lines) + "\n"


def main() -> None:
    pkgs = unpack(Path(sys.argv[1]))
    (STATIC / "fonts").mkdir(parents=True, exist_ok=True)
    # Latin subset only: Cyrillic, Greek and CJK names fall back to the system font.
    for family in ("inter", "jetbrains-mono", "sora"):
        # npm names a scoped package's tarball `fontsource-variable-<family>-<version>.tgz`.
        pkg = pkgs.get(family) or pkgs[f"fontsource-variable-{family}"]
        file = f"{family}-latin-wght-normal.woff2"
        shutil.copy(pkg / "files" / file, STATIC / "fonts" / file)

    lucide = pkgs["lucide-static"]
    symbols = []
    for name in sorted(set(ICONS)):
        path = lucide / "icons" / f"{name}.svg"
        if not path.exists():
            sys.exit(f"missing Lucide icon: {name}")
        symbols.append((name, "0 0 24 24", inner(path.read_text())))
    (STATIC / "icons.svg").write_text(
        sprite(symbols, "Lucide icons, ISC license: https://lucide.dev/license")
    )

    simple = pkgs["simple-icons"]
    symbols, missing = [], []
    for name in sorted(set(LOGOS)):
        path = simple / "icons" / f"{name}.svg"
        if not path.exists():
            missing.append(name)
            continue
        symbols.append((name, "0 0 24 24", inner(path.read_text())))
    (STATIC / "logos.svg").write_text(
        sprite(symbols, "Simple Icons, CC0-1.0. The marks belong to their owners.")
    )
    print(f"icons: {len(ICONS)}  logos: {len(symbols)}  missing logos: {missing}")


if __name__ == "__main__":
    main()
