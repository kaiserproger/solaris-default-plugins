//! Homes, warps, teleports, TPA, and private messages for the standard pack.
//!
//! The component keeps the Lua package's storage formats and command replies while
//! using server-authoritative component events for player identity, session, and
//! position. `configure` validates the complete configuration; `init` recreates
//! all runtime-only maps for a fresh component generation.

use std::collections::BTreeMap;

use solaris_plugin_sdk::commands::TeleportPlayer;
use solaris_plugin_sdk::events::{
    CommandInvoked, Event, OnlinePlayersAnswered, PlayerSnapshot, PlayerTeleportOutcome,
};
use solaris_plugin_sdk::types::Position;
use solaris_plugin_sdk::{
    cancel_timer, export_plugin, list_online_players, message_session, schedule_timer, storage_cas,
    storage_get, Command, Config, EventContext, Failure, InitContext, Plugin, StorageCasOutcome,
    StorageGetOutcome,
};
use toml::{Table, Value};

const MAX_HOMES: usize = 8;
const MAX_WARPS: usize = 24;
const MAX_TPA_EXPIRY_TICKS: u64 = 72_000;
const MAX_ONLINE_QUERY: u32 = 256;
const MAX_MESSAGE_BYTES: usize = 256;

#[derive(Clone)]
struct Settings {
    maximum_homes_per_player: usize,
    maximum_warps: usize,
    tpa_expiry_ticks: u64,
    maximum_online_query: u32,
}

impl Settings {
    fn parse(config: &Config) -> Result<Self, Failure> {
        let table = config
            .toml()
            .and_then(|value| value.as_table().cloned())
            .ok_or(Failure::Invalid)?;
        Ok(Self {
            maximum_homes_per_player: bounded_usize(
                &table,
                "maximum_homes_per_player",
                1,
                MAX_HOMES,
            )?,
            maximum_warps: bounded_usize(&table, "maximum_warps", 1, MAX_WARPS)?,
            tpa_expiry_ticks: bounded_u64(&table, "tpa_expiry_ticks", 20, MAX_TPA_EXPIRY_TICKS)?,
            maximum_online_query: u32::try_from(bounded_u64(
                &table,
                "maximum_online_query",
                1,
                u64::from(MAX_ONLINE_QUERY),
            )?)
            .expect("configured online-query bound fits u32"),
        })
    }
}

#[derive(Clone, Copy)]
struct StoredPosition {
    x: f64,
    y: f64,
    z: f64,
}

impl From<Position> for StoredPosition {
    fn from(value: Position) -> Self {
        Self {
            x: value.x,
            y: value.y,
            z: value.z,
        }
    }
}

impl From<StoredPosition> for Position {
    fn from(value: StoredPosition) -> Self {
        Self {
            x: value.x,
            y: value.y,
            z: value.z,
        }
    }
}

#[derive(Clone, Copy)]
enum StorageKind {
    HomeSet,
    HomeGet,
    HomeDelete,
    WarpSet,
    WarpGet,
    WarpDelete,
}

impl StorageKind {
    fn is_get(self) -> bool {
        matches!(self, Self::HomeGet | Self::WarpGet)
    }

    fn is_set(self) -> bool {
        matches!(self, Self::HomeSet | Self::WarpSet)
    }

    fn is_delete(self) -> bool {
        matches!(self, Self::HomeDelete | Self::WarpDelete)
    }
}

#[derive(Clone)]
struct StorageRequest {
    session: u64,
    key: String,
    kind: StorageKind,
    name: String,
    value: Option<StoredPosition>,
    old: StoredPosition,
}

#[derive(Clone)]
struct SaveRequest {
    session: u64,
    name: String,
    kind: StorageKind,
}

#[derive(Clone)]
struct TeleportRequest {
    session: u64,
    old: StoredPosition,
    label: String,
}

#[derive(Clone)]
struct OnlineRequest {
    kind: OnlineKind,
    session: u64,
    username: String,
    target_name: String,
    message: Option<String>,
    target_position: StoredPosition,
    accepted_tpa: Option<TpaRequest>,
}

