"""First-party economy gameplay/client checks; no package policy belongs in core."""

from __future__ import annotations

import json
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any

from tools.harness import runtime
from tools.harness.compatibility import observe_screen_class, run_inventory_probe
from tools.harness.mcp import McpClient

REPO_ROOT = runtime.REPO_ROOT
PLUGINS_ROOT = Path(__file__).resolve().parent.parent


def recent_chat_contains(observed: dict[str, Any], expected: str) -> bool:
    recent = observed.get("recent_chat")
    return isinstance(recent, list) and any(expected in str(message) for message in recent)


def write_server_only_config(
    run_dir: Path, port: int, *, operators: tuple[str, ...] = ()
) -> Path:
    plugin_root = run_dir / "plugins"
    plugin_root.mkdir()
    shutil.copytree(
        PLUGINS_ROOT / "basic-economy",
        plugin_root / "basic-economy",
    )
    economy_config = plugin_root / "basic-economy" / "config.toml"
    economy_text = economy_config.read_text()
    economy_text = economy_text.replace('resource = "minecraft:emerald"', 'resource = "minecraft:dirt"', 1)
    economy_text = economy_text.replace('singular = "Emerald"', 'singular = "Dirt"', 1)
    economy_text = economy_text.replace('plural = "Emeralds"', 'plural = "Dirt"', 1)
    economy_text = economy_text.replace("price = 3", "price = 1", 1)
    economy_config.write_text(economy_text)
    world_dir = run_dir / "world"
    world_dir.mkdir()
    operators_toml = ", ".join(f'"{name}"' for name in operators)
    config = run_dir / "server-only.toml"
    config.write_text(
        "\n".join(
            [
                "[server]",
                'name = "server-only-plugin-gate"',
                'motd = "Solaris server-only plugin real-client gate"',
                "view_distance = 4",
                "simulation_distance = 4",
                "",
                "[network]",
                'bind_address = "127.0.0.1"',
                f"port = {port}",
                "",
                "[auth]",
                "online_mode = false",
                "whitelist_enabled = false",
                "whitelist = []",
                "banned_players = []",
                "",
                "[admin]",
                f"operators = [{operators_toml}]",
                "allow_local_dev_operators = false",
                "",
                "[plugins]",
                f'directory = "{plugin_root.relative_to(REPO_ROOT).as_posix()}"',
                "",
                "[data]",
                f'world_dir = "{world_dir.relative_to(REPO_ROOT).as_posix()}"',
                "seed = 0",
                'worldgen_mode = "tellus_like"',
                "",
                "[simulation]",
                "random_tick_speed = 0",
                "save_interval_ticks = 1200",
                "friendly_spawn_interval_ticks = 0",
                "hostile_spawn_interval_ticks = 0",
                "",
                "[chunk_pipeline]",
                "chunk_send_rate = 8",
                "chunk_load_rate = 16",
                "chunk_generate_rate = 16",
                "chunk_prepare_budget_ms = 0",
                "chunk_prepare_batch_size = 8",
                "chunk_result_queue_size = 64",
                "region_cache_size = 9",
                "",
                "[autoscale]",
                "enabled = false",
                'profile = "balanced"',
                "min_view_distance = 4",
                "max_view_distance = 4",
                "",
            ]
        )
    )
    return config


def wait_for_economy_menu(client: Any, timeout_seconds: float) -> dict[str, Any]:
    deadline = time.monotonic() + timeout_seconds
    last = runtime.current_client_state(client, min(timeout_seconds, 120.0))
    while True:
        container = last.get("container")
        if (
            observe_screen_class(last).endswith("ContainerScreen")
            and isinstance(container, dict)
            and str(container.get("menu_class", "")).endswith("ChestMenu")
        ):
            return last
        if time.monotonic() >= deadline:
            break
        last = runtime.next_client_state(client, last, deadline)
    raise RuntimeError(
        "economy ChestMenu did not open: "
        + json.dumps(last, ensure_ascii=False, sort_keys=True)
    )



