"""Contrast audit core shared by every finalist's contrast.py (WCAG 2.x). Python 3 stdlib only.

Reads tokens.css, resolves both themes and checks:
  TEXT  pairs against 4.5:1  (SC 1.4.3)
  UI    pairs against 3:1    (SC 1.4.11: input/button borders, focus ring, selected-state fills, status dots, chart series)
Alpha colours are composited over the pair's background. Decorative hairlines (--border, --grid) are exempt by
definition (SC 1.4.11 only covers what is needed to identify a component or state) and are listed as such.
"""
import re, os, sys

def _vars(block): return dict(re.findall(r'--([a-z0-9-]+)\s*:\s*([^;]+);', block))

def load(path):
    css = re.sub(r'/\*.*?\*/', '', open(path).read(), flags=re.S)
    base, themes = {}, {"dark": {}, "light": {}}
    for m in re.finditer(r'([^{}]+)\{([^{}]*)\}', css):
        sel, v = m.group(1).strip(), _vars(m.group(2))
        if sel.startswith("@font-face"): continue
        if sel == ":root": base.update(v)
        elif 'data-theme="dark"' in sel: themes["dark"].update(v)
        elif 'data-theme="light"' in sel: themes["light"].update(v)
    return {t: {**base, **tv} for t, tv in themes.items()}

def color(v, vars_, bg=None, depth=0):
    v = v.strip()
    m = re.fullmatch(r'var\(--([a-z0-9-]+)\)', v)
    if m and depth < 6 and m.group(1) in vars_: return color(vars_[m.group(1)], vars_, bg, depth + 1)
    m = re.fullmatch(r'#([0-9a-fA-F]{6})', v)
    if m: h = m.group(1); return tuple(int(h[i:i+2], 16) for i in (0, 2, 4))
    m = re.fullmatch(r'#([0-9a-fA-F]{3})', v)
    if m: return tuple(int(c * 2, 16) for c in m.group(1))
    m = re.fullmatch(r'rgb\(\s*(\d+)\s+(\d+)\s+(\d+)\s*(?:/\s*([\d.]+))?\)', v)
    if m:
        r, g, b = (int(m.group(i)) for i in (1, 2, 3)); a = float(m.group(4) or 1)
        if a < 1 and bg: return tuple(round(a * c + (1 - a) * k) for c, k in zip((r, g, b), bg))
        return (r, g, b)
    return None

def lum(c):
    f = lambda x: (x / 255 / 12.92) if x / 255 <= .03928 else (((x / 255) + .055) / 1.055) ** 2.4
    return .2126 * f(c[0]) + .7152 * f(c[1]) + .0722 * f(c[2])

def ratio(a, b):
    la, lb = lum(a), lum(b)
    if la < lb: la, lb = lb, la
    return (la + .05) / (lb + .05)

SURF = ["bg", "surface", "surface-2", "surface-3"]
TEXT = []
for fg in ("text", "text-2", "text-3"):
    for bg in SURF + ["log-bg", "accent-soft"]: TEXT.append((fg, bg, "body / secondary / meta text"))
for bg in SURF + ["accent-soft"]: TEXT.append(("accent-text", bg, "links, active nav, accent text"))
TEXT += [("err-ink", "err", "text on destructive button"), ("accent-ink", "accent", "text on primary button / status bar"), ("accent-ink", "accent-hover", "primary button hover")]
for s in ("ok", "warn", "err", "info"):
    for bg in ("bg", "surface", "surface-2", "surface-3", s + "-soft", "log-bg"): TEXT.append((s, bg, "status text"))
TEXT += [("accent-text", "log-bg", "log STEP level"), ("text", "err-soft", "banner text"), ("text-2", "err-soft", "banner meta"),
         ("text", "ok-soft", "banner text"), ("text", "warn-soft", "banner text"), ("text", "info-soft", "banner text")]

UI = []
for bg in ("bg", "surface", "surface-2"):
    UI += [("border-ui", bg, "input, select, textarea, button, checkbox, switch outline"), ("focus", bg, "focus ring"),
           ("accent", bg, "selected fill: checkbox, switch, tab bar, progress")]
    for s in ("ok", "warn", "err", "info"): UI.append((s, bg, "status dot / icon"))
    for i in range(1, 7): UI.append((f"chart-{i}", bg, "chart series stroke"))
UI += [("accent-ink", "accent", "check mark / switch thumb on accent")]
UI += [("text-3", "surface", "icon-only glyph (empty state, input icon)")]

def audit(path):
    out = []
    for theme, v in load(path).items():
        bg0 = color(v["bg"], v)
        for kind, pairs, need in (("text", TEXT, 4.5), ("ui", UI, 3.0)):
            for fg, bgn, use in pairs:
                if fg not in v or bgn not in v: continue
                b = color(v[bgn], v, bg0); f = color(v[fg], v, b)
                if not b or not f: continue
                out.append(dict(theme=theme, kind=kind, fg=fg, bg=bgn, use=use, need=need, ratio=ratio(f, b), fg_hex="#%02x%02x%02x" % f, bg_hex="#%02x%02x%02x" % b))
    return out

def report(path, md=False):
    rows = audit(path); bad = [r for r in rows if r["ratio"] + 1e-9 < r["need"]]
    if md:
        o = []
        for theme in ("dark", "light"):
            o.append(f"#### {theme.capitalize()}\n\n| Kind | Foreground | Background | Use | Ratio | Needs | |\n|---|---|---|---|--:|--:|:-:|")
            for r in rows:
                if r["theme"] != theme: continue
                o.append(f"| {r['kind']} | `--{r['fg']}` {r['fg_hex']} | `--{r['bg']}` {r['bg_hex']} | {r['use']} | {r['ratio']:.2f} | {r['need']:g} | {'ok' if r['ratio'] + 1e-9 >= r['need'] else 'FAIL'} |")
            o.append("")
        return "\n".join(o), bad, rows
    return None, bad, rows

def main(here):
    path = os.path.join(here, "tokens.css")
    _, bad, rows = report(path)
    name = os.path.basename(here)
    for theme in ("dark", "light"):
        rs = [r for r in rows if r["theme"] == theme]
        t = [r for r in rs if r["kind"] == "text"]; u = [r for r in rs if r["kind"] == "ui"]
        print(f"{name} {theme:5} text pairs {len(t):3}  min {min(r['ratio'] for r in t):5.2f} (need 4.5)   ui pairs {len(u):3}  min {min(r['ratio'] for r in u):5.2f} (need 3.0)")
    for r in bad: print(f"  FAIL {r['theme']:5} {r['kind']:4} --{r['fg']} {r['fg_hex']} on --{r['bg']} {r['bg_hex']} = {r['ratio']:.2f} < {r['need']:g}  ({r['use']})")
    print(f"{name}: {len(rows)} pairs checked, {len(bad)} failures")
    return 1 if bad else 0
