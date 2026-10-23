//! P7 behavioral acceptance for the first-party standard components.
//!
//! These tests live with their component packages. The core build and core-only
//! tests do not depend on this sibling; the `standard-pack` gate runs them here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mc_plugin_host::bindings::exports::solaris::plugin::events::{
    CommandInvoked, Event, EventContext, OperationAnswered, OperationOutcome, OperationPayload,
    OperationRefused, StorageCasAnswered, StorageGetAnswered, ZoneCommandAnswered,
    ZoneCommandOutcome,
};
use mc_plugin_host::bindings::exports::solaris::plugin::lifecycle::InitContext;
use mc_plugin_host::bindings::solaris::plugin::commands::{
    Command, MessageTarget, StorageMutation,
};
use mc_plugin_host::bindings::solaris::plugin::operation_types::OperationFailure;
use mc_plugin_host::bindings::solaris::plugin::storage::{
    StorageCasOutcome, StorageGetOutcome, StorageRecord,
};
use mc_plugin_host::bindings::solaris::plugin::types::Position;
use mc_plugin_host::{
    CompiledPlugin, DeploymentConfig, DiscoveryMode, HostQueues, HostServices, LogLevel,
    NoSessions, PlayerSessions, PluginInstance, PluginLimits, PluginStartup, engine, linker,
    start_deployment,
};
use mc_script::{
    PlayerCommandAdmission, ScriptBoundary, ScriptCommand, ScriptPlayerContext, ScriptPlayerId,
};

const STANDARD_PACK: [(&str, &[&str]); 5] = [
    ("solaris-permissions", &["storage"]),
    (
        "solaris-essentials",
        &["storage", "player_teleport", "player_queries"],
    ),
    ("solaris-economy", &["storage", "storage_batches"]),
    ("solaris-towns", &["storage", "zones", "player_queries"]),
    ("solaris-audit", &["storage"]),
];

const API_VERSION: &str = "0.8.0";
const PLAYER: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const SESSION: u64 = 7;

#[derive(Default)]
struct Services {
    id: String,
}

impl HostServices for Services {
    fn log(&mut self, _level: LogLevel, _message: &str) {}

    fn plugin_id(&self) -> &str {
        &self.id
    }
}

struct Sessions;

impl PlayerSessions for Sessions {
    fn session_of(&self, player: &str) -> Option<u64> {
        (player == PLAYER).then_some(SESSION)
    }
}

fn player_context() -> ScriptPlayerContext {
    ScriptPlayerContext::try_new(PLAYER, "Ada", false, 32.0, 64.0, -16.0)
        .expect("bounded standard-pack player context")
}

fn enqueue_money(boundary: &ScriptBoundary) {
    assert_eq!(
        boundary.try_enqueue_player_command_with_context(
            ScriptPlayerId::new(SESSION),
            player_context(),
            "money",
        ),
        Ok(PlayerCommandAdmission::Enqueued),
        "the economy command is admitted to its live component"
    );
}

async fn accept_chat_message(boundary: &ScriptBoundary) {
    let command = tokio::time::timeout(Duration::from_secs(10), boundary.recv_command())
        .await
        .expect("the hosted standard pack answers within the bounded wait")
        .expect("the standard-pack command channel remains open");
    let admitted = boundary
        .accept_host_command(command)
        .expect("the host admits the standard-pack response");
    assert!(
        matches!(admitted.request(), ScriptCommand::SendChatMessage { .. }),
        "the loaded economy component returns a chat response"
    );
}

fn nearest_rank_us(samples: &mut [Duration], percent: usize) -> f64 {
    samples.sort_unstable();
    let rank = (percent * samples.len()).div_ceil(100);
    samples[rank.saturating_sub(1).min(samples.len() - 1)].as_secs_f64() * 1e6
}

fn peak_rss_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("Linux reports process RSS");
    let line = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .expect("Linux reports peak resident set");
    line.split_whitespace()
        .nth(1)
        .expect("VmHWM carries KiB")
        .parse()
        .expect("VmHWM is numeric")
}

fn process_cpu_ticks() -> (u64, u64) {
    let stat = std::fs::read_to_string("/proc/self/stat").expect("Linux reports process CPU time");
    let (_, fields) = stat
        .rsplit_once(") ")
        .expect("Linux process stat has a comm terminator");
    let fields = fields.split_whitespace().collect::<Vec<_>>();
    (
        fields[11].parse().expect("process utime is numeric"),
        fields[12].parse().expect("process stime is numeric"),
    )
}

fn plugins_root() -> PathBuf {
    if let Some(root) = std::env::var_os("SOLARIS_DEFAULT_PLUGINS_ROOT") {
        return PathBuf::from(root);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("integration test crate belongs to the plugin repository")
        .to_path_buf()
}

fn package_path(id: &str) -> PathBuf {
    plugins_root().join(id)
}

fn staged_standard_pack() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("staging directory");
    for (id, _) in STANDARD_PACK {
        let source = package_path(id);
        let target = root.path().join(id);
        std::fs::create_dir(&target).expect("package staging directory");
        for name in ["plugin.toml", "plugin.wasm", "config.toml"] {
            std::fs::copy(source.join(name), target.join(name))
                .unwrap_or_else(|error| panic!("stages {id}/{name}: {error}"));
        }
    }
    root
}

