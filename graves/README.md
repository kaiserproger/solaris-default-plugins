# Graves

Death graves for Solaris. When a player dies with `keepInventory` off, the
server's own death path takes the drops into a barrel at the death spot in the
same tick, and this package protects that barrel for its owner.

The package declares the `death_graves` capability, which is the deployment
surface: the native death path creates a grave only when a deployed package
declares it. Removing the package returns deaths to vanilla drops.

## What the server does (core, not this package)

- The exact stacks the death produced move into a `minecraft:barrel` at the
  death cell, in one tick, journaled with the world. Barrel rather than chest so
  a grave can never pair into a double container.
- Safe placement: death cell, then the cell above, then a ring of radius 1–2 at
  level, one above and one below. A cell inside another package's protected zone
  is skipped, so a grave never appears in someone else's claim. Every candidate
  must stand on full-block collision ground that is not already a hopper (a slab
  or rail leaves room for a hopper minecart): redstone machinery ignores zones, so
  the barrel is never placed where a hopper or a minecart could reach it now or
  later, and mid-air deaths move it to the nearest grounded ring cell. When
  nothing is free, the drops stay as ordinary items.
- Stacks the 27 slots cannot hold are dropped beside the grave (`spilled`), so a
  full inventory never loses items.
- Experience is retained by the player instead of being reset: Solaris spawns no
  death XP orbs, so "no loss" means the bar survives.
- `keepInventory` wins: with the gamerule on, no grave is created at all.

## What this package does

- `player.grave_created` → a protected zone over the grave cell and the cell
  directly beneath it (the only cell a hopper can pull from), with the owner's
  UUID as its only allowed actor (server operators are always admitted by the
  native zone owner), a chat line with the coordinates and the private window,
  and a durable record so a restart neither resets the deadline nor forgets the
  protection.
- After `public_after_seconds` the zone is removed and anyone may open or break
  the grave. The owner is told.
- Breaking a grave removes its record and its zone; the container drops its
  contents as vanilla does.

The native zone owner guards players and environmental fire and blasts, not
redstone machinery, so this package closes the machinery routes in two ways:
placement never puts the barrel where an existing hopper could pull from it, and
the protected box includes the cell beneath the barrel so nobody but the owner
can break that block or place a hopper there afterwards. A piston aimed from the
side can still move the barrel out of its guarded cells; that is the zone
contract's documented scope, not a hole opened by this package.

## Configuration

```toml
public_after_seconds = 300
```

The private window, 1..604800 seconds. The sweep runs every second and flips
every grave whose deadline passed, so a restart resumes the same absolute
deadlines rather than restarting the clock.

## Capacity

At most 128 graves are tracked. A grave beyond that pushes the oldest record out
early: its protection is removed, which makes it public sooner rather than
leaving a newer grave unprotected.
