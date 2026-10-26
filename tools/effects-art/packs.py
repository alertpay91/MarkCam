# New effect packs (2026-10-26): letters and numbers, face fun, animated friends, more
# borders and things. Writes SVG / .emoji sources into assets/effects (rendered by effects-art).
# Generate the new MarkCam effect sources (SVG / .emoji) into assets/effects.
#   python -X utf8 tools/effects-art/packs.py .
import os
import subprocess
import sys
import urllib.parse

from PIL import Image

ROOT = sys.argv[1]
ASSETS = os.path.join(ROOT, 'assets')
EFF = os.path.join(ASSETS, 'effects')
CACHE = os.path.join(ASSETS, 'cache', 'fluent')
written = []


def fluent_file(name):
    f = os.path.join(CACHE, name + '.png')
    if os.path.isfile(f) and os.path.getsize(f) > 0:
        return f
    os.makedirs(CACHE, exist_ok=True)
    d = urllib.parse.quote(name)
    low = name.lower().replace(' ', '_')
    base = 'https://raw.githubusercontent.com/microsoft/fluentui-emoji/main/assets/' + d
    for url in (f'{base}/3D/{low}_3d.png', f'{base}/Default/3D/{low}_3d_default.png'):
        if subprocess.run(['curl', '-sfL', '-o', f, url]).returncode == 0:
            return f
    raise SystemExit('missing fluent ' + name)


_bbox = {}


def bbox(name):
    """Content box of a Fluent image, as fractions of its square: (x0, y0, x1, y1)."""
    if name not in _bbox:
        im = Image.open(fluent_file(name)).convert('RGBA')
        a = im.getchannel('A').point(lambda v: 255 if v > 24 else 0)
        x0, y0, x1, y1 = a.getbbox()
        _bbox[name] = (x0 / im.width, y0 / im.height, x1 / im.width, y1 / im.height)
    return _bbox[name]


def fl(name, cx, cy, w=None, h=None, attrs=''):
    """<image> of a Fluent emoji whose visible content is centered at (cx, cy) and is `w` wide
    (or `h` tall)."""
    x0, y0, x1, y1 = bbox(name)
    cw, ch = x1 - x0, y1 - y0
    s = w / cw if w is not None else h / ch
    ix = cx - (x0 + cw / 2) * s
    iy = cy - (y0 + ch / 2) * s
    return f'<image href="emoji:fluent:{name}" x="{ix:.4f}" y="{iy:.4f}" width="{s:.4f}" height="{s:.4f}" {attrs}/>'


def save(cat, name, text):
    d = os.path.join(EFF, cat)
    os.makedirs(d, exist_ok=True)
    ext = '.emoji' if not text.lstrip().startswith('<') else '.svg'
    p = os.path.join(d, name + ext)
    with open(p, 'w', encoding='utf-8', newline='\n') as f:
        f.write(text.strip() + '\n')
    written.append(p)


SHADOW = '<filter id="sh" x="-20%" y="-20%" width="140%" height="150%"><feDropShadow dx="0" dy="10" stdDeviation="10" flood-color="#000" flood-opacity="0.35"/></filter>'
SHADOW_S = '<filter id="sh" x="-30%" y="-30%" width="160%" height="170%"><feDropShadow dx="0" dy="0.04" stdDeviation="0.04" flood-color="#000" flood-opacity="0.35"/></filter>'

# ─────────────────────────────── Letters and numbers ───────────────────────────────
LN = '12 Letters and numbers'
PALETTE = [('#ff6b6b', '#d62f4b'), ('#ffa94d', '#e8590c'), ('#ffd43b', '#f08c00'), ('#69db7c', '#2b8a3e'),
           ('#4dabf7', '#1864ab'), ('#9775fa', '#5f3dc4'), ('#f783ac', '#c2255c'), ('#3bc9db', '#0b7285')]
WORDS = {
    'A': ('apple', 'Red apple'), 'B': ('banana', 'Banana'), 'C': ('cat', 'Cat face'), 'D': ('dog', 'Dog face'),
    'E': ('elephant', 'Elephant'), 'F': ('fish', 'Tropical fish'), 'G': ('grapes', 'Grapes'), 'H': ('house', 'House'),
    'I': ('ice cream', 'Ice cream'), 'J': ('juice', 'Beverage box'), 'K': ('kite', 'Kite'), 'L': ('lion', 'Lion'),
    'M': ('monkey', 'Monkey face'), 'N': ('nest', 'Nest with eggs'), 'O': ('octopus', 'Octopus'), 'P': ('penguin', 'Penguin'),
    'Q': ('question', 'Red question mark'), 'R': ('rabbit', 'Rabbit face'), 'S': ('sun', 'Sun'), 'T': ('tiger', 'Tiger face'),
    'U': ('umbrella', 'Umbrella'), 'V': ('violin', 'Violin'), 'W': ('watermelon', 'Watermelon'), 'X': ('fox', 'Fox'),
    'Y': ('yo-yo', 'Yo-yo'), 'Z': ('zebra', 'Zebra'),
}
TXT = 'font-family="Segoe UI" font-weight="900"'
OUTLINE = 'stroke="#1b1b2f" stroke-opacity="0.55" stroke-width="10" stroke-linejoin="round" paint-order="stroke"'


def card(w, h, c0, c1):
    return f'''<defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{c0}"/><stop offset="1" stop-color="{c1}"/></linearGradient>
    <linearGradient id="gl" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff" stop-opacity="0.55"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient>
    {SHADOW}
  </defs>
  <rect x="20" y="16" width="{w - 40}" height="{h - 44}" rx="56" fill="url(#bg)" stroke="#fff" stroke-width="10" filter="url(#sh)"/>
  <path d="M76 26 H{w - 76} A48 48 0 0 1 {w - 30} 70 V{int(h * 0.3)} Q{w / 2} {int(h * 0.38)} 30 {int(h * 0.3)} V70 A48 48 0 0 1 76 26 Z" fill="url(#gl)"/>'''


for i, (L, (word, pic)) in enumerate(WORDS.items()):
    c0, c1 = PALETTE[i % len(PALETTE)]
    k = word.rfind('x') if L == 'X' else 0
    w_html = (f'{word[:k]}<tspan fill="{c1}">{word[k]}</tspan>{word[k + 1:]}')
    save(LN, f'Letter {L}', f'''
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 600 640">
  {card(600, 640, c0, c1)}
  <text x="300" y="300" text-anchor="middle" {TXT} font-size="270" fill="#fff" {OUTLINE}>{L}{L.lower()}</text>
  <rect x="50" y="368" width="500" height="214" rx="40" fill="#fff" fill-opacity="0.92"/>
  {fl(pic, 150, 475, h=170) if bbox(pic)[3] - bbox(pic)[1] >= bbox(pic)[2] - bbox(pic)[0] else fl(pic, 150, 475, w=170)}
  <text x="{410 if len(word) < 6 else 395}" y="500" text-anchor="middle" {TXT} font-size="{74 if len(word) < 5 else 62 if len(word) < 7 else 52 if len(word) < 9 else 44}" fill="#1b1b2f">{w_html}</text>
</svg>''')

NUMBERS = [(1, 'one', 'Sun'), (2, 'two', 'Cat face'), (3, 'three', 'Banana'), (4, 'four', 'Star'), (5, 'five', 'Red apple'),
           (6, 'six', 'Tropical fish'), (7, 'seven', 'Strawberry'), (8, 'eight', 'Balloon'), (9, 'nine', 'Tangerine'),
           (10, 'ten', 'Red heart')]
