//! Durable bounded audit history for the Solaris standard plugin pack.
//!
//! Records use the Lua package's `actions-v1` key and `v1|` encoding. Runtime
//! writes are serialized through compare-and-swap so the in-memory history only
//! advances after the host reports a durable commit.

use std::collections::VecDeque;

use solaris_plugin_sdk::events::CommandInvoked;
use solaris_plugin_sdk::{
    export_plugin, message_session, storage_cas, storage_get, Command, Config, Event, EventContext,
    Failure, InitContext, Plugin, StorageCasOutcome, StorageGetOutcome,
};
use toml::{Table, Value};

const STORAGE_KEY: &str = "actions-v1";
const MAX_RECORDS: usize = 24;
const MAX_PENDING_RECORDS: usize = 32;
const MAX_LOOKUP_COUNT: usize = 20;
const MAX_RADIUS: i32 = 128;
const MAX_SINCE_TICKS: u64 = 630_720_000;

#[derive(Clone)]
struct Settings {
    dimension: String,
    maximum_records: usize,
    maximum_pending_records: usize,
    default_lookup_count: usize,
    maximum_lookup_count: usize,
    maximum_radius: i32,
}

impl Settings {
    fn parse(config: &Config) -> Result<Self, Failure> {
        let table = config
            .toml()
            .and_then(|value| value.as_table().cloned())
            .ok_or(Failure::Invalid)?;
        let dimension = required_string(&table, "dimension")?;
        if !valid_resource_id(dimension) {
            return Err(Failure::Invalid);
        }
        let maximum_records = bounded_usize(&table, "maximum_records", 1, MAX_RECORDS)?;
        let maximum_pending_records =
            bounded_usize(&table, "maximum_pending_records", 1, MAX_PENDING_RECORDS)?;
        let default_lookup_count =
            bounded_usize(&table, "default_lookup_count", 1, MAX_LOOKUP_COUNT)?;
        let maximum_lookup_count = bounded_usize(
            &table,
            "maximum_lookup_count",
            default_lookup_count,
            MAX_LOOKUP_COUNT,
        )?;
        let maximum_radius = bounded_i32(&table, "maximum_radius", 1, MAX_RADIUS)?;
        Ok(Self {
            dimension: dimension.to_owned(),
            maximum_records,
            maximum_pending_records,
            default_lookup_count,
            maximum_lookup_count,
            maximum_radius,
        })
    }

    fn decode(&self, value: Option<&str>) -> Option<VecDeque<Record>> {
        let Some(value) = value else {
            return Some(VecDeque::new());
        };
        if value == "v1|" {
            return Some(VecDeque::new());
        }
        let rows = value.strip_prefix("v1|")?;
        let mut records = VecDeque::new();
        for row in rows.split(';').filter(|row| !row.is_empty()) {
            let mut fields = row.split(',');
            let tick = parse_unsigned(fields.next()?)?;
            let kind = fields.next()?;
            let actor = normalize_uuid(fields.next()?)?;
            let x = parse_signed(fields.next()?)?;
            let y = parse_signed(fields.next()?)?;
            let z = parse_signed(fields.next()?)?;
            let detail = fields.next()?;
            if fields.next().is_some()
                || kind.is_empty()
                || !kind
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            {
                return None;
            }
            records.push_back(Record {
                tick,
                kind: kind.to_owned(),
                actor,
                x,
                y,
                z,
                detail: detail.to_owned(),
            });
            if records.len() > self.maximum_records {
                return None;
            }
        }
        Some(records)
    }

    fn encode(records: &VecDeque<Record>) -> String {
        let rows = records
            .iter()
            .map(|record| {
                format!(
                    "{},{},{},{},{},{},{}",
                    record.tick,
                    record.kind,
                    record.actor,
                    record.x,
                    record.y,
                    record.z,
                    record.detail
                )
            })
            .collect::<Vec<_>>();
        format!("v1|{}", rows.join(";"))
    }
}

#[derive(Clone)]
struct Record {
    tick: u64,
    kind: String,
    actor: String,
    x: i32,
    y: i32,
    z: i32,
    detail: String,
}

struct Pending {
    request: String,
    record: Record,
    records: VecDeque<Record>,
}