fn standard_pack_deployment(root: &Path) -> DeploymentConfig {
    DeploymentConfig {
        root: root.to_path_buf(),
        mode: DiscoveryMode::Strict,
        expected: STANDARD_PACK
            .into_iter()
            .map(|(id, _)| id.to_owned())
            .collect(),
        grants: STANDARD_PACK
            .into_iter()
            .map(|(id, capabilities)| {
                (
                    id.to_owned(),
                    capabilities
                        .iter()
                        .map(|capability| (*capability).to_owned())
                        .collect(),
                )
            })
            .collect::<BTreeMap<_, _>>(),
        require_grants: true,
        precommit_hooks: Vec::new(),
    }
}

fn standard_pack_packages(
    root: &Path,
    limits: &PluginLimits,
) -> Vec<mc_plugin_host::LoadedPackage> {
    mc_plugin_host::discover(&standard_pack_deployment(root), limits)
        .expect("strict standard-pack deployment discovers")
        .into_packages()
}

fn instance(id: &str) -> (PluginInstance<Services>, Vec<Command>) {
    let package = package_path(id);
    let component = std::fs::read(package.join("plugin.wasm")).expect("tracked component exists");
    let config =
        std::fs::read_to_string(package.join("config.toml")).expect("package config exists");
    let limits = PluginLimits::default();
    let engine = engine(&limits).expect("host engine builds");
    let compiled = CompiledPlugin::compile(&engine, &component, &limits, API_VERSION)
        .expect("tracked component compiles under the current host contract");
    let linker = linker::<Services>(&engine).expect("host linker builds");
    let mut startup = PluginStartup::instantiate(
        &linker,
        compiled.component(),
        Services { id: id.to_owned() },
        limits,
    )
    .expect("startup instance builds");
    assert!(
        startup
            .configure(&config)
            .expect("package configuration is accepted")
            .is_none(),
        "standard packages do not declare startup contributions"
    );
    let mut plugin = PluginInstance::instantiate(
        &linker,
        compiled.component(),
        Services { id: id.to_owned() },
        limits,
    )
    .expect("runtime instance builds");
    let commands = plugin
        .init(
            &config,
            InitContext {
                plugin_id: id.to_owned(),
                api_version: API_VERSION.to_owned(),
                world_fingerprint: String::new(),
            },
        )
        .expect("package initialization is accepted")
        .into_commands();
    (plugin, commands)
}

fn context(count: u32) -> EventContext {
    EventContext {
        tick: 42,
        first_sequence: 0,
        count,
    }
}

fn command(name: &str, arguments: &[&str], raw_arguments: &str, operator: bool) -> Event {
    Event::CommandInvoked(CommandInvoked {
        player: PLAYER.to_owned(),
        session: SESSION,
        username: "Ada".to_owned(),
        operator,
        position: Position {
            x: 32.0,
            y: 64.0,
            z: -16.0,
        },
        name: name.to_owned(),
        arguments: arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
        raw_arguments: raw_arguments.to_owned(),
    })
}

fn storage_get(commands: Vec<Command>) -> (String, String) {
    assert_eq!(
        commands.len(),
        1,
        "initialization has one bounded storage request"
    );
    match commands.into_iter().next().expect("one command") {
        Command::StorageGet(get) => (get.request, get.key),
        other => panic!("expected storage get, saw {other:?}"),
    }
}

fn storage_key(commands: Vec<Command>) -> String {
    storage_get(commands).1
}

fn storage_read(request: String) -> Event {
    Event::StorageGetAnswered(StorageGetAnswered {
        request,
        outcome: StorageGetOutcome::Read(StorageRecord {
            value: None,
            version: None,
        }),
    })
}

fn storage_cas(commands: Vec<Command>) -> (String, String) {
    assert_eq!(
        commands.len(),
        1,
        "one bounded storage mutation is expected"
    );
    match commands.into_iter().next().expect("one command") {
        Command::StorageCas(cas) => (cas.request, cas.key),
        other => panic!("expected storage compare-and-swap, saw {other:?}"),
    }
}

fn storage_cas_committed(request: String) -> Event {
    Event::StorageCasAnswered(StorageCasAnswered {
        request,
        outcome: StorageCasOutcome::Committed(1),
    })
}

fn storage_cas_refused(request: String) -> Event {
    Event::StorageCasAnswered(StorageCasAnswered {
        request,
        outcome: StorageCasOutcome::Refused,
    })
}

