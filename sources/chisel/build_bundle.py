#!/usr/bin/env python3
"""Build the chisel Loader client bundle (stdlib only).

Reads ``client-src/`` (``solaris-client.json`` index template plus asset
files), writes a deterministic zip to ``chisel/client/chisel-content.zip``
with ``solaris-client.json`` as the first entry and the assets after it in
sorted order (fixed 2000-01-01 timestamps), then rewrites the sha256 /
size_bytes entries of the index and of ``chisel/plugin.toml``.

Run from the repository root: ``python3 sources/chisel/build_bundle.py``.
Idempotent: rerunning without source changes yields a byte-identical zip and
rewrites identical hashes.
"""
from __future__ import annotations

import hashlib
import json
import tomllib
import zipfile
from collections import OrderedDict
from pathlib import Path

SOURCE_ROOT = Path(__file__).resolve().parent / "client-src"
PACKAGE_ROOT = Path(__file__).resolve().parent.parent.parent / "chisel"

INDEX_ASSETS = (
    ("chisel:chisel_item_definition", "assets/chisel/items/chisel.json"),
    ("chisel:chisel_item_model", "assets/chisel/models/item/chisel.json"),
    ("chisel:chisel_item_texture", "assets/chisel/textures/item/chisel.png"),
)

FIXED_DATE = (2000, 1, 1, 0, 0, 0)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def unique_object(pairs: list[tuple[str, object]]) -> dict:
    result = OrderedDict()
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON field: {key}")
        result[key] = value
    return result


def build_index() -> bytes:
    entries = []
    for asset_id, relpath in INDEX_ASSETS:
        data = (SOURCE_ROOT / relpath).read_bytes()
        entries.append(OrderedDict((
            ("id", asset_id),
            ("path", relpath),
            ("sha256", sha256(data)),
            ("size_bytes", len(data)),
        )))
    index = OrderedDict((
        ("schema", 2),
        ("items", [OrderedDict((
            ("id", "chisel:chisel"),
            ("base_item", "minecraft:stick"),
            ("name", "Chisel"),
        ))]),
        ("assets", entries),
    ))
    return (json.dumps(index, separators=(",", ":")) + "\n").encode()


def write_zip(index_bytes: bytes, target: Path) -> bytes:
    names = [relpath for _, relpath in INDEX_ASSETS]
    assert len(set(names)) == len(names), "duplicate asset path"
    assert names == sorted(names), "assets must be sorted for determinism"
    with zipfile.ZipFile(target, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        info = zipfile.ZipInfo("solaris-client.json", date_time=FIXED_DATE)
        info.compress_type = zipfile.ZIP_DEFLATED
        info.create_system = 3
        archive.writestr(info, index_bytes)
        for relpath in names:
            info = zipfile.ZipInfo(relpath, date_time=FIXED_DATE)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 3
            archive.writestr(info, (SOURCE_ROOT / relpath).read_bytes())
    return target.read_bytes()


def rewrite_toml_manifest(artifact_sha256: str, artifact_size: int) -> None:
    manifest_path = PACKAGE_ROOT / "plugin.toml"
    lines = manifest_path.read_text().splitlines(keepends=True)
    replaced = {"sha256": False, "size_bytes": False}
    for index, line in enumerate(lines):
        stripped = line.strip()
        if stripped.startswith("sha256"):
            lines[index] = f'sha256 = "{artifact_sha256}"\n'
            replaced["sha256"] = True
        elif stripped.startswith("size_bytes"):
            lines[index] = f"size_bytes = {artifact_size}\n"
            replaced["size_bytes"] = True
    if not all(replaced.values()):
        raise ValueError(f"plugin.toml lacks bundle hash fields: {manifest_path}")
    manifest_path.write_text("".join(lines))
    manifest = tomllib.loads(manifest_path.read_text())
    bundle = manifest["client"]["bundles"][0]
    assert bundle["sha256"] == artifact_sha256
    assert bundle["size_bytes"] == artifact_size


def main() -> None:
    for _, relpath in INDEX_ASSETS:
        if not (SOURCE_ROOT / relpath).is_file():
            raise ValueError(f"missing client-src asset: {relpath}")
    index_bytes = build_index()
    # Parse back with duplicate rejection: what the Loader reads is what we wrote.
    parsed = json.loads(index_bytes, object_pairs_hook=unique_object)
    assert parsed["schema"] == 2
    assert len(parsed["assets"]) == len(INDEX_ASSETS)
    target = PACKAGE_ROOT / "client" / "chisel-content.zip"
    target.parent.mkdir(parents=True, exist_ok=True)
    artifact = write_zip(index_bytes, target)
    with zipfile.ZipFile(target) as archive:
        names = archive.namelist()
        assert names[0] == "solaris-client.json", names
        assert names[1:] == sorted(names[1:]), names
        assert archive.read("solaris-client.json") == index_bytes
    rewrite_toml_manifest(sha256(artifact), len(artifact))
    print(f"wrote {target} ({len(artifact)} bytes, sha256 {sha256(artifact)})")


if __name__ == "__main__":
    main()
