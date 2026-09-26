//! Durable towns and chunk claims for the standard pack.
//!
//! This component preserves the original Lua package's town storage format,
//! command wording, and optimistic storage/zone commit protocol. Player identity
//! is always the stable UUID supplied by the server; command replies and
//! connection-local invite effects retain their originating sessions.

use std::collections::{BTreeMap, BTreeSet};

use solaris_plugin_sdk::events::{
    CommandInvoked, Event, OnlinePlayersAnswered, ZoneCommandOutcome,
};
use solaris_plugin_sdk::{
    commands, export_plugin, list_online_players, message_session, schedule_timer, storage_cas,
    storage_get, types, Command, Config, EventContext, Failure, InitContext, Plugin,
    StorageCasOutcome, StorageGetOutcome,
};
use toml::{Table, Value};

const STORAGE_KEY: &str = "towns-v1";
const MAX_TOWNS: usize = 12;
const MAX_MEMBERS: usize = 8;
const MAX_CLAIMS: usize = 24;
const MAX_INVITE_EXPIRY_TICKS: u64 = 72_000;
const MAX_ONLINE_QUERY: u32 = 256;

#[derive(Clone)]
struct Settings {
    dimension: String,
    minimum_y: i64,
    maximum_y: i64,
    maximum_towns: usize,
    maximum_members_per_town: usize,
    maximum_claims: usize,
    invite_expiry_ticks: u64,
    maximum_online_query: u32,
}

impl Settings {
    fn parse(config: &Config) -> Result<Self, Failure> {
        let table = config
            .toml()
            .and_then(|value| value.as_table().cloned())
            .ok_or(Failure::Invalid)?;
        let dimension = required_string(&table, "dimension")?.to_owned();
        if !valid_dimension(&dimension) {
            return Err(Failure::Invalid);
        }
        let minimum_y = required_i64(&table, "minimum_y")?;
        let maximum_y = required_i64(&table, "maximum_y")?;
        if minimum_y > maximum_y {
            return Err(Failure::Invalid);
        }
        Ok(Self {
            dimension,
            minimum_y,
            maximum_y,
            maximum_towns: bounded_usize(&table, "maximum_towns", 1, MAX_TOWNS)?,
            maximum_members_per_town: bounded_usize(
                &table,
                "maximum_members_per_town",
                1,
                MAX_MEMBERS,
            )?,
            maximum_claims: bounded_usize(&table, "maximum_claims", 1, MAX_CLAIMS)?,
            invite_expiry_ticks: bounded_u64(
                &table,
                "invite_expiry_ticks",
                20,
                MAX_INVITE_EXPIRY_TICKS,
            )?,
            maximum_online_query: u32::try_from(bounded_u64(
                &table,
                "maximum_online_query",
                1,
                u64::from(MAX_ONLINE_QUERY),
            )?)
            .expect("validated online player bound fits u32"),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Leader,
    Officer,
    Member,
}

impl Role {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "leader" => Some(Self::Leader),
            "officer" => Some(Self::Officer),
            "member" => Some(Self::Member),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Leader => "leader",
            Self::Officer => "officer",
            Self::Member => "member",
        }
    }
}

#[derive(Clone)]
struct Claim {
    x: i64,
    z: i64,
}

#[derive(Clone)]
struct Town {
    name: String,
    leader: String,
    members: BTreeMap<String, Role>,
    claims: BTreeMap<String, Claim>,
}

enum ZoneAction {
    Add,
    Remove,
}

enum PendingStage {
    Save,
    Zone,
    Rollback,
}

struct PendingUpdate {
    request: String,
    session: u64,
    before: BTreeMap<String, Town>,
    after: BTreeMap<String, Town>,
    message: String,
    zone_action: Option<ZoneAction>,
    town_name: Option<String>,
    claim: Option<Claim>,
    stage: PendingStage,
}

struct InviteQuery {
    session: u64,
    town: String,
    target_name: String,
    inviter: String,
}

#[derive(Default)]
struct Towns {
    settings: Option<Settings>,
    towns: BTreeMap<String, Town>,
    storage_version: Option<u64>,
    loaded: bool,
    load_request: Option<String>,
    pending: Option<PendingUpdate>,
    pending_queries: BTreeMap<String, InviteQuery>,
    invites: BTreeMap<String, String>,
    invite_timers: BTreeMap<String, String>,
    startup_zones: Option<(BTreeSet<String>, bool)>,
    sequence: u64,
}

impl Towns {
    fn settings(&self) -> &Settings {
        self.settings
            .as_ref()
            .expect("init establishes validated settings before events")
    }

