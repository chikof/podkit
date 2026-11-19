"""Contact sheet of a concept's six screens (top 900px each) for quick review. usage: sheet.py <concept> <theme> [out]"""
import sys, os
from PIL import Image
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
c, t = sys.argv[1], sys.argv[2]
out = sys.argv[3] if len(sys.argv) > 3 else f"{os.environ.get('TMPDIR','/tmp')}/sheet-{c[:2]}-{t}.png"
S = ["projects", "environment", "app", "templates", "server", "playground"]
W, H = 760, 475
sheet = Image.new("RGB", (W * 2 + 6, H * 3 + 12), (128, 128, 128))
for i, s in enumerate(S):
    im = Image.open(f"{ROOT}/_shots/{c}/{s}-{t}-1440.png").convert("RGB")
    im = im.crop((0, 0, 1440, 900)).resize((W, H), Image.LANCZOS)
    sheet.paste(im, ((i % 2) * (W + 6), (i // 2) * (H + 6)))
sheet.save(out)
print(out)