#[derive(Clone, Copy)]
enum OnlineKind {
    Tpa,
    TpaAccept,
    Message,
}

#[derive(Clone)]
struct TpaRequest {
    from_session: u64,
}

#[derive(Clone)]
struct CallbackCorrelations {
    pending_storage: BTreeMap<String, StorageRequest>,
    pending_saves: BTreeMap<String, SaveRequest>,
    pending_queries: BTreeMap<String, OnlineRequest>,
    pending_teleports: BTreeMap<String, TeleportRequest>,
    tpa_by_target: BTreeMap<u64, TpaRequest>,
    replies: BTreeMap<u64, u64>,
}

#[derive(Default)]
struct Essentials {
    settings: Option<Settings>,
    pending_storage: BTreeMap<String, StorageRequest>,
    pending_saves: BTreeMap<String, SaveRequest>,
    pending_queries: BTreeMap<String, OnlineRequest>,
    pending_teleports: BTreeMap<String, TeleportRequest>,
    tpa_by_target: BTreeMap<u64, TpaRequest>,
    replies: BTreeMap<u64, u64>,
    backs: BTreeMap<u64, StoredPosition>,
    last_callback: Option<CallbackCorrelations>,
    sequence: u64,
}

impl Essentials {
    fn settings(&self) -> &Settings {
        self.settings
            .as_ref()
            .expect("init establishes validated settings before events")
    }
    fn snapshot_correlations(&mut self) {
        self.last_callback = Some(CallbackCorrelations {
            pending_storage: self.pending_storage.clone(),
            pending_saves: self.pending_saves.clone(),
            pending_queries: self.pending_queries.clone(),
            pending_teleports: self.pending_teleports.clone(),
            tpa_by_target: self.tpa_by_target.clone(),
            replies: self.replies.clone(),
        });
    }

    fn restore_correlations(&mut self) {
        let Some(previous) = self.last_callback.take() else {
            return;
        };
        self.pending_storage = previous.pending_storage;
        self.pending_saves = previous.pending_saves;
        self.pending_queries = previous.pending_queries;
        self.pending_teleports = previous.pending_teleports;
        self.tpa_by_target = previous.tpa_by_target;
        self.replies = previous.replies;
    }

    fn next_request(&mut self, prefix: &str) -> String {
        self.sequence += 1;
        format!("{prefix}-{}", self.sequence)
    }

    fn read_positions(
        &mut self,
        invoked: &CommandInvoked,
        kind: StorageKind,
        key: String,
        name: String,
        value: Option<StoredPosition>,
    ) -> Command {
        let request = self.next_request("read");
        self.pending_storage.insert(
            request.clone(),
            StorageRequest {
                session: invoked.session,
                key: key.clone(),
                kind,
                name,
                value,
                old: invoked.position.into(),
            },
        );
        storage_get(&request, &key)
    }

    fn begin_teleport(
        &mut self,
        session: u64,
        target: StoredPosition,
        old: StoredPosition,
        label: &str,
    ) -> Command {
        let request = self.next_request("teleport");
        self.pending_teleports.insert(
            request.clone(),
            TeleportRequest {
                session,
                old,
                label: label.to_owned(),
            },
        );
        Command::TeleportPlayer(TeleportPlayer {
            request,
            session,
            position: target.into(),
        })
    }

    fn query(
        &mut self,
        invoked: &CommandInvoked,
        kind: OnlineKind,
        target_name: &str,
        message: Option<String>,
    ) -> Command {
        let request = self.next_request("online");
        let position: StoredPosition = invoked.position.into();
        self.pending_queries.insert(
            request.clone(),
            OnlineRequest {
                kind,
                session: invoked.session,
                username: invoked.username.clone(),
                target_name: target_name.to_ascii_lowercase(),
                message,
                target_position: position,
                accepted_tpa: None,
            },
        );
        list_online_players(&request, self.settings().maximum_online_query)
    }

