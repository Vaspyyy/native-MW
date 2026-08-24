# Flag atlas provenance

`flag-atlas.rgba` and `flag-atlas.json` are generated from the official
[`lipis/flag-icons`](https://github.com/lipis/flag-icons) **v7.5.0** release,
using its `flags/4x3` SVG collection. The upstream tag resolves to commit
`7aa5b2bdddd570ece62c812c0cb588ccdc099e2e` (annotated tag object
`50a8bff005239b0d2d661254094dedb9c75dbef3`). The source artwork is MIT
licensed; see `LICENSE-flag-icons.txt`.

Regenerate from a v7.5.0 checkout at the repository root:

```sh
python3 scripts/build-flag-atlas.py /path/to/flag-icons
```

The raw atlas is 2048x1024 RGBA8. Flags are lexicographically ordered by code
in 64x44 cells, with stretched 62x40 content centered in transparent padding.
