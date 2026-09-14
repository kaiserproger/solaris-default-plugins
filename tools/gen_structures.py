#!/usr/bin/env python3
"""Authored-data generator for the `solaris-settlements` blueprint catalog.

This tool is the single source of truth for
`solaris-settlements/structures/*.toml`. It emits deterministic, byte-stable
TOML: cells are sorted by `(x, y, z)`, palette entries are derived from the
materials actually used and sorted by `(block, properties)`, and the four
construction stages always appear in the order `foundation, frame, roof,
fitting`.

The authored format is schema 1, frozen by the plugin authoring reference and
decoded only by `crates/mc-worldgen/src/settlement_catalog.rs` in the core
repository (`parse_blueprint`, `build_blueprint`, `check_closed_keys`). Every
palette entry must carry the complete, legal property set of the real block
definition in `crates/mc-data/data/required_blocks.json`, every state must
survive all four quarter turns, and the union of the construction stages must
equal the body `blocks`.

Usage:
    python3 tools/gen_structures.py --write   # (re)generate the catalog
    python3 tools/gen_structures.py --check   # CI: committed files match

`--check` exits non-zero when a committed file is missing, stale, or when an
unexpected extra `.toml` file appears in the catalog directory.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

# --- stages ---------------------------------------------------------------

FOUNDATION = "foundation"
FRAME = "frame"
ROOF = "roof"
FITTING = "fitting"
STAGE_ORDER = (FOUNDATION, FRAME, ROOF, FITTING)

MAX_DISTINCT_BLOCKS = 16

# --- materials ------------------------------------------------------------


def M(name: str, **properties: str) -> tuple[str, dict[str, str]]:
    """A block state spec: `M("oak_door", facing="south", half="lower")`."""
    return (f"minecraft:{name}", dict(properties))


TORCH = M("torch")
LANTERN = M("lantern", hanging="false", waterlogged="false")

DIRS = {
    "north": (0, -1),
    "south": (0, 1),
    "east": (1, 0),
    "west": (-1, 0),
}


def planks(kind: str = "oak") -> tuple[str, dict[str, str]]:
    return M(f"{kind}_planks")


def log(kind: str = "oak", axis: str = "y") -> tuple[str, dict[str, str]]:
    return M(f"{kind}_log", axis=axis)


def stairs(kind: str, facing: str) -> tuple[str, dict[str, str]]:
    return M(f"{kind}_stairs", facing=facing, half="bottom", shape="straight", waterlogged="false")


def slab(kind: str) -> tuple[str, dict[str, str]]:
    return M(f"{kind}_slab", type="bottom", waterlogged="false")


def fence(kind: str) -> tuple[str, dict[str, str]]:
    return M(
        f"{kind}_fence",
        north="true",
        east="true",
        south="true",
        west="true",
        waterlogged="false",
    )


def bars(kind: str = "iron") -> tuple[str, dict[str, str]]:
    return M(
        f"{kind}_bars",
        north="false",
        east="false",
        south="false",
        west="false",
        waterlogged="false",
    )


def door(kind: str, facing: str, half: str) -> tuple[str, dict[str, str]]:
    return M(
        f"{kind}_door",
        facing=facing,
        half=half,
        hinge="left",
        open="false",
        powered="false",
    )


def ladder(facing: str) -> tuple[str, dict[str, str]]:
    return M("ladder", facing=facing, waterlogged="false")


def bed(facing: str, part: str) -> tuple[str, dict[str, str]]:
    return M("red_bed", facing=facing, occupied="false", part=part)


def chest(facing: str = "north") -> tuple[str, dict[str, str]]:
    return M("chest", type="single", facing=facing, waterlogged="false")


def barrel() -> tuple[str, dict[str, str]]:
    return M("barrel", facing="up", open="false")


def composter() -> tuple[str, dict[str, str]]:
    return M("composter", level="0")


def lectern(facing: str) -> tuple[str, dict[str, str]]:
    return M("lectern", facing=facing, has_book="true", powered="false")


WATER = M("water", level="0")
FARMLAND = M("farmland", moisture="7")
WHEAT = M("wheat", age="7")
HAY = M("hay_block", axis="y")
COBBLE = M("cobblestone")
STONE_BRICKS = M("stone_bricks")
GLASS = M("glass")
DIRT_PATH = M("dirt_path")
GRAVEL = M("gravel")
BOOKSHELF = M("bookshelf")
CRAFTING_TABLE = M("crafting_table")
SMITHING_TABLE = M("smithing_table")
STONECUTTER = M("stonecutter", facing="south")
GRINDSTONE = M("grindstone", face="floor", facing="south")
FURNACE = M("furnace", facing="south", lit="false")
BLAST_FURNACE = M("blast_furnace", facing="south", lit="false")
ANVIL = M("anvil", facing="north")
CAULDRON = M("cauldron")
CAMPFIRE = M("campfire", facing="south", lit="false", signal_fire="false", waterlogged="false")
BELL = M("bell", attachment="floor", facing="north", powered="false")
RAIL = M("rail", shape="north_south", waterlogged="false")
WHITE_WOOL = M("white_wool")
RED_WOOL = M("red_wool")


# --- plan model -----------------------------------------------------------


def ring(x0: int, z0: int, x1: int, z1: int) -> list[tuple[int, int]]:
    """Perimeter cells of a rectangle, corners once, deterministic order."""
    cells: list[tuple[int, int]] = []
    for x in range(x0, x1 + 1):
        cells.append((x, z0))
    if z1 != z0:
        for x in range(x0, x1 + 1):
            cells.append((x, z1))
    for z in range(z0 + 1, z1):
        cells.append((x0, z))
        cells.append((x1, z))
    return cells


def window_cells(x0: int, z0: int, x1: int, z1: int, y: int) -> list[tuple[int, int, int]]:
    """Spaced window openings on every wall, corners and door columns free."""
    cells: list[tuple[int, int, int]] = []
    for x in range(x0 + 2, x1 - 1, 3):
        cells.append((x, y, z0))
        cells.append((x, y, z1))
    for z in range(z0 + 2, z1 - 1, 3):
        cells.append((x0, y, z))
        cells.append((x1, y, z))
    return cells


class Plan:
    """One authored blueprint under construction."""

    def __init__(self, blueprint_id: str, size: tuple[int, int, int]):
        self.id = blueprint_id
        self.size = list(size)
        self.cells: dict[tuple[int, int, int], tuple[tuple, str]] = {}
        self.pois: list[tuple[str, str, tuple[int, int, int], int]] = []
        self.connections: list[tuple[tuple[int, int, int], str]] = []
        self.entities: list[tuple[tuple[int, int, int], str]] = []
        self.restoration: dict[str, dict[tuple[int, int, int], tuple]] = {}
        self.anchor: list[int] | None = None
        self.variant_of: str | None = None
        self.doors: list[tuple[tuple[int, int, int], tuple[int, int, int], tuple[int, int, int]]] = []

    # cells ---------------------------------------------------------------

    def set(self, x: int, y: int, z: int, spec: tuple, stage: str) -> None:
        assert stage in STAGE_ORDER, stage
        self.cells[(x, y, z)] = (spec, stage)

    def clear(self, x: int, y: int, z: int) -> None:
        self.cells.pop((x, y, z), None)

    def box(self, x0: int, y0: int, z0: int, x1: int, y1: int, z1: int, spec: tuple, stage: str) -> None:
        for x in range(x0, x1 + 1):
            for y in range(y0, y1 + 1):
                for z in range(z0, z1 + 1):
                    self.set(x, y, z, spec, stage)

    # furniture -----------------------------------------------------------

    def add_door(
        self,
        x: int,
        z: int,
        facing: str,
        y: int,
        kind: str = "oak",
        inside: bool = True,
    ) -> None:
        lower = door(kind, facing, "lower")
        upper = door(kind, facing, "upper")
        self.clear(x, y, z)
        self.clear(x, y + 1, z)
        self.set(x, y, z, lower, FRAME)
        self.set(x, y + 1, z, upper, FRAME)
        nx, nz = DIRS[facing]
        side = -1 if inside else 1
        self.doors.append(
            (
                (x, y, z),
                (x, y + 1, z),
                (x + side * nx, y, z + side * nz),
            )
        )

    def add_bed(self, x: int, z: int, facing: str, y: int) -> None:
        dx, dz = DIRS[facing]
        self.set(x, y, z, bed(facing, "foot"), FITTING)
        self.set(x + dx, y, z + dz, bed(facing, "head"), FITTING)
        self.entities.append(((x, y, z), "bed"))

    def add_ladder(self, x: int, z: int, y0: int, y1: int, facing: str) -> None:
        for y in range(y0, y1 + 1):
            self.set(x, y, z, ladder(facing), FITTING)

    # records -------------------------------------------------------------

    def poi(self, name: str, kind: str, at: tuple[int, int, int], capacity: int) -> None:
        self.pois.append((name, kind, at, capacity))

    def link(self, at: tuple[int, int, int], facing: str) -> None:
        self.connections.append((at, facing))

    def entity(self, at: tuple[int, int, int], kind: str) -> None:
        self.entities.append((at, kind))

    # validation ----------------------------------------------------------

    def stage_cells(self) -> dict[str, dict[tuple[int, int, int], tuple]]:
        stages: dict[str, dict[tuple[int, int, int], tuple]] = {}
        for pos, (spec, stage) in self.cells.items():
            stages.setdefault(stage, {})[pos] = spec
        return stages

    def palette(self) -> list[tuple[str, dict[str, str]]]:
        used: set[tuple] = set()
        for spec, _ in self.cells.values():
            used.add((spec[0], tuple(sorted(spec[1].items()))))
        for cells in self.restoration.values():
            for spec in cells.values():
                used.add((spec[0], tuple(sorted(spec[1].items()))))
        return [(block, dict(props)) for block, props in sorted(used)]

    def validate(self) -> None:
        stages = self.stage_cells()
        assert 3 <= len(stages) <= 4, f"{self.id}: {len(stages)} stages"
        for stage, cells in stages.items():
            assert cells, f"{self.id}: stage {stage} is empty"
        union: set[tuple[int, int, int]] = set()
        for cells in stages.values():
            for pos in cells:
                assert pos not in union, f"{self.id}: {pos} in two stages"
                union.add(pos)
        assert union == set(self.cells), f"{self.id}: stage union != body"

        palette = self.palette()
        assert len(palette) <= MAX_DISTINCT_BLOCKS, f"{self.id}: {len(palette)} block ids"
        for stage, cells in stages.items():
            distinct = {spec[0] for spec in cells.values()}
            assert len(distinct) <= MAX_DISTINCT_BLOCKS, f"{self.id}: stage {stage}"

        for pos in list(self.cells) + [p for cells in self.restoration.values() for p in cells]:
            assert all(0 <= pos[axis] < self.size[axis] for axis in range(3)), f"{self.id}: {pos}"
        for eyes in self.pois:
            _, _, at, capacity = eyes
            assert all(0 <= at[axis] < self.size[axis] for axis in range(3)), f"{self.id}: poi {at}"
            assert capacity > 0, f"{self.id}: poi capacity"
        assert len({name for name, _, _, _ in self.pois}) == len(self.pois), f"{self.id}: poi ids"
        assert self.connections, f"{self.id}: no street connection"
        assert self.anchor is not None, f"{self.id}: no anchor"
        assert all(0 <= self.anchor[axis] < self.size[axis] for axis in range(3)), f"{self.id}: anchor"
        assert 1 <= self.size[0] <= 64 and 1 <= self.size[1] <= 64 and 1 <= self.size[2] <= 64

        # POIs stand on solid ground with at least two clear cells of headroom.
        for name, _, at, _ in self.pois:
            x, y, z = at
            assert (x, y, z) not in self.cells, f"{self.id}: poi {name} inside a block"
            assert (x, y - 1, z) in self.cells, f"{self.id}: poi {name} unsupported"
            for step in (1, 2):
                if y + step < self.size[1]:
                    assert (
                        x, y + step, z
                    ) not in self.cells, f"{self.id}: poi {name} lacks headroom"

        # Door openings keep the passage inside (and outside when in bounds) clear.
        for lower, upper, inside in self.doors:
            assert self.cells[lower][0][1].get("half") == "lower", f"{self.id}: door {lower}"
            assert self.cells[upper][0][1].get("half") == "upper", f"{self.id}: door {upper}"
            assert inside not in self.cells, f"{self.id}: door blocked at {inside}"

    def damage(self, removed) -> "Plan":
        """A ruined copy of this plan with `removed(pos)` cells collapsed."""
        ruined = Plan(self.id, tuple(self.size))
        ruined.anchor = list(self.anchor)
        ruined.variant_of = self.variant_of
        ruined.cells = {pos: cell for pos, cell in self.cells.items() if not removed(pos)}
        ruined.pois = list(self.pois)
        ruined.connections = list(self.connections)
        ruined.entities = [entry for entry in self.entities if entry[0] in ruined.cells]
        return ruined

    def restore_from(self, intact: "Plan") -> None:
        """Restoration work: every intact cell this ruined body is missing."""
        missing = {
            pos: spec
            for pos, (spec, _) in intact.cells.items()
            if pos not in self.cells
        }
        self.restoration = {}
        lower = {pos: spec for pos, spec in missing.items() if pos[1] <= 6}
        upper = {pos: spec for pos, spec in missing.items() if pos[1] >= 7}
        if lower:
            self.restoration["shore_base"] = lower
        if upper:
            self.restoration["rebuild_upper"] = upper


# --- building kit ---------------------------------------------------------


def cabin(
    plan: Plan,
    x0: int,
    z0: int,
    x1: int,
    z1: int,
    base: int,
    top: int,
    roof_y: int,
    floor_mat: tuple,
    wall_mat: tuple,
    roof_mat: tuple,
    door_spec: tuple | None = None,
    win_y: int | None = None,
    window_mat: tuple = GLASS,
    corner_mat: tuple | None = None,
    base_course: tuple | None = None,
) -> None:
    """Hollow single-storey shell: floor at `base`, walls, optional door, roof."""
    plan.box(x0, base, z0, x1, base, z1, floor_mat, FOUNDATION)
    for y in range(base + 1, top + 1):
        for x, z in ring(x0, z0, x1, z1):
            plan.set(x, y, z, wall_mat, FRAME)
    if base_course is not None:
        for x, z in ring(x0, z0, x1, z1):
            plan.set(x, base + 1, z, base_course, FRAME)
    if corner_mat is not None:
        for x, z in ((x0, z0), (x0, z1), (x1, z0), (x1, z1)):
            for y in range(base + 1, top + 1):
                plan.set(x, y, z, corner_mat, FRAME)
    if win_y is not None:
        for x, y, z in window_cells(x0, z0, x1, z1, win_y):
            plan.set(x, y, z, window_mat, FRAME)
    if door_spec is not None:
        dx, dz, facing, kind = door_spec
        plan.add_door(dx, dz, facing, base + 1, kind=kind)
    plan.box(x0, roof_y, z0, x1, roof_y, z1, roof_mat, ROOF)


def merlons(plan: Plan, x0: int, z0: int, x1: int, z1: int, y: int, mat: tuple) -> None:
    for x, z in ring(x0, z0, x1, z1):
        if (x + z) % 2 == 0:
            plan.set(x, y, z, mat, ROOF)


def posts(plan: Plan, positions, y0: int, y1: int, mat: tuple, stage: str = FRAME) -> None:
    for x, z in positions:
        for y in range(y0, y1 + 1):
            plan.set(x, y, z, mat, stage)


# --- blueprints -----------------------------------------------------------


def house_small() -> Plan:
    p = Plan("solaris:house_small", (11, 7, 9))
    cabin(
        p, 0, 0, 10, 8, 1, 5, 6,
        planks("oak"), planks("oak"), M("oak_slab", type="bottom", waterlogged="false"),
        door_spec=(5, 8, "south", "oak"), win_y=4, corner_mat=log("oak"),
    )
    p.box(0, 6, 0, 10, 6, 8, planks("oak"), ROOF)
    for x in range(2, 7):
        p.add_bed(x, 5, "north", 2)
    p.set(8, 2, 2, CRAFTING_TABLE, FITTING)
    p.set(9, 2, 2, chest("north"), FITTING)
    p.entity((9, 2, 2), "empty_container")
    p.set(1, 2, 1, TORCH, FITTING)
    p.set(9, 2, 7, TORCH, FITTING)
    p.poi("home", "home", (5, 2, 2), 5)
    p.link((5, 0, 8), "south")
    p.anchor = [5, 0, 8]
    return p


def house_large() -> Plan:
    p = Plan("solaris:house_large", (13, 9, 11))
    cabin(
        p, 0, 0, 12, 10, 1, 4, 5,
        planks("oak"), planks("oak"), planks("oak"),
        door_spec=(6, 10, "south", "oak"), win_y=3, corner_mat=log("oak"),
    )
    # Upper storey: walls, then the roof slab.
    for y in (6, 7):
        for x, z in ring(0, 0, 12, 10):
            p.set(x, y, z, planks("oak"), ROOF)
    for x, z in ((0, 0), (0, 10), (12, 0), (12, 10)):
        for y in (6, 7):
            p.set(x, y, z, log("oak"), ROOF)
    for x, y, z in window_cells(0, 0, 12, 10, 6):
        p.set(x, y, z, GLASS, ROOF)
    p.add_door(6, 10, "south", 6, kind="oak", inside=True)
    p.clear(1, 5, 1)
    p.box(0, 8, 0, 12, 8, 10, planks("oak"), ROOF)
    p.add_ladder(1, 1, 2, 7, "east")
    for x in (2, 3, 4, 5):
        p.add_bed(x, 7, "north", 2)
    for x in (2, 3, 4, 5):
        p.add_bed(x, 8, "north", 6)
    p.set(11, 2, 2, chest("north"), FITTING)
    p.entity((11, 2, 2), "empty_container")
    p.set(11, 6, 2, chest("north"), FITTING)
    p.entity((11, 6, 2), "empty_container")
    p.set(1, 2, 8, TORCH, FITTING)
    p.poi("home", "home", (6, 2, 5), 8)
    p.link((6, 0, 10), "south")
    p.anchor = [6, 0, 10]
    return p


def plaza_well() -> Plan:
    p = Plan("solaris:plaza_well", (15, 5, 15))
    p.box(0, 0, 0, 14, 0, 14, COBBLE, FOUNDATION)
    # Well: 3x3 stone-brick ring with water in the middle.
    for y in (1, 2):
        for x, z in ring(6, 6, 8, 8):
            p.set(x, y, z, STONE_BRICKS, FRAME)
    p.set(7, 1, 7, WATER, FITTING)
    p.set(7, 3, 7, log("oak"), FRAME)
    p.set(7, 4, 7, M("lantern", hanging="false", waterlogged="false"), FITTING)
    posts(p, ((0, 0), (0, 14), (14, 0), (14, 14)), 1, 3, log("oak"))
    for x, z in ((0, 0), (0, 14), (14, 0), (14, 14)):
        p.set(x, 4, z, M("lantern", hanging="false", waterlogged="false"), FITTING)
    for x, z in ((3, 3), (11, 3), (3, 11), (11, 11)):
        p.set(x, 1, z, TORCH, FITTING)
    for x in (2, 3, 4):
        p.set(x, 1, 7, slab("oak"), FITTING)
    p.poi("fair", "meeting", (7, 1, 4), 8)
    for at, facing in (
        ((7, 0, 14), "south"),
        ((7, 0, 0), "north"),
        ((14, 0, 7), "east"),
        ((0, 0, 7), "west"),
    ):
        p.link(at, facing)
    p.anchor = [7, 0, 7]
    return p


def market() -> Plan:
    p = Plan("solaris:market", (17, 6, 13))
    p.box(0, 0, 0, 16, 0, 12, COBBLE, FOUNDATION)
    stalls = ((1, 1), (11, 1), (1, 7), (11, 7))
    for sx, sz in stalls:
        for x in (sx, sx + 4):
            for z in (sz, sz + 2):
                for y in (1, 2):
                    p.set(x, y, z, log("oak"), FRAME)
        p.box(sx, 3, sz, sx + 4, 3, sz + 2, WHITE_WOOL, ROOF)
        for x in range(sx + 1, sx + 4):
            p.set(x, 1, sz + 2, slab("oak"), FITTING)
        p.set(sx + 1, 1, sz, barrel(), FITTING)
        p.entity((sx + 1, 1, sz), "empty_container")
        p.set(sx + 3, 1, sz, chest("north"), FITTING)
        p.entity((sx + 3, 1, sz), "empty_container")
    p.set(8, 1, 3, RED_WOOL, ROOF)
    p.set(8, 2, 3, log("oak"), FRAME)
    p.set(3, 1, 6, TORCH, FITTING)
    p.set(13, 1, 6, TORCH, FITTING)
    p.poi("trading", "work", (3, 1, 5), 4)
    p.poi("gathering", "meeting", (8, 1, 6), 4)
    p.link((8, 0, 12), "south")
    p.anchor = [8, 0, 12]
    return p


def warehouse() -> Plan:
    p = Plan("solaris:warehouse", (15, 7, 13))
    cabin(
        p, 0, 0, 14, 12, 1, 5, 6,
        COBBLE, planks("oak"), planks("oak"),
        door_spec=(7, 12, "south", "oak"), win_y=4, corner_mat=log("oak"),
        base_course=STONE_BRICKS,
    )
    # Wide loading bay opening on the south wall.
    for x in (5, 6, 8, 9):
        p.clear(x, 2, 12)
        p.clear(x, 3, 12)
    p.add_ladder(1, 1, 2, 5, "east")
    for x in (2, 3, 4):
        p.set(x, 2, 2, chest("north"), FITTING)
        p.entity((x, 2, 2), "empty_container")
    for x in (2, 3, 4):
        p.set(x, 2, 4, barrel(), FITTING)
        p.entity((x, 2, 4), "empty_container")
    p.box(11, 2, 2, 12, 2, 2, HAY, FITTING)
    p.box(11, 2, 4, 12, 2, 4, HAY, FITTING)
    p.set(11, 3, 2, HAY, FITTING)
    p.set(12, 3, 4, HAY, FITTING)
    p.set(7, 2, 6, TORCH, FITTING)
    p.poi("stores", "work", (7, 2, 9), 8)
    p.link((7, 0, 12), "south")
    p.anchor = [7, 0, 12]
    return p


def farm() -> Plan:
    p = Plan("solaris:farm", (13, 5, 13))
    for x in range(13):
        for z in range(13):
            if x in (0, 12) or z in (0, 12):
                p.set(x, 0, z, DIRT_PATH, FOUNDATION)
            elif x == 6:
                p.set(x, 0, z, WATER, FOUNDATION)
            else:
                p.set(x, 0, z, FARMLAND, FOUNDATION)
    for x in range(13):
        for z in range(13):
            if (x in (0, 12) or z in (0, 12)) and not (x == 6 and z == 0):
                p.set(x, 1, z, fence("oak"), FRAME)
    for x in range(1, 12):
        if x == 6:
            continue
        for z in range(1, 12):
            p.set(x, 1, z, WHEAT, FITTING)
    p.set(2, 1, 2, composter(), FITTING)
    p.set(3, 1, 2, composter(), FITTING)
    p.box(10, 1, 10, 11, 1, 10, HAY, FITTING)
    p.set(10, 2, 10, HAY, FITTING)
    p.poi("field", "work", (6, 1, 0), 6)
    p.link((6, 0, 0), "north")
    p.anchor = [6, 0, 0]
    return p


def sawmill() -> Plan:
    p = Plan("solaris:sawmill", (13, 6, 11))
    cabin(
        p, 0, 0, 12, 10, 1, 4, 5,
        COBBLE, planks("oak"), planks("oak"),
        door_spec=(6, 10, "south", "oak"), win_y=3, corner_mat=log("oak"),
    )
    p.set(3, 2, 2, STONECUTTER, FITTING)
    p.set(5, 2, 2, CRAFTING_TABLE, FITTING)
    p.set(7, 2, 2, GRINDSTONE, FITTING)
    p.set(9, 2, 2, chest("north"), FITTING)
    p.entity((9, 2, 2), "empty_container")
    for x in (2, 3, 4):
        p.set(x, 2, 8, log("oak", axis="x"), FITTING)
        p.set(x, 3, 8, log("oak", axis="x"), FITTING)
    for x in (6, 7):
        p.set(x, 2, 8, log("oak"), FITTING)
        p.set(x, 3, 8, log("oak"), FITTING)
    p.set(1, 2, 1, TORCH, FITTING)
    p.poi("mill", "work", (6, 2, 6), 4)
    p.link((6, 0, 10), "south")
    p.anchor = [6, 0, 10]
    return p


def pen() -> Plan:
    p = Plan("solaris:pen", (13, 4, 11))
    p.box(0, 0, 0, 12, 0, 10, DIRT_PATH, FOUNDATION)
    for x in range(13):
        for z in range(11):
            if x in (0, 12) or z in (0, 10):
                if not (x == 6 and z == 10):
                    p.set(x, 1, z, fence("oak"), FRAME)
    for x in (2, 3):
        p.set(x, 1, 3, WATER, FITTING)
    for x, z in ((2, 2), (3, 2), (1, 3), (4, 3)):
        p.set(x, 1, z, STONE_BRICKS, FITTING)
    p.box(10, 1, 1, 11, 1, 2, HAY, FITTING)
    p.set(10, 2, 1, HAY, FITTING)
    p.poi("pasture", "work", (6, 1, 5), 6)
    p.link((6, 0, 10), "south")
    p.anchor = [6, 0, 10]
    return p


def fishing_pier() -> Plan:
    p = Plan("solaris:fishing_pier", (9, 4, 15))
    p.box(0, 0, 0, 8, 0, 14, WATER, FOUNDATION)
    p.box(0, 1, 0, 8, 1, 14, planks("oak"), FOUNDATION)
    for x in (1, 7):
        for z in (1, 4, 7, 10, 13):
            for y in (0, 1, 2):
                p.set(x, y, z, log("oak"), FRAME)
    for z in range(1, 14):
        p.set(0, 2, z, fence("oak"), FRAME)
        p.set(8, 2, z, fence("oak"), FRAME)
    p.box(1, 3, 9, 7, 3, 13, planks("oak"), ROOF)
    p.set(3, 2, 11, barrel(), FITTING)
    p.entity((3, 2, 11), "empty_container")
    p.set(5, 2, 11, barrel(), FITTING)
    p.entity((5, 2, 11), "empty_container")
    p.set(4, 2, 11, chest("north"), FITTING)
    p.entity((4, 2, 11), "empty_container")
    p.set(1, 2, 2, TORCH, FITTING)
    p.poi("docks", "work", (4, 2, 5), 4)
    p.link((4, 1, 14), "south")
    p.anchor = [4, 1, 14]
    return p


def mine_entrance() -> Plan:
    p = Plan("solaris:mine_entrance", (11, 5, 9))
    p.box(0, 0, 0, 10, 0, 8, GRAVEL, FOUNDATION)
    p.box(0, 1, 2, 10, 3, 4, STONE_BRICKS, FRAME)
    for x in range(4, 7):
        p.clear(x, 1, 2)
        p.clear(x, 2, 2)
        p.clear(x, 1, 3)
        p.clear(x, 2, 3)
        p.clear(x, 1, 4)
        p.clear(x, 2, 4)
    for x in (3, 7):
        for y in (1, 2, 3):
            p.set(x, y, 2, log("oak"), FRAME)
    for x in (3, 4, 5, 6, 7):
        p.set(x, 3, 3, log("oak", axis="x"), FRAME)
    for z in range(2, 9):
        p.set(5, 1, z, RAIL, FITTING)
    p.set(3, 1, 1, TORCH, FITTING)
    p.set(7, 1, 1, TORCH, FITTING)
    p.set(3, 4, 3, TORCH, FITTING)
    p.set(7, 4, 3, TORCH, FITTING)
    p.poi("shaft", "work", (3, 1, 6), 6)
    p.link((5, 0, 8), "south")
    p.anchor = [5, 0, 8]
    return p


def smithy() -> Plan:
    p = Plan("solaris:smithy", (13, 6, 11))
    cabin(
        p, 0, 0, 12, 10, 1, 4, 5,
        COBBLE, STONE_BRICKS, STONE_BRICKS,
        door_spec=(6, 10, "south", "oak"), win_y=3, corner_mat=log("oak"),
    )
    p.set(2, 2, 1, FURNACE, FITTING)
    p.set(4, 2, 1, BLAST_FURNACE, FITTING)
    p.set(6, 2, 1, ANVIL, FITTING)
    p.set(8, 2, 1, SMITHING_TABLE, FITTING)
    p.set(10, 2, 1, chest("north"), FITTING)
    p.entity((10, 2, 1), "empty_container")
    p.set(11, 2, 1, CAULDRON, FITTING)
    p.set(1, 2, 1, TORCH, FITTING)
    p.set(1, 4, 5, M("lantern", hanging="false", waterlogged="false"), FITTING)
    p.poi("forge", "work", (6, 2, 6), 4)
    p.link((6, 0, 10), "south")
    p.anchor = [6, 0, 10]
    return p


def barracks() -> Plan:
    p = Plan("solaris:barracks", (17, 7, 13))
    cabin(
        p, 0, 0, 16, 12, 1, 5, 6,
        STONE_BRICKS, planks("oak"), planks("oak"),
        door_spec=(8, 12, "south", "oak"), win_y=4, corner_mat=log("oak"),
        window_mat=bars("iron"),
    )
    p.add_door(4, 12, "south", 2, kind="oak")
    for x in (2, 4, 6, 8):
        p.add_bed(x, 9, "north", 2)
    for x in (2, 4, 6, 8):
        p.add_bed(x, 4, "north", 2)
    for z in range(2, 8):
        p.set(15, 2, z, barrel(), FITTING)
        p.entity((15, 2, z), "empty_container")
    p.set(14, 2, 2, chest("north"), FITTING)
    p.entity((14, 2, 2), "empty_container")
    p.set(14, 2, 3, chest("north"), FITTING)
    p.entity((14, 2, 3), "empty_container")
    p.set(3, 2, 6, M("lantern", hanging="false", waterlogged="false"), FITTING)
    p.set(13, 2, 6, M("lantern", hanging="false", waterlogged="false"), FITTING)
    p.poi("armory", "work", (13, 2, 9), 8)
    p.poi("post", "guard", (5, 2, 6), 8)
    p.link((8, 0, 12), "south")
    p.anchor = [8, 0, 12]
    return p


def watch_post() -> Plan:
    p = Plan("solaris:watch_post", (7, 9, 7))
    cabin(
        p, 0, 0, 6, 6, 1, 6, 7,
        COBBLE, planks("oak"), planks("oak"),
        door_spec=(3, 6, "south", "oak"), win_y=4, corner_mat=log("oak"),
        window_mat=bars("iron"),
    )
    p.clear(1, 7, 1)
    for x, z in ring(0, 0, 6, 6):
        if (x + z) % 2 == 0:
            p.set(x, 8, z, COBBLE, ROOF)
    p.set(3, 8, 3, TORCH, FITTING)
    p.add_ladder(1, 1, 2, 7, "east")
    p.poi("lookout", "guard", (4, 2, 4), 4)
    p.link((3, 0, 6), "south")
    p.anchor = [3, 0, 6]
    return p


def palisade_gate() -> Plan:
    p = Plan("solaris:palisade_gate", (11, 6, 5))
    for x in range(11):
        for z in range(1, 4):
            p.set(x, 0, z, DIRT_PATH, FOUNDATION)
    for x in range(11):
        if x in (4, 5, 6):
            continue
        for y in (1, 2, 3, 4):
            p.set(x, y, 2, log("oak"), FRAME)
    for x in (4, 5, 6):
        p.set(x, 4, 2, log("oak", axis="x"), FRAME)
    for x in (2, 8):
        p.set(x, 5, 2, TORCH, FITTING)
    p.poi("gate", "guard", (5, 1, 2), 4)
    for at, facing in (
        ((0, 0, 2), "west"),
        ((10, 0, 2), "east"),
        ((5, 0, 0), "north"),
        ((5, 0, 4), "south"),
    ):
        p.link(at, facing)
    p.anchor = [5, 0, 2]
    return p


def stone_wall() -> Plan:
    p = Plan("solaris:stone_wall", (13, 6, 5))
    p.box(0, 0, 1, 12, 0, 3, COBBLE, FOUNDATION)
    p.box(0, 1, 1, 12, 3, 3, STONE_BRICKS, FRAME)
    p.box(0, 4, 1, 12, 4, 3, STONE_BRICKS, ROOF)
    for x in range(13):
        if x % 2 == 0:
            p.set(x, 5, 1, STONE_BRICKS, ROOF)
        else:
            p.set(x, 5, 3, STONE_BRICKS, ROOF)
    p.set(1, 2, 4, stairs("stone_brick", "north"), FITTING)
    p.set(2, 3, 4, stairs("stone_brick", "north"), FITTING)
    p.set(0, 1, 4, TORCH, FITTING)
    p.set(12, 1, 4, TORCH, FITTING)
    p.poi("rampart", "guard", (6, 5, 2), 2)
    p.link((0, 0, 2), "west")
    p.link((12, 0, 2), "east")
    p.anchor = [6, 0, 2]
    return p


def stone_tower() -> Plan:
    p = Plan("solaris:stone_tower", (9, 11, 9))
    cabin(
        p, 0, 0, 8, 8, 1, 8, 9,
        COBBLE, STONE_BRICKS, STONE_BRICKS,
        door_spec=(4, 8, "south", "oak"), win_y=5, corner_mat=log("oak"),
        window_mat=bars("iron"),
    )
    p.clear(1, 9, 1)
    merlons(p, 0, 0, 8, 8, 10, STONE_BRICKS)
    p.set(4, 10, 4, TORCH, FITTING)
    p.add_ladder(1, 1, 2, 8, "east")
    p.poi("watch", "guard", (4, 2, 4), 4)
    p.link((4, 0, 8), "south")
    p.anchor = [4, 0, 8]
    return p


def manor_hall() -> Plan:
    p = Plan("solaris:manor_hall", (21, 9, 15))
    cabin(
        p, 0, 0, 20, 14, 1, 6, 7,
        planks("oak"), STONE_BRICKS, planks("oak"),
        door_spec=(10, 14, "south", "oak"), win_y=4, corner_mat=log("oak"),
    )
    p.add_door(5, 14, "south", 2, kind="oak")
    # Hearth: stone surround with a campfire.
    p.box(9, 1, 1, 11, 1, 1, STONE_BRICKS, FOUNDATION)
    p.set(10, 2, 1, CAMPFIRE, FITTING)
    p.box(8, 2, 1, 8, 3, 1, STONE_BRICKS, FRAME)
    p.box(12, 2, 1, 12, 3, 1, STONE_BRICKS, FRAME)
    for x in range(4, 17):
        p.set(x, 2, 7, slab("oak"), FITTING)
    for x in (5, 8, 11, 14):
        p.set(x, 2, 6, stairs("oak", "south"), FITTING)
        p.set(x, 2, 8, stairs("oak", "north"), FITTING)
    for x in (3, 17):
        for z in (4, 7, 10):
            p.set(x, 5, z, WHITE_WOOL, FITTING)
    p.set(2, 2, 12, chest("north"), FITTING)
    p.entity((2, 2, 12), "empty_container")
    p.set(18, 2, 12, M("lantern", hanging="false", waterlogged="false"), FITTING)
    p.poi("hall", "meeting", (10, 2, 4), 8)
    p.poi("steward", "work", (16, 2, 4), 4)
    p.link((10, 0, 14), "south")
    p.anchor = [10, 0, 14]
    return p


def keep_body() -> Plan:
    p = Plan("solaris:keep", (17, 13, 17))
    cabin(
        p, 0, 0, 16, 16, 1, 6, 7,
        STONE_BRICKS, STONE_BRICKS, STONE_BRICKS,
        door_spec=(8, 16, "south", "oak"), win_y=4, corner_mat=log("oak"),
        window_mat=bars("iron"),
    )
    # Mid floor, upper storey walls, top platform and battlements.
    p.box(0, 7, 0, 16, 7, 16, STONE_BRICKS, ROOF)
    for y in (8, 9, 10):
        for x, z in ring(0, 0, 16, 16):
            p.set(x, y, z, STONE_BRICKS, ROOF)
    for x, z in ((0, 0), (0, 16), (16, 0), (16, 16)):
        for y in (8, 9, 10):
            p.set(x, y, z, log("oak"), ROOF)
    for x, y, z in window_cells(0, 0, 16, 16, 9):
        p.set(x, y, z, bars("iron"), ROOF)
    p.add_door(8, 16, "south", 8, kind="oak")
    p.clear(1, 7, 1)
    p.box(0, 11, 0, 16, 11, 16, STONE_BRICKS, ROOF)
    p.clear(1, 11, 1)
    merlons(p, 0, 0, 16, 16, 12, COBBLE)
    p.add_ladder(1, 1, 2, 11, "east")
    p.set(13, 2, 3, chest("north"), FITTING)
    p.entity((13, 2, 3), "empty_container")
    p.set(13, 2, 5, barrel(), FITTING)
    p.entity((13, 2, 5), "empty_container")
    p.set(11, 2, 3, CRAFTING_TABLE, FITTING)
    p.set(1, 2, 14, TORCH, FITTING)
    p.set(15, 8, 15, TORCH, FITTING)
    p.poi("garrison", "guard", (8, 2, 8), 12)
    p.poi("armory", "work", (13, 2, 8), 4)
    for at, facing in (((8, 0, 16), "south"), ((0, 0, 8), "west")):
        p.link(at, facing)
    p.anchor = [8, 0, 16]
    return p


def keep_ruined() -> Plan:
    intact = keep_body()

    def removed(pos: tuple[int, int, int]) -> bool:
        x, y, z = pos
        if x == 8 and z == 16:
            return False  # keep the entrance doorway
        if y >= 11:
            return (x + z) % 5 == 0 or x + z >= 30
        if y == 7:
            return x + z >= 24
        return x >= 13 and z >= 13 and (x + z) % 2 == 0

    ruined = intact.damage(removed)
    ruined.id = "solaris:keep.ruined"
    ruined.variant_of = "solaris:keep"
    # Debris piles stand in the collapsed corner, in cells the intact keep
    # leaves empty, so the restoration set is exactly intact minus the ruin.
    for x, y, z in ((14, 2, 14), (15, 2, 13), (13, 2, 12), (12, 2, 14), (13, 3, 14)):
        ruined.set(x, y, z, M("gravel"), FITTING)
    ruined.restore_from(intact)
    return ruined


def town_hall() -> Plan:
    p = Plan("solaris:town_hall", (19, 9, 13))
    cabin(
        p, 0, 0, 18, 12, 1, 5, 6,
        planks("oak"), STONE_BRICKS, planks("oak"),
        door_spec=(9, 12, "south", "oak"), win_y=4, corner_mat=log("oak"),
    )
    # Bell tower above the roof.
    for y in (7, 8):
        for x, z in ring(7, 4, 11, 8):
            p.set(x, y, z, STONE_BRICKS, ROOF)
    p.set(9, 7, 6, BELL, FITTING)
    for x in (2, 4, 6):
        p.set(x, 2, 1, BOOKSHELF, FITTING)
    p.set(8, 2, 1, lectern("south"), FITTING)
    for x in range(10, 15):
        p.set(x, 2, 8, slab("oak"), FITTING)
    for x in (11, 13):
        p.set(x, 2, 7, stairs("oak", "south"), FITTING)
        p.set(x, 2, 9, stairs("oak", "north"), FITTING)
    p.set(15, 2, 1, chest("north"), FITTING)
    p.entity((15, 2, 1), "empty_container")
    p.set(2, 2, 10, M("lantern", hanging="false", waterlogged="false"), FITTING)
    p.set(16, 2, 10, M("lantern", hanging="false", waterlogged="false"), FITTING)
    p.set(3, 5, 1, RED_WOOL, FITTING)
    p.set(15, 5, 1, RED_WOOL, FITTING)
    p.poi("council", "meeting", (9, 2, 4), 12)
    p.poi("clerk", "work", (14, 2, 4), 4)
    p.link((9, 0, 12), "south")
    p.anchor = [9, 0, 12]
    return p


def library() -> Plan:
    p = Plan("solaris:library", (15, 7, 13))
    cabin(
        p, 0, 0, 14, 12, 1, 5, 6,
        planks("oak"), STONE_BRICKS, planks("oak"),
        door_spec=(7, 12, "south", "oak"), win_y=4, corner_mat=log("oak"),
    )
    for x in range(1, 14):
        for y in (2, 3):
            p.set(x, y, 1, BOOKSHELF, FITTING)
            if x != 7:
                p.set(x, y, 11, BOOKSHELF, FITTING)
    for x in range(3, 8):
        p.set(x, 2, 6, slab("oak"), FITTING)
    for x in (4, 6):
        p.set(x, 2, 5, stairs("oak", "south"), FITTING)
        p.set(x, 2, 7, stairs("oak", "north"), FITTING)
    p.set(2, 2, 4, lectern("east"), FITTING)
    p.set(10, 2, 4, lectern("west"), FITTING)
    p.set(12, 2, 9, chest("north"), FITTING)
    p.entity((12, 2, 9), "empty_container")
    p.set(1, 2, 9, M("lantern", hanging="false", waterlogged="false"), FITTING)
    p.set(7, 2, 10, M("lantern", hanging="false", waterlogged="false"), FITTING)
    p.poi("archive", "work", (5, 2, 9), 6)
    p.poi("reading", "meeting", (10, 2, 9), 4)
    p.link((7, 0, 12), "south")
    p.anchor = [7, 0, 12]
    return p


def watchtower() -> Plan:
    p = Plan("solaris:watchtower", (9, 11, 9))
    cabin(
        p, 0, 0, 8, 8, 1, 8, 9,
        COBBLE, planks("spruce"), planks("spruce"),
        door_spec=(4, 8, "south", "spruce"), win_y=4, corner_mat=log("dark_oak"),
        base_course=COBBLE,
    )
    for y in (6, 7):
        for x, z in ring(0, 0, 8, 8):
            p.set(x, y, z, planks("spruce"), FRAME)
    p.clear(1, 9, 1)
    for x, z in ring(0, 0, 8, 8):
        p.set(x, 10, z, fence("spruce"), ROOF)
    p.set(4, 10, 4, M("lantern", hanging="false", waterlogged="false"), FITTING)
    p.add_ladder(1, 1, 2, 8, "east")
    p.poi("spire", "guard", (4, 2, 4), 4)
    p.link((4, 0, 8), "south")
    p.anchor = [4, 0, 8]
    return p


CATALOG = (
    house_small,
    house_large,
    plaza_well,
    market,
    warehouse,
    farm,
    sawmill,
    pen,
    fishing_pier,
    mine_entrance,
    smithy,
    barracks,
    watch_post,
    palisade_gate,
    stone_wall,
    stone_tower,
    manor_hall,
    keep_body,
    keep_ruined,
    town_hall,
    library,
    watchtower,
)


# --- emission -------------------------------------------------------------


def inline_table(properties: dict[str, str]) -> str:
    if not properties:
        return "{}"
    body = ", ".join(f'{key} = "{value}"' for key, value in sorted(properties.items()))
    return "{ " + body + " }"


def cell_line(pos: tuple[int, int, int], pal: int) -> str:
    return f'  {{ x = {pos[0]}, y = {pos[1]}, z = {pos[2]}, palette = {pal} }},'


def render(plan: Plan) -> str:
    palette = plan.palette()
    index_of = {(block, tuple(sorted(props.items()))): i for i, (block, props) in enumerate(palette)}
    lines: list[str] = []
    lines.append(f'id = "{plan.id}"')
    lines.append("revision = 1")
    if plan.variant_of is not None:
        lines.append(f'variant_of = "{plan.variant_of}"')
    lines.append("")
    lines.append("[footprint]")
    lines.append(f"size = [{plan.size[0]}, {plan.size[1]}, {plan.size[2]}]")
    lines.append(f"anchor = [{plan.anchor[0]}, {plan.anchor[1]}, {plan.anchor[2]}]")
    lines.append("")
    for i, (block, props) in enumerate(palette):
        lines.append("[[palette]]")
        lines.append(f"index = {i}")
        lines.append(f'block = "{block}"')
        lines.append(f"properties = {inline_table(props)}")
        lines.append("")
    for pos in sorted(plan.cells):
        spec, _ = plan.cells[pos]
        lines.append("[[blocks]]")
        lines.append(f"x = {pos[0]}")
        lines.append(f"y = {pos[1]}")
        lines.append(f"z = {pos[2]}")
        lines.append(f"palette = {index_of[(spec[0], tuple(sorted(spec[1].items())))]}")
        lines.append("")
    for name, kind, at, capacity in sorted(plan.pois):
        lines.append("[[poi]]")
        lines.append(f'id = "{name}"')
        lines.append(f'kind = "{kind}"')
        lines.append(f"at = [{at[0]}, {at[1]}, {at[2]}]")
        lines.append(f"capacity = {capacity}")
        lines.append("")
    for at, facing in sorted(plan.connections, key=lambda entry: (entry[0], entry[1])):
        lines.append("[[street_connection]]")
        lines.append(f"at = [{at[0]}, {at[1]}, {at[2]}]")
        lines.append(f'facing = "{facing}"')
        lines.append("")
    for at, kind in sorted(plan.entities, key=lambda entry: (entry[0], entry[1])):
        lines.append("[[block_entity]]")
        lines.append(f"at = [{at[0]}, {at[1]}, {at[2]}]")
        lines.append(f'kind = "{kind}"')
        lines.append("")
    stages = plan.stage_cells()
    for stage in STAGE_ORDER:
        if stage not in stages:
            continue
        lines.append("[[stage]]")
        lines.append(f'id = "{stage}"')
        lines.append("blocks = [")
        for pos in sorted(stages[stage]):
            spec = stages[stage][pos]
            lines.append(cell_line(pos, index_of[(spec[0], tuple(sorted(spec[1].items())))]))
        lines.append("]")
        lines.append("")
    for name in sorted(plan.restoration):
        lines.append("[[restoration_stage]]")
        lines.append(f'id = "{name}"')
        lines.append("blocks = [")
        for pos in sorted(plan.restoration[name]):
            spec = plan.restoration[name][pos]
            lines.append(cell_line(pos, index_of[(spec[0], tuple(sorted(spec[1].items())))]))
        lines.append("]")
        lines.append("")
    return "\n".join(lines)


def build_catalog() -> dict[str, str]:
    files: dict[str, str] = {}
    for builder in CATALOG:
        plan = builder()
        plan.validate()
        files[f"{plan.id.split(':', 1)[1]}.toml"] = render(plan)
    return files


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--write", action="store_true", help="write the catalog")
    group.add_argument("--check", action="store_true", help="verify the catalog is current")
    args = parser.parse_args()

    root = Path(__file__).resolve().parent.parent
    directory = root / "solaris-settlements" / "structures"
    catalog = build_catalog()

    if args.write:
        directory.mkdir(parents=True, exist_ok=True)
        for name, text in sorted(catalog.items()):
            (directory / name).write_text(text, encoding="utf-8")
            print(f"wrote {name}")
        return 0

    committed = sorted(path.name for path in directory.glob("*.toml"))
    expected = sorted(catalog)
    problems: list[str] = []
    if committed != expected:
        missing = sorted(set(expected) - set(committed))
        extra = sorted(set(committed) - set(expected))
        if missing:
            problems.append(f"missing: {', '.join(missing)}")
        if extra:
            problems.append(f"unexpected: {', '.join(extra)}")
    for name, text in sorted(catalog.items()):
        path = directory / name
        if not path.is_file():
            continue
        if path.read_text(encoding="utf-8") != text:
            problems.append(f"stale: {name}")
    if problems:
        print("catalog is not current:", file=sys.stderr)
        for problem in problems:
            print(f"  {problem}", file=sys.stderr)
        print("run `python3 tools/gen_structures.py --write`", file=sys.stderr)
        return 1
    print(f"catalog is current: {len(catalog)} blueprints")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