def find_nearby_dirt_currency(client: Any) -> tuple[dict[str, int], dict[str, int] | None]:
    observed = client.call_tool("minecraft_observe")
    player = observed.get("player")
    if not isinstance(player, dict):
        raise RuntimeError("player snapshot missing while searching for dirt currency")
    px = int(float(player["x"]) // 1)
    py = int(float(player["y"]) // 1)
    pz = int(float(player["z"]) // 1)
    scan = client.call_tool(
        "minecraft_scan_blocks",
        {
            "min_x": px - 8,
            "min_y": py - 5,
            "min_z": pz - 8,
            "max_x": px + 8,
            "max_y": py + 1,
            "max_z": pz + 8,
            "max_blocks": 2048,
        },
    )
    blocks = scan.get("blocks")
    if not isinstance(blocks, list):
        raise RuntimeError("block scan omitted blocks")
    indexed = {
        (int(block["x"]), int(block["y"]), int(block["z"])): block
        for block in blocks
        if isinstance(block, dict)
    }
    candidates: list[tuple[float, dict[str, Any]]] = []
    for (x, y, z), block in indexed.items():
        if block.get("block_id") not in {"minecraft:dirt", "minecraft:grass_block"}:
            continue
        above = indexed.get((x, y + 1, z))
        if not isinstance(above, dict) or not (
            above.get("is_air") or above.get("block_id") in {"minecraft:snow", "minecraft:snow_block"}
        ):
            continue
        distance = (x + 0.5 - float(player["x"])) ** 2 + (z + 0.5 - float(player["z"])) ** 2
        candidates.append((distance, block))
    if not candidates:
        counts: dict[str, int] = {}
        for block in indexed.values():
            name = str(block.get("block_id"))
            counts[name] = counts.get(name, 0) + 1
        raise RuntimeError(
            f"no reachable dirt/grass surface in the loaded 17x7x17 scan at {(px, py, pz)}; "
            f"block counts: {sorted(counts.items(), key=lambda pair: -pair[1])[:12]}"
        )
    candidates.sort(key=lambda entry: entry[0])
    selected = candidates[0][1]
    target = {"x": int(selected["x"]), "y": int(selected["y"]), "z": int(selected["z"])}
    above = indexed[(target["x"], target["y"] + 1, target["z"])]
    cover = None if above.get("is_air") else {**target, "y": target["y"] + 1}
    return target, cover


def economy_menu_owned_count(observed: dict[str, Any], expected: int) -> bool:
    container = observed.get("container")
    if not isinstance(container, dict):
        return False
    slots = container.get("slots")
    if not isinstance(slots, list) or not slots:
        return False
    first = slots[0]
    return isinstance(first, dict) and f"owned {expected}" in str(first.get("name", ""))


def run_server_only(
    root: Path,
    display: str,
    timeout_seconds: float,
    *,
    inventory: bool = False,
) -> dict[str, Any]:
    run_dir = root / "server-only"
    run_dir.mkdir()
    operators = ("ServerOnlyGate",) if inventory else ()
    server_port = runtime.reserve_port()
    mcp_port = runtime.reserve_port()
    config = write_server_only_config(run_dir, server_port, operators=operators)
    token = f"server-only-{time.time_ns()}"
    server_log_path = run_dir / "server.log"
    server_log = server_log_path.open("wb")
    client_log_handle = None
    server = client_process = None
    mcp = None
    try:
        server = subprocess.Popen(
            [str(REPO_ROOT / "target" / "debug" / "mc-server"), "--config", str(config)],
            cwd=REPO_ROOT,
            stdout=server_log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        runtime.wait_port(
            server_port, min(timeout_seconds, 60.0), server, log_path=server_log_path
        )
        game_dir = run_dir / "game"
        game_dir.mkdir()
        (game_dir / "options.txt").write_text("version:4790\nonboardAccessibility:false\n")
        client_process, client_log_handle = runtime.start_client(
            game_dir=game_dir,
            log_path=run_dir / "client.log",
            token=token,
            mcp_port=mcp_port,
            username="ServerOnlyGate",
            display=display,
            platform=None,
        )
        mcp = McpClient(
            f"http://127.0.0.1:{mcp_port}/mcp",
            token,
            request_timeout_seconds=30.0,
        )
        mcp.initialize()
        runtime.wait_client_ready_for_connect(mcp, min(timeout_seconds, 120.0))
        mcp.call_tool("minecraft_connect", {"server_addr": f"127.0.0.1:{server_port}"})
        play = mcp.call_tool(
            "minecraft_wait_for_play",
            {"timeout_seconds": min(timeout_seconds, 120.0)},
        )
        if not play.get("in_play"):
            raise RuntimeError(f"server-only client did not reach Play: {play}")

        mcp.call_tool(
            "minecraft_wait_for_inventory",
            {"item_id": "minecraft:dirt", "count": 0, "timeout_seconds": 10.0},
        )
        mcp.call_tool(
            "minecraft_wait_for_inventory",
            {"item_id": "minecraft:apple", "count": 0, "timeout_seconds": 10.0},
        )
        dirt_target, cover_target = find_nearby_dirt_currency(mcp)
        mcp.call_tool(
            "minecraft_navigate_to_block",
            {**(cover_target or dirt_target), "timeout_seconds": 30.0},
        )
        if cover_target is not None:
            cover_result = mcp.call_tool(
                "minecraft_break_block",
                {
                    **cover_target,
                    "face": "up",
                    "expected_drop_item_id": "minecraft:snowball",
                    "expected_drop_count": 0,
                    "timeout_seconds": 30.0,
                },
            )
            if not cover_result.get("became_air"):
                raise RuntimeError(f"natural snow cover was not removed: {cover_result}")
        break_result = mcp.call_tool(
            "minecraft_break_block",
            {
                **dirt_target,
                "face": "up",
                "expected_drop_item_id": "minecraft:dirt",
                "expected_drop_count": 1,
                "timeout_seconds": 30.0,
            },
        )
        if not break_result.get("pickup_confirmed"):
            raise RuntimeError(f"natural dirt currency was not picked up: {break_result}")
        mcp.call_tool(
            "minecraft_wait_for_inventory",
            {"item_id": "minecraft:dirt", "count": 1, "timeout_seconds": 20.0},
        )

        mcp.call_tool("minecraft_send_chat", {"message": "economy", "command": True})
        menu = wait_for_economy_menu(mcp, 20.0)
        if not economy_menu_owned_count(menu, 0):
            raise RuntimeError("fresh economy ledger did not render owned 0 for the first product")
        first_slot = menu.get("container", {}).get("slots", [{}])[0]
        if "buy 1 Dirt" not in str(first_slot.get("name", "")):
            raise RuntimeError(f"economy gate config did not expose the one-dirt purchase: {first_slot}")
        mcp.call_tool(
            "minecraft_click_container_slot",
            {"slot": 0, "button": "primary", "timeout_seconds": 20.0},
        )
        mcp.call_tool(
            "minecraft_wait_for_inventory",
            {"item_id": "minecraft:dirt", "count": 0, "timeout_seconds": 20.0},
        )
        mcp.call_tool(
            "minecraft_wait_for_inventory",
            {"item_id": "minecraft:apple", "count": 2, "timeout_seconds": 20.0},
        )
        committed = mcp.call_tool("minecraft_observe")
        if not recent_chat_contains(committed, "Purchased Apples."):
            raise RuntimeError(
                "economy purchase committed inventory but did not publish the plugin success message"
            )

        if not inventory:
            mcp.call_tool("minecraft_send_chat", {"message": "economy", "command": True})
            refreshed_menu = wait_for_economy_menu(mcp, 20.0)
            if not economy_menu_owned_count(refreshed_menu, 1):
                raise RuntimeError("durable economy ledger did not refresh to owned 1 after purchase")
            mcp.call_tool(
                "minecraft_click_container_slot",
                {"slot": 0, "button": "secondary", "timeout_seconds": 20.0},
            )
            mcp.call_tool(
                "minecraft_wait_for_inventory",
                {"item_id": "minecraft:dirt", "count": 1, "timeout_seconds": 20.0},
            )
            mcp.call_tool(
                "minecraft_wait_for_inventory",
                {"item_id": "minecraft:apple", "count": 0, "timeout_seconds": 20.0},
            )
            refunded = mcp.call_tool("minecraft_observe")
            if not recent_chat_contains(refunded, "Refunded Apples."):
                raise RuntimeError(
                    "economy refund committed inventory but did not publish the plugin success message"
                )
            mcp.call_tool("minecraft_send_chat", {"message": "economy", "command": True})
            refunded_menu = wait_for_economy_menu(mcp, 20.0)
            if not economy_menu_owned_count(refunded_menu, 0):
                raise RuntimeError("durable economy ledger did not return to owned 0 after refund")
        result: dict[str, Any] = {
            "passed": True,
            "server_port": server_port,
            "mcp_port": mcp_port,
            "in_play": True,
            "plugin": "basic-economy",
            "economy_menu_screen": observe_screen_class(menu),
            "game_dir": str(game_dir.relative_to(REPO_ROOT)),
            "fixture_operators": list(operators),
            "gameplay_loop": {
                "currency": "minecraft:dirt",
                "currency_source": "natural_dirt_or_grass_block",
                "currency_target": dirt_target,
                "break_result": break_result,
                "purchase_slot": 0,
                "currency_after_purchase": 0,
                "apples_after_purchase": 2,
                "success_message": "Purchased Apples.",
                "ledger_owned_after_purchase": 1,
            },
        }
        if not inventory:
            result["gameplay_loop"].update(
                {
                    "refund_slot": 0,
                    "currency_after_refund": 1,
                    "apples_after_refund": 0,
                    "refund_success_message": "Refunded Apples.",
                    "ledger_owned_after_refund": 0,
                }
            )
        if inventory:
            result["inventory_probe"] = run_inventory_probe(mcp, run_dir)
        mcp.call_tool("minecraft_disconnect")
        return result
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