for i, (n, word, pic) in enumerate(NUMBERS):
    c0, c1 = PALETTE[(i * 3) % len(PALETTE)]
    W, H = 1000, 600
    per_row = n if n <= 5 else (n + 1) // 2
    rows = 1 if n <= 5 else 2
    size = min(540 / per_row, 400 / rows) * 0.85
    items = []
    for j in range(n):
        r, c = divmod(j, per_row)
        cnt = per_row if r < rows - 1 or n % per_row == 0 else n - per_row * (rows - 1)
        x = 690 + (c - (cnt - 1) / 2) * (560 / per_row)
        y = (300 if rows == 1 else 215 + r * 175) - 10
        items.append(fl(pic, x, y, w=size) if bbox(pic)[2] - bbox(pic)[0] >= bbox(pic)[3] - bbox(pic)[1] else fl(pic, x, y, h=size))
    save(LN, f'Number {n}', f'''
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}">
  {card(W, H, c0, c1)}
  <text x="200" y="370" text-anchor="middle" {TXT} font-size="{300 if n < 10 else 230}" fill="#fff" {OUTLINE}>{n}</text>
  <text x="200" y="490" text-anchor="middle" {TXT} font-size="72" fill="#fff" {OUTLINE.replace('10', '7')}>{word}</text>
  <rect x="390" y="70" width="580" height="440" rx="40" fill="#fff" fill-opacity="0.92"/>
  {chr(10).join('  ' + s for s in items)}
</svg>''')

# ─────────────────────────────────── Face fun ───────────────────────────────────
# Face units: origin between the eyes, eyes at x = ±0.6, mouth ≈ +1.2, chin ≈ +2.1, top of head ≈ −1.9.
FF = '13 Face fun'


def face(vb, body, frames=None, ms=40):
    anim = f' data-frames="{frames}" data-ms="{ms}"' if frames else ''
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{vb}" data-face="true"{anim}>\n{body}\n</svg>'


save(FF, 'Sunglasses', face('-1.45 -0.6 2.9 1.2', '  ' + fl('Sunglasses', 0, 0.02, w=2.75)))
save(FF, 'Blue glasses', face('-1.45 -0.6 2.9 1.2', '  ' + fl('Glasses', 0, 0.02, w=2.6)))
save(FF, 'Ski goggles', face('-1.6 -2.45 3.2 3.0', '  ' + fl('Goggles', 0, -0.92, w=3.0)))
# Diving mask: the lens (upper part of the picture) over the eyes, snorkel on the side.
x0, y0, x1, y1 = bbox('Diving mask')
save(FF, 'Snorkel mask', face('-1.35 -0.9 3.2 3.45', '  ' + fl('Diving mask', 0.25, 0.82, w=2.9)))

AVIATOR_LENS = 'M-0.5 -0.32 C-0.15 -0.36 0.42 -0.36 0.5 -0.22 C0.58 -0.05 0.46 0.3 0.12 0.38 C-0.2 0.44 -0.52 0.3 -0.56 0.02 C-0.6 -0.18 -0.6 -0.3 -0.5 -0.32 Z'
save(FF, 'Aviators', face('-1.45 -0.6 2.9 1.15', f'''  <defs>
    <linearGradient id="lens" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#1a1410" stop-opacity="0.95"/><stop offset="0.55" stop-color="#4a2f1c" stop-opacity="0.85"/><stop offset="1" stop-color="#b07a48" stop-opacity="0.7"/></linearGradient>
    <linearGradient id="gold" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#fff1b8"/><stop offset="0.45" stop-color="#d4a437"/><stop offset="1" stop-color="#7a5714"/></linearGradient>
    <linearGradient id="shine" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#fff" stop-opacity="0.55"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient>
    {SHADOW_S}
  </defs>
  <g filter="url(#sh)">
    <path d="M-1.18 -0.24 L-1.42 -0.2 M1.18 -0.24 L1.42 -0.2" stroke="url(#gold)" stroke-width="0.05" stroke-linecap="round"/>
    <path d="M-0.16 -0.26 Q0 -0.34 0.16 -0.26 M-0.2 -0.33 Q0 -0.4 0.2 -0.33" stroke="url(#gold)" stroke-width="0.035" fill="none"/>
    <g transform="translate(-0.66 0)"><path d="{AVIATOR_LENS}" transform="scale(-1 1)" fill="url(#lens)" stroke="url(#gold)" stroke-width="0.045"/></g>
    <g transform="translate(0.66 0)"><path d="{AVIATOR_LENS}" fill="url(#lens)" stroke="url(#gold)" stroke-width="0.045"/></g>
  </g>
  <path d="M-1.0 -0.24 L-0.78 -0.27 L-1.12 0.12 L-1.18 0.0 Z" fill="url(#shine)"/>
  <path d="M0.4 -0.27 L0.6 -0.29 L0.26 0.14 L0.2 0.02 Z" fill="url(#shine)"/>'''))

HEART = 'M0 0.36 C-0.3 0.16 -0.5 0.02 -0.5 -0.17 C-0.5 -0.33 -0.38 -0.43 -0.24 -0.43 C-0.13 -0.43 -0.04 -0.37 0 -0.28 C0.04 -0.37 0.13 -0.43 0.24 -0.43 C0.38 -0.43 0.5 -0.33 0.5 -0.17 C0.5 0.02 0.3 0.16 0 0.36 Z'
save(FF, 'Heart glasses', face('-1.5 -0.75 3.0 1.45', f'''  <defs>
    <linearGradient id="lens" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ff4d8d" stop-opacity="0.8"/><stop offset="1" stop-color="#c2185b" stop-opacity="0.9"/></linearGradient>
    <linearGradient id="fr" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ff8fb5"/><stop offset="1" stop-color="#b0003a"/></linearGradient>
    {SHADOW_S}
  </defs>
  <g filter="url(#sh)">
    <path d="M-0.18 -0.18 Q0 -0.3 0.18 -0.18" stroke="url(#fr)" stroke-width="0.08" fill="none" stroke-linecap="round"/>
    <path d="M-1.1 -0.2 L-1.45 -0.22 M1.1 -0.2 L1.45 -0.22" stroke="url(#fr)" stroke-width="0.08" stroke-linecap="round"/>
    <g transform="translate(-0.64 0.0) scale({{{{osc2 1 1.1}}}})"><path d="{HEART}" transform="scale(1.15)" fill="url(#lens)" stroke="url(#fr)" stroke-width="0.075"/><ellipse cx="-0.2" cy="-0.22" rx="0.1" ry="0.06" fill="#fff" fill-opacity="0.7" transform="rotate(-30 -0.2 -0.22)"/></g>
    <g transform="translate(0.64 0.0) scale({{{{osc2 1 1.1}}}})"><path d="{HEART}" transform="scale(1.15)" fill="url(#lens)" stroke="url(#fr)" stroke-width="0.075"/><ellipse cx="-0.2" cy="-0.22" rx="0.1" ry="0.06" fill="#fff" fill-opacity="0.7" transform="rotate(-30 -0.2 -0.22)"/></g>
  </g>''', frames=24, ms=40))

STAR = 'M0 -0.5 L0.12 -0.16 L0.48 -0.15 L0.19 0.07 L0.3 0.42 L0 0.21 L-0.3 0.42 L-0.19 0.07 L-0.48 -0.15 L-0.12 -0.16 Z'
save(FF, 'Star glasses', face('-1.5 -0.75 3.0 1.45', f'''  <defs>
    <linearGradient id="lens" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#4dd0ff" stop-opacity="0.75"/><stop offset="1" stop-color="#7b2ff7" stop-opacity="0.85"/></linearGradient>
    <linearGradient id="fr" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff3a0"/><stop offset="1" stop-color="#e09a00"/></linearGradient>
    {SHADOW_S}
  </defs>
  <g filter="url(#sh)">
    <path d="M-0.2 -0.12 Q0 -0.24 0.2 -0.12" stroke="url(#fr)" stroke-width="0.08" fill="none" stroke-linecap="round"/>
    <path d="M-1.12 -0.16 L-1.45 -0.2 M1.12 -0.16 L1.45 -0.2" stroke="url(#fr)" stroke-width="0.08" stroke-linecap="round"/>
    <g transform="translate(-0.64 0) rotate({{{{osc -6 6}}}}) scale(1.2)"><path d="{STAR}" fill="url(#lens)" stroke="url(#fr)" stroke-width="0.07" stroke-linejoin="round"/><circle cx="-0.08" cy="-0.12" r="0.06" fill="#fff" fill-opacity="0.75"/></g>
    <g transform="translate(0.64 0) rotate({{{{osc 6 -6}}}}) scale(1.2)"><path d="{STAR}" fill="url(#lens)" stroke="url(#fr)" stroke-width="0.07" stroke-linejoin="round"/><circle cx="-0.08" cy="-0.12" r="0.06" fill="#fff" fill-opacity="0.75"/></g>
  </g>''', frames=32, ms=40))