    fn next_request(&mut self, prefix: &str) -> String {
        self.sequence += 1;
        format!("{prefix}-{}", self.sequence)
    }

    fn save(
        &mut self,
        session: u64,
        after: BTreeMap<String, Town>,
        message: impl Into<String>,
        zone_action: Option<ZoneAction>,
        town_name: Option<String>,
        claim: Option<Claim>,
    ) -> Vec<Command> {
        if self.pending.is_some() {
            return vec![message_session(
                session,
                "Another town update is committing; retry.",
            )];
        }
        let revision = self
            .storage_version
            .map_or_else(|| "new".to_owned(), |version| version.to_string());
        let request = format!("save-v{revision}");
        let value = encode_towns(&after);
        self.pending = Some(PendingUpdate {
            request: request.clone(),
            session,
            before: self.towns.clone(),
            after,
            message: message.into(),
            zone_action,
            town_name,
            claim,
            stage: PendingStage::Save,
        });
        vec![storage_cas(
            &request,
            STORAGE_KEY,
            self.storage_version,
            value,
        )]
    }

    fn command(&mut self, invoked: &CommandInvoked) -> Vec<Command> {
        if invoked.name != "town" {
            return Vec::new();
        }
        if !self.loaded {
            return vec![message_session(invoked.session, "Towns are still loading.")];
        }
        let Some(uuid) = normalize_uuid(&invoked.player) else {
            return Vec::new();
        };
        let words = invoked
            .arguments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let own = member_town(&self.towns, &uuid).map(|town| town.name.clone());
        let action = words
            .first()
            .copied()
            .unwrap_or("info")
            .to_ascii_lowercase();
        match action.as_str() {
            "info" if words.len() <= 2 => self.info(invoked.session, own.as_deref(), &words),
            "create" if words.len() == 2 => self.create(invoked, &uuid, own.as_deref(), words[1]),
            "invite" if words.len() == 2 => self.invite(invoked, &uuid, own.as_deref(), words[1]),
            "join" if words.len() == 2 => {
                self.join(invoked.session, &uuid, own.as_deref(), words[1])
            }
            "leave" if words.len() == 1 => self.leave(invoked.session, &uuid, own.as_deref()),
            "role" if words.len() == 3 => self.role(invoked.session, &uuid, own.as_deref(), &words),
            "claim" | "unclaim" if words.len() == 1 => {
                self.claim(invoked, &uuid, own.as_deref(), action == "claim")
            }
            _ => vec![message_session(
                invoked.session,
                "Usage: /town <info|create|invite|join|leave|role|claim|unclaim>",
            )],
        }
    }

    fn info(&self, session: u64, own: Option<&str>, words: &[&str]) -> Vec<Command> {
        let town = words
            .get(1)
            .and_then(|name| self.towns.get(&name.to_ascii_lowercase()))
            .or_else(|| own.and_then(|name| self.towns.get(name)));
        let Some(town) = town else {
            return vec![message_session(session, "Town not found.")];
        };
        vec![message_session(
            session,
            format!(
                "{}: {} members, {} claims.",
                town.name,
                town.members.len(),
                town.claims.len()
            ),
        )]
    }

    fn create(
        &mut self,
        invoked: &CommandInvoked,
        uuid: &str,
        own: Option<&str>,
        raw_name: &str,
    ) -> Vec<Command> {
        let name = raw_name.to_ascii_lowercase();
        if own.is_some()
            || !valid_name(&name)
            || self.towns.contains_key(&name)
            || self.towns.len() >= self.settings().maximum_towns
        {
            return vec![message_session(invoked.session, "Cannot create that town.")];
        }
        let mut after = self.towns.clone();
        after.insert(
            name.clone(),
            Town {
                name: name.clone(),
                leader: uuid.to_owned(),
                members: BTreeMap::from([(uuid.to_owned(), Role::Leader)]),
                claims: BTreeMap::new(),
            },
        );
        self.save(
            invoked.session,
            after,
            format!("Town {name} created."),
            None,
            None,
            None,
        )
    }

