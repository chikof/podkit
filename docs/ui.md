# Working on the web UI

The server also serves the web UI at <http://localhost:8080/>. It is plain
HTML rendered by the server, made interactive with
[Datastar](https://data-star.dev), so there is no separate front-end build.
Templates use Tailwind utility classes in `class="..."`; `crates/ui/build.rs`
runs [encre-css](https://crates.io/crates/encre-css) at compile time to emit
only the CSS those classes need. The design system (tokens, components, the
rules for using them) is specified in [`design/README.md`](design/README.md).
Shared components live in `crates/ui/src/components/`, one file each; their
colors (`bg-primary`, `text-muted-foreground`, ...) are the `--name` variables
in `crates/ui/src/theme.css`, which has light and dark values, follows the
system setting unless the user picked a theme (a cookie the server renders as a
class on `<html>`), and makes a variable added there usable as a color.
`crates/ui/src/components.css` holds what utilities can't express (dialogs,
popovers, anchor positioning, view transitions, container queries). Fonts,
icons and the one small `podkit.js` are vendored in `crates/ui/static/`, so the UI
loads nothing from another origin. In a debug build (or with
`PODKIT_UI_GALLERY=1`) a signed-in user can open `/_ui` to see every component.

Write each class out in full inside a string literal under `crates/ui/src`.
The build fails when a class in a `class` attribute, or in a `&str` constant in
`components/`, is neither a utility nor a `.ui-*` rule in `components.css`.
It never sees maud's `.class` shorthand or class names built at
runtime with `format!`; those get no CSS and nothing reports it. It also can't
tell when a class generates the wrong CSS: encre-css 0.21.1's `divide-y` puts
its border on the wrong side, so use row borders instead, and it has no
`size-*` (write `h-4 w-4`).