save(FF, 'Nerd glasses', face('-1.45 -0.6 2.9 1.2', f'''  <defs>
    <linearGradient id="lens" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#fff" stop-opacity="0.35"/><stop offset="0.5" stop-color="#cfe8ff" stop-opacity="0.12"/><stop offset="1" stop-color="#fff" stop-opacity="0.25"/></linearGradient>
    {SHADOW_S}
  </defs>
  <g filter="url(#sh)" fill="url(#lens)" stroke="#16161a" stroke-width="0.12" stroke-linejoin="round">
    <rect x="-1.12" y="-0.34" width="0.92" height="0.68" rx="0.16"/>
    <rect x="0.2" y="-0.34" width="0.92" height="0.68" rx="0.16"/>
    <path d="M-0.2 -0.1 Q0 -0.22 0.2 -0.1" fill="none"/>
    <path d="M-1.12 -0.2 L-1.42 -0.24 M1.12 -0.2 L1.42 -0.24" fill="none"/>
  </g>
  <path d="M-1.0 -0.24 L-0.84 -0.24 L-1.02 0.2 L-1.06 0.08 Z M0.32 -0.24 L0.48 -0.24 L0.3 0.2 L0.26 0.08 Z" fill="#fff" fill-opacity="0.45"/>
  <rect x="-0.09" y="-0.17" width="0.18" height="0.12" rx="0.03" fill="#fff" stroke="#999" stroke-width="0.015"/>'''))

save(FF, '3D glasses', face('-1.45 -0.6 2.9 1.2', f'''  <defs>{SHADOW_S}</defs>
  <g filter="url(#sh)">
    <rect x="-1.25" y="-0.38" width="2.5" height="0.76" rx="0.14" fill="#f4f4f4" stroke="#c9c9c9" stroke-width="0.02"/>
    <rect x="-1.1" y="-0.26" width="0.86" height="0.52" rx="0.1" fill="#ff2a2a" fill-opacity="0.82"/>
    <rect x="0.24" y="-0.26" width="0.86" height="0.52" rx="0.1" fill="#16d7ff" fill-opacity="0.8"/>
    <path d="M-0.18 0.38 Q0 0.12 0.18 0.38 Z" fill="#000" fill-opacity="0.0"/>
  </g>
  <path d="M-1.02 -0.2 L-0.86 -0.2 L-1.0 0.14 Z M0.32 -0.2 L0.48 -0.2 L0.34 0.14 Z" fill="#fff" fill-opacity="0.45"/>'''))

save(FF, 'Superhero mask', face('-1.45 -0.65 2.9 1.25', f'''  <defs>
    <linearGradient id="m" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ff4b4b"/><stop offset="1" stop-color="#9b0d1a"/></linearGradient>
    {SHADOW_S}
  </defs>
  <g filter="url(#sh)">
    <path fill-rule="evenodd" fill="url(#m)" stroke="#5c0010" stroke-width="0.03" d="M-1.3 -0.42 C-0.8 -0.2 -0.35 -0.42 0 -0.22 C0.35 -0.42 0.8 -0.2 1.3 -0.42 C1.3 -0.05 1.15 0.36 0.7 0.4 C0.35 0.42 0.15 0.22 0 0.22 C-0.15 0.22 -0.35 0.42 -0.7 0.4 C-1.15 0.36 -1.3 -0.05 -1.3 -0.42 Z M-0.6 -0.2 C-0.85 -0.2 -0.95 0.0 -0.88 0.1 C-0.78 0.2 -0.42 0.18 -0.34 0.04 C-0.3 -0.08 -0.4 -0.2 -0.6 -0.2 Z M0.6 -0.2 C0.85 -0.2 0.95 0.0 0.88 0.1 C0.78 0.2 0.42 0.18 0.34 0.04 C0.3 -0.08 0.4 -0.2 0.6 -0.2 Z"/>
  </g>
  <path d="M-1.15 -0.3 C-0.8 -0.14 -0.4 -0.3 -0.1 -0.16" stroke="#fff" stroke-opacity="0.45" stroke-width="0.04" fill="none" stroke-linecap="round"/>'''))

save(FF, 'Eye patch', face('-1.7 -1.05 3.4 1.6', f'''  <defs>
    <radialGradient id="p" cx="0.4" cy="0.35" r="0.7"><stop offset="0" stop-color="#3a3a3a"/><stop offset="1" stop-color="#050505"/></radialGradient>
    {SHADOW_S}
  </defs>
  <g filter="url(#sh)">
    <path d="M-1.65 -0.85 L1.2 0.0 M0.0 -0.05 L1.65 -0.5" stroke="#111" stroke-width="0.07" stroke-linecap="round"/>
    <path d="M0.6 -0.36 C0.95 -0.36 1.0 0.0 0.95 0.18 C0.85 0.42 0.35 0.42 0.25 0.18 C0.2 0.0 0.25 -0.36 0.6 -0.36 Z" fill="url(#p)"/>
  </g>'''))

# Puppy: floppy ears, nose, tongue that licks.
save(FF, 'Puppy', face('-2.35 -2.75 4.7 4.9', f'''  <defs>
    <linearGradient id="ear" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#a86a3d"/><stop offset="1" stop-color="#6b3f1f"/></linearGradient>
    <linearGradient id="in" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#d79a72"/><stop offset="1" stop-color="#b07250"/></linearGradient>
    <radialGradient id="nose" cx="0.38" cy="0.3" r="0.75"><stop offset="0" stop-color="#5a5a5a"/><stop offset="0.5" stop-color="#1b1b1b"/><stop offset="1" stop-color="#000"/></radialGradient>
    <linearGradient id="tg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ff7b9c"/><stop offset="1" stop-color="#e2456b"/></linearGradient>
    {SHADOW_S}
  </defs>
  <g filter="url(#sh)">
    <g transform="translate(-1.15 -1.75) rotate({{{{osc -4 10}}}})"><path d="M0 0 C-0.55 -0.2 -1.15 0.25 -1.1 1.15 C-1.05 1.8 -0.55 2.0 -0.3 1.6 C-0.05 1.2 0.2 0.6 0.25 0.15 Z" fill="url(#ear)"/><path d="M-0.15 0.25 C-0.55 0.15 -0.9 0.55 -0.85 1.15 C-0.82 1.5 -0.55 1.6 -0.4 1.35 C-0.2 1.0 -0.05 0.6 0 0.3 Z" fill="url(#in)"/></g>
    <g transform="translate(1.15 -1.75) rotate({{{{osc 4 -10}}}}) scale(-1 1)"><path d="M0 0 C-0.55 -0.2 -1.15 0.25 -1.1 1.15 C-1.05 1.8 -0.55 2.0 -0.3 1.6 C-0.05 1.2 0.2 0.6 0.25 0.15 Z" fill="url(#ear)"/><path d="M-0.15 0.25 C-0.55 0.15 -0.9 0.55 -0.85 1.15 C-0.82 1.5 -0.55 1.6 -0.4 1.35 C-0.2 1.0 -0.05 0.6 0 0.3 Z" fill="url(#in)"/></g>
    <g transform="translate(0 1.28) scale(1 {{{{osc2 0.75 1.15}}}})"><path d="M-0.26 0 L0.26 0 C0.3 0.35 0.24 0.62 0 0.64 C-0.24 0.62 -0.3 0.35 -0.26 0 Z" fill="url(#tg)"/><path d="M0 0.05 L0 0.4" stroke="#c42d55" stroke-width="0.03" stroke-linecap="round"/></g>
    <path d="M-0.34 0.42 C-0.34 0.28 0.34 0.28 0.34 0.42 C0.34 0.62 0.12 0.78 0 0.78 C-0.12 0.78 -0.34 0.62 -0.34 0.42 Z" fill="url(#nose)"/>
  </g>
  <ellipse cx="-0.1" cy="0.42" rx="0.1" ry="0.05" fill="#fff" fill-opacity="0.6"/>''', frames=30, ms=40))

