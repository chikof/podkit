# Licenses

Everything under `design/finalists` is permissively licensed or a trademark used only as a design placeholder. No runtime network requests: fonts, icons and logos are local files.

## Fonts (self-hosted, latin subset, woff2, unmodified)

All SIL Open Font License 1.1, from the Fontsource packages on npm (https://fontsource.org is only the origin; nothing is fetched at runtime).

| Finalist | Font | Use | Package |
|---|---|---|---|
| 01-terminal-ops | JetBrains Mono (variable) | all text | @fontsource-variable/jetbrains-mono 5.3.0 |
| 02-quiet-linear | Hanken Grotesk (variable) | UI, headings | @fontsource-variable/hanken-grotesk 5.3.0 |
| 02-quiet-linear | Spline Sans Mono (variable) | ids, ports, numbers | @fontsource-variable/spline-sans-mono 5.3.0 |
| 09-data-dashboard | IBM Plex Sans (variable) | UI, headings | @fontsource-variable/ibm-plex-sans 5.3.0 |
| 09-data-dashboard | IBM Plex Mono 400, 500 | values, ids | @fontsource/ibm-plex-mono 5.3.0 |

The gallery page (`index.html`) reuses Hanken Grotesk from `02-quiet-linear/fonts/`.

## Icons

Lucide (ISC License), the same sprite podkit ships in `server/static/icons.svg`, inlined per page as an SVG sprite; the few extra glyphs come from `lucide-static`.

## Logos (30 files in `.assets/logos`)

Copied from `design/concepts/_assets/logos` (podkit's `server/static/logos` and template catalog, sourced from homarr-labs/dashboard-icons, Apache-2.0). The marks are trademarks of their owners and appear only as realistic placeholders so the mockups show real resource names; they are not part of any finalist's identity and must be swapped or confirmed before anything ships. Services without a logo (prometheus, loki, alloy, a generic "my-app") show the monogram fallback on purpose.

`caddy.svg`, `clickhouse.svg`, `deepseek.svg`, `docker.svg`, `excalidraw.svg`, `flowise.svg`, `ghost.svg`, `github.svg`, `gotify.svg`, `grafana.svg`, `hermes-agent.svg`, `immich.svg`, `jellyfin.svg`, `langfuse.svg`, `listmonk.png`, `n8n.svg`, `nextdotjs.svg`, `nocodb.svg`, `ollama.svg`, `open-webui.svg`, `paperless.svg`, `penpot.svg`, `plausible.svg`, `pocketbase.svg`, `postgres.svg`, `qwen.svg`, `redis.svg`, `umami.png`, `vaultwarden.svg`, `vllm.svg`

## Code

Everything else (HTML, CSS, JS, generator) is original to this repository. No third-party code is bundled.