fn session_message(commands: Vec<Command>) -> String {
    assert_eq!(commands.len(), 1, "one command reply is expected");
    match commands.into_iter().next().expect("one command") {
        Command::SendMessage(message) => {
            match message.target {
                MessageTarget::Session(session) => assert_eq!(session, SESSION),
                other => panic!("expected session reply, saw {other:?}"),
            }
            message.text
        }
        other => panic!("expected a session message, saw {other:?}"),
    }
}

#[test]
#[ignore = "requires the explicit first-party sibling integration gate"]
fn standard_components_preserve_their_durable_initialization_keys() {
    for (id, expected_key) in [
        ("solaris-permissions", Some("assignments-v1")),
        ("solaris-essentials", None),
        ("solaris-economy", Some("ledger-v1")),
        ("solaris-towns", Some("towns-v1")),
        ("solaris-audit", Some("actions-v1")),
    ] {
        let (mut plugin, commands) = instance(id);
        match expected_key {
            Some(key) => assert_eq!(storage_key(commands), key, "{id} storage identity"),
            None => assert!(
                commands.is_empty(),
                "{id} does not load durable state at init"
            ),
        }
        plugin.shutdown().expect("clean component shutdown");
    }
}

#[test]
#[ignore = "requires the explicit first-party sibling integration gate"]
fn standard_component_commands_keep_declared_state_boundaries() {
    let (mut essentials, init) = instance("solaris-essentials");
    assert!(
        init.is_empty(),
        "essentials initializes without a global storage read"
    );
    let home_request = essentials
        .on_events(context(1), &[command("sethome", &["base"], "base", false)])
        .expect("sethome delivery succeeds")
        .into_commands();
    assert_eq!(
        storage_key(home_request),
        format!("homes:{}", PLAYER.replace('-', "")),
        "home state remains bound to the caller UUID"
    );
    essentials.shutdown().expect("clean component shutdown");

    for (id, name, arguments, raw_arguments, operator, expected) in [
        (
            "solaris-permissions",
            "perm",
            vec!["groups"],
            "groups",
            false,
            "Permissions are still loading.",
        ),
        (
            "solaris-economy",
            "money",
            Vec::new(),
            "",
            false,
            "Economy is still loading.",
        ),
        (
            "solaris-towns",
            "town",
            Vec::new(),
            "",
            false,
            "Towns are still loading.",
        ),
        (
            "solaris-audit",
            "audit",
            Vec::new(),
            "",
            true,
            "Audit history is still loading.",
        ),
    ] {
        let (mut plugin, _) = instance(id);
        let events = [command(name, &arguments, raw_arguments, operator)];
        let reply = plugin
            .on_events(context(1), &events)
            .expect("command delivery succeeds")
            .into_commands();
        assert_eq!(
            session_message(reply),
            expected,
            "{id} refuses before its durable state loads"
        );
        plugin.shutdown().expect("clean component shutdown");
    }
}

#[test]
#[ignore = "requires the explicit first-party sibling integration gate"]
fn standard_components_apply_normal_storage_aware_commands() {
    let (mut permissions, init) = instance("solaris-permissions");
    let (request, key) = storage_get(init);
    assert_eq!(key, "assignments-v1");
    assert!(
        permissions
            .on_events(context(1), &[storage_read(request)])
            .expect("permissions storage load succeeds")
            .into_commands()
            .is_empty()
    );
    let groups = permissions
        .on_events(context(1), &[command("perm", &["groups"], "groups", false)])
        .expect("permissions command succeeds after its storage load")
        .into_commands();
    assert_eq!(session_message(groups), "Groups: admin, moderator, player");
    permissions.shutdown().expect("clean component shutdown");

    let (mut economy, init) = instance("solaris-economy");
    let (request, key) = storage_get(init);
    assert_eq!(key, "ledger-v1");
    assert!(
        economy
            .on_events(context(1), &[storage_read(request)])
            .expect("economy storage load succeeds")
            .into_commands()
            .is_empty()
    );
    let balance = economy
        .on_events(context(1), &[command("money", &[], "", false)])
        .expect("economy command succeeds after its storage load")
        .into_commands();
    assert_eq!(session_message(balance), "Balance: 100 coins.");
    economy.shutdown().expect("clean component shutdown");

    let (mut towns, init) = instance("solaris-towns");
    let (request, key) = storage_get(init);
    assert_eq!(key, "towns-v1");
    assert!(
        towns
            .on_events(context(1), &[storage_read(request)])
            .expect("town storage load succeeds")
            .into_commands()
            .is_empty()
    );
    let create = towns
        .on_events(
            context(1),
            &[command("town", &["create", "oak"], "create oak", false)],
        )
        .expect("town creation command succeeds after its storage load")
        .into_commands();
    let (request, key) = storage_cas(create);
    assert_eq!(key, "towns-v1");
    let created = towns
        .on_events(context(1), &[storage_cas_committed(request)])
        .expect("town creation receives its committed storage outcome")
        .into_commands();
    assert_eq!(session_message(created), "Town oak created.");
    towns.shutdown().expect("clean component shutdown");

    let (mut audit, init) = instance("solaris-audit");
    let (request, key) = storage_get(init);
    assert_eq!(key, "actions-v1");
    assert!(
        audit
            .on_events(context(1), &[storage_read(request)])
            .expect("audit storage load succeeds")
            .into_commands()
            .is_empty()
    );
    let records = audit
        .on_events(context(1), &[command("audit", &[], "", true)])
        .expect("audit command succeeds after its storage load")
        .into_commands();
    assert_eq!(
        session_message(records),
        "No matching bounded audit records."
    );
    audit.shutdown().expect("clean component shutdown");

    let (mut essentials, _) = instance("solaris-essentials");
    let read = essentials
        .on_events(context(1), &[command("sethome", &["base"], "base", false)])
        .expect("sethome requests its durable state")
        .into_commands();
    let (request, key) = storage_get(read);
    assert_eq!(key, format!("homes:{}", PLAYER.replace('-', "")));
    let save = essentials
        .on_events(context(1), &[storage_read(request)])
        .expect("sethome handles an empty durable record")
        .into_commands();
    let (request, key) = storage_cas(save);
    assert_eq!(key, format!("homes:{}", PLAYER.replace('-', "")));
    let saved = essentials
        .on_events(context(1), &[storage_cas_committed(request)])
        .expect("sethome receives its committed storage outcome")
        .into_commands();
    assert_eq!(session_message(saved), "base saved.");
    essentials.shutdown().expect("clean component shutdown");
}