save(FF, 'Kitty', face('-2.0 -2.7 4.0 3.8', f'''  <defs>
    <linearGradient id="ear" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffb347"/><stop offset="1" stop-color="#e07b12"/></linearGradient>
    <linearGradient id="in" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffc0cf"/><stop offset="1" stop-color="#ff8fab"/></linearGradient>
    <radialGradient id="nose" cx="0.4" cy="0.3" r="0.7"><stop offset="0" stop-color="#ffb3c6"/><stop offset="1" stop-color="#e05780"/></radialGradient>
    {SHADOW_S}
  </defs>
  <g filter="url(#sh)">
    <g transform="translate(-1.0 -1.6) rotate({{{{osc2 -3 5}}}})"><path d="M-0.55 0.25 L-0.35 -0.95 Q-0.3 -1.05 -0.2 -0.98 L0.6 -0.05 Z" fill="url(#ear)" stroke="#b85f0a" stroke-width="0.03" stroke-linejoin="round"/><path d="M-0.38 0.08 L-0.28 -0.68 L0.3 -0.08 Z" fill="url(#in)"/></g>
    <g transform="translate(1.0 -1.6) rotate({{{{osc2 3 -5}}}}) scale(-1 1)"><path d="M-0.55 0.25 L-0.35 -0.95 Q-0.3 -1.05 -0.2 -0.98 L0.6 -0.05 Z" fill="url(#ear)" stroke="#b85f0a" stroke-width="0.03" stroke-linejoin="round"/><path d="M-0.38 0.08 L-0.28 -0.68 L0.3 -0.08 Z" fill="url(#in)"/></g>
    <path d="M-0.17 0.5 L0.17 0.5 Q0.18 0.54 0.14 0.58 L0.02 0.7 Q0 0.72 -0.02 0.7 L-0.14 0.58 Q-0.18 0.54 -0.17 0.5 Z" fill="url(#nose)"/>
  </g>
  <g stroke="#3b2a20" stroke-opacity="0.8" stroke-width="0.03" stroke-linecap="round" fill="none">
    <path d="M-0.35 0.68 Q-0.9 0.55 -1.5 0.5"/><path d="M-0.35 0.76 Q-0.9 0.76 -1.5 0.84"/><path d="M-0.35 0.84 Q-0.85 0.95 -1.35 1.12"/>
    <path d="M0.35 0.68 Q0.9 0.55 1.5 0.5"/><path d="M0.35 0.76 Q0.9 0.76 1.5 0.84"/><path d="M0.35 0.84 Q0.85 0.95 1.35 1.12"/>
  </g>''', frames=30, ms=40))

save(FF, 'Mouse ears', face('-2.3 -3.0 4.6 4.0', f'''  <defs>
    <radialGradient id="ear" cx="0.4" cy="0.35" r="0.7"><stop offset="0" stop-color="#b9b9c4"/><stop offset="1" stop-color="#6e6e7c"/></radialGradient>
    <radialGradient id="in" cx="0.45" cy="0.4" r="0.6"><stop offset="0" stop-color="#ffc9d6"/><stop offset="1" stop-color="#f08aa6"/></radialGradient>
    <radialGradient id="nose" cx="0.4" cy="0.3" r="0.7"><stop offset="0" stop-color="#ffb3c6"/><stop offset="1" stop-color="#d94a72"/></radialGradient>
    {SHADOW_S}
  </defs>
  <g filter="url(#sh)">
    <g transform="translate(-1.35 -2.0) rotate({{{{osc -4 4}}}})"><circle r="0.85" fill="url(#ear)"/><circle r="0.58" cx="0.04" cy="0.04" fill="url(#in)"/></g>
    <g transform="translate(1.35 -2.0) rotate({{{{osc 4 -4}}}})"><circle r="0.85" fill="url(#ear)"/><circle r="0.58" cx="-0.04" cy="0.04" fill="url(#in)"/></g>
    <circle cx="0" cy="0.58" r="0.14" fill="url(#nose)"/>
  </g>
  <g stroke="#333" stroke-opacity="0.75" stroke-width="0.025" stroke-linecap="round" fill="none">
    <path d="M-0.25 0.66 Q-0.8 0.56 -1.35 0.56"/><path d="M-0.25 0.74 Q-0.8 0.78 -1.3 0.9"/>
    <path d="M0.25 0.66 Q0.8 0.56 1.35 0.56"/><path d="M0.25 0.74 Q0.8 0.78 1.3 0.9"/>
  </g>''', frames=30, ms=40))

save(FF, 'Bunny ears', face('-1.9 -4.6 3.8 5.45', f'''  <defs>
    <linearGradient id="ear" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#e9e6ef"/><stop offset="0.5" stop-color="#ffffff"/><stop offset="1" stop-color="#d7d2e0"/></linearGradient>
    <linearGradient id="in" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffb8cb"/><stop offset="1" stop-color="#ff8fab"/></linearGradient>
    <radialGradient id="nose" cx="0.4" cy="0.3" r="0.7"><stop offset="0" stop-color="#ffc2d1"/><stop offset="1" stop-color="#e05780"/></radialGradient>
    {SHADOW_S}
  </defs>
  <g filter="url(#sh)">
    <g transform="translate(-0.65 -1.7) rotate({{{{osc -14 -6}}}})"><path d="M-0.42 0 C-0.6 -1.0 -0.5 -2.4 0 -2.7 C0.5 -2.4 0.6 -1.0 0.42 0 Z" fill="url(#ear)" stroke="#cfc8da" stroke-width="0.03"/><path d="M-0.22 -0.15 C-0.34 -0.9 -0.28 -2.05 0 -2.35 C0.28 -2.05 0.34 -0.9 0.22 -0.15 Z" fill="url(#in)"/></g>
    <g transform="translate(0.65 -1.7) rotate({{{{ph 0.3 16 4}}}})"><path d="M-0.42 0 C-0.6 -1.0 -0.5 -2.4 0 -2.7 C0.5 -2.4 0.6 -1.0 0.42 0 Z" fill="url(#ear)" stroke="#cfc8da" stroke-width="0.03"/><path d="M-0.22 -0.15 C-0.34 -0.9 -0.28 -2.05 0 -2.35 C0.28 -2.05 0.34 -0.9 0.22 -0.15 Z" fill="url(#in)"/></g>
    <path d="M-0.15 0.52 L0.15 0.52 Q0.16 0.56 0.12 0.6 L0.02 0.7 Q0 0.72 -0.02 0.7 L-0.12 0.6 Q-0.16 0.56 -0.15 0.52 Z" fill="url(#nose)"/>
  </g>''', frames=36, ms=40))

