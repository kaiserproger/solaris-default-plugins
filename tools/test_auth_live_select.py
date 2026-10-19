"""Focused socket-keyed select/readiness test for tools/auth_live_check.py.

Real local socketpairs (no server): proves _readable returns socket objects
usable directly as mapping keys, and that _targets identity routing works
for a two-client layout mirroring the live check's primary/observer loop.
"""
from __future__ import annotations

import socket
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "tools"))

import importlib.util

SPEC = importlib.util.spec_from_file_location(
    "auth_live_check", str(Path(__file__).resolve().parents[1] / "tools" / "auth_live_check.py"))
assert SPEC is not None and SPEC.loader is not None
mod = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(mod)


class FakeClient:
    def __init__(self, sock: socket.socket) -> None:
        self.sock = sock
        self.seen_chunks: set[tuple[int, int]] = set()
        self.closed = False

    def close(self) -> None:
        self.closed = True


def test_readable_returns_socket_keys_usable_as_mapping_keys() -> None:
    left_a, right_a = socket.socketpair()
    left_b, right_b = socket.socketpair()
    try:
        left_a.setblocking(False)
        left_b.setblocking(False)
        client_a = FakeClient(left_a)
        client_b = FakeClient(left_b)
        chats_a: list[str] = []
        chats_b: list[str] = []
        mapping = mod._targets(client_a, chats_a, [(client_b, chats_b)])
        # Only B has data: readable must return B's socket object itself.
        right_b.send(b"x")
        readable = mod._readable(list(mapping), 5.0)
        assert len(readable) == 1, f"want only the ready socket, got {len(readable)}"
        assert readable[0] is left_b, "ready entry must be the socket object, not a fileno int"
        target, tchats = mapping[readable[0]]
        assert target is client_b and tchats is chats_b
        # Both ready: identity routing resolves each side.
        right_a.send(b"y")
        readable = mod._readable(list(mapping), 5.0)
        assert {id(s) for s in readable} == {id(left_a), id(left_b)}
        seen = {mapping[s][0] for s in readable}
        assert seen == {client_a, client_b}
        # Deleting by socket key (post-close path) keeps the survivor usable.
        del mapping[left_b]
        assert list(mapping) == [left_a]
    finally:
        for sock in (left_a, right_a, left_b, right_b):
            try:
                sock.close()
            except OSError:
                pass


def test_empty_select_returns_without_busy_fail() -> None:
    left, right = socket.socketpair()
    try:
        left.setblocking(False)
        started = time.monotonic()
        assert mod._readable([left], 0.1) == []
        assert time.monotonic() - started < 5.0
    finally:
        left.close()
        right.close()


if __name__ == "__main__":
    test_readable_returns_socket_keys_usable_as_mapping_keys()
    test_empty_select_returns_without_busy_fail()
    print("auth select-key tests: 2 passed")
