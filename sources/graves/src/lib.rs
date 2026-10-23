//! Graves for the Solaris standard pack.
//!
//! The server's own death path does the custody: when a deployment declares the
//! `death_graves` capability, one death's drops move into a barrel at the death
//! spot in the same tick, and `player.grave_created` tells this package where it
//! is. This component owns the product policy around that container: it protects
//! the grave for its owner through the native zone owner, tells the owner the
//! coordinates, and after a configured time takes the protection away so anyone
//! may open it.
//!
//! Nothing here holds items. The container is the only custody, the server
//! journals it with the rest of the world, and a component restart loses nothing
//! but the pending schedule, which the stored table restores.

use std::collections::BTreeMap;

use solaris_plugin_sdk::events::{Event, ZoneCommandOutcome};
use solaris_plugin_sdk::{
    commands, export_plugin, message_player, schedule_timer, storage_cas, storage_get, types,
    Command, Config, EventContext, Failure, InitContext, Plugin, StorageCasOutcome,
    StorageGetOutcome,
};
use toml::{Table, Value};

const STORAGE_KEY: &str = "graves-v1";
const SWEEP_TIMER: &str = "grave-sweep";
/// How often the sweep re-checks the deadlines. One second keeps the public
/// transition well inside a player's patience while staying four orders of
/// magnitude below the contract's timer budget.
const SWEEP_TICKS: u64 = 20;
const MAX_GRAVES: usize = 128;
const MAX_PUBLIC_AFTER_SECONDS: u64 = 604_800;
const TICKS_PER_SECOND: u64 = 20;

#[derive(Clone, Copy)]
struct Settings {
    public_after_seconds: u64,
}

impl Settings {
    fn parse(config: &Config) -> Result<Self, Failure> {
        let table = config
            .toml()
            .and_then(|value| value.as_table().cloned())
            .ok_or(Failure::Invalid)?;
        Ok(Self {
            public_after_seconds: bounded_u64(
                &table,
                "public_after_seconds",
                1,
                MAX_PUBLIC_AFTER_SECONDS,
            )?,
        })
    }

    fn public_after_ticks(self) -> u64 {
        self.public_after_seconds.saturating_mul(TICKS_PER_SECOND)
    }

    fn minutes(self) -> u64 {
        self.public_after_seconds.div_ceil(60)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            public_after_seconds: 300,
        }
    }
}

#[derive(Clone)]
struct Grave {
    zone: String,
    dimension: String,
    x: i32,
    y: i32,
    z: i32,
    owner: String,
    public_at: u64,
    /// Whether the native zone owner still holds this grave's protection. A
    /// refused upsert leaves it false, so the sweep does not ask for a removal it
    /// never created.
    protected: bool,
}

struct PendingSave {
    request: String,
}

#[derive(Default)]
struct Graves {
    settings: Option<Settings>,
    graves: BTreeMap<String, Grave>,
    storage_version: Option<u64>,
    loaded: bool,
    sweep_armed: bool,
    dirty: bool,
    pending: Option<PendingSave>,
    sequence: u64,
    /// The greatest tick this instance has observed, so a load answered after a
    /// restart can flip deadlines the sweep timer has not reached yet.
    tick: u64,
}

impl Graves {
    fn settings(&self) -> Settings {
        self.settings.unwrap_or_default()
    }

    fn next_request(&mut self, prefix: &str) -> String {
        self.sequence += 1;
        format!("{prefix}-{}", self.sequence)
    }

    /// The durable image: the sweep deadline is an absolute tick, so a restart
    /// neither resets nor shortens a grave's private window.
    fn encode(&self) -> String {
        let rows = self
            .graves
            .values()
            .map(|grave| {
                format!(
                    "{},{},{},{},{},{},{}",
                    grave.dimension,
                    grave.x,
                    grave.y,
                    grave.z,
                    grave.owner,
                    grave.public_at,
                    if grave.protected { "p" } else { "u" }
                )
            })
            .collect::<Vec<_>>()
            .join(";");
        format!("v1|{rows}")
    }