    fn invite(
        &mut self,
        invoked: &CommandInvoked,
        uuid: &str,
        own: Option<&str>,
        target_name: &str,
    ) -> Vec<Command> {
        let Some(own) = own.and_then(|name| self.towns.get(name)) else {
            return vec![message_session(
                invoked.session,
                "Only a leader or officer can invite.",
            )];
        };
        if !matches!(own.members.get(uuid), Some(Role::Leader | Role::Officer)) {
            return vec![message_session(
                invoked.session,
                "Only a leader or officer can invite.",
            )];
        }
        if own.members.len() >= self.settings().maximum_members_per_town {
            return vec![message_session(
                invoked.session,
                "Town member limit reached.",
            )];
        }
        let town = own.name.clone();
        let request = self.next_request("invite");
        self.pending_queries.insert(
            request.clone(),
            InviteQuery {
                session: invoked.session,
                town,
                target_name: target_name.to_ascii_lowercase(),
                inviter: invoked.username.clone(),
            },
        );
        vec![list_online_players(
            &request,
            self.settings().maximum_online_query,
        )]
    }

    fn join(
        &mut self,
        session: u64,
        uuid: &str,
        own: Option<&str>,
        raw_name: &str,
    ) -> Vec<Command> {
        let name = raw_name.to_ascii_lowercase();
        if own.is_some() || self.invites.get(uuid) != Some(&name) || !self.towns.contains_key(&name)
        {
            return vec![message_session(session, "No matching invitation.")];
        }
        let Some(town) = self.towns.get(&name) else {
            return vec![message_session(session, "No matching invitation.")];
        };
        if town.members.len() >= self.settings().maximum_members_per_town {
            return vec![message_session(session, "Town member limit reached.")];
        }
        let mut after = self.towns.clone();
        after
            .get_mut(&name)
            .expect("existing town remains in cloned map")
            .members
            .insert(uuid.to_owned(), Role::Member);
        self.invites.remove(uuid);
        self.save(session, after, format!("Joined {name}."), None, None, None)
    }

    fn leave(&mut self, session: u64, uuid: &str, own: Option<&str>) -> Vec<Command> {
        let Some(name) = own else {
            return vec![message_session(session, "You are not in a town.")];
        };
        let town = &self.towns[name];
        if town.leader == uuid {
            return vec![message_session(
                session,
                "A leader cannot leave; leadership transfer is not in this alpha.",
            )];
        }
        let mut after = self.towns.clone();
        after
            .get_mut(name)
            .expect("membership town remains in cloned map")
            .members
            .remove(uuid);
        self.save(session, after, format!("Left {name}."), None, None, None)
    }

    fn role(
        &mut self,
        session: u64,
        uuid: &str,
        own: Option<&str>,
        words: &[&str],
    ) -> Vec<Command> {
        let Some(name) = own else {
            return vec![message_session(session, "Only the leader can set roles.")];
        };
        let town = &self.towns[name];
        if town.leader != uuid {
            return vec![message_session(session, "Only the leader can set roles.")];
        }
        let target = normalize_uuid(words[1]);
        let role = Role::parse(&words[2].to_ascii_lowercase());
        let Some(target) = target else {
            return vec![message_session(
                session,
                "Use a member UUID and role member/officer.",
            )];
        };
        let Some(role) = role.filter(|role| !matches!(role, Role::Leader)) else {
            return vec![message_session(
                session,
                "Use a member UUID and role member/officer.",
            )];
        };
        if target == town.leader || !town.members.contains_key(&target) {
            return vec![message_session(
                session,
                "Use a member UUID and role member/officer.",
            )];
        }
        let mut after = self.towns.clone();
        after
            .get_mut(name)
            .expect("leader town remains in cloned map")
            .members
            .insert(target, role);
        self.save(session, after, "Role updated.", None, None, None)
    }