    fn command(&mut self, invoked: &CommandInvoked) -> Vec<Command> {
        let words = invoked
            .arguments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        match invoked.name.as_str() {
            "sethome" | "home" | "delhome" => self.home_command(invoked, &words),
            "setwarp" | "delwarp" | "setspawn" => self.warp_mutation(invoked, &words),
            "warp" | "spawn" => self.warp_lookup(invoked, &words),
            "back" => self.back(invoked, &words),
            "tpa" => self.tpa(invoked, &words),
            "tpaccept" => self.tpa_accept(invoked, &words),
            "msg" => self.message(invoked),
            "reply" => self.reply(invoked),
            _ => Vec::new(),
        }
    }

    fn home_command(&mut self, invoked: &CommandInvoked, words: &[&str]) -> Vec<Command> {
        let name = words
            .first()
            .copied()
            .unwrap_or("home")
            .to_ascii_lowercase();
        if words.len() > 1 || !valid_name(&name) {
            return vec![message_session(
                invoked.session,
                "Use one lowercase home name.",
            )];
        }
        let kind = match invoked.name.as_str() {
            "sethome" => StorageKind::HomeSet,
            "delhome" => StorageKind::HomeDelete,
            _ => StorageKind::HomeGet,
        };
        let Some(uuid) = normalize_uuid(&invoked.player) else {
            return Vec::new();
        };
        vec![self.read_positions(
            invoked,
            kind,
            format!("homes:{uuid}"),
            name,
            kind.is_set().then(|| invoked.position.into()),
        )]
    }

    fn warp_mutation(&mut self, invoked: &CommandInvoked, words: &[&str]) -> Vec<Command> {
        if !invoked.operator {
            return vec![message_session(
                invoked.session,
                "Only an operator can change warps.",
            )];
        }
        let name = if invoked.name == "setspawn" {
            "spawn".to_owned()
        } else {
            words.first().copied().unwrap_or("").to_ascii_lowercase()
        };
        if (invoked.name == "setspawn" && !words.is_empty())
            || !valid_name(&name)
            || (invoked.name != "setspawn" && words.len() != 1)
        {
            return vec![message_session(
                invoked.session,
                "Usage: /setwarp <name>, /delwarp <name>, or /setspawn",
            )];
        }
        let kind = if invoked.name == "delwarp" {
            StorageKind::WarpDelete
        } else {
            StorageKind::WarpSet
        };
        vec![self.read_positions(
            invoked,
            kind,
            "warps-v1".to_owned(),
            name,
            kind.is_set().then(|| invoked.position.into()),
        )]
    }

    fn warp_lookup(&mut self, invoked: &CommandInvoked, words: &[&str]) -> Vec<Command> {
        let name = if invoked.name == "spawn" {
            "spawn".to_owned()
        } else {
            words.first().copied().unwrap_or("").to_ascii_lowercase()
        };
        if !valid_name(&name)
            || (invoked.name == "spawn" && !words.is_empty())
            || (invoked.name == "warp" && words.len() != 1)
        {
            return vec![message_session(
                invoked.session,
                "Usage: /warp <name> or /spawn",
            )];
        }
        vec![self.read_positions(
            invoked,
            StorageKind::WarpGet,
            "warps-v1".to_owned(),
            name,
            None,
        )]
    }

    fn back(&mut self, invoked: &CommandInvoked, words: &[&str]) -> Vec<Command> {
        if !words.is_empty() {
            return vec![message_session(
                invoked.session,
                "No back location is available.",
            )];
        }
        let Some(target) = self.backs.get(&invoked.session).copied() else {
            return vec![message_session(
                invoked.session,
                "No back location is available.",
            )];
        };
        vec![self.begin_teleport(invoked.session, target, invoked.position.into(), "Back")]
    }

    fn tpa(&mut self, invoked: &CommandInvoked, words: &[&str]) -> Vec<Command> {
        if words.len() != 1 {
            return vec![message_session(
                invoked.session,
                "Usage: /tpa <online-player>",
            )];
        }
        vec![self.query(invoked, OnlineKind::Tpa, words[0], None)]
    }