#[derive(Default)]
struct Audit {
    settings: Option<Settings>,
    records: VecDeque<Record>,
    queue: VecDeque<Record>,
    storage_version: Option<u64>,
    loaded: bool,
    pending: Option<Pending>,
    latest_tick: u64,
    sequence: u64,
}

impl Audit {
    fn settings(&self) -> &Settings {
        self.settings
            .as_ref()
            .expect("init establishes validated settings before events")
    }

    fn next_request(&mut self, prefix: &str) -> String {
        self.sequence += 1;
        format!("{prefix}-{}", self.sequence)
    }

    fn append(
        &mut self,
        player: &str,
        dimension: &str,
        kind: &str,
        detail: &str,
        x: i32,
        y: i32,
        z: i32,
    ) -> Vec<Command> {
        if dimension != self.settings().dimension
            || self.queue.len() >= self.settings().maximum_pending_records
        {
            return Vec::new();
        }
        let Some(actor) = normalize_uuid(player) else {
            return Vec::new();
        };
        self.queue.push_back(Record {
            tick: self.latest_tick,
            kind: kind.to_owned(),
            actor,
            x,
            y,
            z,
            detail: compact(detail),
        });
        self.process_queue()
    }

    fn process_queue(&mut self) -> Vec<Command> {
        if !self.loaded || self.pending.is_some() {
            return Vec::new();
        }
        let Some(record) = self.queue.pop_front() else {
            return Vec::new();
        };
        let mut records = self.records.clone();
        records.push_back(record.clone());
        while records.len() > self.settings().maximum_records {
            records.pop_front();
        }
        let revision = self
            .storage_version
            .map_or_else(|| "new".to_owned(), |version| version.to_string());
        let request = format!("append-v{revision}");
        let value = Settings::encode(&records);
        self.pending = Some(Pending {
            request: request.clone(),
            record,
            records,
        });
        vec![storage_cas(
            &request,
            STORAGE_KEY,
            self.storage_version,
            value,
        )]
    }

    fn command(&self, invoked: &CommandInvoked) -> Vec<Command> {
        if !invoked.operator {
            return vec![message_session(
                invoked.session,
                "Only an operator can query audit history.",
            )];
        }
        if !self.loaded {
            return vec![message_session(
                invoked.session,
                "Audit history is still loading.",
            )];
        }
        let words = invoked
            .raw_arguments
            .split_ascii_whitespace()
            .collect::<Vec<_>>();
        if words.first() == Some(&"rollback") {
            return vec![message_session(
                invoked.session,
                "Rollback unavailable: API 0.6 events omit exact prior block state and block-entity data.",
            )];
        }
        let mut mode = Lookup::All;
        let mut wanted = self.settings().default_lookup_count;
        match words.first().copied() {
            Some("actor") if words.get(1).is_some() => {
                let Some(actor) = normalize_uuid(words[1]) else {
                    return vec![message_session(
                        invoked.session,
                        "Usage: /audit actor <uuid> [count]",
                    )];
                };
                if words.len() > 3 {
                    return vec![message_session(
                        invoked.session,
                        "Usage: /audit actor <uuid> [count]",
                    )];
                }
                wanted = words
                    .get(2)
                    .and_then(|value| parse_lua_integer(value))
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(self.settings().default_lookup_count);
                mode = Lookup::Actor(actor);
            }
            Some("here") => {
                if words.len() > 3 {
                    return vec![message_session(
                        invoked.session,
                        "Usage: /audit here <radius> [count]",
                    )];
                }
                let radius = words
                    .get(1)
                    .and_then(|value| parse_lua_integer(value))
                    .unwrap_or(8);
                if !(0..=self.settings().maximum_radius as i64).contains(&radius) {
                    return vec![message_session(
                        invoked.session,
                        "Usage: /audit here <radius> [count]",
                    )];
                }
                wanted = words
                    .get(2)
                    .and_then(|value| parse_lua_integer(value))
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(self.settings().default_lookup_count);
                mode = Lookup::Here(radius as i32);
            }
            Some("since") => {
                if words.len() > 3 {
                    return vec![message_session(
                        invoked.session,
                        "Usage: /audit since <ticks> [count]",
                    )];
                }
                let ticks = words
                    .get(1)
                    .and_then(|value| parse_lua_integer(value))
                    .unwrap_or(-1);
                if !(0..=MAX_SINCE_TICKS as i64).contains(&ticks) {
                    return vec![message_session(
                        invoked.session,
                        "Usage: /audit since <ticks> [count]",
                    )];
                }
                wanted = words
                    .get(2)
                    .and_then(|value| parse_lua_integer(value))
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(self.settings().default_lookup_count);
                mode = Lookup::Since(ticks as u64);
            }
            Some(value) => {
                wanted = parse_lua_integer(value)
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or(0)
            }
            None => {}
        }
        if words.len() > 1 && !matches!(mode, Lookup::Actor(_) | Lookup::Here(_) | Lookup::Since(_))
        {
            wanted = 0;
        }
        if !(1..=self.settings().maximum_lookup_count).contains(&wanted) {
            return vec![message_session(
                invoked.session,
                format!("Count must be 1..{}.", self.settings().maximum_lookup_count),
            )];
        }
        let mut commands = Vec::new();
        for record in self.records.iter().rev() {
            if mode.matches(record, invoked, self.latest_tick) {
                commands.push(message_session(
                    invoked.session,
                    format!(
                        "t{} {} {} @ {},{},{} {}",
                        record.tick,
                        record.kind,
                        record.actor,
                        record.x,
                        record.y,
                        record.z,
                        record.detail
                    ),
                ));
                if commands.len() == wanted {
                    break;
                }
            }
        }
        if commands.is_empty() {
            commands.push(message_session(
                invoked.session,
                "No matching bounded audit records.",
            ));
        }
        commands
    }