    fn claim(
        &mut self,
        invoked: &CommandInvoked,
        uuid: &str,
        own: Option<&str>,
        add: bool,
    ) -> Vec<Command> {
        let Some(name) = own else {
            return vec![message_session(
                invoked.session,
                "Only the leader can manage claims.",
            )];
        };
        let town = &self.towns[name];
        if town.leader != uuid {
            return vec![message_session(
                invoked.session,
                "Only the leader can manage claims.",
            )];
        }
        let x = (invoked.position.x / 16.0).floor() as i64;
        let z = (invoked.position.z / 16.0).floor() as i64;
        let key = claim_key(x, z);
        let owner = claim_owner(&self.towns, &key).map(|town| town.name.clone());
        if add {
            if owner.is_some() || total_claims(&self.towns) >= self.settings().maximum_claims {
                return vec![message_session(
                    invoked.session,
                    "Chunk is claimed or the claim limit is reached.",
                )];
            }
            let claim = Claim { x, z };
            let mut after = self.towns.clone();
            after
                .get_mut(name)
                .expect("leader town remains in cloned map")
                .claims
                .insert(key, claim.clone());
            self.save(
                invoked.session,
                after,
                format!("Chunk claimed for {name}."),
                Some(ZoneAction::Add),
                Some(name.to_owned()),
                Some(claim),
            )
        } else {
            if owner.as_deref() != Some(name) {
                return vec![message_session(
                    invoked.session,
                    "Your town does not claim this chunk.",
                )];
            }
            let mut after = self.towns.clone();
            let claim = after
                .get_mut(name)
                .expect("leader town remains in cloned map")
                .claims
                .remove(&key)
                .expect("claimed key exists for owning town");
            self.save(
                invoked.session,
                after,
                "Chunk unclaimed.",
                Some(ZoneAction::Remove),
                Some(name.to_owned()),
                Some(claim),
            )
        }
    }

    fn storage_get(&mut self, request: &str, outcome: &StorageGetOutcome) -> Vec<Command> {
        if self.load_request.as_deref() != Some(request) {
            return Vec::new();
        }
        self.load_request = None;
        let StorageGetOutcome::Read(record) = outcome else {
            self.loaded = false;
            return Vec::new();
        };
        let Some(towns) = decode_towns(record.value.as_deref(), self.settings()) else {
            self.loaded = false;
            return Vec::new();
        };
        self.towns = towns;
        self.storage_version = record.version;
        let mut remaining = BTreeSet::new();
        let mut commands = Vec::new();
        for town in self.towns.values() {
            for claim in town.claims.values() {
                let zone = zone_id(town, claim);
                remaining.insert(zone.clone());
                commands.push(upsert_claim(town, claim, self.settings()));
            }
        }
        if remaining.is_empty() {
            self.loaded = true;
        } else {
            self.startup_zones = Some((remaining, false));
        }
        commands
    }

    fn storage_cas(&mut self, request: &str, outcome: &StorageCasOutcome) -> Vec<Command> {
        let Some(mut pending) = self.pending.take() else {
            return Vec::new();
        };
        if pending.request != request {
            self.pending = Some(pending);
            return Vec::new();
        }
        if matches!(pending.stage, PendingStage::Rollback) {
            if let StorageCasOutcome::Committed(version) = outcome {
                self.towns = pending.before;
                self.storage_version = Some(*version);
                self.loaded = true;
            } else {
                self.loaded = false;
            }
            return vec![message_session(
                pending.session,
                "Claim protection failed; the town change was rolled back.",
            )];
        }
        let StorageCasOutcome::Committed(version) = outcome else {
            return vec![message_session(
                pending.session,
                "Town data changed concurrently; retry.",
            )];
        };
        self.towns = pending.after.clone();
        self.storage_version = Some(*version);
        let Some(action) = pending.zone_action.take() else {
            return vec![message_session(pending.session, pending.message)];
        };
        let town = self
            .towns
            .get(
                pending
                    .town_name
                    .as_deref()
                    .expect("zone update names its town"),
            )
            .expect("committed update retains its town");
        let claim = pending.claim.as_ref().expect("zone update names its claim");
        pending.stage = PendingStage::Zone;
        let command = match action {
            ZoneAction::Add => upsert_claim(town, claim, self.settings()),
            ZoneAction::Remove => remove_zone(&zone_id(town, claim)),
        };
        self.pending = Some(pending);
        vec![command]
    }