# Flower crown along the top of the head.
import math
flowers = ['Cherry blossom', 'Hibiscus', 'Blossom', 'Tulip', 'Sunflower', 'Rose', 'Cherry blossom', 'Hibiscus', 'Blossom']
crown = []
leaves = []
for j, fname in enumerate(flowers):
    a = math.pi * (1.0 - (j + 0.5) / len(flowers))
    x, y = 1.75 * math.cos(a), -1.45 - 0.55 * math.sin(a)
    big = 0.62 if j % 2 == 0 else 0.5
    la = math.degrees(a)
    leaves.append(f'<ellipse cx="{x + 0.25 * math.cos(a + 1.2):.3f}" cy="{y - 0.12:.3f}" rx="0.26" ry="0.1" fill="url(#lf)" transform="rotate({-la + 60:.1f} {x:.3f} {y:.3f})"/>')
    crown.append(f'<g transform="translate({x:.3f} {y:.3f}) rotate({{{{ph {j / len(flowers):.2f} -5 5}}}})">{fl(fname, 0, 0, w=big)}</g>')
save(FF, 'Flower crown', face('-2.25 -2.45 4.5 1.85', f'''  <defs>
    <linearGradient id="lf" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#8be08b"/><stop offset="1" stop-color="#2f8f3a"/></linearGradient>
    {SHADOW_S}
  </defs>
  <g filter="url(#sh)">
  <path d="M-1.8 -1.45 Q0 -2.4 1.8 -1.45" stroke="#3d7a35" stroke-width="0.06" fill="none"/>
  {chr(10).join('  ' + s for s in leaves)}
  {chr(10).join('  ' + s for s in crown)}
  </g>''', frames=36, ms=40))

save(FF, 'Halo', face('-1.6 -3.3 3.2 1.5', f'''  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fffbe0"/><stop offset="0.5" stop-color="#ffd84a"/><stop offset="1" stop-color="#e0a200"/></linearGradient>
    <filter id="glow" x="-50%" y="-100%" width="200%" height="300%"><feGaussianBlur stdDeviation="0.08"/></filter>
  </defs>
  <g transform="translate(0 {{{{osc -2.45 -2.6}}}})">
    <ellipse rx="1.05" ry="0.3" fill="none" stroke="#ffe66b" stroke-opacity="{{{{osc 0.5 0.9}}}}" stroke-width="0.28" filter="url(#glow)"/>
    <ellipse rx="1.05" ry="0.3" fill="none" stroke="url(#g)" stroke-width="0.13"/>
    <ellipse rx="1.05" ry="0.3" fill="none" stroke="#fff" stroke-opacity="0.7" stroke-width="0.03" stroke-dasharray="0.6 2.2" stroke-dashoffset="{{{{lin 0 -5.6}}}}"/>
  </g>''', frames=40, ms=40))

HORN = 'M0 0 C-0.1 -0.45 0.0 -0.85 0.38 -1.05 C0.28 -0.75 0.3 -0.4 0.42 -0.05 Z'
save(FF, 'Devil horns', face('-1.6 -3.2 3.2 1.65', f'''  <defs>
    <linearGradient id="h" x1="0" y1="1" x2="0.4" y2="0"><stop offset="0" stop-color="#8b0000"/><stop offset="0.6" stop-color="#e52222"/><stop offset="1" stop-color="#ff7a5c"/></linearGradient>
    {SHADOW_S}
  </defs>
  <g filter="url(#sh)">
    <g transform="translate(-1.05 -1.65) scale(-1.2 1.2)"><path d="{HORN}" fill="url(#h)" stroke="#5c0000" stroke-width="0.02"/><path d="M0.1 -0.2 C0.06 -0.5 0.14 -0.75 0.33 -0.9" stroke="#fff" stroke-opacity="0.5" stroke-width="0.03" fill="none"/></g>
    <g transform="translate(1.05 -1.65) scale(1.2 1.2)"><path d="{HORN}" fill="url(#h)" stroke="#5c0000" stroke-width="0.02"/><path d="M0.1 -0.2 C0.06 -0.5 0.14 -0.75 0.33 -0.9" stroke="#fff" stroke-opacity="0.5" stroke-width="0.03" fill="none"/></g>
  </g>'''))

save(FF, 'Party hat', face('-1.3 -4.5 3.3 3.35', f'''  <defs>
    <linearGradient id="c" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#7b2ff7"/><stop offset="0.55" stop-color="#b366ff"/><stop offset="1" stop-color="#5a14c9"/></linearGradient>
    <clipPath id="cone"><path d="M0 -2.0 L0.85 0.25 Q0 0.5 -0.85 0.25 Z"/></clipPath>
    {SHADOW_S}
  </defs>
  <g transform="translate(0.3 -1.85) rotate(12)" filter="url(#sh)">
    <path d="M0 -2.0 L0.85 0.25 Q0 0.5 -0.85 0.25 Z" fill="url(#c)"/>
    <g clip-path="url(#cone)">
      <path d="M-1 -0.9 L1 -1.5 L1 -1.2 L-1 -0.6 Z M-1 -0.1 L1 -0.7 L1 -0.4 L-1 0.2 Z" fill="#ffd43b"/>
      <circle cx="-0.25" cy="-0.95" r="0.07" fill="#ff6bcb"/><circle cx="0.3" cy="-0.35" r="0.07" fill="#3bc9db"/><circle cx="-0.4" cy="-0.2" r="0.06" fill="#69db7c"/><circle cx="0.1" cy="-1.45" r="0.05" fill="#3bc9db"/>
    </g>
    <path d="M-0.9 0.25 Q0 0.55 0.9 0.25" stroke="#ff6bcb" stroke-width="0.14" stroke-dasharray="0.08 0.05" fill="none"/>
    <g transform="translate(0 -2.05) scale({{{{osc2 0.9 1.15}}}})"><circle r="0.2" fill="#ff6bcb"/><circle r="0.2" fill="#ffd43b" fill-opacity="0.5" transform="rotate(45) scale(0.6 1)"/></g>
  </g>''', frames=24, ms=40))

save(FF, 'Top hat', face('-1.35 -4.05 2.7 2.3', '  ' + fl('Top hat', 0, -2.9, w=2.45)))
save(FF, 'Sun hat', face('-1.9 -3.95 3.8 2.8', '  ' + fl('Womans hat', 0, -2.55, w=3.6)))
save(FF, 'Cap', face('-1.6 -3.5 3.2 2.2', '  ' + fl('Billed cap', 0, -2.45, w=2.8)))
save(FF, 'Clown nose', face('-0.5 0.15 1.0 0.9', '''  <defs><radialGradient id="n" cx="0.36" cy="0.32" r="0.75"><stop offset="0" stop-color="#ff8a80"/><stop offset="0.45" stop-color="#e01b1b"/><stop offset="1" stop-color="#7a0000"/></radialGradient></defs>
  <circle cx="0" cy="0.58" r="{{osc2 0.32 0.36}}" fill="url(#n)"/>
  <ellipse cx="-0.1" cy="0.46" rx="0.1" ry="0.06" fill="#fff" fill-opacity="0.75" transform="rotate(-30 -0.1 0.46)"/>''').replace('data-face="true"', 'data-face="true" data-frames="20" data-ms="40"'))
save(FF, 'Blush', face('-1.55 0.2 3.1 1.1', '''  <defs><radialGradient id="b"><stop offset="0" stop-color="#ff6f91" stop-opacity="0.7"/><stop offset="1" stop-color="#ff6f91" stop-opacity="0"/></radialGradient></defs>
  <ellipse cx="-0.95" cy="0.75" rx="0.5" ry="0.3" fill="url(#b)"/><ellipse cx="0.95" cy="0.75" rx="0.5" ry="0.3" fill="url(#b)"/>
  <g stroke="#e8436b" stroke-opacity="0.75" stroke-width="0.04" stroke-linecap="round"><path d="M-1.15 0.62 L-1.25 0.86 M-0.97 0.62 L-1.07 0.86 M-0.79 0.62 L-0.89 0.86 M0.79 0.62 L0.69 0.86 M0.97 0.62 L0.87 0.86 M1.15 0.62 L1.05 0.86"/></g>'''))
