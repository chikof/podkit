"""Mobile contact sheet: row 1 = environment, row 2 = app, columns = 5 concepts. usage: mobile_sheet.py <first-index> <theme>"""
import sys, os, glob
from PIL import Image
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
first, theme = int(sys.argv[1]), sys.argv[2]
cs = sorted(d for d in os.listdir(f"{ROOT}/_shots") if d[:2].isdigit())[first:first + 5]
W, H = 260, 560
sheet = Image.new("RGB", (len(cs) * (W + 6), 2 * (H + 6)), (128, 128, 128))
for i, c in enumerate(cs):
    for j, s in enumerate(["environment", "app"]):
        im = Image.open(f"{ROOT}/_shots/{c}/{s}-{theme}-390.png").convert("RGB").crop((0, 0, 390, 840)).resize((W, H), Image.LANCZOS)
        sheet.paste(im, (i * (W + 6), j * (H + 6)))
out = f"{os.environ.get('TMPDIR','/tmp')}/mob-{first}-{theme}.png"
sheet.save(out); print(out, cs)
