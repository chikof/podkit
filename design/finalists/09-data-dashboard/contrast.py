#!/usr/bin/env python3
"""Contrast audit for this finalist (WCAG 2.x, both themes). Exit code 0 = every pair passes.
   python3 contrast.py            print summary + failures
   python3 contrast.py --all      print every pair
The checking logic is the same file in every finalist folder (contrast_core.py, copied next to this script)."""
import os, sys
here = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, here)
import contrast_core as C
if "--all" in sys.argv:
    for r in C.audit(os.path.join(here, "tokens.css")):
        print(f"{r['theme']:5} {r['kind']:4} --{r['fg']:<12} {r['fg_hex']} on --{r['bg']:<10} {r['bg_hex']}  {r['ratio']:6.2f}  need {r['need']:g}  {'ok' if r['ratio'] + 1e-9 >= r['need'] else 'FAIL'}  {r['use']}")
sys.exit(C.main(here))
