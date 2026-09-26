//! Durable permission-group assignments for the Solaris standard plugin pack.
//!
//! The component preserves the Lua package's public command replies, storage key
//! and deterministic `v1|<uuid>,<group>;...` value. Configuration is parsed in
//! both lifecycle stores: `configure` validates it and `init` rebuilds all
//! runtime state before issuing the initial storage read.

use std::collections::{BTreeMap, BTreeSet};

use solaris_plugin_sdk::events::CommandInvoked;
use solaris_plugin_sdk::{
    export_plugin, message_session, storage_cas, storage_get, types, Command, Config, Event,
    EventContext, Failure, InitContext, Plugin, StorageCasOutcome, StorageGetOutcome,
};
use toml::{Table, Value};

const STORAGE_KEY: &str = "assignments-v1";
const MAX_ASSIGNMENTS: usize = 64;
const MAX_GROUPS: usize = 16;
const MAX_GROUP_NODES: usize = 64;
const MAX_NODE_BYTES: usize = 128;

#[derive(Clone)]
struct Settings {
    default_group: String,
    maximum_assignments: usize,
    groups: BTreeMap<String, BTreeSet<String>>,
}

impl Settings {
    fn parse(config: &Config) -> Result<Self, Failure> {
        let table = config
            .toml()
            .and_then(|value| value.as_table().cloned())
            .ok_or(Failure::Invalid)?;
        let default_group = required_string(&table, "default_group")?.to_owned();
        let maximum_assignments = table
            .get("maximum_assignments")
            .and_then(Value::as_integer)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| (1..=MAX_ASSIGNMENTS).contains(value))
            .ok_or(Failure::Invalid)?;
        let groups = table
            .get("groups")
            .and_then(Value::as_array)
            .filter(|groups| (1..=MAX_GROUPS).contains(&groups.len()))
            .ok_or(Failure::Invalid)?;
        let mut parsed_groups = BTreeMap::new();
        for group in groups {
            let table = group.as_table().ok_or(Failure::Invalid)?;
            let name = required_string(table, "name")?;
            if !valid_group_name(name) || parsed_groups.contains_key(name) {
                return Err(Failure::Invalid);
            }
            let nodes = table
                .get("nodes")
                .and_then(Value::as_array)
                .filter(|nodes| nodes.len() <= MAX_GROUP_NODES)
                .ok_or(Failure::Invalid)?;
            let mut parsed_nodes = BTreeSet::new();
            for node in nodes {
                let node = node.as_str().ok_or(Failure::Invalid)?;
                if !valid_node(node) {
                    return Err(Failure::Invalid);
                }
                parsed_nodes.insert(node.to_owned());
            }
            parsed_groups.insert(name.to_owned(), parsed_nodes);
        }
        if !parsed_groups.contains_key(&default_group) {
            return Err(Failure::Invalid);
        }
        Ok(Self {
            default_group,
            maximum_assignments,
            groups: parsed_groups,
        })
    }

    fn decode_assignments(&self, value: Option<&str>) -> Option<BTreeMap<String, String>> {
        let Some(value) = value else {
            return Some(BTreeMap::new());
        };
        if value == "v1|" {
            return Some(BTreeMap::new());
        }
        let rows = value.strip_prefix("v1|")?;
        let mut assignments = BTreeMap::new();
        for row in rows.split(';').filter(|row| !row.is_empty()) {
            let (uuid, group) = row.split_once(',')?;
            let uuid = normalize_uuid(uuid)?;
            if !self.groups.contains_key(group) || assignments.contains_key(&uuid) {
                return None;
            }
            assignments.insert(uuid, group.to_owned());
            if assignments.len() > self.maximum_assignments {
                return None;
            }
        }
        Some(assignments)
    }

    fn encode_assignments(&self, assignments: &BTreeMap<String, String>) -> String {
        let rows = assignments
            .iter()
            .map(|(uuid, group)| format!("{uuid},{group}"))
            .collect::<Vec<_>>();
        format!("v1|{}", rows.join(";"))
    }

    fn effective_group<'a>(
        &'a self,
        assignments: &'a BTreeMap<String, String>,
        uuid: &str,
    ) -> &'a str {
        assignments
            .get(uuid)
            .map(String::as_str)
            .unwrap_or(&self.default_group)
    }

    fn has_node(
        &self,
        assignments: &BTreeMap<String, String>,
        uuid: &str,
        node: &str,
        context: Option<&str>,
    ) -> bool {
        let nodes = &self.groups[self.effective_group(assignments, uuid)];
        nodes.contains("*")
            || context.is_some_and(|context| nodes.contains(&format!("{node}@{context}")))
            || nodes.contains(node)
    }
}

struct PendingUpdate {
    request: String,
    session: types::SessionId,
    assignments: BTreeMap<String, String>,
    message: String,
}

#[derive(Default)]
struct Permissions {
    settings: Option<Settings>,
    assignments: BTreeMap<String, String>,
    storage_version: Option<u64>,
    loaded: bool,
    pending: Option<PendingUpdate>,
    sequence: u64,
}

