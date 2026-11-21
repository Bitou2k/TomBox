"""Render an OBJ to a PNG with a tiny z-buffered software rasterizer (flat shading)."""
import sys, math
from PIL import Image
def load(p, with_uv=False):
    V, F, T, FT = [], [], [], []
    for l in open(p):
        if l.startswith("v "): V.append(tuple(map(float, l.split()[1:4])))
        elif l.startswith("vt "): T.append(tuple(map(float, l.split()[1:3])))
        elif l.startswith("f "):
            parts = [x.split("/") for x in l.split()[1:4]]
            F.append(tuple(int(x[0]) - 1 for x in parts))
            if len(parts[0]) > 1 and parts[0][1]: FT.append(tuple(int(x[1]) - 1 for x in parts))
    return (V, F, T, FT) if with_uv else (V, F)
def render(V, F, size=640, yaw=35, pitch=25, T=None, FT=None, tex=None):
    lo = [min(v[i] for v in V) for i in range(3)]; hi = [max(v[i] for v in V) for i in range(3)]
    c = [(a + b) / 2 for a, b in zip(lo, hi)]; r = max(math.dist(lo, hi) / 2, 1e-6)
    cy, sy, cp, sp = math.cos(math.radians(yaw)), math.sin(math.radians(yaw)), math.cos(math.radians(pitch)), math.sin(math.radians(pitch))
    def xf(v):  # Z-up model: yaw about Z, then pitch
        x, y, z = v[0] - c[0], v[1] - c[1], v[2] - c[2]
        x, y = x * cy - y * sy, x * sy + y * cy
        y, z = y * cp - z * sp, y * sp + z * cp
        return x, z, y  # screen x, screen up, depth (into screen)
    P = [xf(v) for v in V]
    s = size * 0.45 / r
    img = Image.new("RGB", (size, size), (30, 32, 38)); px = img.load(); zb = [1e30] * (size * size)
    L = (0.4, 0.8, -0.45); ln = math.sqrt(sum(a * a for a in L)); L = [a / ln for a in L]
    tpx = tex.load() if tex else None
    for fi, f in enumerate(F):
        a, b, d = P[f[0]], P[f[1]], P[f[2]]
        u = [b[i] - a[i] for i in range(3)]; w = [d[i] - a[i] for i in range(3)]
        n = (u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]); nl = math.sqrt(sum(x * x for x in n)) or 1
        sh = abs(sum(n[i] / nl * L[i] for i in range(3))); col = (int(60 + 170 * sh), int(65 + 160 * sh), int(75 + 150 * sh))
        q = [(size / 2 + p[0] * s, size / 2 - p[1] * s, p[2]) for p in (a, b, d)]
        x0, x1 = max(int(min(p[0] for p in q)), 0), min(int(max(p[0] for p in q)) + 1, size - 1)
        y0, y1 = max(int(min(p[1] for p in q)), 0), min(int(max(p[1] for p in q)) + 1, size - 1)
        den = (q[1][1] - q[2][1]) * (q[0][0] - q[2][0]) + (q[2][0] - q[1][0]) * (q[0][1] - q[2][1])
        if abs(den) < 1e-12: continue
        for y in range(y0, y1 + 1):
            for x in range(x0, x1 + 1):
                l0 = ((q[1][1] - q[2][1]) * (x - q[2][0]) + (q[2][0] - q[1][0]) * (y - q[2][1])) / den
                l1 = ((q[2][1] - q[0][1]) * (x - q[2][0]) + (q[0][0] - q[2][0]) * (y - q[2][1])) / den
                l2 = 1 - l0 - l1
                if l0 < 0 or l1 < 0 or l2 < 0: continue
                z = l0 * q[0][2] + l1 * q[1][2] + l2 * q[2][2]
                if z < zb[y * size + x]:
                    zb[y * size + x] = z
                    if tpx is not None and FT:
                        t = FT[fi]; u = l0 * T[t[0]][0] + l1 * T[t[1]][0] + l2 * T[t[2]][0]; v = l0 * T[t[0]][1] + l1 * T[t[1]][1] + l2 * T[t[2]][1]
                        c = tpx[int((u % 1.0) * tex.width) % tex.width, int(((1 - v) % 1.0) * tex.height) % tex.height]
                        px[x, y] = tuple(int(c[k] * (0.35 + 0.65 * sh)) for k in range(3))
                    else:
                        px[x, y] = col
    return img
def checker(n=8, size=256):
    img = Image.new("RGB", (size, size)); p = img.load(); c = size // n
    for y in range(size):
        for x in range(size):
            k = ((x // c) + (y // c)) % 2
            p[x, y] = (230, 120, 40) if k else (40, 90, 200)
            if x < 6 and y < 6: p[x, y] = (255, 255, 255)
    return img

if __name__ == "__main__":
    # objshot.py <obj> <png> [size] [texture.png | checker]
    V, F, T, FT = load(sys.argv[1], True)
    size = int(sys.argv[3]) if len(sys.argv) > 3 else 640
    tex = None
    if len(sys.argv) > 4:
        tex = checker() if sys.argv[4] == "checker" else Image.open(sys.argv[4]).convert("RGB")
    render(V, F, size, T=T, FT=FT, tex=tex).save(sys.argv[2])
