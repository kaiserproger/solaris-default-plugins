//! Component acceptance for the `graves` package.
//!
//! The native half (death custody, the barrel, the journal) is core's own and is
//! covered by core tests. This file drives the compiled component the way the
//! production host does: real Wasmtime instance, real `player.grave_created`
//! record, real command enums. It proves the deployment surface, the owner-only
//! protection, the announcement, the sweep deadline, the restart path and the
//! zone removal - the parts this package actually owns.

use mc_plugin_host::bindings::exports::solaris::plugin::events::{
    Event, EventContext, PlayerGraveCreated, StorageCasAnswered, StorageGetAnswered, TimerFired,
    ZoneCommandAnswered, ZoneCommandOutcome,
};
use mc_plugin_host::bindings::exports::solaris::plugin::lifecycle::InitContext;
use mc_plugin_host::bindings::solaris::plugin::commands::{Command, MessageTarget};
use mc_plugin_host::bindings::solaris::plugin::storage::{
    StorageCasOutcome, StorageGetOutcome, StorageRecord,
};
use mc_plugin_host::bindings::solaris::plugin::world_events::BlockPosition;
use mc_plugin_host::{
    engine, linker, CompiledPlugin, HostServices, LogLevel, PluginInstance, PluginLimits,
    PluginStartup,
};

const API_VERSION: &str = "0.8.0";
const PLUGIN_ID: &str = "graves";
const PLAYER: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const SESSION: u64 = 7;
const STORAGE_KEY: &str = "graves-v1";
const SWEEP_TIMER: &str = "grave-sweep";
const CONFIG: &str = "public_after_seconds = 60\n";
const PUBLIC_AFTER_TICKS: u64 = 60 * 20;
const GRAVE_TICK: u64 = 100;

#[derive(Default)]
struct Services {
    id: String,
}

impl HostServices for Services {
    fn log(&mut self, level: LogLevel, message: &str) {
        panic!("graves must not write host logs: {level:?} {message}");
    }

    fn plugin_id(&self) -> &str {
        &self.id
    }
}

