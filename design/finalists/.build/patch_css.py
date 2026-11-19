"""One-shot source patch: control borders use --border-ui (WCAG 1.4.11, 3:1); hover borders use --text-2.
Also strips the old max-width:760px blocks (mobile.css replaces them). Safe to re-run (idempotent)."""
import re, sys, os
HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CTRL = re.compile(r'(\.btn(?![-\w])|\.input(?![-\w])|\.search(?![-\w])|\.icon-btn(?![-\w])|\.team(?![-\w])|\.var(?![-\w])|\.chip--btn(?![-\w])|\.pw-btn|textarea)')
def fix(css):
    def rule(m):
        sel, body = m.group(1), m.group(2)
        if '--border-strong' in body and CTRL.search(sel) and '.kbd' not in sel:
            body = body.replace('var(--border-strong)', 'var(--text-2)' if ':hover' in sel else 'var(--border-ui)')
        if CTRL.search(sel) and '.kbd' not in sel and ':hover' not in sel:
            body = re.sub(r'(border(?:-color)?:)((?:var\(--bw\) solid )?)var\(--border\)', r'\1\2var(--border-ui)', body)
        return sel + '{' + body + '}'
    return re.sub(r'([^{}]+)\{([^{}]*)\}', rule, css)
def strip760(css):
    # remove "@media (max-width:760px){...}" blocks (balanced braces)
    out, i = [], 0
    while True:
        j = css.find('@media (max-width:760px){', i)
        if j < 0: out.append(css[i:]); break
        out.append(css[i:j]); k = j + len('@media (max-width:760px){'); depth = 1
        while depth and k < len(css):
            depth += {'{': 1, '}': -1}.get(css[k], 0); k += 1
        i = k
    return ''.join(out)
for p in [f'{HERE}/structure.css'] + [f'{ROOT}/{s}/concept.css' for s in ('01-terminal-ops', '02-quiet-linear', '09-data-dashboard')]:
    c = open(p).read(); n = strip760(fix(c)) if 'concept.css' in p else fix(c)
    open(p, 'w').write(n); print(p.split('finalists/')[1], len(c), '->', len(n))
