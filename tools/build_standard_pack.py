#!/usr/bin/env python3
"""Build independent first-party components, then natively validate the entire candidate before publication."""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass
from pathlib import Path

from content_pipeline import (build_options, digest, file_hashes, profile_flags, publish, run,
                              source_revision, validate_loader, validate_native, client_costs)


@dataclass(frozen=True)
class Package:
    package_id: str
    module_name: str


PACKAGES = (
    Package("solaris-permissions", "solaris_permissions_plugin.wasm"),
    Package("solaris-essentials", "solaris_essentials_plugin.wasm"),
    Package("solaris-economy", "solaris_economy_plugin.wasm"),
    Package("solaris-towns", "solaris_towns_plugin.wasm"),
    Package("solaris-audit", "solaris_audit_plugin.wasm"),
    Package("basic-economy", "basic_economy_plugin.wasm"),
    Package("solaris-auth", "solaris_auth_plugin.wasm"),
    Package("chisel", "chisel_plugin.wasm"),
    Package("graves", "solaris_graves_plugin.wasm"),
)
# Rebuild every shipped component on SDK changes; installation remains opt-in.
# Optional addons (chisel) stay out of the default selection; build them with
# --package. test_content_pipeline.py pins this selection.
STANDARD_PACKAGES = (
    Package("solaris-permissions", "solaris_permissions_plugin.wasm"),
    Package("solaris-essentials", "solaris_essentials_plugin.wasm"),
    Package("solaris-economy", "solaris_economy_plugin.wasm"),
    Package("solaris-towns", "solaris_towns_plugin.wasm"),
    Package("solaris-audit", "solaris_audit_plugin.wasm"),
    Package("basic-economy", "basic_economy_plugin.wasm"),
    Package("solaris-auth", "solaris_auth_plugin.wasm"),
)
TARGET = "wasm32-unknown-unknown"


def sha256(path: Path) -> str:
    return digest(path.read_bytes())


def parse_args() -> argparse.Namespace:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--core-root", type=Path, default=root.parent / "solaris")
    parser.add_argument("--source-root", type=Path, default=root, help="clean plugin repository authoring inputs")
    parser.add_argument("--package", choices=[package.package_id for package in PACKAGES])
    parser.add_argument("--build-root", type=Path, help="isolated Cargo output for clean deterministic rebuilds")
    parser.add_argument("--output-dir", type=Path, help="publish a new immutable deployment plus exact source/artifact receipt")
    parser.add_argument("--native-validator", type=Path, help="previously compiled authoring validator, never required by server runtime")
    parser.add_argument("--component-encoder", type=Path, help="previously compiled native component encoder")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="fail unless tracked components match rebuilt source")
    mode.add_argument("--write", action="store_true", help="update tracked components only after all native validation passes")
    build_options(parser)
    args = parser.parse_args()
    if args.output_dir and (args.check or args.write):
        parser.error("immutable --output-dir and tracked --check/--write are separate publication modes")
    return args


def authoring_inputs(root: Path, selected: tuple[Package, ...]) -> dict:
    inputs = {}
    for package in selected:
        inputs[f"sources/{package.package_id}"] = file_hashes(root / "sources" / package.package_id)
        deployed = root / package.package_id
        files = file_hashes(deployed)
        manifest = tomllib.loads((deployed / "plugin.toml").read_text())
        files.pop(manifest["entry"], None)
        for bundle in manifest.get("client", {}).get("bundles", []):
            files.pop(bundle["artifact"], None)
        files.pop("README.md", None)
        inputs[package.package_id] = files
    inputs["pipeline"] = file_hashes(Path(__file__).parent / "content-validator") | {
        name: sha256(Path(__file__).parent / name) for name in
        ("build_standard_pack.py", "content_pipeline.py")}
    return inputs