save(FF, 'Kisses', face('-1.55 0.2 3.1 1.2', '  ' + fl('Kiss mark', -1.0, 0.8, w=0.6, attrs='transform="rotate(-20 -1.0 0.8)"') + '\n  ' + fl('Kiss mark', 1.0, 0.75, w=0.55, attrs='transform="rotate(15 1.0 0.75)"') + '\n  ' + fl('Kiss mark', 0.85, -1.2, w=0.45, attrs='transform="rotate(10 0.85 -1.2)"')).replace('viewBox="-1.55 0.2 3.1 1.2"', 'viewBox="-1.55 -1.5 3.1 2.9"'))

# ─────────────────────────────── Animated friends ───────────────────────────────
AF = '14 Animated friends'


def obj(vb, body, frames=40, ms=40):
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{vb}" data-frames="{frames}" data-ms="{ms}">\n  <defs>{SHADOW}</defs>\n{body}\n</svg>'


def zzz(x, y):
    out = []
    for k in range(3):
        p = k / 3
        out.append(f'<g transform="translate({{{{loop {p:.2f} {x} {x + 110}}}}} {{{{loop {p:.2f} {y} {y - 200}}}}}) scale({{{{loop {p:.2f} 0.4 1.1}}}})" opacity="{{{{ph {p + 0.5:.2f} 0 1}}}}">{fl("Zzz", 0, 0, w=110)}</g>')
    return '\n  '.join(out)


save(AF, 'Sleepy panda', obj('0 0 600 600', f'''  <ellipse cx="300" cy="545" rx="190" ry="26" fill="#000" fill-opacity="0.18"/>
  <g transform="translate(270 360) rotate({{{{osc -3 3}}}}) scale({{{{osc 1 1.04}}}} {{{{osc 1 0.97}}}})" filter="url(#sh)">
    {fl('Panda', 0, 0, w=360)}
    <ellipse cx="-62" cy="-8" rx="38" ry="32" fill="#4a3d6e"/><ellipse cx="62" cy="-8" rx="38" ry="32" fill="#4a3d6e"/>
    <path d="M-82 -6 Q-62 12 -42 -6 M42 -6 Q62 12 82 -6" stroke="#1a1430" stroke-width="7" fill="none" stroke-linecap="round"/>
  </g>
  {zzz(390, 220)}'''))

save(AF, 'Singing cat', obj('0 0 600 600', f'''  <g transform="translate(260 350) rotate({{{{osc -6 6}}}})" filter="url(#sh)">
    {fl('Cat face', 0, -40, w=300)}
    <g transform="translate(40 120) rotate({{{{osc2 -8 2}}}})">{fl('Guitar', 0, 0, w=260)}</g>
  </g>
  {''.join(f'<g transform="translate({{{{loop {k / 3:.2f} 400 520}}}} {{{{loop {k / 3:.2f} 260 40}}}}) rotate({{{{ph {k / 3:.2f} -20 20}}}})" opacity="{{{{ph {k / 3 + 0.5:.2f} 0 1}}}}">{fl("Musical note" if k % 2 else "Musical notes", 0, 0, w=80)}</g>' for k in range(3))}'''))

save(AF, 'Hungry cat', obj('0 0 600 600', f'''  <g transform="translate(220 400) rotate({{{{osc2 -3 3}}}})" filter="url(#sh)">{fl('Cat face', 0, 0, w=320)}</g>
  <g transform="translate(420 170) scale({{{{osc 0.95 1.05}}}})">
    {fl('Thought balloon', 0, 0, w=260)}
    <g transform="rotate({{{{osc -10 10}}}})">{fl('Fish', 0, -10, w=130)}</g>
  </g>'''))

save(AF, 'Chewing cat', obj('0 0 600 600', f'''  <g transform="translate(300 330)" filter="url(#sh)">
    <g transform="scale({{{{osc2 1.0 1.04}}}} {{{{osc2 1.0 0.94}}}}) translate(0 {{{{osc2 0 10}}}})">{fl('Cat face', 0, 0, w=380)}</g>
    <g transform="translate(140 110) rotate({{{{osc2 -10 6}}}})">{fl('Cookie', 0, 0, w=130)}</g>
  </g>
  {''.join(f'<circle cx="{c}" cy="{{{{loop {k / 4:.2f} 460 560}}}}" r="7" fill="#f2c27a" opacity="{{{{ph {k / 4 + 0.5:.2f} 0 1}}}}"/>' for k, c in enumerate([370, 400, 430, 455]))}''', frames=24, ms=40))

save(AF, 'Fainting cat', obj('0 0 700 600', f'''  <g transform="translate(440 520) rotate({{{{osc 0 -80}}}}) translate(0 -165)" filter="url(#sh)">{fl('Cat face', 0, 0, w=320)}</g>
  {''.join(f'<g transform="translate({{{{ph {k / 4:.2f} 300 560}}}} {{{{ph {k / 4 + 0.25:.2f} 110 190}}}})" opacity="{{{{osc 0.2 1}}}}">{fl("Dizzy", 0, 0, w=70)}</g>' for k in range(4))}''', frames=48, ms=40))

save(AF, 'Royal corgi', obj('0 0 600 600', f'''  <ellipse cx="290" cy="540" rx="200" ry="22" fill="#000" fill-opacity="0.18"/>
  <g transform="translate(290 {{{{osc2 360 345}}}})" filter="url(#sh)">
    {fl('Dog', 0, 0, w=440)}
    <g transform="translate(-120 -185) rotate({{{{osc2 -12 -4}}}})">{fl('Crown', 0, 0, w=130)}</g>
  </g>
  {''.join(f'<g transform="translate({x} {y}) scale({{{{ph {k / 3:.2f} 0.3 1}}}})" opacity="{{{{ph {k / 3:.2f} 0 1}}}}">{fl("Sparkles", 0, 0, w=70)}</g>' for k, (x, y) in enumerate([(90, 120), (470, 90), (520, 260)]))}''', frames=24, ms=40))

save(AF, 'Party dog', obj('0 0 600 600', f'''  <g transform="translate(300 {{{{osc2 350 330}}}}) rotate({{{{osc -6 6}}}})" filter="url(#sh)">
    {fl('Dog face', 0, 30, w=340)}
    <g transform="translate(90 -150) rotate(18)">
      <path d="M0 -150 L62 20 Q0 40 -62 20 Z" fill="#7b2ff7"/><path d="M-30 -60 L40 -80 L48 -58 L-22 -38 Z M-50 0 L55 -30 L60 -10 L-55 20 Z" fill="#ffd43b"/>
      <circle cx="0" cy="-152" r="17" fill="#ff6bcb"/>
    </g>
  </g>
  {''.join(f'<rect x="{x}" y="{{{{loop {k / 9:.2f} -20 620}}}}" width="14" height="22" rx="3" fill="{col}" transform="rotate({{{{loop {k / 9:.2f} 0 {360 if k % 2 else -360}}}}} {x + 7} 300)"/>' for k, (x, col) in enumerate(zip([40, 110, 170, 240, 320, 390, 450, 520, 570], ['#ff6b6b', '#ffd43b', '#4dabf7', '#69db7c', '#f783ac', '#9775fa', '#ffa94d', '#3bc9db', '#ff6b6b'])))}''', frames=40, ms=40))

save(AF, 'Swimming fish', obj('0 0 700 500', f'''  <g transform="translate({{{{loop 0 -150 850}}}} {{{{osc2 260 220}}}}) rotate({{{{osc2 -6 6}}}}) scale(-1 1)" filter="url(#sh)">{fl('Tropical fish', 0, 0, w=240)}</g>
  {''.join(f'<circle cx="{{{{ph {k / 5:.2f} {200 + 60 * k} {230 + 60 * k}}}}}" cy="{{{{loop {k / 5:.2f} 470 20}}}}" r="{8 + 4 * (k % 3)}" fill="#bfefff" fill-opacity="0.45" stroke="#fff" stroke-opacity="0.8" stroke-width="2"/>' for k in range(5))}''', frames=60, ms=40))

