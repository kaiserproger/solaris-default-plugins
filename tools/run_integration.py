#!/usr/bin/env python3
"""First-party component gates. Core supplies APIs and generic harness execution."""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

PLUGINS_ROOT = Path(__file__).resolve().parent.parent
STANDARD_PACK = (
    ("solaris-permissions", ("storage",)),
    ("solaris-essentials", ("storage", "player_teleport", "player_queries")),
    ("solaris-economy", ("storage", "storage_batches")),
    ("solaris-towns", ("storage", "zones", "player_queries")),
    ("solaris-audit", ("storage",)),
)
TEST_MANIFEST = PLUGINS_ROOT / "sources" / "integration-tests" / "Cargo.toml"


def run(argv: list[str], cwd: Path) -> None:
    print(f"[plugins] cwd={cwd} command={' '.join(argv)}", flush=True)
    subprocess.run(argv, cwd=cwd, check=True)


def cargo_test(target: str, *extra: str) -> None:
    run(
        ["cargo", "test", "--manifest-path", str(TEST_MANIFEST), "--test", target,
         "--", "--ignored", *extra],
        PLUGINS_ROOT,
    )


def check_source(core_root: Path, package: str | None = None) -> None:
    command = [sys.executable, str(PLUGINS_ROOT / "tools" / "build_standard_pack.py"),
               "--check", "--core-root", str(core_root)]
    if package is not None:
        command.extend(["--package", package])
    run(command, PLUGINS_ROOT)


def standard_pack(core_root: Path, artifact_dir: Path, real_client: bool, timeout_seconds: float) -> None:
    check_source(core_root)
    cargo_test("plugin_standard_pack")
    cargo_test("wasm_economy_payment", "--exact",
               "payment_receipt_prevents_second_debit_after_token_eviction_reload_and_restart")
    cargo_test("plugin_live_workload", "--nocapture")
    deployment = artifact_dir / "standard-pack" / "plugins"
    for package_id, _ in STANDARD_PACK:
        source = PLUGINS_ROOT / package_id
        target = deployment / package_id
        target.mkdir(parents=True)
        for name in ("plugin.toml", "plugin.wasm", "config.toml"):
            file = source / name
            if not file.is_file():
                raise RuntimeError(f"standard-pack artifact is missing: {file}")
            shutil.copyfile(file, target / name)
    expected = ", ".join(f'"{package_id}"' for package_id, _ in STANDARD_PACK)
    grants = "\n".join(
        f"[plugins.grants.{package_id}]\ncapabilities = ["
        + ", ".join(f'"{capability}"' for capability in capabilities) + "]"
        for package_id, capabilities in STANDARD_PACK
    )
    config = artifact_dir / "standard-pack.toml"
    config.write_text(
        "\n".join((
            "[server]", 'name = "Standard Pack Check"', 'motd = "Standard Pack Check"',
            "", "[network]", 'bind_address = "127.0.0.1"', "port = 25565",
            "", "[data]", f'world_dir = "{artifact_dir / "standard-pack-world"}"',
            "", "[plugins]", f'directory = "{deployment}"', "strict = true",
            f"expected = [{expected}]", "", grants, "",
        ))
    )
    run(["cargo", "build", "-p", "mc-server"], core_root)
    run([str(core_root / "target" / "debug" / "mc-server"), "--check", "--config", str(config)], core_root)
    if real_client:
        sys.path.insert(0, str(core_root))
        from standard_pack_client import run_standard_pack
        result = run_standard_pack(timeout_seconds, artifact_dir, PLUGINS_ROOT, STANDARD_PACK)
        if result.get("passed") is not True:
            raise RuntimeError(f"standard-pack client scenario did not pass: {result}")


def settlement_cargo(core_root: Path) -> None:
    check_source(core_root, "solaris-settlements")
    cargo_test("settlement_component_haul")
    run(
        ["cargo", "test", "--manifest-path", str(TEST_MANIFEST),
         "--test", "settlement_wasm_readiness",
         "--", "--ignored", "--exact",
         "settlement_vanilla_village_warehouse_funds_and_commits_through_wasm"],
        PLUGINS_ROOT,
    )
    # These are core API guarantees, not assertions about settlement policy.
    # Run the owning native suites rather than restating 80 individual test names.
    for scope in ("resident_settlement_tests", "resident_order_tests", "settlement_tests",
                  "server_owned_warehouse_deposit"):
        run(["cargo", "test", "-p", "mc-net", scope, "--lib"], core_root)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("profile", choices=("standard-pack", "settlement-cargo"))
    parser.add_argument("--core-root", type=Path, required=True)
    parser.add_argument("--artifact-dir", type=Path, required=True)
    parser.add_argument("--real-client", action="store_true")
    parser.add_argument("--timeout-seconds", type=float, default=600.0)
    args = parser.parse_args()
    core_root = args.core_root.resolve()
    artifact_dir = args.artifact_dir.resolve()
    if not (core_root / "Cargo.toml").is_file() or not TEST_MANIFEST.is_file():
        raise RuntimeError("both the core workspace and sibling integration crate are required")
    artifact_dir.mkdir(parents=True, exist_ok=True)
    if args.profile == "standard-pack":
        standard_pack(core_root, artifact_dir, args.real_client, args.timeout_seconds)
    else:
        settlement_cargo(core_root)


if __name__ == "__main__":
    main()