    fn tpa_accept(&mut self, invoked: &CommandInvoked, words: &[&str]) -> Vec<Command> {
        if !words.is_empty() {
            return vec![message_session(invoked.session, "Usage: /tpaccept")];
        }
        let Some(accepted_tpa) = self.tpa_by_target.remove(&invoked.session) else {
            return vec![message_session(
                invoked.session,
                "No TPA request is pending.",
            )];
        };
        let request = self.next_request("online");
        self.pending_queries.insert(
            request.clone(),
            OnlineRequest {
                kind: OnlineKind::TpaAccept,
                session: invoked.session,
                username: invoked.username.clone(),
                target_name: String::new(),
                message: None,
                target_position: invoked.position.into(),
                accepted_tpa: Some(accepted_tpa),
            },
        );
        vec![
            cancel_timer(&tpa_timer(invoked.session)),
            list_online_players(&request, self.settings().maximum_online_query),
        ]
    }

    fn message(&mut self, invoked: &CommandInvoked) -> Vec<Command> {
        let arguments = invoked.raw_arguments.trim_start();
        let Some(separator) = arguments.find(char::is_whitespace) else {
            return vec![message_session(
                invoked.session,
                "Usage: /msg <online-player> <message>",
            )];
        };
        let target = &arguments[..separator];
        let message = arguments[separator..].trim_start();
        if target.is_empty() || message.is_empty() || message.len() > MAX_MESSAGE_BYTES {
            return vec![message_session(
                invoked.session,
                "Usage: /msg <online-player> <message>",
            )];
        }
        vec![self.query(
            invoked,
            OnlineKind::Message,
            target,
            Some(message.to_owned()),
        )]
    }

    fn reply(&mut self, invoked: &CommandInvoked) -> Vec<Command> {
        let message = invoked.raw_arguments.trim();
        let Some(&target) = self.replies.get(&invoked.session) else {
            return vec![message_session(
                invoked.session,
                "Usage: /reply <message> after receiving a message.",
            )];
        };
        if message.is_empty() || message.len() > MAX_MESSAGE_BYTES {
            return vec![message_session(
                invoked.session,
                "Usage: /reply <message> after receiving a message.",
            )];
        }
        vec![
            message_session(
                target,
                format!("[reply from {}] {message}", invoked.username),
            ),
            message_session(invoked.session, format!("[to reply target] {message}")),
        ]
    }

    fn storage_get(&mut self, request: &str, outcome: &StorageGetOutcome) -> Vec<Command> {
        let Some(current) = self.pending_storage.remove(request) else {
            return Vec::new();
        };
        let StorageGetOutcome::Read(record) = outcome else {
            return vec![message_session(
                current.session,
                "Location storage is unavailable.",
            )];
        };
        let maximum = if current.key == "warps-v1" {
            self.settings().maximum_warps
        } else {
            self.settings().maximum_homes_per_player
        };
        let Some(values) = decode_positions(record.value.as_deref(), maximum) else {
            return vec![message_session(
                current.session,
                "Stored locations are invalid.",
            )];
        };
        if current.kind.is_get() {
            let Some(target) = values.get(&current.name).copied() else {
                return vec![message_session(current.session, "Location not found.")];
            };
            return vec![self.begin_teleport(current.session, target, current.old, &current.name)];
        }
        let mut values = values;
        if current.kind.is_set() {
            if !values.contains_key(&current.name) && values.len() >= maximum {
                return vec![message_session(current.session, "Location limit reached.")];
            }
            values.insert(
                current.name.clone(),
                current.value.expect("set request carries position"),
            );
        } else if values.remove(&current.name).is_none() {
            return vec![message_session(current.session, "Location not found.")];
        }
        let revision = record
            .version
            .map_or_else(|| "new".to_owned(), |version| version.to_string());
        let prefix = if current.key == "warps-v1" {
            "warps".to_owned()
        } else {
            format!("home-{}", &current.key[6..])
        };
        let save = format!("{prefix}-v{revision}");
        self.pending_saves.insert(
            save.clone(),
            SaveRequest {
                session: current.session,
                name: current.name,
                kind: current.kind,
            },
        );
        vec![storage_cas(
            &save,
            &current.key,
            record.version,
            encode_positions(&values),
        )]
    }

