"""Graphical first-party pack acceptance; core provides only client/runtime drivers."""

from __future__ import annotations

import hashlib
import json
import shutil
import subprocess
import time
import uuid
from pathlib import Path
from typing import Any

from tools.harness import runtime
from tools.harness.mcp import McpClient

REPO_ROOT = runtime.REPO_ROOT


def recent_chat_contains(observed: dict[str, Any], expected: str) -> bool:
    recent = observed.get("recent_chat")
    return isinstance(recent, list) and any(expected in str(message) for message in recent)


def run_standard_pack_client(
    root: Path,
    display: str,
    timeout_seconds: float,
    plugins_root: Path,
    standard_pack: tuple[tuple[str, tuple[str, ...]], ...],
) -> dict[str, Any]:
    """Run one ordinary command through the strict first-party component pack."""
    run_dir = root / "standard-pack-client"
    run_dir.mkdir()
    plugin_root = run_dir / "plugins"
    plugin_root.mkdir()
    for package_id, _ in standard_pack:
        source = plugins_root / package_id
        target = plugin_root / package_id
        target.mkdir()
        for name in ("plugin.toml", "plugin.wasm", "config.toml"):
            shutil.copyfile(source / name, target / name)

    server_port = runtime.reserve_port()
    mcp_port = runtime.reserve_port()
    world_dir = run_dir / "world"
    world_dir.mkdir()
    config = run_dir / "server.toml"
    config.write_text(
        f"""[server]
name = "standard-pack-client-gate"
motd = "Solaris standard pack real-client gate"
view_distance = 4
simulation_distance = 4
[network]
bind_address = "127.0.0.1"
port = {server_port}
[auth]
online_mode = false
[plugins]
directory = "{plugin_root.as_posix()}"
[data]
world_dir = "{world_dir.as_posix()}"
seed = 0
"""
    )
    # solaris-v2 reads vanilla ops.json beside its config.
    operator = "StandardPackGate"
    (run_dir / "ops.json").write_text(json.dumps([{
        "uuid": str(uuid.UUID(bytes=hashlib.md5(f"OfflinePlayer:{operator}".encode(),
                                               usedforsecurity=False).digest(), version=3)),
        "name": operator, "level": 4, "bypassesPlayerLimit": False,
    }]) + "\n")
    token = f"standard-pack-{time.time_ns()}"
    server_log_path = run_dir / "server.log"
    server_log = server_log_path.open("wb")
    client_log_handle = None
    server = client_process = None
    mcp = None
    try:
        server = subprocess.Popen(
            [str(REPO_ROOT / "target" / "debug" / "solaris-v2"), "--config", str(config)],
            cwd=REPO_ROOT,
            stdout=server_log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        runtime.wait_port(
            server_port, min(timeout_seconds, 120.0), server, log_path=server_log_path
        )
        game_dir = run_dir / "game"
        game_dir.mkdir()
        (game_dir / "options.txt").write_text("version:4790\nonboardAccessibility:false\n")
        client_process, client_log_handle = runtime.start_client(
            game_dir=game_dir,
            log_path=run_dir / "client.log",
            token=token,
            mcp_port=mcp_port,
            username="StandardPackGate",
            display=display,
            platform=None,
        )
        mcp = McpClient(
            f"http://127.0.0.1:{mcp_port}/mcp", token, request_timeout_seconds=30.0
        )
        mcp.initialize()
        runtime.wait_client_ready_for_connect(mcp, min(timeout_seconds, 120.0))
        mcp.call_tool("minecraft_connect", {"server_addr": f"127.0.0.1:{server_port}"})
        play = mcp.call_tool(
            "minecraft_wait_for_play",
            {"timeout_seconds": min(timeout_seconds, 120.0)},
        )
        if not play.get("in_play"):
            raise RuntimeError(f"standard-pack client did not enter Play: {play}")
        observed = runtime.current_client_state(mcp, 30.0)
        mcp.call_tool("minecraft_send_chat", {"message": "money", "command": True})
        expected_reply = "Balance: 100 coins."
        deadline = time.monotonic() + 30.0
        while not recent_chat_contains(observed, expected_reply):
            observed = runtime.next_client_state(mcp, observed, deadline)
        mcp.call_tool("minecraft_disconnect")
        return {
            "passed": True,
            "in_play": True,
            "command": "/money",
            "reply": expected_reply,
            "plugins": [package_id for package_id, _ in standard_pack],
        }
    finally:
        if mcp is not None:
            try:
                mcp.close()
            except Exception:
                pass
        runtime.stop_process(client_process)
        runtime.stop_process(server, interrupt=True)
        if client_log_handle is not None:
            client_log_handle.close()
        server_log.close()


def run_standard_pack(
    timeout_seconds: float,
    artifact_dir: Path,
    plugins_root: Path,
    standard_pack: tuple[tuple[str, tuple[str, ...]], ...],
) -> dict[str, Any]:
    result: dict[str, Any] = {}
    xvfb = None
    xvfb_log = None
    try:
        xvfb, display, xvfb_log = runtime.start_xvfb(artifact_dir)
        result = run_standard_pack_client(
            artifact_dir, display, timeout_seconds, plugins_root, standard_pack
        )
        return result
    finally:
        runtime.stop_process(xvfb)
        if xvfb_log is not None:
            xvfb_log.close()
        (artifact_dir / "standard-pack-client.json").write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n"
        )