    fn storage_get(&mut self, outcome: &StorageGetOutcome) -> Vec<Command> {
        let StorageGetOutcome::Read(record) = outcome else {
            self.loaded = false;
            return Vec::new();
        };
        let Some(records) = self.settings().decode(record.value.as_deref()) else {
            self.loaded = false;
            return Vec::new();
        };
        self.records = records;
        self.storage_version = record.version;
        self.loaded = true;
        self.process_queue()
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
            self.queue.push_front(pending.record);
            self.loaded = false;
            let request = self.next_request("reload");
            return vec![storage_get(&request, STORAGE_KEY)];
        };
        self.records = pending.records;
        self.storage_version = Some(*version);
        self.process_queue()
    }
}

enum Lookup {
    All,
    Actor(String),
    Here(i32),
    Since(u64),
}

impl Lookup {
    fn matches(&self, record: &Record, invoked: &CommandInvoked, latest_tick: u64) -> bool {
        match self {
            Self::All => true,
            Self::Actor(actor) => record.actor == *actor,
            Self::Here(radius) => {
                (f64::from(record.x) - invoked.position.x).abs() <= f64::from(*radius)
                    && (f64::from(record.z) - invoked.position.z).abs() <= f64::from(*radius)
            }
            Self::Since(ticks) => record.tick >= latest_tick.saturating_sub(*ticks),
        }
    }
}

impl Plugin for Audit {
    fn configure(
        &mut self,
        config: &Config,
    ) -> Result<Option<solaris_plugin_sdk::StartupContribution>, Failure> {
        Settings::parse(config)?;
        Ok(None)
    }

    fn init(&mut self, config: &Config, _context: &InitContext) -> Result<Vec<Command>, Failure> {
        self.settings = Some(Settings::parse(config)?);
        self.records.clear();
        self.queue.clear();
        self.storage_version = None;
        self.loaded = false;
        self.pending = None;
        self.latest_tick = 0;
        self.sequence = 0;
        Ok(vec![storage_get("audit-load", STORAGE_KEY)])
    }