    fn storage_cas(&mut self, request: &str, outcome: &StorageCasOutcome) -> Vec<Command> {
        let Some(current) = self.pending_saves.remove(request) else {
            return Vec::new();
        };
        if !matches!(outcome, StorageCasOutcome::Committed(_)) {
            return vec![message_session(
                current.session,
                "Location changed concurrently; retry.",
            )];
        }
        let verb = if current.kind.is_delete() {
            " removed."
        } else {
            " saved."
        };
        vec![message_session(
            current.session,
            format!("{}{}", current.name, verb),
        )]
    }

    fn online_players(&mut self, answered: &OnlinePlayersAnswered) -> Vec<Command> {
        let Some(current) = self.pending_queries.remove(&answered.request) else {
            return Vec::new();
        };
        if let Some(request) = current.accepted_tpa {
            let Some(requester) = answered
                .players
                .iter()
                .find(|player| player.session == request.from_session)
            else {
                return vec![message_session(
                    current.session,
                    "Requester is no longer online.",
                )];
            };
            return vec![
                self.begin_teleport(
                    requester.session,
                    current.target_position,
                    requester.position.into(),
                    "TPA",
                ),
                message_session(current.session, "TPA accepted."),
            ];
        }
        let Some(target) = find_player(&answered.players, &current.target_name) else {
            return vec![message_session(
                current.session,
                "Online player not found or ambiguous.",
            )];
        };
        if target.session == current.session {
            return vec![message_session(
                current.session,
                "Online player not found or ambiguous.",
            )];
        }
        match current.kind {
            OnlineKind::Tpa => {
                self.tpa_by_target.insert(
                    target.session,
                    TpaRequest {
                        from_session: current.session,
                    },
                );
                vec![
                    schedule_timer(&tpa_timer(target.session), self.settings().tpa_expiry_ticks),
                    message_session(
                        target.session,
                        format!("{} requested a teleport. Use /tpaccept.", current.username),
                    ),
                    message_session(current.session, "TPA request sent."),
                ]
            }
            OnlineKind::Message => {
                self.replies.insert(current.session, target.session);
                self.replies.insert(target.session, current.session);
                let message = current.message.expect("message query carries text");
                vec![
                    message_session(
                        target.session,
                        format!("[from {}] {message}", current.username),
                    ),
                    message_session(current.session, format!("[to {}] {message}", target.name)),
                ]
            }
            OnlineKind::TpaAccept => unreachable!("accept queries carry their TPA snapshot"),
        }
    }

    fn teleport(&mut self, request: &str, outcome: &PlayerTeleportOutcome) -> Vec<Command> {
        let Some(current) = self.pending_teleports.remove(request) else {
            return Vec::new();
        };
        match outcome {
            PlayerTeleportOutcome::Committed => {
                self.backs.insert(current.session, current.old);
                vec![message_session(
                    current.session,
                    format!("{} teleport complete.", current.label),
                )]
            }
            PlayerTeleportOutcome::Refused(failure) => vec![message_session(
                current.session,
                format!("Teleport failed: {failure:?}."),
            )],
        }
    }

    fn timer(&mut self, timer_id: &str) -> Vec<Command> {
        let Some(target) = timer_id
            .strip_prefix("tpa-")
            .and_then(|target| target.parse::<u64>().ok())
        else {
            return Vec::new();
        };
        self.tpa_by_target
            .remove(&target)
            .map_or_else(Vec::new, |_| {
                vec![message_session(target, "TPA request expired.")]
            })
    }

    fn player_left(&mut self, session: u64) -> Vec<Command> {
        self.backs.remove(&session);
        self.replies.retain(|_, target| *target != session);
        self.replies.remove(&session);
        self.tpa_by_target.remove(&session);
        let mut commands = vec![cancel_timer(&tpa_timer(session))];
        let targets = self
            .tpa_by_target
            .iter()
            .filter_map(|(target, request)| (request.from_session == session).then_some(*target))
            .collect::<Vec<_>>();
        for target in targets {
            self.tpa_by_target.remove(&target);
            commands.push(cancel_timer(&tpa_timer(target)));
        }
        commands
    }
}

impl Plugin for Essentials {
    fn configure(
        &mut self,
        config: &Config,
    ) -> Result<Option<solaris_plugin_sdk::StartupContribution>, Failure> {
        Settings::parse(config)?;
        Ok(None)
    }