#[test]
#[ignore = "requires the explicit first-party sibling integration gate"]
fn standard_components_report_durable_conflicts() {
    let (mut essentials, _) = instance("solaris-essentials");
    let read = essentials
        .on_events(context(1), &[command("sethome", &["base"], "base", false)])
        .expect("sethome requests durable state")
        .into_commands();
    let (request, _) = storage_get(read);
    let save = essentials
        .on_events(context(1), &[storage_read(request)])
        .expect("sethome receives durable state")
        .into_commands();
    let (request, _) = storage_cas(save);
    let conflict = essentials
        .on_events(context(1), &[storage_cas_refused(request)])
        .expect("sethome receives a rejected durable write")
        .into_commands();
    assert_eq!(
        session_message(conflict),
        "Location changed concurrently; retry."
    );
    essentials.shutdown().expect("clean component shutdown");

    let (mut towns, init) = instance("solaris-towns");
    let (request, _) = storage_get(init);
    assert!(
        towns
            .on_events(context(1), &[storage_read(request)])
            .expect("towns receives durable state")
            .into_commands()
            .is_empty()
    );
    let save = towns
        .on_events(
            context(1),
            &[command("town", &["create", "oak"], "create oak", false)],
        )
        .expect("town creation requests a durable write")
        .into_commands();
    let (request, _) = storage_cas(save);
    let conflict = towns
        .on_events(context(1), &[storage_cas_refused(request)])
        .expect("town creation receives a rejected durable write")
        .into_commands();
    assert_eq!(
        session_message(conflict),
        "Town data changed concurrently; retry."
    );
    towns.shutdown().expect("clean component shutdown");
}

fn zone_upsert(commands: Vec<Command>) -> (Vec<String>, bool, String) {
    assert_eq!(commands.len(), 1, "one claim zone is rewritten");
    match commands.into_iter().next().expect("one command") {
        Command::UpsertProtectedZone(zone) => {
            (zone.allowed_actor_uuids, zone.public_use, zone.zone)
        }
        other => panic!("expected a protected zone upsert, saw {other:?}"),
    }
}

fn zone_answered(zone: String, outcome: ZoneCommandOutcome) -> Event {
    Event::ZoneCommandAnswered(ZoneCommandAnswered { zone, outcome })
}