def stage_package(source: Path, target: Path) -> None:
    # Copy authored/native package files only. Authoring source is not a deployed package resource.
    shutil.copytree(source, target, ignore=shutil.ignore_patterns("plugin.wasm", "client-src", "content-src", "README.md"))
    manifest = tomllib.loads((source / "plugin.toml").read_text())
    if manifest["id"] != source.name or manifest["api"] != "0.8.0" or manifest["entry"] != "plugin.wasm":
        raise ValueError("package identity, API 0.8.0 and plugin.wasm entry must stay authored")


def package_bindings(deployment: Path) -> list[dict]:
    bindings = []
    for package in sorted(deployment.iterdir()):
        manifest = tomllib.loads((package / "plugin.toml").read_text())
        bindings.append({"id": manifest["id"], "version": manifest["version"], "api": manifest["api"],
                         "manifest_sha256": sha256(package / "plugin.toml"),
                         "component_sha256": sha256(package / manifest["entry"]),
                         "client_bundles": manifest.get("client", {}).get("bundles", [])})
    return bindings


def main() -> int:
    args = parse_args()
    root = args.source_root.resolve()
    core_root = args.core_root.resolve()
    encoder_manifest = core_root / "crates" / "mc-plugin-host" / "Cargo.toml"
    sdk = core_root / "sdk" / "rust" / "solaris-plugin-sdk"
    if not encoder_manifest.is_file() or not (sdk / "Cargo.toml").is_file():
        raise SystemExit("selected --core-root must contain the native encoder and actual guest SDK")
    selected = tuple(package for package in PACKAGES if package.package_id == args.package) if args.package else STANDARD_PACKAGES
    build_root = (args.build_root or root / ".analysis" / "standard-pack-build").resolve()
    target_dir = build_root / "target"
    build_root.mkdir(parents=True, exist_ok=True)
    destination = args.output_dir.resolve() if args.output_dir else None
    if destination and destination.exists():
        raise ValueError(f"immutable output already exists: {destination}")
    parent = destination.parent if destination else build_root
    parent.mkdir(parents=True, exist_ok=True)
    before = authoring_inputs(root, selected)
    sdk_before = {"sdk": file_hashes(core_root / "sdk" / "rust"),
                  "wit": file_hashes(core_root / "crates" / "mc-script" / "wit")}
    with tempfile.TemporaryDirectory(prefix=".standard-pack-candidate-", dir=parent) as temporary:
        stage = Path(temporary)
        candidate = stage / "candidate"
        deployment = candidate / "plugins"
        deployment.mkdir(parents=True)
        rebuilt = []
        for package in selected:
            source_dir = root / "sources" / package.package_id
            source_manifest = source_dir / "Cargo.toml"
            if not source_manifest.is_file():
                raise ValueError(f"missing source manifest: {source_manifest}")
            deployed = root / package.package_id / "plugin.wasm"
            if not deployed.is_file() and not (args.write or destination):
                raise ValueError(f"missing package component: {deployed}")
            guest = stage / "sources" / package.package_id
            shutil.copytree(source_dir, guest, ignore=shutil.ignore_patterns("target"))
            manifest = guest / "Cargo.toml"
            source = manifest.read_text()
            dependency = tomllib.loads(source)["dependencies"]["solaris-plugin-sdk"]["path"]
            token = f'path = "{dependency}"'
            if source.count(token) != 1:
                raise ValueError("expected exactly one canonical SDK path dependency")
            source = source.replace(token, f'path = "{sdk.as_posix()}"')
            manifest.write_text(source + "\n[workspace]\n")
            # Only throwaway manifests change SDK paths. Remap all authoring/staging paths
            # so clean builds do not embed checkout names or random temporary directories.
            remaps = [(stage, "/solaris-authoring"), (root, "/solaris-plugins"),
                      (core_root, "/solaris-core"), (build_root, "/solaris-build")]
            flags = os.environ.get("RUSTFLAGS", "") + " " + " ".join(
                f"--remap-path-prefix={source}={target}" for source, target in remaps)
            build_env = os.environ | {"CARGO_TARGET_DIR": str(target_dir), "RUSTFLAGS": flags.strip()}
            run(["nice", "-n", "19", "cargo", "build", "--locked", "-j", str(args.jobs), "--manifest-path", str(manifest),
                 "--target", TARGET, *profile_flags(args.profile)], root, build_env)
            module = target_dir / TARGET / args.profile / package.module_name
            if not module.is_file():
                raise ValueError(f"guest build did not produce: {module}")
            target = deployment / package.package_id
            stage_package(root / package.package_id, target)
            component = target / "plugin.wasm"
            if args.component_encoder:
                run([str(args.component_encoder.resolve()), str(module), str(component)], core_root)
            else:
                run(["nice", "-n", "19", "cargo", "run", "--locked", "--quiet", "--manifest-path", str(encoder_manifest),
                     "-j", str(args.jobs), *profile_flags(args.profile), "--example", "plugin_component", "--",
                     str(module), str(component)], core_root,
                    os.environ | {"CARGO_TARGET_DIR": str(core_root / "target")})
            rebuilt.append((package, component, deployed))
        report = validate_native(core_root, deployment, args.profile, args.jobs, core_root / "target", args.native_validator)
        validate_loader(args.loader_root, deployment, report["loader_manifest"], args.jobs)
        if before != authoring_inputs(root, selected) or sdk_before != {
            "sdk": file_hashes(core_root / "sdk" / "rust"), "wit": file_hashes(core_root / "crates" / "mc-script" / "wit")}:
            raise ValueError("authoring/SDK inputs changed during build; nothing was published")
        receipt = {"schema": 1, "profile": args.profile,
                   "source_revision": source_revision(before), "inputs": before,
                   "sdk_revision": source_revision(sdk_before),
                   "core_source_revision": source_revision({
                       "crates": file_hashes(core_root / "crates"),
                       "manifest": sha256(core_root / "Cargo.toml"),
                       "lock": sha256(core_root / "Cargo.lock")}),
                   "pipeline_revision": source_revision(before["pipeline"]),
                   "native_validator_revision": source_revision({
                       "host": file_hashes(core_root / "crates" / "mc-plugin-host" / "src"),
                       "data": file_hashes(core_root / "crates" / "mc-data"),
                       "loader": sha256(core_root / "crates" / "mc-loader" / "src" / "lib.rs"),
                       "loader_java": file_hashes(args.loader_root / "loader-core" / "src" / "main"),
                       "lock": sha256(core_root / "Cargo.lock")}),
                   "compiler": subprocess.check_output(["rustc", "-vV"], text=True).strip(),
                   "artifacts": file_hashes(deployment), "native": report,
                   "package_bindings": package_bindings(deployment),
                   "catalog_costs": {
                       "recipe_count": sum(catalog["count"] for catalog in report["catalogs"].values()),
                       "serialized_bytes": sum(catalog["serialized_bytes"] for catalog in report["catalogs"].values())},
                   "client_costs": client_costs(deployment, report["loader_manifest"]),
                   "component_encoder_sha256": sha256(args.component_encoder.resolve()) if args.component_encoder else
                       sha256(core_root / "target" / args.profile / "examples" / "plugin_component"),
                   "native_validator_sha256": sha256(args.native_validator.resolve()) if args.native_validator else
                       sha256(core_root / "target" / args.profile / "solaris-content-validator"),
                   "activation": "not exercised; authoring-native validation only"}
        (candidate / "content-receipt.json").write_text(json.dumps(receipt, indent=2, sort_keys=True) + "\n")
        mismatches = []
        for package, component, deployed in rebuilt:
            rebuilt_hash = sha256(component)
            tracked_hash = sha256(deployed) if deployed.is_file() else None
            print(f"{package.package_id} {tracked_hash} {rebuilt_hash}")
            if tracked_hash != rebuilt_hash:
                mismatches.append(package.package_id)
        if destination:
            publish(candidate, destination)
        elif args.write:
            for _, component, deployed in rebuilt:
                shutil.copyfile(component, deployed)
        elif mismatches:
            print("component mismatch; review source, then use --write: " + ", ".join(mismatches), file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
