"""Builds assets/logo/veyra.svg from the original logo bitmap.

The outline of each shape (the V and the wing) is traced from the image's
alpha mask (boundary following), simplified, and turned into smooth Bezier
curves. The fold (where the right ribbon passes in front of the left one) is
traced from the original's shading. Run: python tools/logo-trace.py"""
import math
from PIL import Image

SRC = 'A:/Cooding/browser/assets/logo/veyra-original.png'
OUT = 'A:/Cooding/browser/assets/logo/veyra.svg'

im = Image.open(SRC).convert('RGBA')
W, H = im.size
A = im.getchannel('A').load()
L = im.convert('L').load()
inside = lambda x, y: 0 <= x < W and 0 <= y < H and A[x, y] > 128


def trace(start):
    """Moore-neighbour boundary following from a boundary pixel."""
    dirs = [(1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1)]
    pts = [start]
    cur, d = start, 6
    for _ in range(200000):
        for k in range(8):
            nd = (d + 6 + k) % 8  # start left of the previous direction
            nx, ny = cur[0] + dirs[nd][0], cur[1] + dirs[nd][1]
            if inside(nx, ny):
                cur, d = (nx, ny), nd
                break
        if cur == start:
            break
        pts.append(cur)
    return pts


def components():
    seen = set()
    out = []
    for y in range(H):
        for x in range(W):
            if inside(x, y) and not inside(x - 1, y) and (x, y) not in seen:
                pts = trace((x, y))
                seen.update(pts)
                if len(pts) > 200:
                    out.append(pts)
    return out


def rdp(pts, eps):
    if len(pts) < 3:
        return pts
    (x1, y1), (x2, y2) = pts[0], pts[-1]
    dx, dy = x2 - x1, y2 - y1
    n = math.hypot(dx, dy) or 1
    best, idx = 0, 0
    for i in range(1, len(pts) - 1):
        d = abs(dy * pts[i][0] - dx * pts[i][1] + x2 * y1 - y2 * x1) / n
        if d > best:
            best, idx = d, i
    if best > eps:
        return rdp(pts[: idx + 1], eps)[:-1] + rdp(pts[idx:], eps)
    return [pts[0], pts[-1]]


def closed_bezier(p):
    n = len(p)
    d = f"M{p[0][0]:.1f},{p[0][1]:.1f}"
    for i in range(n):
        p0, p1, p2, p3 = p[i - 1], p[i], p[(i + 1) % n], p[(i + 2) % n]
        # Sharp corners (the tips) stay sharp: no tangent smoothing there.
        def turn(a, b, c):
            v1 = (b[0] - a[0], b[1] - a[1]); v2 = (c[0] - b[0], c[1] - b[1])
            n1, n2 = math.hypot(*v1) or 1, math.hypot(*v2) or 1
            return (v1[0] * v2[0] + v1[1] * v2[1]) / (n1 * n2)
        k1 = 0 if turn(p0, p1, p2) < 0.2 else 1 / 6
        k2 = 0 if turn(p1, p2, p3) < 0.2 else 1 / 6
        c1 = (p1[0] + (p2[0] - p0[0]) * k1, p1[1] + (p2[1] - p0[1]) * k1)
        c2 = (p2[0] - (p3[0] - p1[0]) * k2, p2[1] - (p3[1] - p1[1]) * k2)
        d += f" C{c1[0]:.1f},{c1[1]:.1f} {c2[0]:.1f},{c2[1]:.1f} {p2[0]:.1f},{p2[1]:.1f}"
    return d + " Z"


comps = components()
comps.sort(key=lambda c: -len(c))
body, wing = comps[0], comps[1]
def rdp_loop(pts, eps):
    # Split the loop at the point farthest from the start (RDP needs two
    # distinct endpoints).
    far = max(range(len(pts)), key=lambda i: math.dist(pts[0], pts[i]))
    return rdp(pts[: far + 1], eps)[:-1] + rdp(pts[far:] + [pts[0]], eps)[:-1]


body_s = rdp_loop(body, 4.0)
wing_s = rdp_loop(wing, 4.0)