    fn zone_answered(&mut self, zone: &str, outcome: &ZoneCommandOutcome) -> Vec<Command> {
        if let Some((remaining, failed)) = &mut self.startup_zones {
            if remaining.remove(zone) {
                *failed |= matches!(outcome, ZoneCommandOutcome::Refused);
                if remaining.is_empty() {
                    self.loaded = !*failed;
                    self.startup_zones = None;
                }
                return Vec::new();
            }
        }
        let Some(mut pending) = self.pending.take() else {
            return Vec::new();
        };
        if !matches!(pending.stage, PendingStage::Zone) {
            self.pending = Some(pending);
            return Vec::new();
        }
        let town = self
            .towns
            .get(
                pending
                    .town_name
                    .as_deref()
                    .expect("zone update names its town"),
            )
            .expect("committed update retains its town");
        let claim = pending.claim.as_ref().expect("zone update names its claim");
        if zone_id(town, claim) != zone {
            self.pending = Some(pending);
            return Vec::new();
        }
        if matches!(outcome, ZoneCommandOutcome::Applied) {
            return vec![message_session(pending.session, pending.message)];
        }
        let request = format!(
            "rollback-v{}",
            self.storage_version
                .expect("committed town update has a storage version")
        );
        pending.request = request.clone();
        pending.stage = PendingStage::Rollback;
        let value = encode_towns(&pending.before);
        self.pending = Some(pending);
        vec![storage_cas(
            &request,
            STORAGE_KEY,
            self.storage_version,
            value,
        )]
    }

    fn online_players(&mut self, answered: &OnlinePlayersAnswered) -> Vec<Command> {
        let Some(query) = self.pending_queries.remove(&answered.request) else {
            return Vec::new();
        };
        let mut found = None;
        for player in &answered.players {
            if player.name.eq_ignore_ascii_case(&query.target_name) {
                if found.is_some() {
                    found = None;
                    break;
                }
                found = Some(player);
            }
        }
        let Some(found) = found else {
            return vec![message_session(
                query.session,
                "Online unclaimed player not found or ambiguous.",
            )];
        };
        let Some(uuid) = normalize_uuid(&found.player) else {
            return Vec::new();
        };
        if member_town(&self.towns, &uuid).is_some() {
            return vec![message_session(
                query.session,
                "Online unclaimed player not found or ambiguous.",
            )];
        }
        self.invites.insert(uuid.clone(), query.town.clone());
        let timer_id = format!("invite-{}", found.session);
        self.invite_timers.insert(timer_id.clone(), uuid);
        vec![
            schedule_timer(&timer_id, self.settings().invite_expiry_ticks),
            message_session(
                found.session,
                format!(
                    "{} invited you to {}. Use /town join {}.",
                    query.inviter, query.town, query.town
                ),
            ),
            message_session(query.session, "Invitation sent."),
        ]
    }

    fn timer(&mut self, timer_id: &str) {
        if let Some(uuid) = self.invite_timers.remove(timer_id) {
            self.invites.remove(&uuid);
        }
    }
}

impl Plugin for Towns {
    fn configure(
        &mut self,
        config: &Config,
    ) -> Result<Option<solaris_plugin_sdk::StartupContribution>, Failure> {
        Settings::parse(config)?;
        Ok(None)
    }

    fn init(&mut self, config: &Config, _context: &InitContext) -> Result<Vec<Command>, Failure> {
        self.settings = Some(Settings::parse(config)?);
        self.towns.clear();
        self.storage_version = None;
        self.loaded = false;
        self.pending = None;
        self.pending_queries.clear();
        self.invites.clear();
        self.invite_timers.clear();
        self.startup_zones = None;
        self.sequence = 0;
        let request = self.next_request("load");
        self.load_request = Some(request.clone());
        Ok(vec![storage_get(&request, STORAGE_KEY)])
    }

