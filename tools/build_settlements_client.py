#!/usr/bin/env python3
"""Build or check the first-party settlement Loader view bundle."""

import argparse
import hashlib
import io
import json
from pathlib import Path
import re
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1] / "solaris-settlements"
SOURCE = ROOT / "client-src" / "solaris-client.json"
ARCHIVE = ROOT / "client" / "settlements.zip"
MANIFEST = ROOT / "plugin.toml"
GATE = ROOT / "structures" / "palisade_gate.toml"
# The content hash is produced by mc-worldgen's validated BlueprintCatalog,
# not by hashing TOML bytes. The source digest makes an edited blueprint fail
# this build until the native hash and preview are checked together again.
GATE_SOURCE_SHA256 = "cd9a9317d5d211f54b4633c7c070794fec83ec359d85761fdc9f5e6c704aa75d"
GATE_CONTENT_HASH = "6a27ec4da2a39d40624c1676185ded1b4d0271435f2ae759e39b54fe0c6dcd3a"
SOUND_PATH = "assets/solaris-settlements/sounds/construction_portion.ogg"
SOUND = ROOT / "client-src" / SOUND_PATH


def sound_bytes():
    sound = SOUND.read_bytes()
    if not sound or len(sound) > 256 * 1024:
        raise ValueError("construction sound exceeds Loader's per-clip budget")
    return sound



def index_bytes():
    source = GATE.read_bytes()
    if hashlib.sha256(source).hexdigest() != GATE_SOURCE_SHA256:
        raise ValueError("palisade gate blueprint changed; recheck the native content hash")
    blueprint = tomllib.loads(source.decode("utf-8"))
    palette = {entry["index"]: entry["block"] for entry in blueprint["palette"]}
    size_x, size_y, size_z = blueprint["footprint"]["size"]
    blocks = [
        {
            "x": block["x"],
            "y": block["y"],
            "z": block["z"],
            "block_id": palette[block["palette"]],
        }
        for block in blueprint["blocks"]
    ]
    index = json.loads(SOURCE.read_text())
    index["world_previews"] = [
        {
            "id": f"solaris-settlements:palisade_gate_{turn}",
            "blueprint_id": blueprint["id"],
            "content_hash": GATE_CONTENT_HASH,
            "rotation": turn,
            "size_x": size_x,
            "size_y": size_y,
            "size_z": size_z,
            "blocks": blocks,
        }
        for turn in range(4)
    ]
    sound = sound_bytes()
    index["assets"] = [
        {
            "id": "solaris-settlements:construction_clip",
            "path": SOUND_PATH,
            "sha256": hashlib.sha256(sound).hexdigest(),
            "size_bytes": len(sound),
        }
    ]
    index["sounds"] = [{"id": "solaris-settlements:construction_portion"}]
    encoded = (json.dumps(index, separators=(",", ":"), ensure_ascii=False) + "\n").encode("utf-8")
    if len(encoded) > 64 * 1024:
        raise ValueError("settlement preview index exceeds Loader's 64 KiB cap")
    return encoded


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail if archive or manifest differs")
    args = parser.parse_args()

    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as archive:
        entry = zipfile.ZipInfo("solaris-client.json", (2000, 1, 1, 0, 0, 0))
        entry.compress_type = zipfile.ZIP_DEFLATED
        entry.external_attr = 0o100644 << 16
        archive.writestr(entry, index_bytes())
        sound_entry = zipfile.ZipInfo(SOUND_PATH, (2000, 1, 1, 0, 0, 0))
        sound_entry.compress_type = zipfile.ZIP_DEFLATED
        sound_entry.external_attr = 0o100644 << 16
        archive.writestr(sound_entry, sound_bytes())
    data = output.getvalue()
    manifest = MANIFEST.read_text()
    if len(re.findall(r'^sha256 = "[0-9a-f]{64}"$', manifest, re.MULTILINE)) != 1 or len(
        re.findall(r"^size_bytes = [0-9]+$", manifest, re.MULTILINE)
    ) != 1:
        parser.error("expected exactly one pinned bundle SHA-256 and byte size")
    expected = re.sub(
        r'^sha256 = "[0-9a-f]{64}"$',
        f'sha256 = "{hashlib.sha256(data).hexdigest()}"',
        manifest,
        count=1,
        flags=re.MULTILINE,
    )
    expected = re.sub(
        r"^size_bytes = [0-9]+$",
        f"size_bytes = {len(data)}",
        expected,
        count=1,
        flags=re.MULTILINE,
    )
    if args.check:
        if not ARCHIVE.is_file() or ARCHIVE.read_bytes() != data or manifest != expected:
            parser.error("settlement archive or pinned manifest is stale; rebuild without --check")
    else:
        ARCHIVE.parent.mkdir(parents=True, exist_ok=True)
        ARCHIVE.write_bytes(data)
        MANIFEST.write_text(expected)
    print(f"settlement view bundle: {len(data)} bytes, {hashlib.sha256(data).hexdigest()}")


if __name__ == "__main__":
    main()
