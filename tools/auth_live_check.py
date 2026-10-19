#!/usr/bin/env python3
"""Raw-protocol auth acceptance against a real solaris-v2 server (run-ready).

Real TCP only: handshake -> login -> configuration -> Play, then:
  1. decode the ACTUAL first SyncPlayerPosition; wait for the
     LevelChunkWithLight covering the spawn column (residency proof);
     probe blocks at/around the spawn feet with locked StartDestroyBlock
     until one repairs a SOLID state (non-air, state id != 0; air is 0
     per mc-data blocks test). Send locked Start+Finish on that SAME
     target: assert BlockUpdate repair(s) with the IDENTICAL solid state
     plus BlockChangedAck for BOTH sequences, connection alive.
     (AuthGate contract: locked Start/Finish always resyncs Ack+inventory,
     BlockUpdate iff the cell is resident; Ack alone also happens on
     success, so only the identical-repair pair counts. Cancel emits
     nothing and is never sent.)
  2. /register <pw> <pw> -> fresh 'registered' (+ auto 'logged in').
  3. POSITIVE CONTROL on the same unlocked session and SAME target:
     Start+Finish must now BREAK it: BlockUpdate with a DIFFERENT state
     (air). Same-target unauth-repair vs auth-break is the A/B proof the
     locked repair was auth refusal, not terrain/range refusal.
  4. disconnect -> relog -> fresh 'log in' prompt -> wrong password ->
     fresh 'Wrong password' -> /login <pw> -> fresh 'logged in'.
  5. fresh guest stays silent (keepalives/teleport confirms only) ->
     PlayDisconnect 'Authentication timeout'.
  6. secrecy: neither password in any received SystemChat (primary,
     second observer client, timeout guest), in disconnect reasons, or in
     server.log. The observer registers with its own password, stays
     logged in via serviced keepalives, and witnesses the whole run.

Cursor-scoped chat waits: every wait takes an explicit cursor captured
BEFORE connect/send and matches chats[cursor:] (including responses that
already arrived during connect), so no stale or lost response is possible.
All multi-client select loops iterate only the readable sockets returned
by select (never the full wake list). Event-driven waits only; no sleeps.

Native argon2id: no hash timers, no guest seed, auth config.toml absent
by design (config optional, never asserted).

Usage:
  python3 tools/auth_live_check.py \
    --server-bin /path/to/solaris-v2 \
    --plugins-dir /path/to/candidate/plugins \
    [--password AuthCheck9] [--output-dir .analysis/codex-logs/solaris-auth/run-X]
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import os
import select
import socket
import struct
import subprocess
import sys
import time
import tomllib
import zlib
from datetime import datetime, timezone
from pathlib import Path

TOOLS_ROOT = Path(__file__).resolve().parents[1]
CORE_ROOT = Path(os.environ.get("SOLARIS_CORE_ROOT", str(TOOLS_ROOT.parent / "solaris")))
DEFAULT_OUT = TOOLS_ROOT / ".analysis" / "codex-logs" / "solaris-auth"

PROTOCOL_VERSION = 775
PASSWORD_DEFAULT = "AuthCheck9"
OBSERVER_PASSWORD_DEFAULT = "ObsCheck7"
AIR_STATE_ID = 0  # mc-data blocks test: minecraft:air states[0].id == 0

# Wire IDs (verified against mc-protocol + wire-probe).
HS_HANDSHAKE = 0x00
LOGIN_START = 0x00
LOGIN_SET_COMPRESSION = 0x03
LOGIN_SUCCESS = 0x02
LOGIN_ACK = 0x03
LOGIN_DISCONNECT = 0x00
CFG_KNOWN_PACKS = 0x0E
CFG_SB_KNOWN_PACKS = 0x07
CFG_FINISH = 0x03
CFG_ACK = 0x03
CFG_DISCONNECT = 0x02
PLAY_SYNC_POS = 0x48
PLAY_KEEPALIVE = 0x2C
PLAY_DISCONNECT = 0x20
PLAY_CHAT_CMD = 0x07
PLAY_SYSTEM_CHAT = 0x79
PLAY_BLOCK_UPDATE = 0x08
PLAY_BLOCK_ACK = 0x04
PLAY_CHUNK = 0x2D
SB_KEEPALIVE = 0x1C
SB_PLAYER_ACTION = 0x29
DIG_START = 0
DIG_FINISH = 2  # StopDestroyBlock on the wire == Finish in core DigAction


def load_core_runtime():
    """Canonical server lifecycle from the core harness (no local polling)."""
    path = CORE_ROOT / "tools" / "harness" / "runtime.py"
    if not path.is_file():
        raise RuntimeError(f"missing core harness runtime: {path}")
    spec = importlib.util.spec_from_file_location("solaris_core_harness_runtime", str(path))
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def encode_varint(value: int) -> bytes:
    out = bytearray()
    value &= 0xFFFFFFFF
    while True:
        byte = value & 0x7F
        value >>= 7
        if value:
            out.append(byte | 0x80)
        else:
            out.append(byte)
            return bytes(out)


def decode_varint(data: bytes, offset: int = 0) -> tuple[int, int]:
    result = 0
    shift = 0
    pos = offset
    while True:
        if pos >= len(data):
            raise ValueError("truncated varint")
        byte = data[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        shift += 7
        if not (byte & 0x80):
            if result >= (1 << 31):
                result -= 1 << 32
            return result, pos
        if shift >= 35:
            raise ValueError("oversized varint")


def encode_string(text: str) -> bytes:
    raw = text.encode("utf-8")
    return encode_varint(len(raw)) + raw


def offline_uuid(name: str) -> bytes:
    digest = hashlib.md5(("OfflinePlayer:" + name).encode("utf-8")).digest()
    raw = bytearray(digest)
    raw[6] = (raw[6] & 0x0F) | 0x30
    raw[8] = (raw[8] & 0x3F) | 0x80
    return bytes(raw)


def pack_block_pos(x: int, y: int, z: int) -> int:
    return (((x & 0x3FFFFFF) << 38) | ((z & 0x3FFFFFF) << 12) | (y & 0xFFF))


def parse_sync_pos(body: bytes) -> tuple[int, float, float, float]:
    """Decode SyncPlayerPosition wire layout (varint + 6xf64 + 2xf32 + i32)."""
    teleport_id, pos = decode_varint(body, 0)
    x, y, z = struct.unpack_from(">ddd", body, pos)
    return teleport_id, x, y, z


def redact(text: str, secrets: list[str]) -> str:
    for secret in secrets:
        if secret:
            text = text.replace(secret, "***")
    return text


class RawClient:
    def __init__(self, addr: tuple[str, int], timeout: float = 10.0) -> None:
        self.sock = socket.create_connection(addr, timeout=timeout)
        self.sock.settimeout(timeout)
        self.compression: int | None = None
        self.seq = 1
        # Every LevelChunkWithLight coordinate observed on this connection.
        # _service records it on EVERY receive path, so a wait that drains
        # chat (e.g. wait_fresh) can never consume the only spawn chunk.
        self.seen_chunks: set[tuple[int, int]] = set()
    def close(self) -> None:
        try:
            self.sock.close()
        except OSError:
            pass

    def _send_frame(self, packet_id: int, body: bytes) -> None:
        payload = encode_varint(packet_id) + body
        if self.compression is None:
            frame = encode_varint(len(payload)) + payload
        elif len(payload) >= self.compression:
            inner = encode_varint(len(payload)) + zlib.compress(payload)
            frame = encode_varint(len(inner)) + inner
        else:
            inner = encode_varint(0) + payload
            frame = encode_varint(len(inner)) + inner
        self.sock.sendall(frame)

    def _recv_bytes(self, count: int, deadline: float) -> bytes:
        out = bytearray()
        while len(out) < count:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("timed out reading socket")
            self.sock.settimeout(remaining)
            try:
                chunk = self.sock.recv(count - len(out))
            except socket.timeout as exc:
                raise TimeoutError("timed out reading socket") from exc
            if not chunk:
                raise ConnectionError("peer closed")
            out.extend(chunk)
        return bytes(out)

    def _fill_varint(self, deadline: float) -> int:
        raw = bytearray()
        while True:
            raw += self._recv_bytes(1, deadline)
            try:
                value, pos = decode_varint(bytes(raw), 0)
                if pos == len(raw):
                    return value
            except ValueError:
                continue

    def recv_frame(self, timeout: float) -> tuple[int, bytes]:
        deadline = time.monotonic() + timeout
        length = self._fill_varint(deadline)
        if length > 16 * 1024 * 1024:
            raise ValueError("oversized frame")
        data = self._recv_bytes(length, deadline)
        if self.compression is None:
            packet_id, pos = decode_varint(data, 0)
            return packet_id, data[pos:]
        uncompressed_len, pos = decode_varint(data, 0)
        rest = data[pos:]
        if uncompressed_len:
            rest = zlib.decompress(rest)
        packet_id, pos2 = decode_varint(rest, 0)
        return packet_id, rest[pos2:]

    def send_handshake(self, port: int) -> None:
        body = encode_varint(PROTOCOL_VERSION) + encode_string("127.0.0.1")
        body += struct.pack(">H", port) + encode_varint(2)
        self._send_frame(HS_HANDSHAKE, body)

    def send_login_start(self, name: str) -> None:
        self._send_frame(LOGIN_START, encode_string(name) + offline_uuid(name))

    def send_login_ack(self) -> None:
        self._send_frame(LOGIN_ACK, b"")

    def send_known_packs_echo(self, body: bytes) -> None:
        self._send_frame(CFG_SB_KNOWN_PACKS, body)

    def send_config_ack(self) -> None:
        self._send_frame(CFG_ACK, b"")

    def send_chat_command(self, command: str) -> None:
        self._send_frame(PLAY_CHAT_CMD, encode_string(command))

    def send_keepalive(self, keep_id: int) -> None:
        self._send_frame(SB_KEEPALIVE, struct.pack(">q", keep_id))

    def send_teleport_confirm(self, teleport_id: int) -> None:
        self._send_frame(0x00, encode_varint(teleport_id))

    def send_dig(self, action: int, packed: int, sequence: int) -> None:
        body = (encode_varint(action) + struct.pack(">q", packed)
                + encode_varint(1) + encode_varint(sequence))
        self._send_frame(SB_PLAYER_ACTION, body)

    def send_dig_start(self, packed: int, sequence: int) -> None:
        self.send_dig(DIG_START, packed, sequence)

    def send_dig_finish(self, packed: int, sequence: int) -> None:
        self.send_dig(DIG_FINISH, packed, sequence)


def check_package(plugins_dir: Path) -> dict[str, str]:
    pkg = plugins_dir / "solaris-auth"
    manifest_path = pkg / "plugin.toml"
    wasm_path = pkg / "plugin.wasm"
    if not manifest_path.is_file():
        raise RuntimeError(f"missing package manifest: {manifest_path}")
    if not wasm_path.is_file():
        raise RuntimeError(f"missing built package wasm: {wasm_path} (parent must provide built auth package)")
    manifest = tomllib.loads(manifest_path.read_text())
    for key, want in (("id", "solaris-auth"), ("api", "0.7.0"), ("entry", "plugin.wasm")):
        if manifest.get(key) != want:
            raise RuntimeError(f"solaris-auth manifest {key}={manifest.get(key)!r}, want {want!r}")
    for cmd in ("register", "login", "changepassword"):
        if cmd not in manifest.get("player_commands", []):
            raise RuntimeError(f"solaris-auth manifest missing player command {cmd}")
    if "player.joined" not in manifest.get("events", []) or "player.left" not in manifest.get("events", []):
        raise RuntimeError("solaris-auth manifest must declare player.joined/player.left")
    if "before-build" in manifest.get("hooks", []):
        raise RuntimeError("solaris-auth must use the native login gate, not a before-build hook")
    for capability in ("storage", "auth"):
        if capability not in manifest.get("capabilities", []):
            raise RuntimeError(f"solaris-auth manifest must declare {capability} capability")
    # config.toml intentionally absent (native argon2id, no seed): optional, never asserted.
    return {"manifest": str(manifest_path), "wasm": str(wasm_path),
            "config_present": str((pkg / "config.toml").is_file())}


def _service(client: RawClient, chats: list[str], packet_id: int, body: bytes) -> str:
    """Answer keepalive/teleport; collect chats and chunk coords."""
    # Chunk coordinates are recorded on EVERY receive path (connect_to_play,
    # wait_fresh, denial/break waits): chat waits must never consume the only
    # spawn chunk. (Sibling triage rule: chat helpers that discard chunk
    # frames cause false residency timeouts.)
    if packet_id == PLAY_CHUNK and len(body) >= 8:
        got_x, got_z = struct.unpack(">ii", body[:8])
        client.seen_chunks.add((got_x, got_z))
        return "chunk"
    if packet_id == PLAY_KEEPALIVE and len(body) == 8:
        (keep_id,) = struct.unpack(">q", body)
        client.send_keepalive(keep_id)
        return "keepalive"
    if packet_id == PLAY_SYNC_POS:
        teleport_id, _ = decode_varint(body, 0)
        client.send_teleport_confirm(teleport_id)
        return "sync"
    if packet_id == PLAY_SYSTEM_CHAT:
        chats.append(body.decode("utf-8", errors="ignore"))
        return "chat"
    return "other"


def _readable(socks: list[socket.socket], timeout: float) -> list[socket.socket]:
    try:
        readable, _, _ = select.select(socks, [], [], max(0.0, timeout))
    except (OSError, ValueError) as exc:
        raise RuntimeError(f"socket select failed: {exc}") from exc
    return readable


def _targets(client: RawClient, chats: list[str],
             background: list[tuple[RawClient, list[str]]] | None) -> dict[socket.socket, tuple[RawClient, list[str]]]:
    mapping = {client.sock: (client, chats)}
    for other, ochats in background or []:
        if other is not None:
            mapping[other.sock] = (other, ochats)
    return mapping


def connect_to_play(host: str, port: int, name: str, timeout: float,
                     chats: list[str]) -> tuple[RawClient, tuple[float, float, float]]:
    client = RawClient((host, port), timeout=10.0)
    try:
        client.send_handshake(port)
        client.send_login_start(name)
        deadline = time.monotonic() + timeout
        while True:
            packet_id, body = client.recv_frame(max(1.0, deadline - time.monotonic()))
            if packet_id == LOGIN_SET_COMPRESSION and len(body) <= 5:
                threshold, _ = decode_varint(body, 0)
                client.compression = None if threshold < 0 else int(threshold)
                continue
            if packet_id == LOGIN_SUCCESS:
                client.send_login_ack()
                break
            if packet_id == LOGIN_DISCONNECT:
                raise RuntimeError(f"login refused: {body.decode('utf-8', errors='ignore')[:300]}")
            raise RuntimeError(f"unexpected login frame 0x{packet_id:02X}")
        echoed = False
        deadline = time.monotonic() + timeout
        while True:
            packet_id, body = client.recv_frame(max(1.0, deadline - time.monotonic()))
            if packet_id == CFG_DISCONNECT:
                raise RuntimeError(f"configuration refused: {body.decode('utf-8', errors='ignore')[:300]}")
            if packet_id == CFG_KNOWN_PACKS and not echoed:
                client.send_known_packs_echo(body)
                echoed = True
                continue
            if packet_id == CFG_FINISH and echoed and len(body) == 0:
                client.send_config_ack()
                break
        deadline = time.monotonic() + timeout
        while True:
            packet_id, body = client.recv_frame(max(1.0, deadline - time.monotonic()))
            kind = _service(client, chats, packet_id, body)
            if kind == "sync":
                _, x, y, z = parse_sync_pos(body)
                return client, (x, y, z)
            if packet_id == PLAY_DISCONNECT:
                raise RuntimeError(f"disconnected entering Play: {body.decode('utf-8', errors='ignore')[:300]}")
    except Exception:
        client.close()
        raise


def wait_fresh(client: RawClient, chats: list[str], needle: str, timeout: float, cursor: int,
               background: list[tuple[RawClient, list[str]]] | None = None,
               notes: list[dict] | None = None) -> str:
    """Match needle only in chats[cursor:] (cursor captured BEFORE connect/send).

    Already-received responses are matched first, so nothing valid is lost;
    nothing older than the cursor is ever matched, so no stale match occurs.
    Background clients stay serviced; a lost one is recorded, never dropped
    silently from the secrecy claim.
    """
    deadline = time.monotonic() + timeout
    mapping = _targets(client, chats, background)
    while True:
        for chat in chats[cursor:]:
            if needle.lower() in chat.lower():
                return chat
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError(f"missing fresh chat containing {needle!r}")
        for sock in _readable(list(mapping), min(1.0, remaining)):
            target, tchats = mapping[sock]
            try:
                packet_id, body = target.recv_frame(remaining)
            except (TimeoutError, ConnectionError) as exc:
                if target is client:
                    raise
                if notes is not None:
                    notes.append({"event": "background-lost", "at_monotonic": time.monotonic(),
                                  "error": f"{type(exc).__name__}: {exc}"})
                del mapping[sock]
                try:
                    target.close()
                except OSError:
                    pass
                continue
            kind = _service(target, tchats, packet_id, body)
            if target is client and packet_id == PLAY_DISCONNECT:
                tail = tchats[-1][:300] if len(tchats) > cursor else body.decode("utf-8", errors="ignore")[:300]
                raise RuntimeError(f"disconnected waiting for {needle!r}: {tail}")
            if target is not client or kind != "chat":
                continue


def wait_spawn_chunk(client: RawClient, chats: list[str], cx: int, cz: int, timeout: float,
                     background: list[tuple[RawClient, list[str]]] | None = None) -> None:
    """Wait for LevelChunkWithLight covering the spawn column: residency proof."""
    # seen_chunks is recorded by _service on EVERY receive path, so a chunk
    # that arrived during connect_to_play or a chat wait counts immediately.
    if (cx, cz) in client.seen_chunks:
        return
    deadline = time.monotonic() + timeout
    mapping = _targets(client, chats, background)
    while True:
        if remaining <= 0:
            raise TimeoutError(f"spawn chunk ({cx},{cz}) never streamed; dig target would not be resident")
        for sock in _readable(list(mapping), min(1.0, remaining)):
            target, tchats = mapping[sock]
            try:
                packet_id, body = target.recv_frame(remaining)
            except (TimeoutError, ConnectionError):
                if target is client:
                    raise
                del mapping[sock]
                continue
            _service(target, tchats, packet_id, body)
            if target is client and packet_id == PLAY_CHUNK and len(body) >= 8:
                got_x, got_z = struct.unpack(">ii", body[:8])
                if (got_x, got_z) == (cx, cz):
                    return
            if target is client and packet_id == PLAY_DISCONNECT:
                raise RuntimeError("disconnected waiting for spawn chunk")


def wait_single_denial(client: RawClient, chats: list[str], packed: int, seq: int, timeout: float,
                       background: list[tuple[RawClient, list[str]]] | None = None) -> int:
    """One locked Start dig: require repair BlockUpdate + matching Ack. Returns state id."""
    deadline = time.monotonic() + timeout
    mapping = _targets(client, chats, background)
    update_state: int | None = None
    acked = False
    while update_state is None or not acked:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError(
                f"locked dig unproven: update={update_state} ack={acked} "
                "(want BlockUpdate repair at resident target + BlockChangedAck)")
        for sock in _readable(list(mapping), min(1.0, remaining)):
            target, tchats = mapping[sock]
            try:
                packet_id, body = target.recv_frame(remaining)
            except (TimeoutError, ConnectionError):
                if target is client:
                    raise
                del mapping[sock]
                continue
            kind = _service(target, tchats, packet_id, body)
            if target is not client:
                continue
            if packet_id == PLAY_DISCONNECT:
                raise RuntimeError("locked dig disconnected the client; want denial-with-repair, connection alive")
            if packet_id == PLAY_BLOCK_UPDATE and len(body) >= 8:
                (pos,) = struct.unpack(">q", body[:8])
                if pos == packed:
                    state, _ = decode_varint(body, 8)
                    update_state = state
            elif packet_id == PLAY_BLOCK_ACK:
                ack_seq, _ = decode_varint(body, 0)
                if ack_seq == seq:
                    acked = True
            elif kind == "chat":
                raise RuntimeError(f"locked dig produced a chat reply (veto leak?): {tchats[-1][:200]}")
    assert update_state is not None
    return update_state


def wait_locked_pair(client: RawClient, chats: list[str], packed: int, seqs: tuple[int, int],
                     expected_state: int, timeout: float,
                     background: list[tuple[RawClient, list[str]]] | None = None) -> list[int]:
    """Locked Start+Finish: require repair(s) ALL equal to expected_state + BOTH acks."""
    deadline = time.monotonic() + timeout
    mapping = _targets(client, chats, background)
    updates: list[int] = []
    acks: set[int] = set()
    while not (set(seqs) <= acks and updates):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError(
                f"locked Start+Finish unproven: updates={updates} acks={sorted(acks)} "
                "(want identical-state BlockUpdate repair + both BlockChangedAcks)")
        for sock in _readable(list(mapping), min(1.0, remaining)):
            target, tchats = mapping[sock]
            try:
                packet_id, body = target.recv_frame(remaining)
            except (TimeoutError, ConnectionError):
                if target is client:
                    raise
                del mapping[sock]
                continue
            kind = _service(target, tchats, packet_id, body)
            if target is not client:
                continue
            if packet_id == PLAY_DISCONNECT:
                raise RuntimeError("locked pair disconnected the client; want denial-with-repair, connection alive")
            if packet_id == PLAY_BLOCK_UPDATE and len(body) >= 8:
                (pos,) = struct.unpack(">q", body[:8])
                if pos == packed:
                    state, _ = decode_varint(body, 8)
                    updates.append(state)
            elif packet_id == PLAY_BLOCK_ACK:
                ack_seq, _ = decode_varint(body, 0)
                if ack_seq in seqs:
                    acks.add(ack_seq)
            elif kind == "chat":
                raise RuntimeError(f"locked pair produced a chat reply (veto leak?): {tchats[-1][:200]}")
    if any(state != expected_state for state in updates):
        raise RuntimeError(
            f"target state changed under LOCKED Start+Finish: {updates} (want all == {expected_state})")
    return updates


def wait_break(client: RawClient, chats: list[str], packed: int, old_state: int, timeout: float,
               background: list[tuple[RawClient, list[str]]] | None = None) -> tuple[int, list[int]]:
    """Unlocked Start+Finish on the same target: require BlockUpdate to AIR (state 0)."""
    # A break must remove the block to air; merely any different state could be
    # a replacement/transform, not proof the locked repair was auth refusal.
    # Mining duration is covered by the event-driven window (no sleeps): all
    # receives keep servicing keepalives/teleports until AIR arrives or timeout.
    deadline = time.monotonic() + timeout
    mapping = _targets(client, chats, background)
    updates: list[int] = []
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError(
                f"same target did not break to AIR after unlock: updates={updates} "
                f"(locked repair {old_state} would then be terrain refusal, not auth proof)")
        for sock in _readable(list(mapping), min(1.0, remaining)):
            target, tchats = mapping[sock]
            try:
                packet_id, body = target.recv_frame(remaining)
            except (TimeoutError, ConnectionError):
                if target is client:
                    raise
                del mapping[sock]
                continue
            _service(target, tchats, packet_id, body)
            if target is not client:
                continue
            if packet_id == PLAY_DISCONNECT:
                raise RuntimeError("unlocked break disconnected the client")
            if packet_id == PLAY_BLOCK_UPDATE and len(body) >= 8:
                (pos,) = struct.unpack(">q", body[:8])
                if pos == packed:
                    state, _ = decode_varint(body, 8)
                    updates.append(state)
                    if state == AIR_STATE_ID:
                        return state, updates


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server-bin", type=Path, required=True)
    parser.add_argument("--plugins-dir", type=Path, required=True)
    parser.add_argument("--password", default=PASSWORD_DEFAULT)
    parser.add_argument("--observer-password", default=OBSERVER_PASSWORD_DEFAULT)
    parser.add_argument("--base-user", default="AuthGate")
    parser.add_argument("--timeout-leg-seconds", type=float, default=95.0)
    parser.add_argument("--output-dir", type=Path, default=None)
    args = parser.parse_args()

    core_runtime = load_core_runtime()
    password: str = args.password
    obs_password: str = args.observer_password
    for value, label in ((password, "password"), (obs_password, "observer-password")):
        if not (4 <= len(value) <= 128):
            print(f"{label} must be 4-128 bytes", file=sys.stderr)
            return 2
    if password == obs_password:
        print("observer password must differ from primary password", file=sys.stderr)
        return 2
    if not args.server_bin.is_file():
        print(f"missing server binary: {args.server_bin}", file=sys.stderr)
        return 2
    # Fingerprint the EXACT bytes launched (target/debug is mutable; a path
    # alone does not pin the build). Failure to hash fails closed.
    with open(args.server_bin, "rb") as binary:
        digest = hashlib.sha256()
        while chunk := binary.read(1024 * 1024):
            digest.update(chunk)
    server_binary_sha256 = digest.hexdigest()
    package_info = check_package(args.plugins_dir)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S")
    out = args.output_dir or (DEFAULT_OUT / f"run-{stamp}")
    out.mkdir(parents=True, exist_ok=True)

    port = core_runtime.reserve_port()
    world_dir = out / "world"
    world_dir.mkdir(exist_ok=True)
    config_path = out / "server.toml"
    config_path.write_text(
        "[server]\nname = \"solaris-auth-live\"\n"
        "motd = \"auth live gate\"\nview_distance = 4\nsimulation_distance = 4\n"
        f"[network]\nbind_address = \"127.0.0.1\"\nport = {port}\n"
        "[auth]\nonline_mode = false\nrequire_plugin_login = true\n"
        f"[plugins]\ndirectory = \"{args.plugins_dir.resolve().as_posix()}\"\n"
        f"[data]\nworld_dir = \"{world_dir.resolve().as_posix()}\"\nseed = 0\n"
    )
    server_log_path = out / "server.log"
    receipt: dict = {
        "tool": "tools/auth_live_check.py",
        "server_bin": str(args.server_bin),
        "server_binary_sha256": server_binary_sha256,
        "plugins_dir": str(args.plugins_dir.resolve()),
        "package": package_info,
        "config": str(config_path),
        "port": port,
        "steps": [],
        "notes": [],
        "passed": False,
    }

    host = "127.0.0.1"
    user_a = args.base_user + "A"
    user_timeout = args.base_user + "Tmo"
    observer_name = args.base_user + "Obs"
    secrets = [password, obs_password]
    chats_a: list[str] = []
    chats_obs: list[str] = []
    notes: list[dict] = receipt["notes"]

    server_log = server_log_path.open("wb")
    server: subprocess.Popen | None = None
    client_a: RawClient | None = None
    observer: RawClient | None = None
    failed = False
    try:
        server = subprocess.Popen(
            [str(args.server_bin), "--config", str(config_path), "--no-console"],
            cwd=str(CORE_ROOT), stdout=server_log, stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        assert server is not None
        core_runtime.wait_port(port, 60.0, server, log_path=server_log_path)
        receipt["steps"].append({"step": "server-up", "port": port, "pid": server.pid})

        # Observer first: own account, stays logged in, witnesses the run.
        obs_cursor = len(chats_obs)
        observer, _ = connect_to_play(host, port, observer_name, 60.0, chats_obs)
        receipt["steps"].append({"step": "observer-in-play"})
        wait_fresh(observer, chats_obs, "register", 30.0, obs_cursor)
        obs_send = len(chats_obs)
        observer.send_chat_command(f"register {obs_password} {obs_password}")
        wait_fresh(observer, chats_obs, "registered", 30.0, obs_send)
        wait_fresh(observer, chats_obs, "logged in", 30.0, obs_send)
        receipt["steps"].append({"step": "observer-registered"})
        obs_bg = [(observer, chats_obs)]

        a_cursor = len(chats_a)
        client_a, spawn = connect_to_play(host, port, user_a, 60.0, chats_a)
        receipt["steps"].append({"step": "user-a-in-play",
                                 "spawn": [spawn[0], spawn[1], spawn[2]]})
        prompt = wait_fresh(client_a, chats_a, "register", 30.0, a_cursor,
                            background=obs_bg, notes=notes)
        receipt["steps"].append({"step": "prompt-register-seen", "chat_tail": redact(prompt, secrets)[-200:]})

        # 1. Resident SOLID denial target from the ACTUAL spawn + observed chunk.
        cx = math.floor(spawn[0] / 16.0)
        cz = math.floor(spawn[2] / 16.0)
        wait_spawn_chunk(client_a, chats_a, cx, cz, 60.0, background=obs_bg)
        fx, fy, fz = math.floor(spawn[0]), math.floor(spawn[1]) - 1, math.floor(spawn[2])
        candidates = [(fx, fy, fz), (fx, fy - 1, fz), (fx + 1, fy, fz),
                      (fx - 1, fy, fz), (fx, fy, fz + 1), (fx, fy, fz - 1)]
        solid_xyz: tuple[int, int, int] | None = None
        solid_packed = 0
        solid_state = AIR_STATE_ID
        for cand in candidates:
            cand_packed = pack_block_pos(*cand)
            seq = client_a.seq
            client_a.seq += 1
            client_a.send_dig_start(cand_packed, seq)
            state = wait_single_denial(client_a, chats_a, cand_packed, seq, 20.0, background=obs_bg)
            if state != AIR_STATE_ID:
                solid_xyz, solid_packed, solid_state = cand, cand_packed, state
                break
        if solid_xyz is None:
            raise RuntimeError("no solid resident target near spawn (all probed repairs are air)")
        receipt["steps"].append({"step": "denial-target", "block": list(solid_xyz),
                                 "chunk": [cx, cz], "resident": True,
                                 "solid_state": solid_state})
        # Locked Start+Finish pair on the SAME solid target: identical repairs + both acks.
        s1 = client_a.seq
        client_a.seq += 1
        s2 = client_a.seq
        client_a.seq += 1
        client_a.send_dig_start(solid_packed, s1)
        client_a.send_dig_finish(solid_packed, s2)
        pair_updates = wait_locked_pair(client_a, chats_a, solid_packed, (s1, s2),
                                        solid_state, 20.0, background=obs_bg)
        receipt["steps"].append({"step": "unauth-start-finish-denied",
                                 "state_id": solid_state, "seqs": [s1, s2],
                                 "repairs": pair_updates})

        # 2. Register (fresh waits from the send cursor; consume the auto-login follow-up).
        send_cursor = len(chats_a)
        t_register_sent = time.monotonic()
        client_a.send_chat_command(f"register {password} {password}")
        reg_chat = wait_fresh(client_a, chats_a, "registered", 60.0, send_cursor,
                              background=obs_bg, notes=notes)
        auto_login = wait_fresh(client_a, chats_a, "logged in", 30.0, send_cursor,
                                background=obs_bg, notes=notes)
        receipt["steps"].append({"step": "registered",
                                 "chat_tail": redact(reg_chat, secrets)[-200:],
                                 "auto_login_tail": redact(auto_login, secrets)[-200:]})
        for note in notes:
            if note.get("event") == "background-lost" and note.get("at_monotonic", float("inf")) < t_register_sent:
                raise RuntimeError("observer died before witnessing password traffic; secrecy claim unbounded")

        # 3. POSITIVE CONTROL: same session (now unlocked), SAME target must BREAK.
        b1 = client_a.seq
        client_a.seq += 1
        b2 = client_a.seq
        client_a.seq += 1
        client_a.send_dig_start(solid_packed, b1)
        client_a.send_dig_finish(solid_packed, b2)
        new_state, break_updates = wait_break(client_a, chats_a, solid_packed, solid_state,
                                              20.0, background=obs_bg)
        receipt["steps"].append({"step": "positive-control-same-target-breaks",
                                 "before": solid_state, "after": new_state,
                                 "updates": break_updates, "seqs": [b1, b2]})
        client_a.close()
        client_a = None
        receipt["steps"].append({"step": "user-a-disconnect"})

        # 4. Relog on a fresh connection/log: wrong then correct password.
        a_cursor2 = len(chats_a)
        client_a, _ = connect_to_play(host, port, user_a, 60.0, chats_a)
        login_prompt = wait_fresh(client_a, chats_a, "log in", 30.0, a_cursor2,
                                  background=obs_bg, notes=notes)
        receipt["steps"].append({"step": "prompt-login-seen", "chat_tail": redact(login_prompt, secrets)[-200:]})
        wrong_cursor = len(chats_a)
        client_a.send_chat_command("login WrongPass1")
        wrong_chat = wait_fresh(client_a, chats_a, "wrong password", 60.0, wrong_cursor,
                                background=obs_bg, notes=notes)
        receipt["steps"].append({"step": "wrong-password-rejected", "chat_tail": redact(wrong_chat, secrets)[-200:]})
        ok_cursor = len(chats_a)
        client_a.send_chat_command(f"login {password}")
        ok_chat = wait_fresh(client_a, chats_a, "logged in", 60.0, ok_cursor,
                             background=obs_bg, notes=notes)
        receipt["steps"].append({"step": "logged-in", "chat_tail": redact(ok_chat, secrets)[-200:]})
        client_a.close()
        client_a = None

        # 5. Timeout leg: guest silent; observer serviced on the same select loop.
        timeout_chats: list[str] = []
        guest, _ = connect_to_play(host, port, user_timeout, 60.0, timeout_chats)
        receipt["steps"].append({"step": "timeout-guest-in-play"})
        deadline = time.monotonic() + args.timeout_leg_seconds
        reason = ""
        obs_lost_at: float | None = None
        guest_map = {guest.sock: guest}
        if observer is not None:
            guest_map[observer.sock] = observer
        while time.monotonic() < deadline:
            remaining = deadline - time.monotonic()
            for sock in _readable(list(guest_map), min(1.0, remaining)):
                target = guest_map[sock]
                tchats = timeout_chats if target is guest else chats_obs
                try:
                    packet_id, body = target.recv_frame(remaining)
                except (TimeoutError, ConnectionError) as exc:
                    if target is guest:
                        raise
                    notes.append({"event": "observer-lost-during-timeout",
                                  "error": f"{type(exc).__name__}: {exc}"})
                    obs_lost_at = time.monotonic()
                    try:
                        observer.close()
                    except OSError:
                        pass
                    observer = None
                    del guest_map[sock]
                    continue
                _service(target, tchats, packet_id, body)
                if target is guest and packet_id == PLAY_DISCONNECT:
                    reason = body.decode("utf-8", errors="ignore")
                    break
            if reason:
                break
        guest.close()
        if "uthentication timeout" not in reason:
            raise RuntimeError(
                f"timeout leg did not end in Authentication timeout disconnect; got: {reason[:300]!r}")
        receipt["steps"].append({"step": "timeout-disconnect", "reason_tail": redact(reason, secrets)[-200:]})
        for secret in secrets:
            if secret in reason:
                raise RuntimeError("password leaked in timeout disconnect reason")

        # 6. Secrecy across every received chat + observer liveness bound.
        for label, chats in (("primary", chats_a), ("observer", chats_obs),
                             ("timeout-guest", timeout_chats)):
            for chat in chats:
                for secret in secrets:
                    if secret in chat:
                        raise RuntimeError(f"password leaked to {label} chat")
        receipt["steps"].append({"step": "secrecy-chats-clean",
                                 "primary_chats": len(chats_a),
                                 "observer_chats": len(chats_obs),
                                 "observer_alive_to_end": observer is not None,
                                 "observer_lost_at": obs_lost_at})

        receipt["passed"] = True
    except Exception as error:
        receipt["error"] = f"{type(error).__name__}: {error}"
        failed = True
    finally:
        for client in (client_a, observer):
            if client is not None:
                try:
                    client.close()
                except OSError:
                    pass
        if server is not None:
            core_runtime.stop_process(server, interrupt=True)
        try:
            server_log.close()
        except OSError:
            pass
        try:
            logged = server_log_path.read_bytes()
            receipt["server_log_bytes"] = len(logged)
            leaked = [s for s in secrets if s.encode() in logged]
            receipt["server_log_has_password"] = bool(leaked)
            if leaked:
                receipt["passed"] = False
                failed = True
                receipt["error"] = (receipt.get("error", "") + " | password present in server.log").strip(" |")
        except OSError as error:
            receipt["server_log_error"] = str(error)
        receipt["chat_tails"] = {
            "primary": [redact(chat, secrets)[-300:] for chat in chats_a[-20:]],
            "observer": [redact(chat, secrets)[-300:] for chat in chats_obs[-20:]],
        }
        (out / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
        print(json.dumps({"passed": receipt["passed"] and not failed,
                          "receipt": str(out / "receipt.json"),
                          "steps": len(receipt["steps"]), "error": receipt.get("error"),
                          "notes": notes}))
        sys.stdout.flush()
    return 1 if (failed or not receipt.get("passed")) else 0


if __name__ == "__main__":
    raise SystemExit(main())
