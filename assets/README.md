# Construct branding

The faceted diamond echoes Construct's terminal icon. The default palette is
lavender and silver; blue and jade are alternatives.

| File | Palette |
| --- | --- |
| `logo.png` | Lavender / silver (primary) |
| `logo_blue.png` | Ocean / ice |
| `logo_jade.png` | Jade / mint |

All three logos are transparent PNGs generated with OpenAI's built-in image
tool. The accompanying `*.prompt.txt` files record their generation prompts.
They were recolored from the same amber diamond reference.

## Animated crystal

Open [crystal/index.html](crystal/index.html) in a browser. It is a standalone
HTML/CSS/JavaScript demo with no dependencies, build step, or network requests.
It can also be served locally from the repository root:

```sh
python3 -m http.server 8765 --bind 127.0.0.1 --directory assets/crystal
```

Then visit <http://localhost:8765> (choose another port if it is already in use).

The crystal rotates and periodically separates into solid fragments before
reassembling. Controls select the palette, rotation speed, separation, and
fragment count (24, 48, 96, 192, or 384), plus pause and manual breakup.
Increasing fragment count subdivides the surface and creates smaller pieces;
it preserves the assembled shape. Reduced-motion preferences start it paused.

Geometry is generated procedurally and rendered with Canvas 2D using projected
3D vertices and depth-sorted faces. This approximates the static logo rather
than reproducing its exact geometry. Depth sorting is approximate, so fragments
can overlap imperfectly during breakup. The demo is a branding prototype and
is not loaded by the Rust TUI.

`assets/` holds these non-Rust resources; nothing here changes the Cargo build.
