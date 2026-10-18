#!/usr/bin/env python3
"""First-party component gates. Core supplies APIs and generic harness execution."""

from __future__ import annotations

import argparse
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import tomllib
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



def run(argv: list[str], cwd: Path, env: dict[str, str] | None = None) -> None:
    print(f"[plugins] cwd={cwd} command={' '.join(argv)}", flush=True)
    subprocess.run(argv, cwd=cwd, check=True, env=env)


def cargo_test(core_root: Path, target: str, *extra: str) -> None:
    """Test the selected core without modifying the owner's default manifest."""
    source = TEST_MANIFEST.read_text()
    dependencies = tomllib.loads(source)["dependencies"]
    for name, dependency in dependencies.items():
        if isinstance(dependency, dict) and "path" in dependency:
            selected = core_root / "crates" / name
            if not (selected / "Cargo.toml").is_file():
                raise RuntimeError(f"selected core dependency is missing: {selected}")
            source = source.replace(
                f'path = "{dependency["path"]}"', f'path = "{selected.as_posix()}"'
            )
    environment = os.environ.copy()
    environment["SOLARIS_CORE_ROOT"] = str(core_root)
    environment.setdefault("SOLARIS_DEFAULT_PLUGINS_ROOT", str(PLUGINS_ROOT))
    environment.setdefault("CARGO_TARGET_DIR", str(core_root / "target"))
    with tempfile.TemporaryDirectory(prefix="solaris-plugin-integration-") as temporary:
        fixture = Path(temporary)
        # Explicit targets retain the original files (and their relative module
        # paths), while the throwaway package has no implicit src/lib.rs target.
        source += "\n[workspace]\n"
        for test in sorted((TEST_MANIFEST.parent / "tests").glob("*.rs")):
            source += f'\n[[test]]\nname = "{test.stem}"\npath = "{test.as_posix()}"\n'
        manifest = fixture / "Cargo.toml"
        manifest.write_text(source)
        lock = TEST_MANIFEST.with_name("Cargo.lock")
        if lock.is_file():
            shutil.copyfile(lock, fixture / "Cargo.lock")
        run(
            ["nice", "-n", "19", "cargo", "test", "-j", "3",
             "--manifest-path", str(manifest), "--test", target,
             "--", "--ignored", *extra],
            PLUGINS_ROOT,
            environment,
        )


def check_source(core_root: Path) -> None:
    command = [sys.executable, str(PLUGINS_ROOT / "tools" / "build_standard_pack.py"),
               "--check", "--jobs", "3", "--core-root", str(core_root)]
    run(command, PLUGINS_ROOT)


def standard_pack(core_root: Path, artifact_dir: Path, real_client: bool, timeout_seconds: float) -> None:
    check_source(core_root)
    cargo_test(core_root, "plugin_standard_pack")
    start_server(core_root, artifact_dir, "standard-pack",
                 PLUGINS_ROOT, tuple(package_id for package_id, _ in STANDARD_PACK),
                 timeout_seconds)
    start_server(core_root, artifact_dir, "all-shipped",
                 PLUGINS_ROOT, tuple(package_id for package_id, _ in STANDARD_PACK) + ("basic-economy",),
                 timeout_seconds)
    if real_client:
        sys.path.insert(0, str(core_root))
        from standard_pack_client import run_standard_pack
        result = run_standard_pack(timeout_seconds, artifact_dir, PLUGINS_ROOT, STANDARD_PACK)
        if result.get("passed") is not True:
            raise RuntimeError(f"standard-pack client scenario did not pass: {result}")


def start_server(core_root: Path, artifact_dir: Path, gate: str, package_root: Path,
                 package_ids: tuple[str, ...], timeout_seconds: float) -> None:
    deployment = artifact_dir / gate / "plugins"
    for package_id in package_ids:
        source = package_root / package_id
        for name in ("plugin.toml", "plugin.wasm"):
            if not (source / name).is_file():
                raise RuntimeError(f"{gate} artifact is missing: {source / name}")
        # Deploy the package as shipped (config, client bundles, structures), not authoring inputs.
        shutil.copytree(source, deployment / package_id, dirs_exist_ok=True,
                        ignore=shutil.ignore_patterns("client-src", "content-src", "README.md"))
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    config = artifact_dir / f"{gate}.toml"
    config.write_text(
        "\n".join((
            "[server]", 'name = "Standard Pack Check"', 'motd = "Standard Pack Check"',
            "", "[network]", 'bind_address = "127.0.0.1"', f"port = {port}",
            "", "[data]", f'world_dir = "{artifact_dir / (gate + "-world")}"',
            "", "[plugins]", f'directory = "{deployment}"', "",
        ))
    )
    run(["cargo", "build", "-p", "mc-edge", "--bin", "solaris-v2"], core_root)
    # v2 has no --check: exercise real startup, then request a clean save/stop.
    sys.path.insert(0, str(core_root))
    from tools.harness.runtime import stop_process, wait_port

    log_path = artifact_dir / f"{gate}-server.log"
    with log_path.open("wb") as log:
        server = subprocess.Popen(
            [str(core_root / "target" / "debug" / "solaris-v2"), "--config", str(config)],
            cwd=core_root, stdin=subprocess.PIPE, stdout=log, stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            wait_port(port, timeout_seconds, server, log_path=log_path)
            server.communicate(b"stop\n", timeout=timeout_seconds)
            if server.returncode:
                raise RuntimeError(f"{gate} server exited {server.returncode}; see {log_path}")
        finally:
            stop_process(server, interrupt=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("profile", choices=("standard-pack",))
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


if __name__ == "__main__":
    main()
