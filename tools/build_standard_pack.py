#!/usr/bin/env python3
"""Build tracked first-party components from their Rust sources."""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Package:
    package_id: str
    module_name: str


STANDARD_PACKAGES = (
    Package("solaris-permissions", "solaris_permissions_plugin.wasm"),
    Package("solaris-essentials", "solaris_essentials_plugin.wasm"),
    Package("solaris-economy", "solaris_economy_plugin.wasm"),
    Package("solaris-towns", "solaris_towns_plugin.wasm"),
    Package("solaris-audit", "solaris_audit_plugin.wasm"),
    Package("basic-economy", "basic_economy_plugin.wasm"),
)
PACKAGES = STANDARD_PACKAGES + (
    Package("solaris-settlements", "solaris_settlements_plugin.wasm"),
)
TARGET = "wasm32-unknown-unknown"


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(arguments: list[str], *, cwd: Path, env: dict[str, str] | None = None) -> None:
    subprocess.run(arguments, cwd=cwd, env=env, check=True)


def parse_args() -> argparse.Namespace:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--core-root",
        type=Path,
        default=root.parent / "solaris",
        help="Solaris core checkout containing the component encoder.",
    )
    parser.add_argument(
        "--package",
        choices=[package.package_id for package in PACKAGES],
        help="check one first-party component without changing the standard-pack default",
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--check",
        action="store_true",
        help="fail unless tracked package components match freshly rebuilt source",
    )
    mode.add_argument(
        "--write",
        action="store_true",
        help="replace tracked package components with freshly rebuilt source",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    root = Path(__file__).resolve().parents[1]
    core_root = args.core_root.resolve()
    encoder_manifest = core_root / "crates" / "mc-plugin-host" / "Cargo.toml"
    if not encoder_manifest.is_file():
        raise SystemExit(f"missing component encoder manifest: {encoder_manifest}")

    build_root = root / ".analysis" / "standard-pack-build"
    target_dir = build_root / "target"
    component_dir = build_root / "components"
    component_dir.mkdir(parents=True, exist_ok=True)
    build_env = os.environ | {"CARGO_TARGET_DIR": str(target_dir)}

    selected = (
        tuple(package for package in PACKAGES if package.package_id == args.package)
        if args.package
        else STANDARD_PACKAGES
    )
    rebuilt: list[tuple[Package, Path, Path]] = []
    for package in selected:
        source_manifest = root / "sources" / package.package_id / "Cargo.toml"
        deployed = root / package.package_id / "plugin.wasm"
        if not source_manifest.is_file():
            raise SystemExit(f"missing source manifest: {source_manifest}")
        if not deployed.is_file() and not args.write:
            raise SystemExit(f"missing package component: {deployed}")

        run(
            [
                "cargo",
                "build",
                "--manifest-path",
                str(source_manifest),
                "--target",
                TARGET,
                "--release",
            ],
            cwd=root,
            env=build_env,
        )
        module = target_dir / TARGET / "release" / package.module_name
        if not module.is_file():
            raise SystemExit(f"guest build did not produce: {module}")
        component = component_dir / f"{package.package_id}.wasm"
        run(
            [
                "cargo",
                "run",
                "--quiet",
                "--manifest-path",
                str(encoder_manifest),
                "--example",
                "plugin_component",
                "--",
                str(module),
                str(component),
            ],
            cwd=core_root,
        )
        rebuilt.append((package, component, deployed))

    mismatches = []
    for package, component, deployed in rebuilt:
        component_hash = sha256(component)
        if args.write:
            shutil.copyfile(component, deployed)
            deployed_hash = component_hash
        else:
            deployed_hash = sha256(deployed)
        if component_hash != deployed_hash:
            mismatches.append(package.package_id)
        print(f"{package.package_id} {deployed_hash} {component_hash}")

    if mismatches:
        print(
            "component mismatch; rerun with --write only after reviewing the source change: "
            + ", ".join(mismatches),
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