#[test]
#[ignore = "requires the explicit first-party sibling integration gate"]
fn towns_trust_and_flags_rewrite_every_claim_allow_list() {
    const GUEST: &str = "11111111-2222-3333-4444-555555555555";
    let leader = PLAYER.replace('-', "");
    let guest = GUEST.replace('-', "");
    let (mut towns, init) = instance("solaris-towns");
    let (request, _) = storage_get(init);
    towns
        .on_events(context(1), &[storage_read(request)])
        .expect("town storage load succeeds");
    let mut send = |event: Event| {
        towns
            .on_events(context(1), &[event])
            .expect("towns handles the event")
            .into_commands()
    };
    // Create the town, then claim the chunk under the leader's feet.
    let (request, _) = storage_cas(send(command("town", &["create", "oak"], "create oak", false)));
    assert_eq!(
        session_message(send(storage_cas_committed(request))),
        "Town oak created."
    );
    let (request, _) = storage_cas(send(command("town", &["claim"], "claim", false)));
    let (allowed, public_use, zone) = zone_upsert(send(storage_cas_committed(request)));
    assert_eq!((allowed, public_use), (vec![leader.clone()], false));
    assert_eq!(
        session_message(send(zone_answered(zone.clone(), ZoneCommandOutcome::Applied))),
        "Chunk claimed for oak."
    );

    // Trusting a guest rewrites the claim with leader + guest.
    let (request, _) = storage_cas(send(command("town", &["trust", GUEST], GUEST, false)));
    let (allowed, public_use, zone) = zone_upsert(send(storage_cas_committed(request)));
    assert_eq!(allowed, vec![leader.clone(), guest.clone()]);
    assert!(!public_use);
    assert_eq!(
        session_message(send(zone_answered(zone.clone(), ZoneCommandOutcome::Applied))),
        "Guest trusted in your claims."
    );
    // Trusting the same guest twice is refused before any write.
    assert_eq!(
        session_message(send(command("town", &["trust", GUEST], GUEST, false))),
        "That player is already allowed in your claims."
    );

    // The leader opens doors to everyone.
    let (request, _) = storage_cas(send(command("town", &["flag", "use", "on"], "flag use on", false)));
    let (allowed, public_use, zone) = zone_upsert(send(storage_cas_committed(request)));
    assert_eq!(allowed, vec![leader.clone(), guest.clone()]);
    assert!(public_use);
    assert!(
        session_message(send(zone_answered(zone.clone(), ZoneCommandOutcome::Applied)))
            .starts_with("Everyone may now use doors")
    );

    // Untrusting removes the guest from the claim again.
    let (request, _) = storage_cas(send(command("town", &["untrust", GUEST], GUEST, false)));
    let (allowed, public_use, zone) = zone_upsert(send(storage_cas_committed(request)));
    assert_eq!(allowed, vec![leader.clone()]);
    assert!(public_use);
    assert_eq!(
        session_message(send(zone_answered(zone, ZoneCommandOutcome::Applied))),
        "Guest no longer trusted."
    );

    // A refused zone rewrite rolls the durable change back and restores the zone.
    let (request, _) = storage_cas(send(command("town", &["trust", GUEST], GUEST, false)));
    let (_, _, zone) = zone_upsert(send(storage_cas_committed(request)));
    let (request, _) = storage_cas(send(zone_answered(zone, ZoneCommandOutcome::Refused)));
    let restored = send(storage_cas_committed(request));
    assert_eq!(restored.len(), 2, "the old allow-list is re-sent with the notice");
    let message = restored
        .into_iter()
        .filter_map(|command| match command {
            Command::UpsertProtectedZone(zone) => {
                assert_eq!(zone.allowed_actor_uuids, vec![leader.clone()]);
                None
            }
            Command::SendMessage(message) => Some(message.text),
            other => panic!("unexpected rollback command {other:?}"),
        })
        .next()
        .expect("rollback notice");
    assert_eq!(
        message,
        "Claim protection failed; the town change was rolled back."
    );
    drop(send);
    towns.shutdown().expect("clean component shutdown");
}

#[test]
#[ignore = "requires the explicit first-party sibling integration gate"]
fn economy_payment_uses_durable_receipt_and_reloads_after_a_lost_reply() {
    let (mut economy, init) = instance("solaris-economy");
    let (request, key) = storage_get(init);
    assert_eq!(key, "ledger-v1");
    let actor = PLAYER.replace('-', "");
    let old_ledger = format!("v1|{actor},100|{actor}:legacy");
    assert!(
        economy
            .on_events(
                context(1),
                &[Event::StorageGetAnswered(StorageGetAnswered {
                    request,
                    outcome: StorageGetOutcome::Read(StorageRecord {
                        value: Some(old_ledger),
                        version: Some(4),
                    }),
                })],
            )
            .unwrap()
            .into_commands()
            .is_empty()
    );
    let recipient = "ffffffffffffffffffffffffffffffff";
    let raw = format!("{recipient} 7 deposit");
    let commands = economy
        .on_events(context(1), &[command("pay", &[], &raw, false)])
        .unwrap()
        .into_commands();
    let [Command::OperationStatus(status)] = commands.as_slice() else {
        panic!("a payment must check its durable receipt before issuing an effect: {commands:?}");
    };
    let operation_id = format!("pay-{actor}-deposit");
    assert_eq!(status.operation_id, operation_id);
    let answered = Event::OperationAnswered(OperationAnswered {
        request: status.request.clone(),
        operation_id: Some(operation_id.clone()),
        outcome: OperationOutcome::Refused(OperationRefused {
            reason: OperationFailure::NotFound,
            payload: OperationPayload::None,
        }),
    });
    let commands = economy
        .on_events(context(1), &[answered])
        .unwrap()
        .into_commands();
    let [Command::StorageBatchCas(batch)] = commands.as_slice() else {
        panic!("receipt absence starts one durable payment batch: {commands:?}");
    };
    assert_eq!(batch.operation_id, operation_id);
    let [StorageMutation::Cas(mutation)] = batch.mutations.as_slice() else {
        panic!("the payment changes only the existing durable ledger");
    };
    assert_eq!(mutation.key, "ledger-v1");
    assert_eq!(mutation.expected_version, Some(4));
    let ledger = mutation.value.clone();
    assert_eq!(
        ledger,
        format!("v1|{actor},93;{recipient},107|{actor}:legacy;{actor}:deposit")
    );
    // The callback is lost between an owner commit and guest finalization.
    // Reopen a new Store with the ledger image the batch named; native storage
    // receipt and the actual server route are exercised by separate tests.
    economy.shutdown().unwrap();

    let (mut replacement, init) = instance("solaris-economy");
    let (request, _) = storage_get(init);
    assert!(
        replacement
            .on_events(
                context(1),
                &[Event::StorageGetAnswered(StorageGetAnswered {
                    request,
                    outcome: StorageGetOutcome::Read(StorageRecord {
                        value: Some(ledger),
                        version: Some(5),
                    }),
                })],
            )
            .unwrap()
            .into_commands()
            .is_empty()
    );
    let retry = replacement
        .on_events(context(1), &[command("pay", &[], &raw, false)])
        .unwrap()
        .into_commands();
    assert_eq!(
        session_message(retry),
        "That transfer token was already committed."
    );
    let balance = replacement
        .on_events(context(1), &[command("money", &[], "", false)])
        .unwrap()
        .into_commands();
    assert_eq!(session_message(balance), "Balance: 93 coins.");
    replacement.shutdown().unwrap();
}

