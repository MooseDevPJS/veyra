"""From assets/logo/veyra-1024.png (the SVG rendered with a transparent
background, see MEMORY.md) builds every size the app needs: PNGs and the
Windows .ico. Run: python tools/logo-assets.py"""
from PIL import Image

DIR = 'A:/Cooding/browser/assets/logo/'
master = Image.open(DIR + 'veyra-1024.png').convert('RGBA')
# App-icon variant (bolder outline, bigger in the square): small sizes and
# the .ico, which must read on a light taskbar too.
icon = Image.open(DIR + 'veyra-icon-1024.png').convert('RGBA')
for size in (16, 24, 32, 48, 64):
    icon.resize((size, size), Image.LANCZOS).save(DIR + f'veyra-{size}.png')
for size in (128, 256, 512):
    master.resize((size, size), Image.LANCZOS).save(DIR + f'veyra-{size}.png')
icon.save(DIR + 'veyra.ico', sizes=[(16, 16), (20, 20), (24, 24), (32, 32), (40, 40), (48, 48), (64, 64), (128, 128), (256, 256)])
print('ok')
