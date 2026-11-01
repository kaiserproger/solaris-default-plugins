#!/usr/bin/env python3
"""Build the computer client archive and refresh its manifest hash/size.

Like sources/chisel/build_bundle.py: schema-2 index first, sorted assets,
fixed ZIP timestamps. Only original client-src assets enter the archive.
Run this before tools/build_standard_pack.py --package computer --write.
"""
from __future__ import annotations

import io
import json
from pathlib import Path
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
from content_pipeline import digest

SOURCE_ROOT = Path(__file__).resolve().parent / "client-src"
PACKAGE_ROOT = ROOT / "computer"
INDEX_ASSETS = (
    ("computer:monitor_item_definition", "assets/computer/items/monitor.json"),
    ("computer:monitor_block_model", "assets/computer/models/block/monitor.json"),
    ("computer:monitor_item_model", "assets/computer/models/item/monitor.json"),
    ("computer:monitor_front_texture", "assets/computer/textures/block/monitor_front.png"),
    ("computer:monitor_side_texture", "assets/computer/textures/block/monitor_side.png"),
    ("computer:monitor_top_texture", "assets/computer/textures/block/monitor_top.png"),
)
FIXED_DATE = (2000, 1, 1, 0, 0, 0)


def archive_bytes() -> bytes:
    assets = [(asset_id, path, (SOURCE_ROOT / path).read_bytes())
              for asset_id, path in INDEX_ASSETS]
    index = {
        "schema": 2,
        "blocks": [{
            "id": "computer:monitor",
            "model": "computer:block/monitor",
            "name": "Monitor",
            "carrier": "minecraft:gray_glazed_terracotta",
            "drop_item": "computer:monitor",
        }],
        "items": [{
            "id": "computer:monitor",
            "base_item": "minecraft:gray_glazed_terracotta",
            "name": "Monitor",
        }],
        "assets": [{"id": asset_id, "path": path,
                    "sha256": digest(data), "size_bytes": len(data)}
                   for asset_id, path, data in assets],
    }
    entries = [("solaris-client.json", (json.dumps(index, separators=(",", ":")) + "\n").encode())]
    entries.extend((path, data) for _, path, data in sorted(assets, key=lambda asset: asset[1]))
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for path, data in entries:
            info = zipfile.ZipInfo(path, date_time=FIXED_DATE)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 3
            archive.writestr(info, data)
    return output.getvalue()


def main() -> None:
    artifact = archive_bytes()
    manifest_path = PACKAGE_ROOT / "plugin.toml"
    lines = manifest_path.read_text().splitlines(keepends=True)
    for field in ("sha256", "size_bytes"):
        matches = [index for index, line in enumerate(lines) if line.startswith(field + " =")]
        if len(matches) != 1:
            raise ValueError(f"expected one {field} field in {manifest_path}")
        value = json.dumps(digest(artifact)) if field == "sha256" else str(len(artifact))
        lines[matches[0]] = f"{field} = {value}\n"
    target = PACKAGE_ROOT / "client" / "computer-content.zip"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(artifact)
    manifest_path.write_text("".join(lines))
    print(f"wrote {target} ({len(artifact)} bytes, sha256 {digest(artifact)})")


if __name__ == "__main__":
    main()