async fn answer_empty_initial_storage_reads(boundary: &ScriptBoundary) -> Vec<String> {
    let mut keys = Vec::new();
    for _ in 0..4 {
        let command = tokio::time::timeout(Duration::from_secs(10), boundary.recv_command())
            .await
            .expect("storage read reaches the host boundary")
            .expect("the running standard pack keeps its command channel open");
        let admitted = boundary
            .accept_host_command(command)
            .expect("host accepts the standard-pack storage read");
        let ScriptCommand::PluginStorageGet { request } = admitted.request() else {
            panic!(
                "expected initialization storage read, saw {:?}",
                admitted.request()
            );
        };
        keys.push(request.key().to_owned());
        boundary
            .try_enqueue_event(
                admitted
                    .plugin_storage_get_result(None, None)
                    .expect("the storage read admits an empty durable record"),
            )
            .expect("empty storage result reaches its owning component");
    }
    keys.sort_unstable();
    keys
}

#[tokio::test]
#[ignore = "requires the explicit first-party sibling integration gate"]
async fn zero_plugin_deployment_keeps_the_component_boundary_empty() {
    let host = start_deployment(
        Vec::new(),
        PluginLimits::default(),
        HostQueues::default(),
        Arc::new(Sessions),
    )
    .expect("the zero-plugin deployment starts without a synthetic component");
    assert!(
        host.boundary().deployed_packages().is_empty(),
        "the deployment catalog reports no packages"
    );
    assert!(
        host.boundary().player_command_roots().is_empty(),
        "no component owns a command route"
    );
    assert!(
        host.stop().is_empty(),
        "zero plugin instances leave no worker diagnostics"
    );
}

#[tokio::test]
#[ignore = "requires the explicit first-party sibling integration gate"]
async fn all_shipped_components_start_together_under_the_current_host() {
    let staged = staged_standard_pack();
    let target = staged.path().join("basic-economy");
    std::fs::create_dir(&target).expect("basic-economy staging directory");
    for name in ["plugin.toml", "plugin.wasm", "config.toml"] {
        std::fs::copy(package_path("basic-economy").join(name), target.join(name))
            .unwrap_or_else(|error| panic!("stages basic-economy/{name}: {error}"));
    }
    let mut deployment = standard_pack_deployment(staged.path());
    deployment.expected.push("basic-economy".to_owned());
    deployment.grants.insert(
        "basic-economy".to_owned(),
        [
            "storage",
            "inventory_menus",
            "inventory_storage_transactions",
            "zones",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    );
    let limits = PluginLimits::default();
    let packages = mc_plugin_host::discover(&deployment, &limits)
        .expect("all shipped manifests match the current host capability contract")
        .into_packages();
    let host = start_deployment(
        packages,
        limits,
        HostQueues::default(),
        Arc::new(NoSessions),
    )
    .expect("all shipped components instantiate together against the current WIT");
    assert_eq!(
        host.boundary()
            .deployed_packages()
            .iter()
            .map(|package| package.plugin_id())
            .collect::<Vec<_>>(),
        [
            "basic-economy",
            "solaris-audit",
            "solaris-economy",
            "solaris-essentials",
            "solaris-permissions",
            "solaris-towns",
        ],
        "the running catalog contains exactly the six shipped components"
    );
    assert_eq!(
        host.stop().len(),
        STANDARD_PACK.len() + 1,
        "the host returns shutdown counters for every shipped component"
    );
}

#[tokio::test]
#[ignore = "requires the explicit first-party sibling integration gate"]
async fn standard_pack_remains_bounded_while_durable_storage_is_slow() {
    let staged = staged_standard_pack();
    let limits = PluginLimits::default();
    let host = start_deployment(
        standard_pack_packages(staged.path(), &limits),
        limits,
        HostQueues::default(),
        Arc::new(Sessions),
    )
    .expect("strict standard-pack deployment starts");
    let boundary = host.boundary().clone();

    let mut keys = Vec::new();
    for _ in 0..4 {
        let command = tokio::time::timeout(Duration::from_secs(10), boundary.recv_command())
            .await
            .expect("the deferred storage read reaches the host boundary")
            .expect("the running standard pack keeps its command channel open");
        let admitted = boundary
            .accept_host_command(command)
            .expect("the host admits the deferred storage read");
        let ScriptCommand::PluginStorageGet { request } = admitted.request() else {
            panic!(
                "expected initialization storage read, saw {:?}",
                admitted.request()
            );
        };
        keys.push(request.key().to_owned());
    }
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["actions-v1", "assignments-v1", "ledger-v1", "towns-v1"],
        "the actual stateful packages wait for their durable keys"
    );

    enqueue_money(&boundary);
    let command = tokio::time::timeout(Duration::from_secs(10), boundary.recv_command())
        .await
        .expect("a deferred storage read does not block the component worker")
        .expect("the standard-pack command channel remains open");
    let admitted = boundary
        .accept_host_command(command)
        .expect("the host admits the loading response");
    assert!(
        matches!(
            admitted.request(),
            ScriptCommand::SendChatMessage { message, .. } if message == "Economy is still loading."
        ),
        "the actual economy component refuses normal work until its own storage reply"
    );
    host.stop();
}

