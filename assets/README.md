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

## Landing page

[`site/index.html`](../site/index.html) is the landing page: the crystal as a
piece of ICE that charges, breaks apart around a glowing core, drifts, and
snaps back together, over a cyberspace grid, with the install one-liner. It is
one dependency-free HTML file (JetBrains Mono loads from Google Fonts, with a
system monospace fallback). Preview it with:

```sh
python3 -m http.server 8765 --bind 127.0.0.1 --directory site
```

- Rendering is WebGL2: depth-tested solid fragments, glowing facet edges, and a
  cheap bloom (a blurred quarter-size copy, screen-blended). Without WebGL2 a
  static SVG diamond is shown.
- Click the crystal (or press space) to break it on demand. The cycle repeats
  every 13 s on its own.
- The diamonds in the header switch palette (lavender, ocean, jade); the choice
  is remembered.
- Add `?lab` to the URL for shard count, separation and rotation sliders.
- Reduced-motion preferences turn off the automatic cycle, grid scroll, grain
  and title scramble; clicking still works.
- The install URL in `#cmd` points at `install.sh` on `main`, which does not
  exist yet.

`assets/` holds these non-Rust resources; nothing here changes the Cargo build.