    fn decode(&self, value: Option<&str>) -> Option<BTreeMap<String, Grave>> {
        let Some(value) = value else {
            return Some(BTreeMap::new());
        };
        let rows = value.strip_prefix("v1|")?;
        let mut graves = BTreeMap::new();
        for row in rows.split(';').filter(|row| !row.is_empty()) {
            let mut fields = row.split(',');
            let dimension = fields.next()?;
            if !valid_dimension(dimension) {
                return None;
            }
            let x = parse_signed(fields.next()?)?;
            let y = parse_signed(fields.next()?)?;
            let z = parse_signed(fields.next()?)?;
            let owner = normalize_uuid(fields.next()?)?;
            let public_at = parse_unsigned(fields.next()?)?;
            let protected = match fields.next()? {
                "p" => true,
                "u" => false,
                _ => return None,
            };
            if fields.next().is_some() {
                return None;
            }
            let zone = zone_id(dimension, x, y, z);
            graves.insert(
                zone.clone(),
                Grave {
                    zone,
                    dimension: dimension.to_owned(),
                    x,
                    y,
                    z,
                    owner,
                    public_at,
                    protected,
                },
            );
            if graves.len() > MAX_GRAVES {
                return None;
            }
        }
        Some(graves)
    }

    fn upsert(&self, grave: &Grave) -> Command {
        commands::Command::UpsertProtectedZone(commands::UpsertProtectedZone {
            zone: grave.zone.clone(),
            dimension: grave.dimension.clone(),
            allowed_actor_uuids: vec![grave.owner.clone()],
            public_use: false,
            // The container cell plus the cell beneath it. A hopper can only pull
            // out of a container directly above it, so guarding `y - 1` closes the
            // one redstone route that reaches the items without opening the grave:
            // nobody but the owner may break that block or place a hopper there.
            minimum: types::Position {
                x: f64::from(grave.x),
                y: f64::from(grave.y) - 1.0,
                z: f64::from(grave.z),
            },
            maximum: types::Position {
                x: f64::from(grave.x),
                y: f64::from(grave.y),
                z: f64::from(grave.z),
            },
        })
    }

    fn remove(&self, grave: &Grave) -> Command {
        commands::Command::RemoveZone(commands::RemoveZone {
            zone: grave.zone.clone(),
        })
    }
}

impl Plugin for Graves {
    fn configure(
        &mut self,
        config: &Config,
    ) -> Result<Option<solaris_plugin_sdk::StartupContribution>, Failure> {
        Settings::parse(config)?;
        Ok(None)
    }

    fn init(&mut self, config: &Config, _context: &InitContext) -> Result<Vec<Command>, Failure> {
        *self = Self {
            settings: Some(Settings::parse(config)?),
            ..Self::default()
        };
        let request = self.next_request("load");
        Ok(vec![storage_get(&request, STORAGE_KEY)])
    }

    fn on_events(
        &mut self,
        context: &EventContext,
        events: &[Event],
    ) -> Result<Vec<Command>, Failure> {
        let mut commands = Vec::new();
        self.tick = self.tick.max(context.tick);
        for event in events {
            match event {
                Event::PlayerGraveCreated(grave) => {
                    commands.extend(self.grave_created(context, grave));
                }
                Event::StorageGetAnswered(answered) => {
                    commands.extend(self.storage_get(answered.request.as_str(), &answered.outcome));
                }
                Event::StorageCasAnswered(answered) => {
                    commands.extend(self.storage_cas(answered.request.as_str(), &answered.outcome));
                }
                Event::TimerFired(timer) if timer.timer_id == SWEEP_TIMER => {
                    self.sweep_armed = false;
                    commands.extend(self.sweep_and_save(context.tick));
                }
                Event::ZoneCommandAnswered(answered) => {
                    if let Some(grave) = self.graves.get_mut(&answered.zone) {
                        grave.protected = matches!(answered.outcome, ZoneCommandOutcome::Applied);
                    }
                }
                Event::PlayerBlockBroken(broken) => {
                    commands.extend(self.broken(
                        broken.dimension.as_str(),
                        broken.at.x,
                        broken.at.y,
                        broken.at.z,
                    ));
                }
                Event::CommandBatchRejected => {
                    // The batch that carried the save never reached the server.
                    // Keep the in-memory table and let the next change save it.
                    self.pending = None;
                    self.dirty = true;
                }
                _ => {}
            }
        }
        Ok(commands)
    }
}

