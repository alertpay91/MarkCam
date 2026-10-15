# MarkCam's own effects and backgrounds

Everything MarkCam ships is drawn in this repository as SVG and rendered at build time;
no third-party images. (ManyCam's effects can still be used where ManyCam is installed:
Setup → "Also use effects and backgrounds from an installed ManyCam", off by default.)

## Sources

- `assets/effects/<NN Category>/<Name>.svg` — one folder per category; the number only
  sets the order ("01 Hats" shows as "Hats").
- `assets/backgrounds/<Name>.svg` — virtual backgrounds, drawn on a 1920x1080 viewBox,
  rendered at 1280x720 (PNG, or MP4 when animated).

Root attributes:

| attribute | meaning |
|---|---|
| `data-face="true"` | a face sticker; the viewBox is in **face units** (below) |
| `data-fullscreen="true"` | a frame/border over the whole picture (rendered 1920 wide) |
| `data-frames="N" data-ms="M"` | animated: N frames, M ms each (APNG for effects, MP4 for backgrounds) |

Animation expressions inside the SVG text, with t going 0 → 1 over the loop:
`{{lin a b}}` (a→b), `{{osc a b}}` (a↔b, smooth), `{{osc2 a b}}` (twice per loop),
`{{ph p a b}}` (osc shifted by p), `{{loop p a b}}` (a→b repeating, shifted by p).

### Realistic hair (`<strands …/>`)

Inside a face SVG, `<strands kind="…" …/>` is replaced at render time by thousands of thin,
shaded strands (`tools/effects-art/src/strands.rs`). Kinds:

| kind | what | main attributes |
|---|---|---|
| `afro` | tight coils in a rounded volume | `cx cy rx ry` |
| `long` | hair from a side parting over the head, falling down | `length` (end y), `wave`, `curl` (ends turn under: bob), `part` (x), `fringe` (bangs to y; use with `only="fringe"` outside the face mask) |
| `beard`, `mustache`, `stubble` | strands on the lower face, mouth left open | `length`, `wave` |

All take `color`, `tip` (highlight color), `count`, `seed`. Light comes from the top
left; strands are shaded by their place on the head, with a sheen band.

### Weather

Rain, Snow, Thunderstorm, Wind, Sunshine and Fog are procedural (drawn live each frame,
`crates/ovc-effects/src/anim.rs`), under Adjust → Weather. Each also tints the picture
(rainy = cooler and darker, sunny = warmer).

### Face units

Origin between the eyes, +y down the face, 1 unit = 0.84 × the distance between the eyes:
eyes at x = ±0.6, y = 0 · nose tip y ≈ 0.55 · mouth y ≈ 1.15 · chin y ≈ 2.0 · top of the
head y ≈ −1.25 · temples x ≈ ±1.3. The renderer writes `<Name>.face.json`
(`{"center": [x, y], "size": [w, h]}` from the viewBox), which MarkCam uses to place the
sticker on the face, scaled and rotated with the head. Any PNG / GIF / APNG with such a
file next to it (also in "My effects") is a face sticker.

## Render / preview

```powershell
cargo build --release --manifest-path tools\effects-art\Cargo.toml
tools\effects-art\target\release\effects-art.exe assets assets\out "$env:APPDATA\MarkCam\tools\ffmpeg.exe"
# Development: install into %APPDATA%\MarkCam\Packs and screenshot each face effect on a clip
powershell -ExecutionPolicy Bypass -File tools\effects-art\preview.ps1 -Categories "01 Hats,02 Hair"
```

`installer\build-installer.ps1` renders everything and ships it as
`<install dir>\effects\MarkCam\…` and `<install dir>\backgrounds\…`.