    fn on_events(
        &mut self,
        context: &EventContext,
        events: &[Event],
    ) -> Result<Vec<Command>, Failure> {
        self.latest_tick = self.latest_tick.max(context.tick);
        let mut commands = Vec::new();
        for event in events {
            match event {
                Event::CommandInvoked(invoked) if invoked.name == "audit" => {
                    commands.extend(self.command(invoked));
                }
                Event::StorageGetAnswered(answered) => {
                    commands.extend(self.storage_get(&answered.outcome));
                }
                Event::StorageCasAnswered(answered) => {
                    commands.extend(self.storage_cas(&answered.request, &answered.outcome));
                }
                Event::PlayerBlockBroken(event) => commands.extend(self.append(
                    &event.player,
                    &event.dimension,
                    "break",
                    &event.block,
                    event.at.x,
                    event.at.y,
                    event.at.z,
                )),
                Event::PlayerBlockPlaced(event) => commands.extend(self.append(
                    &event.player,
                    &event.dimension,
                    "place",
                    &event.block,
                    event.at.x,
                    event.at.y,
                    event.at.z,
                )),
                Event::PlayerItemCrafted(event) => commands.extend(self.append(
                    &event.player,
                    &event.dimension,
                    "craft",
                    &format!("{}:{}", event.item, event.count),
                    event.position.x.floor() as i32,
                    event.position.y.floor() as i32,
                    event.position.z.floor() as i32,
                )),
                Event::PlayerItemPickedUp(event) => commands.extend(self.append(
                    &event.player,
                    &event.dimension,
                    "pickup",
                    &format!("{}:{}", event.item, event.count),
                    event.position.x.floor() as i32,
                    event.position.y.floor() as i32,
                    event.position.z.floor() as i32,
                )),
                Event::PlayerEntityKilled(event) => commands.extend(self.append(
                    &event.player,
                    &event.dimension,
                    "kill",
                    &event.entity_type,
                    event.position.x.floor() as i32,
                    event.position.y.floor() as i32,
                    event.position.z.floor() as i32,
                )),
                Event::PlayerEntityInteracted(event) => commands.extend(self.append(
                    &event.player,
                    &event.dimension,
                    "interact",
                    &event.entity_type,
                    event.position.x.floor() as i32,
                    event.position.y.floor() as i32,
                    event.position.z.floor() as i32,
                )),
                Event::PlayerDied(event) => commands.extend(self.append(
                    &event.player,
                    &event.dimension,
                    "death",
                    "player",
                    event.position.x.floor() as i32,
                    event.position.y.floor() as i32,
                    event.position.z.floor() as i32,
                )),
                Event::CommandBatchRejected => {
                    if let Some(pending) = self.pending.take() {
                        self.queue.push_front(pending.record);
                    }
                }
                _ => {}
            }
        }
        Ok(commands)
    }
}

fn required_string<'a>(table: &'a Table, key: &str) -> Result<&'a str, Failure> {
    table
        .get(key)
        .and_then(Value::as_str)
        .ok_or(Failure::Invalid)
}

fn bounded_usize(
    table: &Table,
    key: &str,
    minimum: usize,
    maximum: usize,
) -> Result<usize, Failure> {
    table
        .get(key)
        .and_then(Value::as_integer)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or(Failure::Invalid)
}

fn bounded_i32(table: &Table, key: &str, minimum: i32, maximum: i32) -> Result<i32, Failure> {
    table
        .get(key)
        .and_then(Value::as_integer)
        .and_then(|value| i32::try_from(value).ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or(Failure::Invalid)
}

fn valid_resource_id(value: &str) -> bool {
    let Some((namespace, path)) = value.split_once(':') else {
        return false;
    };
    !namespace.is_empty()
        && !path.is_empty()
        && namespace.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
        })
        && path.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'.' | b'/' | b'-')
        })
}

fn normalize_uuid(value: &str) -> Option<String> {
    let normalized = value.replace('-', "").to_ascii_lowercase();
    (normalized.len() == 32 && normalized.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(normalized)
}

fn compact(value: &str) -> String {
    let mut compact = value
        .chars()
        .map(|character| {
            if matches!(character, ',' | ';' | '|') {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    if compact.len() > 48 {
        let mut boundary = 48;
        while !compact.is_char_boundary(boundary) {
            boundary -= 1;
        }
        compact.truncate(boundary);
    }
    compact
}

fn parse_unsigned(value: &str) -> Option<u64> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

fn parse_signed(value: &str) -> Option<i32> {
    let body = value.strip_prefix('-').unwrap_or(value);
    (!body.is_empty() && body.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

fn parse_lua_integer(value: &str) -> Option<i64> {
    let parsed = value.parse::<f64>().ok()?;
    (parsed.is_finite()
        && parsed.fract() == 0.0
        && parsed >= i64::MIN as f64
        && parsed <= i64::MAX as f64)
        .then_some(parsed as i64)
}

export_plugin!(Audit);
