# Licenses

Everything under `design/concepts` is permissively licensed or a trademark used only as a design placeholder. No runtime network requests: all fonts and icons are local files.

## Fonts (self-hosted, latin subset, woff2)

All fonts are SIL Open Font License 1.1 (OFL), obtained as latin-subset woff2 from the Fontsource packages (npm, https://fontsource.org), which redistribute the upstream OFL fonts. The OFL permits embedding, self-hosting and redistribution; the font name is reserved, so the files are shipped unmodified.

| Concept | Font | License | Package (version) |
|---|---|---|---|
| 01-terminal-ops | JetBrains Mono, all text | SIL OFL 1.1 | @fontsource-variable/jetbrains-mono (5.3.0), declared `OFL-1.1` |
| 02-quiet-linear | Hanken Grotesk, UI + headings | SIL OFL 1.1 | @fontsource-variable/hanken-grotesk (5.3.0), declared `OFL-1.1` |
| 02-quiet-linear | Spline Sans Mono, code, ids, numbers | SIL OFL 1.1 | @fontsource-variable/spline-sans-mono (5.3.0), declared `OFL-1.1` |
| 03-mono-precision | Geist, UI + headings | SIL OFL 1.1 | @fontsource-variable/geist (5.3.0), declared `OFL-1.1` |
| 03-mono-precision | Geist Mono, code, metadata | SIL OFL 1.1 | @fontsource-variable/geist-mono (5.3.0), declared `OFL-1.1` |
| 04-tinted-dark | DM Sans, UI + headings | SIL OFL 1.1 | @fontsource-variable/dm-sans (5.3.0), declared `OFL-1.1` |
| 04-tinted-dark | Source Code Pro, code, metrics | SIL OFL 1.1 | @fontsource-variable/source-code-pro (5.3.1), declared `OFL-1.1` |
| 05-editorial | Fraunces, display, headings | SIL OFL 1.1 | @fontsource-variable/fraunces (5.3.0), declared `OFL-1.1` |
| 05-editorial | Newsreader, body | SIL OFL 1.1 | @fontsource-variable/newsreader (5.3.0), declared `OFL-1.1` |
| 05-editorial | Courier Prime, code, figures | SIL OFL 1.1 | @fontsource/courier-prime (5.3.0), declared `OFL-1.1` |
| 06-playful | Bricolage Grotesque, UI + headings | SIL OFL 1.1 | @fontsource-variable/bricolage-grotesque (5.3.0), declared `OFL-1.1` |
| 06-playful | Martian Mono, code, numbers | SIL OFL 1.1 | @fontsource-variable/martian-mono (5.3.0), declared `OFL-1.1` |
| 07-glass-depth | Sora, UI + headings | SIL OFL 1.1 | @fontsource-variable/sora (5.3.0), declared `OFL-1.1` |
| 07-glass-depth | Fira Code, code, metrics | SIL OFL 1.1 | @fontsource-variable/fira-code (5.3.0), declared `OFL-1.1` |
| 08-brutalist | Archivo, display, UI | SIL OFL 1.1 | @fontsource-variable/archivo (5.3.0), declared `OFL-1.1` |
| 08-brutalist | Space Mono, code, labels | SIL OFL 1.1 | @fontsource/space-mono (5.3.0), declared `OFL-1.1` |
| 09-data-dashboard | IBM Plex Sans, UI + headings | SIL OFL 1.1 | @fontsource-variable/ibm-plex-sans (5.3.0), declared `OFL-1.1` |
| 09-data-dashboard | IBM Plex Mono, values, ids | SIL OFL 1.1 | @fontsource/ibm-plex-mono (5.3.0), declared `OFL-1.1` |
| 10-calm-soft | Figtree, UI + headings | SIL OFL 1.1 | @fontsource-variable/figtree (5.3.0), declared `OFL-1.1` |
| 10-calm-soft | DM Mono, code, numbers | SIL OFL 1.1 | @fontsource/dm-mono (5.3.0), declared `OFL-1.1` |

## Icons

Lucide (ISC License), the same sprite podkit already ships in `server/static/icons.svg`; the few extra glyphs come from `lucide-static` on npm. Icons are inlined per page as an SVG sprite.

## Brand and product logos (`_assets/logos`)

Copied from podkit's existing `server/static/logos` (sourced from homarr-labs/dashboard-icons, Apache-2.0, see its `SOURCES.tsv`) and from the `logo.svg|png` files of podkit's template catalog. The marks are trademarks of their owners and are used here only as realistic placeholders so the mockups show real resource names. They are not part of any concept's identity and must be swapped or confirmed before anything ships.

## podkit mark

The inline pod glyph in the app frame is a throwaway placeholder drawn for these mockups (two rounded rectangles and a dot); it is not a proposed logo.