fn plugins_root() -> std::path::PathBuf {
    if let Some(root) = std::env::var_os("SOLARIS_DEFAULT_PLUGINS_ROOT") {
        return std::path::PathBuf::from(root);
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("integration test crate belongs to the plugin repository")
        .to_path_buf()
}

fn instance() -> (PluginInstance<Services>, mc_plugin_host::EpochTicker) {
    let package = plugins_root().join(PLUGIN_ID);
    let component =
        std::fs::read(package.join("plugin.wasm")).expect("graves component is installed");
    let limits = PluginLimits::default();
    let engine = engine(&limits).expect("host engine builds");
    let compiled = CompiledPlugin::compile(&engine, &component, &limits, API_VERSION)
        .expect("graves compiles under the current host contract");
    let ticker =
        mc_plugin_host::EpochTicker::start(engine.clone(), std::time::Duration::from_millis(25))
            .expect("real callback watchdog starts");
    let linker = linker::<Services>(&engine).expect("host linker builds");
    let mut startup = PluginStartup::instantiate(
        &linker,
        compiled.component(),
        Services {
            id: PLUGIN_ID.to_owned(),
        },
        limits,
    )
    .expect("startup instance builds");
    assert!(
        startup
            .configure(CONFIG)
            .expect("graves configuration is accepted")
            .is_none(),
        "graves declares no startup contribution"
    );
    let mut plugin = PluginInstance::instantiate(
        &linker,
        compiled.component(),
        Services {
            id: PLUGIN_ID.to_owned(),
        },
        limits,
    )
    .expect("runtime instance builds");
    let commands = plugin
        .init(
            CONFIG,
            InitContext {
                plugin_id: PLUGIN_ID.to_owned(),
                api_version: API_VERSION.to_owned(),
                world_fingerprint: String::new(),
            },
        )
        .expect("graves init is accepted")
        .into_commands();
    assert_eq!(commands.len(), 1, "init only loads the grave table");
    let request = match &commands[0] {
        Command::StorageGet(get) => {
            assert_eq!(get.key, STORAGE_KEY);
            get.request.clone()
        }
        other => panic!("init emits the table read, saw {other:?}"),
    };
    let loaded = plugin
        .on_events(context(1, 0), &[storage_empty(request)])
        .expect("an empty table loads")
        .into_commands();
    assert!(
        loaded.is_empty(),
        "an empty table schedules nothing and writes nothing"
    );
    (plugin, ticker)
}

fn context(count: u32, tick: u64) -> EventContext {
    EventContext {
        tick,
        first_sequence: 0,
        count,
    }
}

fn grave_created() -> Event {
    Event::PlayerGraveCreated(PlayerGraveCreated {
        player: PLAYER.to_owned(),
        session: SESSION,
        dimension: "minecraft:overworld".to_owned(),
        at: BlockPosition { x: -3, y: 70, z: 8 },
        items: 5,
        spilled: 0,
    })
}

fn storage_empty(request: String) -> Event {
    Event::StorageGetAnswered(StorageGetAnswered {
        request,
        outcome: StorageGetOutcome::Read(StorageRecord {
            value: None,
            version: None,
        }),
    })
}

fn storage_value(request: String, value: &str, version: u64) -> Event {
    Event::StorageGetAnswered(StorageGetAnswered {
        request,
        outcome: StorageGetOutcome::Read(StorageRecord {
            value: Some(value.to_owned()),
            version: Some(version),
        }),
    })
}

fn cas_committed(request: String, version: u64) -> Event {
    Event::StorageCasAnswered(StorageCasAnswered {
        request,
        outcome: StorageCasOutcome::Committed(version),
    })
}

fn take_cas(commands: &[Command]) -> (String, Option<u64>, String) {
    let cas = commands
        .iter()
        .find_map(|command| match command {
            Command::StorageCas(cas) => Some(cas),
            _ => None,
        })
        .expect("the changed table is saved");
    assert_eq!(cas.key, STORAGE_KEY);
    (cas.request.clone(), cas.expected_version, cas.value.clone())
}

fn player_messages(commands: &[Command]) -> Vec<String> {
    commands
        .iter()
        .filter_map(|command| match command {
            Command::SendMessage(message) => match &message.target {
                MessageTarget::Player(player) => {
                    assert_eq!(player, PLAYER, "the grave is announced to its owner");
                    Some(message.text.clone())
                }
                MessageTarget::Session(_) => panic!("graves addresses the stable identity"),
            },
            _ => None,
        })
        .collect()
}

#[test]
fn grave_created_protects_the_cell_for_the_owner_and_saves_the_deadline() {
    let (mut plugin, _ticker) = instance();
    let commands = plugin
        .on_events(context(1, GRAVE_TICK), &[grave_created()])
        .expect("grave delivery succeeds")
        .into_commands();
    assert_eq!(commands.len(), 4, "protect, announce, sweep, save");
    let zone = commands
        .iter()
        .find_map(|command| match command {
            Command::UpsertProtectedZone(zone) => Some(zone),
            _ => None,
        })
        .expect("the grave cell becomes a protected zone");
    assert!(
        zone.zone.starts_with("grave--3-70-8-"),
        "the zone id names the cell, saw {}",
        zone.zone
    );
    assert_eq!(zone.dimension, "minecraft:overworld");
    assert_eq!(zone.allowed_actor_uuids, vec![PLAYER.to_owned()]);
    assert!(!zone.public_use, "doors inside a grave cell stay shut");
    assert_eq!(
        (zone.minimum.x, zone.minimum.y, zone.minimum.z),
        (-3.0, 69.0, 8.0),
        "the cell beneath the container is guarded too"
    );
    assert_eq!(
        (zone.maximum.x, zone.maximum.y, zone.maximum.z),
        (-3.0, 70.0, 8.0)
    );
    let messages = player_messages(&commands);
    assert_eq!(messages.len(), 1);
    for needle in ["-3", "70", "8", "minecraft:overworld", "1 minutes"] {
        assert!(
            messages[0].contains(needle),
            "announcement misses {needle}: {}",
            messages[0]
        );
    }
    let (_, _, value) = take_cas(&commands);
    assert!(
        value.starts_with("v1|"),
        "durable table keeps its version tag"
    );
    assert!(value.contains(PLAYER), "the owner is durable");
    let (request, expected, _) = take_cas(&commands);
    assert_eq!(expected, None, "a fresh table swaps only into an empty key");
    let armed = commands.iter().any(
        |command| matches!(command, Command::ScheduleTimer(timer) if timer.timer_id == SWEEP_TIMER),
    );
    assert!(armed, "the sweep is armed once a grave is private");
    let resumed = plugin
        .on_events(context(1, GRAVE_TICK), &[cas_committed(request, 1)])
        .expect("commit answer succeeds")
        .into_commands();
    assert!(resumed.is_empty(), "a committed save schedules nothing new");
}

#[test]
fn the_sweep_opens_the_grave_once_the_private_window_elapsed() {
    let (mut plugin, _ticker) = instance();
    let created = plugin
        .on_events(context(1, GRAVE_TICK), &[grave_created()])
        .expect("grave delivery succeeds")
        .into_commands();
    let zone_id = created
        .iter()
        .find_map(|command| match command {
            Command::UpsertProtectedZone(zone) => Some(zone.zone.clone()),
            _ => None,
        })
        .expect("the grave cell becomes a protected zone");
    let zone = plugin
        .on_events(
            context(1, GRAVE_TICK),
            &[Event::ZoneCommandAnswered(ZoneCommandAnswered {
                zone: zone_id,
                outcome: ZoneCommandOutcome::Applied,
            })],
        )
        .expect("zone answer succeeds")
        .into_commands();
    assert!(zone.is_empty());
    let (save, _, _) = take_cas(&created);
    let committed = plugin
        .on_events(context(1, GRAVE_TICK), &[cas_committed(save, 1)])
        .expect("commit answer succeeds")
        .into_commands();
    assert!(committed.is_empty());
    let before = plugin
        .on_events(
            context(1, GRAVE_TICK + PUBLIC_AFTER_TICKS - 1),
            &[Event::TimerFired(TimerFired {
                timer_id: SWEEP_TIMER.to_owned(),
                scheduled_tick: GRAVE_TICK + 20,
                fired_tick: GRAVE_TICK + PUBLIC_AFTER_TICKS - 1,
            })],
        )
        .expect("early sweep succeeds")
        .into_commands();
    assert!(
        !before
            .iter()
            .any(|command| matches!(command, Command::RemoveZone(_))),
        "a grave inside its window keeps its protection"
    );
    assert!(
        before
            .iter()
            .any(|command| matches!(command, Command::ScheduleTimer(timer) if timer.timer_id == SWEEP_TIMER)),
        "the sweep reschedules while a grave is private"
    );
    let due = plugin
        .on_events(
            context(1, GRAVE_TICK + PUBLIC_AFTER_TICKS),
            &[Event::TimerFired(TimerFired {
                timer_id: SWEEP_TIMER.to_owned(),
                scheduled_tick: GRAVE_TICK + PUBLIC_AFTER_TICKS,
                fired_tick: GRAVE_TICK + PUBLIC_AFTER_TICKS,
            })],
        )
        .expect("due sweep succeeds")
        .into_commands();
    let removed = due.iter().any(|command| {
        matches!(command, Command::RemoveZone(zone) if zone.zone.starts_with("grave--3-70-8-"))
    });
    assert!(removed, "the window closes with a zone removal");
    let opened = player_messages(&due);
    assert_eq!(opened.len(), 1);
    assert!(opened[0].contains("public"), "{}", opened[0]);
    assert!(
        !due.iter()
            .any(|command| matches!(command, Command::ScheduleTimer(_))),
        "no private grave is left, so the sweep stops"
    );
    take_cas(&due);
}

#[test]
fn a_restart_flips_graves_whose_window_passed_while_the_server_was_down() {
    let (mut plugin, _ticker) = instance();
    let stored = format!("v1|minecraft:overworld,-3,70,8,{PLAYER},{GRAVE_TICK},p");
    let commands = plugin
        .on_events(
            context(1, 9_000),
            &[storage_value("reload-1".to_owned(), &stored, 4)],
        )
        .expect("load after a restart succeeds")
        .into_commands();
    // The empty answer is a first-run load; the stored answer replaces it and the
    // sweep flips the dead-window grave immediately.
    assert!(
        commands.iter().any(|command| {
            matches!(command, Command::RemoveZone(zone) if zone.zone.starts_with("grave--3-70-8-"))
        }),
        "a grave whose window elapsed while down becomes public"
    );
    assert!(player_messages(&commands)[0].contains("public"));
    let (_, expected, value) = take_cas(&commands);
    assert_eq!(expected, Some(4), "the save names the loaded revision");
    assert!(value.contains(",u"), "the table records the public state");
}

#[test]
fn the_deployment_declares_native_custody_and_that_capability_is_known() {
    let root = plugins_root().join(PLUGIN_ID);
    let package = mc_plugin_host::load_package(&root, &PluginLimits::default())
        .expect("the graves package is a valid deployment");
    assert!(
        package
            .requested_capabilities()
            .iter()
            .any(|capability| capability == "death_graves"),
        "the native death path acts only for a package that declares it"
    );
    assert!(
        package
            .requested_capabilities()
            .iter()
            .any(|capability| capability == "zones"),
        "the package protects its grave through the native zone owner"
    );
    assert!(
        package
            .manifest()
            .event_subscriptions()
            .iter()
            .any(|subscription| subscription.event_name() == "player.block_broken"),
        "a broken grave is observed so its record and zone do not outlive it"
    );
}