save(AF, 'Shark fin', obj('0 0 900 360', f'''  <defs><linearGradient id="fin" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#9fb3c8"/><stop offset="1" stop-color="#4a5d73"/></linearGradient>
  <linearGradient id="sea" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#2fa8e0" stop-opacity="0.85"/><stop offset="1" stop-color="#0b4f8a" stop-opacity="0.95"/></linearGradient></defs>
  <g transform="translate({{{{loop 0 -150 1050}}}} {{{{osc2 215 228}}}}) scale(1.7)"><path d="M-70 40 C-40 -10 0 -110 50 -130 C30 -80 40 -10 80 40 Z" fill="url(#fin)"/><path d="M-30 30 C-10 -10 20 -70 40 -100" stroke="#fff" stroke-opacity="0.35" stroke-width="5" fill="none"/></g>
  <path d="M0 240 {' '.join(f'Q{x + 37.5:.1f} {{{{osc 222 258}}}} {x + 75} 240' for x in range(0, 900, 75))} V360 H0 Z" fill="url(#sea)"/>
  <path d="M0 236 {' '.join(f'Q{x + 37.5:.1f} {{{{osc 218 254}}}} {x + 75} 236' for x in range(0, 900, 75))}" stroke="#fff" stroke-opacity="0.75" stroke-width="6" fill="none"/>''', frames=60, ms=40))

for nm, code in [('Happy cat', '1f63a'), ('Cat in love', '1f63b'), ('Shocked cat', '1f640'), ('Crying cat', '1f63f'),
                 ('Laughing cat', '1f639'), ('Poodle', '1f429'), ('Guide dog', '1f9ae'), ('Baby chick', '1f425'),
                 ('Blowfish', '1f421'), ('Dragon', '1f409'), ('Dancer', '1f483')]:
    save(AF, nm, f'source: noto {code}')

# ─────────────────────────────────── Borders ───────────────────────────────────
BD = '10 Borders'


def full(body, frames=None, ms=60):
    # Full-screen frames stay still: 1920x1080 animations would cost hundreds of MB decoded.
    anim = ''
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1920 1080" data-fullscreen="true"{anim}>\n  <defs>{SHADOW}</defs>\n{body}\n</svg>'


# ABC: letter blocks along the top and bottom edges.
blocks = []
cols = ['#ff6b6b', '#ffa94d', '#ffd43b', '#69db7c', '#4dabf7', '#9775fa', '#f783ac', '#3bc9db']
alpha = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'
k = 0
for row_y in (18, 962):
    for i in range(16):
        x = 18 + i * 118.5
        c = cols[k % len(cols)]
        ch = alpha[k % 26]
        rot = (-6, 4, -3, 6, -4)[k % 5]
        blocks.append(f'<g transform="rotate({rot} {x + 50:.0f} {row_y + 50})" filter="url(#sh)"><rect x="{x:.0f}" y="{row_y}" width="100" height="100" rx="16" fill="{c}" stroke="#fff" stroke-width="6"/><text x="{x + 50:.0f}" y="{row_y + 76}" text-anchor="middle" {TXT} font-size="74" fill="#fff" stroke="#00000040" stroke-width="4" paint-order="stroke">{ch}</text></g>')
        k += 1
for col_x in (18, 1802):
    for i in range(7):
        y = 140 + i * 118
        c = cols[k % len(cols)]
        ch = alpha[k % 26]
        blocks.append(f'<g filter="url(#sh)"><rect x="{col_x}" y="{y}" width="100" height="100" rx="16" fill="{c}" stroke="#fff" stroke-width="6"/><text x="{col_x + 50}" y="{y + 76}" text-anchor="middle" {TXT} font-size="74" fill="#fff" stroke="#00000040" stroke-width="4" paint-order="stroke">{ch}</text></g>')
        k += 1
save(BD, 'ABC border', full('  ' + '\n  '.join(blocks)))

# Under the sea: corners with coral, fish, octopus, bubbles rising.
sea = [fl('Coral', 120, 960, w=240), fl('Coral', 1800, 980, w=200), fl('Octopus', 300, 980, w=190),
       fl('Crab', 1620, 1010, w=150), fl('Spiral shell', 470, 1030, w=90),
       f'<g transform="translate({{{{osc 1500 1700}}}} 140) scale({{{{ph 0.25 -1 1}}}} 1)">{fl("Tropical fish", 0, 0, w=150)}</g>',
       f'<g transform="translate({{{{osc 380 160}}}} 200) scale({{{{ph 0.25 1 -1}}}} 1)">{fl("Fish", 0, 0, w=130)}</g>',
       f'<g transform="translate(1760 {{{{osc 600 520}}}})">{fl("Jellyfish", 0, 0, w=140)}</g>']
for j in range(10):
    x = 60 + (j * 197) % 1800
    side = 40 if j % 2 else 1880
    sea.append(f'<circle cx="{{{{ph {j / 10:.2f} {side - 15} {side + 15}}}}}" cy="{{{{loop {j / 10:.2f} 1080 0}}}}" r="{10 + (j % 4) * 5}" fill="#bfefff" fill-opacity="0.35" stroke="#fff" stroke-opacity="0.85" stroke-width="3"/>')
save(BD, 'Under the sea', full('  ' + '\n  '.join(sea), frames=40, ms=60))

halloween = [fl('Spider web', 130, 130, w=280), fl('Spider web', 1790, 130, w=280, attrs='transform="scale(-1 1) translate(-3580 0)"'),
             fl('Jack-o-lantern', 150, 950, w=230), fl('Jack-o-lantern', 1780, 970, w=180), fl('Candy', 340, 1010, w=110),
             f'<g transform="translate({{{{osc 300 700}}}} {{{{osc2 120 170}}}})">{fl("Bat", 0, 0, w=130)}</g>',
             f'<g transform="translate({{{{osc 1600 1250}}}} {{{{osc2 200 140}}}})">{fl("Bat", 0, 0, w=110)}</g>',
             f'<g transform="translate(1790 {{{{osc 420 560}}}})"><path d="M0 -500 V-40" stroke="#ddd" stroke-width="3"/>{fl("Spider", 0, 0, w=110)}</g>',
             f'<g transform="translate(1560 {{{{osc2 900 870}}}})" opacity="{{{{osc 0.6 0.95}}}}">{fl("Ghost", 0, 0, w=170)}</g>']
save(BD, 'Halloween', full('  ' + '\n  '.join(halloween), frames=40, ms=60))

lights = ['<path d="M0 40 ' + ' '.join(f'Q{x + 60} 120 {x + 120} 40' for x in range(0, 1920, 120)) + '" stroke="#2f5d2f" stroke-width="6" fill="none"/>']
bulb_cols = ['#ff3b3b', '#ffd43b', '#4dabf7', '#69db7c', '#f783ac']
for j, x in enumerate(range(60, 1920, 120)):
    c = bulb_cols[j % len(bulb_cols)]
    lights.append(f'<g transform="translate({x} 82)"><rect x="-9" y="-4" width="18" height="16" rx="3" fill="#555"/><ellipse cy="34" rx="17" ry="26" fill="{c}" opacity="{{{{ph {(j % 3) / 3:.2f} 0.45 1}}}}"/><ellipse cy="34" rx="36" ry="44" fill="{c}" opacity="{{{{ph {(j % 3) / 3:.2f} 0 0.35}}}}"/><ellipse cx="-6" cy="24" rx="5" ry="9" fill="#fff" fill-opacity="0.6"/></g>')