impl Permissions {
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
        session: types::SessionId,
        assignments: BTreeMap<String, String>,
        message: String,
    ) -> Vec<Command> {
        if self.pending.is_some() {
            return vec![message_session(
                session,
                "Another permission update is committing; retry.",
            )];
        }
        let revision = self
            .storage_version
            .map_or_else(|| "new".to_owned(), |version| version.to_string());
        let request = format!("save-v{revision}");
        let value = self.settings().encode_assignments(&assignments);
        self.pending = Some(PendingUpdate {
            request: request.clone(),
            session,
            assignments,
            message,
        });
        vec![storage_cas(
            &request,
            STORAGE_KEY,
            self.storage_version,
            value,
        )]
    }

    fn command(&mut self, invoked: &CommandInvoked) -> Vec<Command> {
        if !self.loaded {
            return vec![message_session(
                invoked.session,
                "Permissions are still loading.",
            )];
        }
        let Some(actor) = normalize_uuid(&invoked.player) else {
            return Vec::new();
        };
        let words = invoked
            .arguments
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        match words.first().copied() {
            None | Some("groups") => {
                let groups = self
                    .settings()
                    .groups
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                vec![message_session(
                    invoked.session,
                    format!("Groups: {groups}"),
                )]
            }
            Some("check") if (2..=3).contains(&words.len()) => {
                let node = words[1].to_ascii_lowercase();
                let context = words.get(2).map(|context| context.to_ascii_lowercase());
                let allowed =
                    self.settings()
                        .has_node(&self.assignments, &actor, &node, context.as_deref());
                vec![message_session(
                    invoked.session,
                    if allowed {
                        "Permission granted."
                    } else {
                        "Permission denied."
                    },
                )]
            }
            Some("user") if words.len() >= 3 => self.user_command(invoked, &actor, &words),
            _ => vec![message_session(
                invoked.session,
                "Usage: /perm <groups|check node [context]|user ...>",
            )],
        }
    }

    fn user_command(
        &mut self,
        invoked: &CommandInvoked,
        actor: &str,
        words: &[&str],
    ) -> Vec<Command> {
        if !invoked.operator {
            return vec![message_session(
                invoked.session,
                "Only an operator can change groups.",
            )];
        }
        let target = if words[1] == "me" {
            Some(actor.to_owned())
        } else {
            normalize_uuid(words[1])
        };
        let Some(target) = target else {
            return vec![message_session(invoked.session, "Use a player UUID or me.")];
        };
        match (words[2], words.len()) {
            ("list", 3) => vec![message_session(
                invoked.session,
                format!(
                    "{target} is {}.",
                    self.settings().effective_group(&self.assignments, &target)
                ),
            )],
            ("clear", 3) => {
                let mut assignments = self.assignments.clone();
                assignments.remove(&target);
                let message = format!("Group reset to {}.", self.settings().default_group);
                self.save(invoked.session, assignments, message)
            }
            ("set", 4) => {
                let group = words[3].to_ascii_lowercase();
                if !self.settings().groups.contains_key(&group) {
                    return vec![message_session(invoked.session, "Unknown group.")];
                }
                if !self.assignments.contains_key(&target)
                    && self.assignments.len() >= self.settings().maximum_assignments
                {
                    return vec![message_session(
                        invoked.session,
                        "Assignment limit reached.",
                    )];
                }
                let mut assignments = self.assignments.clone();
                assignments.insert(target, group.clone());
                self.save(
                    invoked.session,
                    assignments,
                    format!("Group set to {group}."),
                )
            }
            _ => vec![message_session(
                invoked.session,
                "Usage: /perm user <uuid|me> <set group|clear|list>",
            )],
        }
    }

    fn storage_get(&mut self, outcome: &StorageGetOutcome) {
        let StorageGetOutcome::Read(record) = outcome else {
            self.loaded = false;
            return;
        };
        let Some(assignments) = self.settings().decode_assignments(record.value.as_deref()) else {
            self.loaded = false;
            return;
        };
        self.assignments = assignments;
        self.storage_version = record.version;
        self.loaded = true;
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
            self.loaded = false;
            let request = self.next_request("reload");
            return vec![
                message_session(
                    pending.session,
                    "Permission update conflicted; reload required.",
                ),
                storage_get(&request, STORAGE_KEY),
            ];
        };
        self.assignments = pending.assignments;
        self.storage_version = Some(*version);
        vec![message_session(pending.session, pending.message)]
    }
}

impl Plugin for Permissions {
    fn configure(
        &mut self,
        config: &Config,
    ) -> Result<Option<solaris_plugin_sdk::StartupContribution>, Failure> {
        Settings::parse(config)?;
        Ok(None)
    }

    fn init(&mut self, config: &Config, _context: &InitContext) -> Result<Vec<Command>, Failure> {
        self.settings = Some(Settings::parse(config)?);
        self.assignments.clear();
        self.storage_version = None;
        self.loaded = false;
        self.pending = None;
        self.sequence = 0;
        let request = self.next_request("load");
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
                Event::CommandInvoked(invoked) if invoked.name == "perm" => {
                    commands.extend(self.command(invoked));
                }
                Event::CommandBatchRejected => self.pending = None,
                Event::StorageGetAnswered(answered) => self.storage_get(&answered.outcome),
                Event::StorageCasAnswered(answered) => {
                    commands.extend(self.storage_cas(&answered.request, &answered.outcome));
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

fn valid_group_name(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn valid_node(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_NODE_BYTES {
        return false;
    }
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !valid_node_name_byte(first) {
        return false;
    }
    let mut suffix = false;
    for byte in bytes {
        if byte == b'@' {
            if suffix {
                return false;
            }
            suffix = true;
        } else if suffix {
            if !valid_context_byte(byte) {
                return false;
            }
        } else if !valid_node_name_byte(byte) {
            if !valid_context_byte(byte) {
                return false;
            }
            suffix = true;
        }
    }
    true
}

fn valid_node_name_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'*' | b'-')
}

fn valid_context_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase()
        || byte.is_ascii_digit()
        || matches!(byte, b'_' | b'.' | b':' | b'/' | b'-')
}

fn normalize_uuid(value: &str) -> Option<String> {
    let normalized = value.replace('-', "").to_ascii_lowercase();
    (normalized.len() == 32 && normalized.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(normalized)
}

export_plugin!(Permissions);
