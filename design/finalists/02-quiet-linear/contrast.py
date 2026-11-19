#!/usr/bin/env python3
"""Contrast audit for this finalist (WCAG 2.x, both themes). Exit code 0 = every pair passes.
   python3 contrast.py            audit podkit's server/src/ui/theme.css (the shipped tokens)
   python3 contrast.py --design   audit this folder's tokens.css (the prototype)
   python3 contrast.py --all      print every pair
   python3 contrast.py <file>     audit another tokens.css or theme.css
The checking logic is the same file in every finalist folder (contrast_core.py, copied next to this script);
this copy also reads podkit's theme.css: shadcn names, oklch in light-dark(), mapped back to Quiet's names."""
import os, sys
here = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, here)
import contrast_core as C
args = [a for a in sys.argv[1:] if not a.startswith("--")]
if "--design" in sys.argv: path = os.path.join(here, "tokens.css")
elif args: path = args[0]
else: path = os.path.normpath(os.path.join(here, "..", "..", "..", "server", "src", "ui", "theme.css"))
if "--all" in sys.argv:
    for r in C.audit(path):
        print(f"{r['theme']:5} {r['kind']:4} --{r['fg']:<12} {r['fg_hex']} on --{r['bg']:<10} {r['bg_hex']}  {r['ratio']:6.2f}  need {r['need']:g}  {'ok' if r['ratio'] + 1e-9 >= r['need'] else 'FAIL'}  {r['use']}")
sys.exit(C.main(here, path))
