"""Contact sheet of the 10 environment-view (dark) shots for the kanban card. Writes JPEG + base64 text."""
import os, io, base64
from PIL import Image
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
cs = sorted(d for d in os.listdir(f"{ROOT}/_shots") if d[:2].isdigit())
W, H = 256, 160
sheet = Image.new("RGB", (W * 5 + 4 * 4, H * 2 + 4), (60, 60, 60))
for i, c in enumerate(cs):
    im = Image.open(f"{ROOT}/_shots/{c}/environment-dark-1440.png").convert("RGB").crop((0, 0, 1440, 900)).resize((W, H), Image.LANCZOS)
    sheet.paste(im, ((i % 5) * (W + 4), (i // 5) * (H + 4)))
out = f"{os.environ.get('TMPDIR','/tmp')}/env-dark-sheet.jpg"
sheet.save(out, "JPEG", quality=52, optimize=True)
print(out, os.path.getsize(out))
open(out + ".b64", "w").write(base64.b64encode(open(out, "rb").read()).decode())