#[tokio::test]
#[ignore = "requires the explicit first-party sibling integration gate"]
async fn standard_pack_flood_stops_at_the_bounded_event_queue() {
    const FLOOD_ATTEMPTS: usize = 64;

    let staged = staged_standard_pack();
    let limits = PluginLimits::default();
    let host = start_deployment(
        standard_pack_packages(staged.path(), &limits),
        limits,
        HostQueues {
            events: 8,
            commands: 32,
            ..HostQueues::default()
        },
        Arc::new(Sessions),
    )
    .expect("strict standard-pack deployment starts");
    let boundary = host.boundary().clone();
    assert_eq!(
        answer_empty_initial_storage_reads(&boundary).await,
        ["actions-v1", "assignments-v1", "ledger-v1", "towns-v1"],
        "the flood starts only after the actual stateful components load"
    );
    enqueue_money(&boundary);
    accept_chat_message(&boundary).await;

    let mut enqueued = 0;
    let mut refused = 0;
    let mut max_queue_depth = boundary.event_queue_depth();
    for _ in 0..FLOOD_ATTEMPTS {
        let result = boundary.try_enqueue_player_command_with_context(
            ScriptPlayerId::new(SESSION),
            player_context(),
            "money",
        );
        max_queue_depth = max_queue_depth.max(boundary.event_queue_depth());
        if matches!(&result, Ok(PlayerCommandAdmission::Enqueued)) {
            enqueued += 1;
        } else {
            assert!(
                result.is_err(),
                "the registered economy command is either enqueued or refused by queue capacity: {result:?}"
            );
            refused += 1;
        }
    }
    assert!(
        refused > 0,
        "the eight-event queue refuses a real-command flood"
    );
    assert!(
        max_queue_depth <= 8,
        "event queue depth never exceeds its configured bound: {max_queue_depth}"
    );

    for _ in 0..enqueued {
        accept_chat_message(&boundary).await;
    }
    println!(
        "--- P7 standard-pack bounded flood --- attempted {FLOOD_ATTEMPTS} | enqueued {enqueued} | refused {refused} | max event depth {max_queue_depth}"
    );
    host.stop();
}