lights += [fl('Christmas tree', 150, 900, h=330), fl('Snowman', 1770, 930, h=280), fl('Wrapped gift', 330, 1010, w=120), fl('Wrapped gift', 1580, 1015, w=110)]
for j in range(14):
    x = (j * 139 + 40) % 1920
    lights.append(f'<g transform="translate({{{{ph {j / 14:.2f} {x - 20} {x + 20}}}}} {{{{loop {j / 14:.2f} 120 1080}}}})" opacity="0.9">{fl("Snowflake", 0, 0, w=34 + (j % 3) * 12)}</g>')
save(BD, 'Christmas', full('  ' + '\n  '.join(lights), frames=30, ms=80))

beach = ['<defs><linearGradient id="sand" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#ffe3a3"/><stop offset="1" stop-color="#e8b864"/></linearGradient></defs>',
         '<path d="M0 960 Q300 920 640 970 Q960 1010 1280 965 Q1620 925 1920 965 V1080 H0 Z" fill="url(#sand)" filter="url(#sh)"/>',
         fl('Palm tree', 170, 760, h=560), fl('Palm tree', 1790, 800, h=460, attrs='transform="scale(-1 1) translate(-3580 0)"'),
         fl('Spiral shell', 560, 1010, w=80), fl('Crab', 1390, 1000, w=120), fl('Bucket', 1560, 990, w=110) if False else fl('Beach with umbrella', 1560, 960, w=150),
         f'<g transform="translate(1700 {{{{osc 120 150}}}})">{fl("Sun", 0, 0, w=200)}</g>']
save(BD, 'Beach', full('  ' + '\n  '.join(beach), frames=24, ms=80))

spring = [fl('Tulip', 70, 980, h=200), fl('Sunflower', 180, 960, h=240), fl('Cherry blossom', 290, 1020, w=110), fl('Hibiscus', 380, 1030, w=100),
          fl('Tulip', 1850, 980, h=200), fl('Sunflower', 1740, 960, h=240), fl('Blossom', 1630, 1020, w=110), fl('Rose', 1540, 1030, w=100),
          fl('Seedling', 470, 1040, w=80), fl('Four leaf clover', 1450, 1040, w=80),
          f'<g transform="translate({{{{osc 300 600}}}} {{{{osc2 250 180}}}}) rotate({{{{osc2 -10 10}}}})">{fl("Butterfly", 0, 0, w=110)}</g>',
          f'<g transform="translate({{{{osc 1650 1350}}}} {{{{osc2 300 230}}}}) rotate({{{{osc2 10 -10}}}})">{fl("Butterfly", 0, 0, w=90)}</g>']
for j in range(8):
    x = (j * 251 + 80) % 1920
    spring.append(f'<g transform="translate({{{{ph {j / 8:.2f} {x - 40} {x + 40}}}}} {{{{loop {j / 8:.2f} -40 1080}}}}) rotate({{{{loop {j / 8:.2f} 0 360}}}})" opacity="0.85">{fl("Cherry blossom", 0, 0, w=40)}</g>')
save(BD, 'Spring flowers', full('  ' + '\n  '.join(spring), frames=40, ms=60))

# ─────────────────────────────── Things and places ───────────────────────────────
TP = '11 Things and places'
for nm, f in [('Racing car', 'Racing car'), ('Sports medal', 'Sports medal'), ('Award ribbon', 'Rosette'), ('Goal', 'Goal net'),
              ('Notebook', 'Notebook with decorative cover'), ('Yo-yo', 'Yo-yo'), ('Flying disc', 'Flying disc'),
              ('Fishing', 'Fishing pole'), ('Christmas tree', 'Christmas tree'), ('Snowman', 'Snowman'),
              ('Magic wand', 'Magic wand'), ('Disco ball', 'Mirror ball')]:
    save(TP, nm, f'source: fluent {f}')

save(TP, 'Blackboard', f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 900 640">
  <defs>
    <linearGradient id="wood" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#c58b4e"/><stop offset="1" stop-color="#7a4a20"/></linearGradient>
    <radialGradient id="slate" cx="0.45" cy="0.4" r="0.8"><stop offset="0" stop-color="#3d6b4f"/><stop offset="1" stop-color="#1d3a29"/></radialGradient>
    {SHADOW}
  </defs>
  <rect x="20" y="20" width="860" height="560" rx="22" fill="url(#wood)" filter="url(#sh)"/>
  <rect x="58" y="58" width="784" height="484" rx="8" fill="url(#slate)"/>
  <path d="M90 470 Q300 430 520 480 T820 460" stroke="#fff" stroke-opacity="0.07" stroke-width="40" fill="none"/>
  <text x="450" y="260" text-anchor="middle" font-family="Segoe Print" font-size="96" fill="#f4f4f0" fill-opacity="0.92">ABC 123</text>
  <text x="450" y="390" text-anchor="middle" font-family="Segoe Print" font-size="64" fill="#ffe08a" fill-opacity="0.9">Hello!</text>
  <rect x="40" y="572" width="820" height="34" rx="8" fill="#6b3f1a"/>
  <rect x="600" y="556" width="110" height="24" rx="6" fill="#f2f2f2"/><rect x="600" y="568" width="110" height="12" rx="4" fill="#3b3b3b"/>
  <rect x="200" y="560" width="70" height="14" rx="7" fill="#fff"/><rect x="290" y="560" width="54" height="14" rx="7" fill="#ffd43b"/>
</svg>''')

save(TP, 'Foam finger', f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 500 700" data-frames="24" data-ms="40">
  <defs>
    <linearGradient id="f" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#1c7ed6"/><stop offset="0.5" stop-color="#4dabf7"/><stop offset="1" stop-color="#1864ab"/></linearGradient>
    {SHADOW}
  </defs>
  <g transform="rotate({{{{osc2 -8 8}}}} 250 640)" filter="url(#sh)">
    <path d="M190 40 Q250 10 300 40 L300 300 L380 300 Q440 300 440 360 L440 560 Q440 640 360 650 L170 650 Q90 640 90 560 L90 380 Q90 330 140 320 L190 320 Z" fill="url(#f)" stroke="#0b4f8a" stroke-width="8" stroke-linejoin="round"/>
    <path d="M190 320 L190 380 M240 300 L240 380 M300 300 L300 380 M360 300 L360 380" stroke="#0b4f8a" stroke-width="6" stroke-linecap="round"/>
    <text x="265" y="560" text-anchor="middle" {TXT} font-size="150" fill="#fff" stroke="#0b4f8a" stroke-width="8" paint-order="stroke">#1</text>
    <path d="M205 60 Q240 45 270 60 L270 270" stroke="#fff" stroke-opacity="0.35" stroke-width="14" fill="none" stroke-linecap="round"/>
  </g>
</svg>''')

save(TP, 'Basketball hoop', f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 600 700" data-frames="30" data-ms="40">
  <defs>{SHADOW}</defs>
  <g filter="url(#sh)">
    <rect x="80" y="30" width="440" height="300" rx="14" fill="#fff" stroke="#ccc" stroke-width="6"/>
    <rect x="215" y="140" width="170" height="130" fill="none" stroke="#e03131" stroke-width="10"/>
    <rect x="80" y="30" width="440" height="300" rx="14" fill="none" stroke="#e03131" stroke-width="12"/>
  </g>
  <g transform="translate({{{{osc 300 300}}}} {{{{lin -120 760}}}}) rotate({{{{lin 0 360}}}})">{fl('Basketball', 0, 0, w=110)}</g>
  <path d="M190 290 L225 520 L375 520 L410 290 M215 290 L265 520 M260 290 L300 520 M340 290 L300 520 M385 290 L335 520 M200 360 L400 360 M210 440 L390 440" stroke="#f1f3f5" stroke-width="7" fill="none" stroke-linejoin="round"/>
  <ellipse cx="300" cy="290" rx="115" ry="22" fill="none" stroke="#f76707" stroke-width="14"/>
</svg>''')

print('\n'.join(os.path.relpath(p, EFF) for p in written))
print(len(written), 'files')
