#!/usr/bin/env python3
"""Build the deterministic Modern Wars RGBA flag atlas from flag-icons 4x3 SVGs."""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import unicodedata
from pathlib import Path


ATLAS_WIDTH = 2048
ATLAS_HEIGHT = 1024
CELL_WIDTH = 64
CELL_HEIGHT = 44
CONTENT_WIDTH = 62
CONTENT_HEIGHT = 40
SCHEMA = "mw.flag-atlas"
SCHEMA_VERSION = 1


def normalized_name(value: object) -> str | None:
    if not isinstance(value, str):
        return None
    return unicodedata.normalize("NFC", " ".join(value.split())) or None


def load_names(path: Path | None) -> dict[str, str]:
    if path is None or not path.is_file():
        return {}
    records = json.loads(path.read_text(encoding="utf-8"))
    names: dict[str, str] = {}
    for record in records:
        code = str(record.get("code", "")).strip().lower()
        name = normalized_name(record.get("name"))
        if code and name:
            names[code] = name
    return names


def render_flag(magick: str, svg: Path) -> bytes:
    command = [
        magick,
        "-background",
        "none",
        str(svg),
        "-alpha",
        "on",
        "-resize",
        f"{CONTENT_WIDTH}x{CONTENT_HEIGHT}!",
        "-depth",
        "8",
        "rgba:-",
    ]
    result = subprocess.run(command, check=True, stdout=subprocess.PIPE)
    expected = CONTENT_WIDTH * CONTENT_HEIGHT * 4
    if len(result.stdout) != expected:
        raise RuntimeError(
            f"ImageMagick returned {len(result.stdout)} bytes for {svg}; expected {expected}"
        )
    return result.stdout


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path, help="flag-icons source checkout")
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=Path("crates/mw-native/assets/flags"),
    )
    parser.add_argument(
        "--country-json",
        type=Path,
        help="optional country.json; defaults to SOURCE/country.json",
    )
    args = parser.parse_args()

    source = args.source.resolve()
    svg_dir = source / "flags" / "4x3"
    if not svg_dir.is_dir():
        raise SystemExit(f"missing flag-icons 4x3 directory: {svg_dir}")
    svgs = sorted(svg_dir.glob("*.svg"), key=lambda item: item.stem)
    capacity = (ATLAS_WIDTH // CELL_WIDTH) * (ATLAS_HEIGHT // CELL_HEIGHT)
    if not svgs:
        raise SystemExit(f"no SVG flags found in {svg_dir}")
    if len(svgs) > capacity:
        raise SystemExit(f"{len(svgs)} flags exceed atlas capacity {capacity}")

    magick = shutil.which("magick")
    if magick is None:
        raise SystemExit("ImageMagick `magick` executable is required")
    country_json = args.country_json or source / "country.json"
    names = load_names(country_json)
    atlas = bytearray(ATLAS_WIDTH * ATLAS_HEIGHT * 4)
    columns = ATLAS_WIDTH // CELL_WIDTH
    entries: dict[str, dict[str, object]] = {}

    for index, svg in enumerate(svgs):
        code = svg.stem.lower()
        cell_x = (index % columns) * CELL_WIDTH
        cell_y = (index // columns) * CELL_HEIGHT
        content_x = cell_x + (CELL_WIDTH - CONTENT_WIDTH) // 2
        content_y = cell_y + (CELL_HEIGHT - CONTENT_HEIGHT) // 2
        pixels = render_flag(magick, svg)
        row_bytes = CONTENT_WIDTH * 4
        for row in range(CONTENT_HEIGHT):
            src_start = row * row_bytes
            dst_start = ((content_y + row) * ATLAS_WIDTH + content_x) * 4
            atlas[dst_start : dst_start + row_bytes] = pixels[
                src_start : src_start + row_bytes
            ]
        entry: dict[str, object] = {
            "cell": index,
            "x": content_x,
            "y": content_y,
            "width": CONTENT_WIDTH,
            "height": CONTENT_HEIGHT,
            "u0": content_x / ATLAS_WIDTH,
            "v0": content_y / ATLAS_HEIGHT,
            "u1": (content_x + CONTENT_WIDTH) / ATLAS_WIDTH,
            "v1": (content_y + CONTENT_HEIGHT) / ATLAS_HEIGHT,
        }
        if code in names:
            entry["name"] = names[code]
        entries[code] = entry

    next_index = len(svgs)
    manifest = {
        "schema": SCHEMA,
        "version": SCHEMA_VERSION,
        "source": {
            "project": "lipis/flag-icons",
            "repository": "https://github.com/lipis/flag-icons.git",
            "version": "7.5.0",
            "tag": "v7.5.0",
            "tagObject": "50a8bff005239b0d2d661254094dedb9c75dbef3",
            "commit": "7aa5b2bdddd570ece62c812c0cb588ccdc099e2e",
            "collection": "flags/4x3",
            "license": "MIT",
        },
        "dimensions": {
            "width": ATLAS_WIDTH,
            "height": ATLAS_HEIGHT,
            "format": "rgba8",
            "cellWidth": CELL_WIDTH,
            "cellHeight": CELL_HEIGHT,
            "contentWidth": CONTENT_WIDTH,
            "contentHeight": CONTENT_HEIGHT,
            "columns": columns,
            "rows": ATLAS_HEIGHT // CELL_HEIGHT,
        },
        "nextCell": {
            "index": next_index,
            "x": (next_index % columns) * CELL_WIDTH,
            "y": (next_index // columns) * CELL_HEIGHT,
        },
        "entries": entries,
    }

    args.output_dir.mkdir(parents=True, exist_ok=True)
    (args.output_dir / "flag-atlas.rgba").write_bytes(atlas)
    (args.output_dir / "flag-atlas.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