#[tokio::test]
#[ignore = "requires the explicit first-party sibling integration gate"]
async fn standard_pack_reports_normal_and_event_storm_command_delivery() {
    const NORMAL_CALLS: usize = 100;
    const STORM_CALLS: usize = 64;
    let peak_before_kib = peak_rss_kib();

    let staged = staged_standard_pack();
    let limits = PluginLimits::default();
    let host = start_deployment(
        standard_pack_packages(staged.path(), &limits),
        limits,
        HostQueues::default(),
        Arc::new(Sessions),
    )
    .expect("strict standard-pack deployment starts");
    let boundary = host.boundary().clone();
    assert_eq!(
        answer_empty_initial_storage_reads(&boundary).await,
        ["actions-v1", "assignments-v1", "ledger-v1", "towns-v1"],
        "the normal workload starts only after the real durable state loads"
    );
    let (cpu_user_before, cpu_system_before) = process_cpu_ticks();

    let mut normal_latencies = Vec::with_capacity(NORMAL_CALLS);
    for _ in 0..NORMAL_CALLS {
        let started = Instant::now();
        enqueue_money(&boundary);
        accept_chat_message(&boundary).await;
        normal_latencies.push(started.elapsed());
    }

    let storm_started = Instant::now();
    for _ in 0..STORM_CALLS {
        enqueue_money(&boundary);
    }
    for _ in 0..STORM_CALLS {
        accept_chat_message(&boundary).await;
    }
    let storm_elapsed = storm_started.elapsed();
    let (cpu_user_after, cpu_system_after) = process_cpu_ticks();
    let peak_after_kib = peak_rss_kib();

    let p50 = nearest_rank_us(&mut normal_latencies.clone(), 50);
    let p95 = nearest_rank_us(&mut normal_latencies.clone(), 95);
    let p99 = nearest_rank_us(&mut normal_latencies, 99);
    assert!(p50.is_finite() && p95.is_finite() && p99.is_finite());
    assert!(p50 <= p95 && p95 <= p99);

    let counters = host.stop();
    let economy = counters
        .iter()
        .find(|(id, _)| id == "solaris-economy")
        .map(|(_, counters)| counters)
        .expect("the actual economy package remains hosted");
    assert!(
        economy.events_delivered >= (NORMAL_CALLS + STORM_CALLS) as u64,
        "every normal and storm command reached the economy component: {economy:?}"
    );
    assert!(
        economy.commands_submitted >= (NORMAL_CALLS + STORM_CALLS) as u64,
        "every normal and storm command published one economy reply: {economy:?}"
    );
    assert_eq!(
        economy.commands_refused, 0,
        "the comparable normal/storm workload stays inside the production bounds"
    );
    let callback_latency = economy.callback_latency;
    assert_eq!(
        callback_latency.samples,
        economy.calls.min(1_200),
        "the final diagnostics retain every actual callback up to their documented bound"
    );
    assert!(
        callback_latency.p50_us <= callback_latency.p95_us
            && callback_latency.p95_us <= callback_latency.p99_us
            && callback_latency.p99_us <= callback_latency.max_us,
        "the actual component callback percentiles are ordered: {callback_latency:?}"
    );

    println!("--- P7 standard-pack command workload (mc-test-harness) ---");
    println!(
        "environment: {} {} | logical CPUs {} | build: cargo test debug | world seed: n/a",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(0),
    );
    println!(
        "guest: strict five-package standard pack; 100 sequential /money commands after durable state load"
    );
    println!(
        "normal end-to-end command latency: p50 {p50:.3} us | p95 {p95:.3} us | p99 {p99:.3} us"
    );
    println!(
        "event storm: {STORM_CALLS} queued /money commands drained in {:.3} ms",
        storm_elapsed.as_secs_f64() * 1e3
    );
    println!(
        "process CPU delta: user {} ticks | system {} ticks",
        cpu_user_after.saturating_sub(cpu_user_before),
        cpu_system_after.saturating_sub(cpu_system_before)
    );
    println!("peak RSS: before hosting {peak_before_kib} KiB | while hosting {peak_after_kib} KiB");
    println!(
        "economy counters: events {} | submitted {} | refused {}",
        economy.events_delivered, economy.commands_submitted, economy.commands_refused
    );
    println!(
        "economy callback latency: samples {} | p50 {} us | p95 {} us | p99 {} us | max {} us",
        callback_latency.samples,
        callback_latency.p50_us,
        callback_latency.p95_us,
        callback_latency.p99_us,
        callback_latency.max_us
    );
}

#[tokio::test]
#[ignore = "requires the explicit first-party sibling integration gate"]
async fn standard_pack_reloads_as_one_strict_component_candidate() {
    let staged = staged_standard_pack();
    let limits = PluginLimits::default();
    let host = start_deployment(
        standard_pack_packages(staged.path(), &limits),
        limits,
        HostQueues::default(),
        Arc::new(NoSessions),
    )
    .expect("strict standard-pack deployment starts");
    let boundary = host.boundary().clone();
    assert_eq!(
        host.boundary()
            .deployed_packages()
            .iter()
            .map(|package| package.plugin_id())
            .collect::<Vec<_>>(),
        [
            "solaris-audit",
            "solaris-economy",
            "solaris-essentials",
            "solaris-permissions",
            "solaris-towns",
        ],
        "the running catalog has only the strict standard pack"
    );

    assert_eq!(
        answer_empty_initial_storage_reads(&boundary).await,
        ["actions-v1", "assignments-v1", "ledger-v1", "towns-v1"],
        "every stateful package reads its existing durable key before reload"
    );

    for generation in 1..=2 {
        let report = host
            .reload(standard_pack_packages(staged.path(), &limits))
            .await
            .unwrap_or_else(|error| {
                panic!("strict standard-pack reload {generation} succeeds: {error}")
            });
        assert_eq!(report.loaded_packages, STANDARD_PACK.len());
        assert_eq!(report.replaced.len(), STANDARD_PACK.len());
        assert_eq!(
            answer_empty_initial_storage_reads(&boundary).await,
            ["actions-v1", "assignments-v1", "ledger-v1", "towns-v1"],
            "generation {generation} re-reads the same externally owned durable keys"
        );
    }
    host.stop();
}
