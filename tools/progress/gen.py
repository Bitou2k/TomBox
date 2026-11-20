"""Render docs/img/progress.svg from tools/progress/progress.json.  usage: py tools/progress/gen.py"""
import json, os
here = os.path.dirname(os.path.abspath(__file__))
d = json.load(open(os.path.join(here, "progress.json"), encoding="utf-8"))
W, pad, bar_w = 860, 24, 812
col = {"works": "#2ea043", "partial": "#d29922", "none": "#6e7681"}
o = []
y = pad + 8
o.append(f'<text x="{pad}" y="{y}" class="h">TomBox progress</text>')
o.append(f'<text x="{W - pad}" y="{y}" class="s" text-anchor="end">updated {d["updated"]}</text>')
y += 22
for b in d["bars"]:
    frac = b["done"] / b["total"]
    o.append(f'<text x="{pad}" y="{y + 12}" class="l">{b["label"]}</text>')
    o.append(f'<text x="{W - pad}" y="{y + 12}" class="l" text-anchor="end">{b["done"]:,} / {b["total"]:,}</text>')
    y += 20
    o.append(f'<rect x="{pad}" y="{y}" width="{bar_w}" height="10" rx="5" fill="#30363d"/>')
    o.append(f'<rect x="{pad}" y="{y}" width="{max(bar_w * frac, 10):.0f}" height="10" rx="5" fill="{"#2ea043" if frac >= 1 else "#1f6feb"}"/>')
    y += 24
    o.append(f'<text x="{pad}" y="{y}" class="s">{b["note"]}</text>')
    y += 22
y += 6
o.append(f'<text x="{pad}" y="{y}" class="l">Subsystems</text>')
y += 12
cw = bar_w // 2
for i, (name, st) in enumerate(d["areas"]):
    x = pad + (i % 2) * cw
    yy = y + (i // 2) * 24
    o.append(f'<circle cx="{x + 6}" cy="{yy + 8}" r="5" fill="{col[st]}"/>')
    o.append(f'<text x="{x + 18}" y="{yy + 12}" class="s2">{name}</text>')
    o.append(f'<text x="{x + cw - 16}" y="{yy + 12}" class="s" text-anchor="end">{st}</text>')
H = y + ((len(d["areas"]) + 1) // 2) * 24 + pad
svg = (f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" font-family="Segoe UI,Helvetica,Arial,sans-serif">'
       '<style>.h{font-size:18px;font-weight:600;fill:#e6edf3}.l{font-size:13px;font-weight:600;fill:#e6edf3}'
       '.s{font-size:12px;fill:#8b949e}.s2{font-size:12px;fill:#c9d1d9}</style>'
       f'<rect width="{W}" height="{H}" rx="10" fill="#0d1117" stroke="#30363d"/>' + "".join(o) + "</svg>")
open(os.path.join(here, "..", "..", "docs", "img", "progress.svg"), "w", encoding="utf-8").write(svg)
print("wrote docs/img/progress.svg", W, H)