    fn on_events(
        &mut self,
        _context: &EventContext,
        events: &[Event],
    ) -> Result<Vec<Command>, Failure> {
        let mut commands = Vec::new();
        for event in events {
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
                Event::TimerFired(timer) => self.timer(&timer.timer_id),
                Event::ZoneCommandAnswered(answered) => {
                    commands.extend(self.zone_answered(&answered.zone, &answered.outcome));
                }
                Event::CommandBatchRejected => {
                    if self
                        .pending
                        .as_ref()
                        .is_some_and(|pending| matches!(pending.stage, PendingStage::Save))
                    {
                        self.pending = None;
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

fn required_i64(table: &Table, key: &str) -> Result<i64, Failure> {
    table
        .get(key)
        .and_then(Value::as_integer)
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
        && !path.contains(':')
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

fn valid_name(value: &str) -> bool {
    (2..=16).contains(&value.len())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn claim_key(x: i64, z: i64) -> String {
    format!("{x}:{z}")
}

fn coordinate_id(value: i64) -> String {
    if value < 0 {
        format!("n{}", value.unsigned_abs())
    } else {
        format!("p{value}")
    }
}

fn zone_id(town: &Town, claim: &Claim) -> String {
    format!(
        "town-{}-{}-{}",
        town.name,
        coordinate_id(claim.x),
        coordinate_id(claim.z)
    )
}

fn member_town<'a>(towns: &'a BTreeMap<String, Town>, uuid: &str) -> Option<&'a Town> {
    towns.values().find(|town| town.members.contains_key(uuid))
}

fn claim_owner<'a>(towns: &'a BTreeMap<String, Town>, key: &str) -> Option<&'a Town> {
    towns.values().find(|town| town.claims.contains_key(key))
}

fn total_claims(towns: &BTreeMap<String, Town>) -> usize {
    towns.values().map(|town| town.claims.len()).sum()
}

fn decode_towns(value: Option<&str>, settings: &Settings) -> Option<BTreeMap<String, Town>> {
    let Some(value) = value else {
        return Some(BTreeMap::new());
    };
    if value == "v1|" {
        return Some(BTreeMap::new());
    }
    let rows = value.strip_prefix("v1|")?;
    let mut towns = BTreeMap::new();
    for row in rows.split(';').filter(|row| !row.is_empty()) {
        let mut fields = row.split(',');
        let (Some(name), Some(leader), Some(members), Some(claims), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return None;
        };
        let leader = normalize_uuid(leader)?;
        if !valid_name(name) || towns.contains_key(name) {
            return None;
        }
        let mut town = Town {
            name: name.to_owned(),
            leader: leader.clone(),
            members: BTreeMap::new(),
            claims: BTreeMap::new(),
        };
        for member in members.split('+').filter(|member| !member.is_empty()) {
            let (uuid, role) = member.split_once(':')?;
            let uuid = normalize_uuid(uuid)?;
            let role = Role::parse(role)?;
            if town.members.insert(uuid, role).is_some() {
                return None;
            }
        }
        if town.members.get(&leader) != Some(&Role::Leader)
            || town.members.len() > settings.maximum_members_per_town
        {
            return None;
        }
        for claim in claims.split('+').filter(|claim| !claim.is_empty()) {
            let (x, z) = claim.split_once(':')?;
            let (Ok(x), Ok(z)) = (x.parse::<i64>(), z.parse::<i64>()) else {
                return None;
            };
            let key = claim_key(x, z);
            if town.claims.insert(key, Claim { x, z }).is_some() {
                return None;
            }
        }
        towns.insert(name.to_owned(), town);
        if towns.len() > settings.maximum_towns || total_claims(&towns) > settings.maximum_claims {
            return None;
        }
    }
    Some(towns)
}

fn encode_towns(towns: &BTreeMap<String, Town>) -> String {
    let rows = towns
        .values()
        .map(|town| {
            let members = town
                .members
                .iter()
                .map(|(uuid, role)| format!("{uuid}:{}", role.as_str()))
                .collect::<Vec<_>>()
                .join("+");
            let claims = town.claims.keys().cloned().collect::<Vec<_>>().join("+");
            format!("{},{},{members},{claims}", town.name, town.leader)
        })
        .collect::<Vec<_>>();
    format!("v1|{}", rows.join(";"))
}

fn upsert_claim(town: &Town, claim: &Claim, settings: &Settings) -> Command {
    commands::Command::UpsertProtectedZone(commands::UpsertProtectedZone {
        zone: zone_id(town, claim),
        dimension: settings.dimension.clone(),
        allowed_actor_uuid: town.leader.clone(),
        minimum: types::Position {
            x: (claim.x * 16) as f64,
            y: settings.minimum_y as f64,
            z: (claim.z * 16) as f64,
        },
        maximum: types::Position {
            x: (claim.x * 16 + 15) as f64,
            y: settings.maximum_y as f64,
            z: (claim.z * 16 + 15) as f64,
        },
    })
}

fn remove_zone(zone: &str) -> Command {
    commands::Command::RemoveZone(commands::RemoveZone {
        zone: zone.to_owned(),
    })
}

export_plugin!(Towns);