# The fold: where the lighter right ribbon passes in front of the left one,
# read off the original's shading (dark crease -> light ribbon), from the
# notch down to the V's left edge.
crease = [(520, 614), (500, 640), (476, 660), (456, 681), (432, 699), (414, 710)]
# Front ribbon: from the notch down along the crease to the bottom of the V,
# then up the V's right outer edge to the right arm's tip and back down its
# inner edge to the notch.
right_tip = max(body, key=lambda p: p[0] - p[1] * 0.2)
bottom = max(body, key=lambda p: p[1])
notch = min(body, key=lambda p: math.dist(p, (521, 603)))  # bottom of the V's inner gap


def path_between(pts, a, b):
    """Boundary points from a to b, the short way round the outline."""
    i, j = pts.index(a), pts.index(b)
    fwd = pts[i : j + 1] if i <= j else pts[i:] + pts[: j + 1]
    back = (pts[j : i + 1] if j <= i else pts[j:] + pts[: i + 1])[::-1]
    return fwd if len(fwd) <= len(back) else back


meet = min(body, key=lambda p: math.dist(p, (406, 712)))
front_outer = rdp(path_between(body, meet, right_tip), 4.0)
front_inner = rdp(path_between(body, right_tip, notch), 4.0)
front = [notch] + crease + front_outer + front_inner[1:-1]
fold = crease + [meet]


def open_bezier(p):
    d = f"M{p[0][0]:.1f},{p[0][1]:.1f}"
    for i in range(len(p) - 1):
        p0 = p[i - 1] if i else p[i]
        p1, p2 = p[i], p[i + 1]
        p3 = p[i + 2] if i + 2 < len(p) else p[i + 1]
        c1 = (p1[0] + (p2[0] - p0[0]) / 6, p1[1] + (p2[1] - p0[1]) / 6)
        c2 = (p2[0] - (p3[0] - p1[0]) / 6, p2[1] - (p3[1] - p1[1]) / 6)
        d += f" C{c1[0]:.1f},{c1[1]:.1f} {c2[0]:.1f},{c2[1]:.1f} {p2[0]:.1f},{p2[1]:.1f}"
    return d


xs = [p[0] for p in body + wing]
ys = [p[1] for p in body + wing]
pad = 24
vb = (min(xs) - pad, min(ys) - pad, max(xs) - min(xs) + 2 * pad, max(ys) - min(ys) + 2 * pad)
svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="{vb[0]} {vb[1]} {vb[2]} {vb[3]}" width="{vb[2]}" height="{vb[3]}">
  <title>Veyra</title>
  <defs>
    <radialGradient id="back" gradientUnits="userSpaceOnUse" cx="508" cy="622" r="290" gradientTransform="translate(508 622) rotate(58) scale(1.45 0.62) translate(-508 -622)">
      <stop offset="0" stop-color="#2E2E32"/>
      <stop offset=".22" stop-color="#6E6E72"/>
      <stop offset=".5" stop-color="#D6D6D9"/>
      <stop offset=".75" stop-color="#F4F4F5"/>
      <stop offset="1" stop-color="#FFFFFF"/>
    </radialGradient>
    <linearGradient id="front" gradientUnits="userSpaceOnUse" x1="760" y1="420" x2="460" y2="760">
      <stop offset="0" stop-color="#FFFFFF"/>
      <stop offset="1" stop-color="#ECECEE"/>
    </linearGradient>
    <linearGradient id="wing" gradientUnits="userSpaceOnUse" x1="880" y1="250" x2="540" y2="540">
      <stop offset="0" stop-color="#FFFFFF"/>
      <stop offset="1" stop-color="#E4E4E7"/>
    </linearGradient>
  </defs>
  <g stroke="#000" stroke-opacity=".2" stroke-width="3" stroke-linejoin="round">
    <path d="{closed_bezier(body_s)}" fill="url(#back)"/>
    <path d="{closed_bezier(front)}" fill="url(#front)" stroke="none"/>
    <path d="{open_bezier([notch] + fold)}" fill="none" stroke-opacity=".16"/>
    <path d="{closed_bezier(wing_s)}" fill="url(#wing)"/>
  </g>
</svg>
'''
open(OUT, 'w', encoding='utf-8').write(svg)
print('components', len(comps), 'body pts', len(body_s), 'wing pts', len(wing_s), 'crease', fold, 'notch', notch, 'tip', right_tip, 'bottom', bottom)