impl Graves {
    fn grave_created(
        &mut self,
        context: &EventContext,
        grave: &solaris_plugin_sdk::PlayerGraveCreated,
    ) -> Vec<Command> {
        let settings = self.settings();
        let zone = zone_id(&grave.dimension, grave.at.x, grave.at.y, grave.at.z);
        if self.graves.contains_key(&zone) {
            return Vec::new();
        }
        let mut commands = Vec::new();
        if self.graves.len() >= MAX_GRAVES {
            // The oldest grave loses its protection early rather than the newest
            // losing it immediately: a full table must never leave a fresh grave
            // open to everyone.
            if let Some(oldest) = self
                .graves
                .values()
                .min_by_key(|grave| grave.public_at)
                .cloned()
            {
                self.graves.remove(&oldest.zone);
                if oldest.protected {
                    commands.push(self.remove(&oldest));
                }
            }
        }
        let record = Grave {
            zone: zone.clone(),
            dimension: grave.dimension.clone(),
            x: grave.at.x,
            y: grave.at.y,
            z: grave.at.z,
            owner: grave.player.clone(),
            public_at: context.tick.saturating_add(settings.public_after_ticks()),
            protected: true,
        };
        commands.push(self.upsert(&record));
        commands.push(message_player(
            &record.owner,
            format!(
                "Your grave is at {}, {}, {} in {}. Only you may open it for {} minutes; after that it is public.",
                record.x,
                record.y,
                record.z,
                record.dimension,
                settings.minutes(),
            ),
        ));
        if grave.spilled > 0 {
            commands.push(message_player(
                &record.owner,
                format!(
                    "{} extra stacks did not fit in the grave and lie beside it.",
                    grave.spilled
                ),
            ));
        }
        self.graves.insert(zone, record);
        if !self.sweep_armed {
            self.sweep_armed = true;
            commands.push(schedule_timer(SWEEP_TIMER, SWEEP_TICKS));
        }
        commands.extend(self.save());
        commands
    }

    fn sweep(&mut self, tick: u64) -> Vec<Command> {
        if !self.loaded {
            return Vec::new();
        }
        let mut commands = Vec::new();
        let mut changed = false;
        let due: Vec<Grave> = self
            .graves
            .values()
            .filter(|grave| grave.protected && grave.public_at <= tick)
            .cloned()
            .collect();
        for mut grave in due {
            grave.protected = false;
            commands.push(self.remove(&grave));
            commands.push(message_player(
                &grave.owner,
                format!(
                    "Your grave at {}, {}, {} in {} is public now.",
                    grave.x, grave.y, grave.z, grave.dimension
                ),
            ));
            self.graves.insert(grave.zone.clone(), grave);
            changed = true;
        }
        if self.graves.values().any(|grave| grave.protected) {
            self.sweep_armed = true;
            commands.push(schedule_timer(SWEEP_TIMER, SWEEP_TICKS));
        }
        if changed {
            self.dirty = true;
        }
        commands
    }

    /// Sweep and persist, so the timer callback, the load answer and the grave
    /// path all write the table exactly once per changed batch.
    fn sweep_and_save(&mut self, tick: u64) -> Vec<Command> {
        let mut commands = self.sweep(tick);
        if std::mem::take(&mut self.dirty) {
            commands.extend(self.save());
        }
        commands
    }

    fn broken(&mut self, dimension: &str, x: i32, y: i32, z: i32) -> Vec<Command> {
        let zone = zone_id(dimension, x, y, z);
        let Some(grave) = self.graves.remove(&zone) else {
            return Vec::new();
        };
        let mut commands = Vec::new();
        if grave.protected {
            commands.push(self.remove(&grave));
        }
        commands.extend(self.save());
        commands
    }