    fn init(&mut self, config: &Config, _context: &InitContext) -> Result<Vec<Command>, Failure> {
        self.settings = Some(Settings::parse(config)?);
        self.pending_storage.clear();
        self.pending_saves.clear();
        self.pending_queries.clear();
        self.pending_teleports.clear();
        self.tpa_by_target.clear();
        self.replies.clear();
        self.backs.clear();
        self.last_callback = None;
        self.sequence = 0;
        Ok(Vec::new())
    }

    fn on_events(
        &mut self,
        _context: &EventContext,
        events: &[Event],
    ) -> Result<Vec<Command>, Failure> {
        let mut commands = Vec::new();
        for event in events {
            if let Event::CommandBatchRejected = event {
                self.restore_correlations();
                continue;
            }
            self.snapshot_correlations();
            match event {
                Event::CommandInvoked(invoked) => commands.extend(self.command(invoked)),
                Event::StorageGetAnswered(answered) => {
                    commands.extend(self.storage_get(&answered.request, &answered.outcome));
                }
                Event::StorageCasAnswered(answered) => {
                    commands.extend(self.storage_cas(&answered.request, &answered.outcome));
                }
                Event::OnlinePlayersAnswered(answered) => {
                    commands.extend(self.online_players(answered))
                }
                Event::PlayerTeleportAnswered(answered) => {
                    commands.extend(self.teleport(&answered.request, &answered.outcome));
                }
                Event::TimerFired(timer) => commands.extend(self.timer(&timer.timer_id)),
                Event::PlayerDied(died) => {
                    self.backs.insert(died.session, died.position.into());
                }
                Event::PlayerLeft(left) => commands.extend(self.player_left(left.session)),
                Event::CommandBatchRejected => unreachable!("rejection handled before snapshot"),
                _ => {}
            }
        }
        Ok(commands)
    }
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

fn bounded_u64(table: &Table, key: &str, minimum: u64, maximum: u64) -> Result<u64, Failure> {
    table
        .get(key)
        .and_then(Value::as_integer)
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or(Failure::Invalid)
}

fn normalize_uuid(value: &str) -> Option<String> {
    let normalized = value.replace('-', "").to_ascii_lowercase();
    (normalized.len() == 32 && normalized.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(normalized)
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 24
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn decode_positions(
    value: Option<&str>,
    maximum: usize,
) -> Option<BTreeMap<String, StoredPosition>> {
    let Some(value) = value else {
        return Some(BTreeMap::new());
    };
    if value == "v1|" {
        return Some(BTreeMap::new());
    }
    let rows = value.strip_prefix("v1|")?;
    let mut positions = BTreeMap::new();
    for row in rows.split(';').filter(|row| !row.is_empty()) {
        let mut fields = row.split(',');
        let (Some(name), Some(x), Some(y), Some(z), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return None;
        };
        let (Ok(x), Ok(y), Ok(z)) = (x.parse::<f64>(), y.parse::<f64>(), z.parse::<f64>()) else {
            return None;
        };
        if !valid_name(name)
            || !x.is_finite()
            || !y.is_finite()
            || !z.is_finite()
            || positions.contains_key(name)
        {
            return None;
        }
        positions.insert(name.to_owned(), StoredPosition { x, y, z });
        if positions.len() > maximum {
            return None;
        }
    }
    Some(positions)
}

fn encode_positions(positions: &BTreeMap<String, StoredPosition>) -> String {
    let rows = positions
        .iter()
        .map(|(name, position)| format!("{name},{},{},{}", position.x, position.y, position.z))
        .collect::<Vec<_>>();
    format!("v1|{}", rows.join(";"))
}

fn find_player<'a>(players: &'a [PlayerSnapshot], username: &str) -> Option<&'a PlayerSnapshot> {
    let mut found = None;
    for player in players {
        if player.name.eq_ignore_ascii_case(username) {
            if found.is_some() {
                return None;
            }
            found = Some(player);
        }
    }
    found
}

fn tpa_timer(session: u64) -> String {
    format!("tpa-{session}")
}

export_plugin!(Essentials);
