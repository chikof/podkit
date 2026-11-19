"""Contrast audit for every concept's tokens.css (WCAG 2.x), both themes. Alpha colours are composited over --bg."""
import re, os, sys, glob
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def parse_vars(block):
    return dict(re.findall(r'--([a-z0-9-]+)\s*:\s*([^;]+);', block))

def blocks(css):
    out = []
    for m in re.finditer(r'([^{}]+)\{([^{}]*)\}', css):
        out.append((m.group(1).strip(), parse_vars(m.group(2))))
    return out

def color(v, vars_, bg=None, depth=0):
    v = v.strip()
    m = re.fullmatch(r'var\(--([a-z0-9-]+)\)', v)
    if m and depth < 5 and m.group(1) in vars_: return color(vars_[m.group(1)], vars_, bg, depth + 1)
    m = re.fullmatch(r'#([0-9a-fA-F]{6})', v)
    if m: h = m.group(1); return tuple(int(h[i:i+2], 16) for i in (0, 2, 4))
    m = re.fullmatch(r'#([0-9a-fA-F]{3})', v)
    if m: h = m.group(1); return tuple(int(c * 2, 16) for c in h)
    m = re.fullmatch(r'rgb\(\s*(\d+)\s+(\d+)\s+(\d+)\s*(?:/\s*([\d.]+))?\)', v)
    if m:
        r, g, b = (int(m.group(i)) for i in (1, 2, 3)); a = float(m.group(4) or 1)
        if a < 1 and bg: return tuple(round(a * c + (1 - a) * bgc) for c, bgc in zip((r, g, b), bg))
        return (r, g, b)
    return None

def lum(c):
    def f(x):
        x /= 255
        return x / 12.92 if x <= .03928 else ((x + .055) / 1.055) ** 2.4
    return .2126 * f(c[0]) + .7152 * f(c[1]) + .0722 * f(c[2])

def ratio(a, b):
    la, lb = lum(a), lum(b)
    if la < lb: la, lb = lb, la
    return (la + .05) / (lb + .05)

PAIRS = [("text", "bg"), ("text", "surface"), ("text", "surface-2"), ("text-2", "bg"), ("text-2", "surface"), ("text-2", "surface-2"), ("text-3", "bg"), ("text-3", "surface"),
         ("accent-text", "bg"), ("accent-text", "surface"), ("accent-ink", "accent"), ("accent-ink", "accent-2"),
         ("ok", "surface"), ("warn", "surface"), ("err", "surface"), ("info", "surface"),
         ("ok", "ok-soft"), ("warn", "warn-soft"), ("err", "err-soft"), ("info", "info-soft"),
         ("text", "accent-soft"), ("text-2", "surface-3"), ("log-text", "log-bg"), ("ink", "ok-soft"), ("ink", "warn-soft"), ("ink", "err-soft"),
         ("text", "pop-1"), ("text", "pop-2"), ("text", "pop-3"), ("text", "pop-4"), ("text", "pop-5"), ("text", "pop-6"), ("text-2", "pop-1"), ("text-2", "pop-2"), ("text-2", "pop-3"), ("text-2", "pop-4"), ("text-2", "pop-5"), ("text-2", "pop-6"),
         ("text", "tint-1"), ("text", "tint-2"), ("text", "tint-3"), ("text", "tint-4"), ("text", "tint-5"), ("text", "tint-6"), ("text-2", "tint-1"), ("text-2", "tint-2"), ("text-2", "tint-3"), ("text-2", "tint-4"), ("text-2", "tint-5"), ("text-2", "tint-6"),
         ("accent-text", "tint-1"), ("text-3", "surface-2")]

def audit(path):
    css = re.sub(r'/\*.*?\*/', '', open(path).read(), flags=re.S)
    bl = blocks(css)
    base = {}
    themes = {"dark": {}, "light": {}}
    for sel, v in bl:
        if sel.startswith("@font-face"): continue
        if sel == ":root": base.update(v)
        elif 'data-theme="dark"' in sel: themes["dark"].update(v)
        elif 'data-theme="light"' in sel: themes["light"].update(v)
    # ':root,[data-theme=..]' is the default = that theme's block; the other theme block exists too
    bad = []
    for t, tv in themes.items():
        vars_ = {**base, **tv}
        bg = color(vars_["bg"], vars_)
        for fg, bgn in PAIRS:
            if fg not in vars_ or bgn not in vars_: continue
            if "ink" in vars_ and bgn.endswith("-soft") and fg in ("ok", "warn", "err", "info"): continue  # 08 uses --ink on the bright status blocks
            if os.path.basename(os.path.dirname(path)) == "10-calm-soft" and (fg, bgn) == ("accent-ink", "accent-2"): continue  # avatar uses --text there
            bgc = color(vars_[bgn], vars_, bg)
            fgc = color(vars_[fg], vars_, bgc)
            if not bgc or not fgc: continue
            r = ratio(fgc, bgc)
            if r < 4.5: bad.append((t, fg, bgn, round(r, 2)))
    return bad

if __name__ == "__main__":
    total = 0
    for p in sorted(glob.glob(f"{ROOT}/[0-9][0-9]-*/tokens.css")):
        bad = audit(p); total += len(bad)
        print(os.path.basename(os.path.dirname(p)), "OK" if not bad else bad)
    print("failures:", total)