    fn storage_get(&mut self, request: &str, outcome: &StorageGetOutcome) -> Vec<Command> {
        let StorageGetOutcome::Read(record) = outcome else {
            self.loaded = false;
            return Vec::new();
        };
        let Some(mut graves) = self.decode(record.value.as_deref()) else {
            // A table this build cannot read is not silently replaced: the owner
            // repairs the key, exactly as the audit package does with its history.
            self.loaded = false;
            return Vec::new();
        };
        let _ = request;
        // A death can arrive before this answer. Its zone is already protected in
        // the world, so the record the load does not know must survive it: local
        // state wins over the stored table for zones only it has.
        for grave in self.graves.values() {
            graves
                .entry(grave.zone.clone())
                .or_insert_with(|| grave.clone());
        }
        self.graves = graves;
        self.storage_version = record.version;
        self.loaded = true;
        self.pending = None;
        // `sweep` arms the next check itself when any grave is still private.
        self.sweep_and_save(self.tick)
    }

    fn storage_cas(&mut self, request: &str, outcome: &StorageCasOutcome) -> Vec<Command> {
        let Some(pending) = self.pending.take() else {
            return Vec::new();
        };
        if pending.request != request {
            self.pending = Some(pending);
            return Vec::new();
        }
        let StorageCasOutcome::Committed(version) = outcome else {
            // A refused swap means another write won or the revision moved:
            // reload and re-apply this table on top of what the key holds now.
            self.loaded = false;
            let request = self.next_request("reload");
            return vec![storage_get(&request, STORAGE_KEY)];
        };
        self.storage_version = Some(*version);
        if std::mem::take(&mut self.dirty) {
            self.save()
        } else {
            Vec::new()
        }
    }

    fn save(&mut self) -> Vec<Command> {
        if !self.loaded || self.pending.is_some() {
            self.dirty = true;
            return Vec::new();
        }
        let request = self.next_request("save");
        let value = self.encode();
        let expected = self.storage_version;
        self.pending = Some(PendingSave {
            request: request.clone(),
        });
        vec![storage_cas(&request, STORAGE_KEY, expected, &value)]
    }
}

fn zone_id(dimension: &str, x: i32, y: i32, z: i32) -> String {
    // A stable, bounded id: the coordinates plus a short digest of the dimension,
    // so the same cell in two dimensions never shares one protection record.
    format!("grave-{x}-{y}-{z}-{:04x}", dimension_hash(dimension))
}

fn dimension_hash(dimension: &str) -> u16 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in dimension.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    (hash & 0xffff) as u16
}

fn bounded_u64(table: &Table, key: &str, minimum: u64, maximum: u64) -> Result<u64, Failure> {
    table
        .get(key)
        .and_then(Value::as_integer)
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or(Failure::Invalid)
}

fn valid_dimension(value: &str) -> bool {
    let Some((namespace, path)) = value.split_once(':') else {
        return false;
    };
    !namespace.is_empty()
        && !path.is_empty()
        && namespace
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && path.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-' | b'.' | b'/')
        })
}

fn normalize_uuid(value: &str) -> Option<String> {
    let value = value.trim();
    if value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        let mut out = String::with_capacity(36);
        for (index, chunk) in value.as_bytes().chunks(4).enumerate() {
            if index > 0 {
                out.push('-');
            }
            out.push_str(
                std::str::from_utf8(chunk)
                    .ok()?
                    .to_ascii_lowercase()
                    .as_str(),
            );
        }
        return Some(out);
    }
    if value.len() == 36
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || matches!(byte, b'-' | b':'))
    {
        return Some(value.replace(':', "-").to_ascii_lowercase());
    }
    None
}

fn parse_unsigned(value: &str) -> Option<u64> {
    value.parse().ok()
}

fn parse_signed(value: &str) -> Option<i32> {
    value.parse().ok()
}

export_plugin!(Graves);
