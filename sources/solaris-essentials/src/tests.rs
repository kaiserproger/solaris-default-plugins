use super::*;
use solaris_plugin_sdk::events::StorageGetAnswered;
use solaris_plugin_sdk::StorageRecord;

fn plugin() -> Essentials {
    let mut plugin = Essentials::default();
    plugin
        .init(
            &Config::new(include_str!("../../../solaris-essentials/config.toml")),
            &InitContext {
                plugin_id: "solaris-essentials".into(),
                api_version: "0.8.0".into(),
                world_fingerprint: String::new(),
            },
        )
        .unwrap();
    plugin
}

fn send(plugin: &mut Essentials, event: Event) -> Vec<Command> {
    plugin
        .on_events(
            &EventContext {
                tick: 1,
                first_sequence: 0,
                count: 1,
            },
            &[event],
        )
        .unwrap()
}

fn command(name: &str) -> Event {
    Event::CommandInvoked(CommandInvoked {
        player: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
        session: 7,
        username: "Ada".into(),
        operator: false,
        position: Position {
            x: 1.0,
            y: 64.0,
            z: 2.0,
        },
        dimension: "minecraft:overworld".into(),
        name: name.into(),
        arguments: vec!["base".into()],
        raw_arguments: "base".into(),
    })
}

fn read(plugin: &mut Essentials, command: Event, value: Option<&str>) -> Vec<Command> {
    let cmds = send(plugin, command);
    let [Command::StorageGet(get)] = cmds.as_slice() else {
        panic!("expected storage read: {cmds:?}")
    };
    send(
        plugin,
        Event::StorageGetAnswered(StorageGetAnswered {
            request: get.request.clone(),
            outcome: StorageGetOutcome::Read(StorageRecord {
                value: value.map(str::to_owned),
                version: value.map(|_| 1),
            }),
        }),
    )
}

#[test]
fn dimensional_homes_save_dimension_through_command_storage_events() {
    let cmds = read(&mut plugin(), command("sethome"), None);
    let [Command::StorageCas(cas)] = cmds.as_slice() else {
        panic!("expected save: {cmds:?}")
    };
    assert_eq!(cas.value, "v2|base,minecraft:overworld,1,64,2");
}

#[test]
fn dimensional_homes_legacy_location_is_never_guessed() {
    let cmds = read(&mut plugin(), command("home"), Some("v1|base,10,70,20"));
    assert!(
        matches!(cmds.as_slice(), [Command::SendMessage(m)] if m.text.contains("dimension is unknown") && m.text.contains("sethome")),
        "{cmds:?}"
    );
}

#[test]
fn dimensional_homes_cross_world_refusal_names_destination_without_teleport() {
    let cmds = read(
        &mut plugin(),
        command("home"),
        Some("v2|base,minecraft:the_nether,10,70,20"),
    );
    assert!(
        matches!(cmds.as_slice(), [Command::SendMessage(m)] if m.text.contains("minecraft:the_nether") && m.text.contains("Cross-dimension")),
        "{cmds:?}"
    );
}

#[test]
fn dimensional_homes_same_world_teleport_keeps_target_dimension() {
    let cmds = read(
        &mut plugin(),
        command("home"),
        Some("v2|base,minecraft:overworld,10,70,20"),
    );
    assert!(
        matches!(cmds.as_slice(), [Command::TeleportPlayer(t)] if t.dimension == "minecraft:overworld" && (t.position.x, t.position.y, t.position.z) == (10.0, 70.0, 20.0)),
        "{cmds:?}"
    );
}

#[test]
fn dimensional_homes_edit_preserves_unknown_legacy_dimensions() {
    let cmds = read(&mut plugin(), command("sethome"), Some("v1|old,10,70,20"));
    let [Command::StorageCas(cas)] = cmds.as_slice() else {
        panic!("expected save: {cmds:?}")
    };
    assert_eq!(
        cas.value,
        "v2|base,minecraft:overworld,1,64,2;old,,10,70,20"
    );
    let cmds = read(&mut plugin(), command("home"), Some("v2|base,,10,70,20"));
    assert!(
        matches!(cmds.as_slice(), [Command::SendMessage(m)] if m.text.contains("dimension is unknown"))
    );
}

#[test]
fn dimensional_homes_nether_save_and_overworld_lookup_are_fenced() {
    let mut nether = command("sethome");
    let Event::CommandInvoked(invoked) = &mut nether else {
        unreachable!()
    };
    invoked.dimension = "minecraft:the_nether".into();
    let cmds = read(&mut plugin(), nether, None);
    let [Command::StorageCas(cas)] = cmds.as_slice() else {
        panic!("expected save: {cmds:?}")
    };
    assert_eq!(cas.value, "v2|base,minecraft:the_nether,1,64,2");
    let cmds = read(&mut plugin(), command("home"), Some(&cas.value));
    assert!(
        matches!(cmds.as_slice(), [Command::SendMessage(m)] if m.text.contains("is in minecraft:the_nether"))
    );
}

#[test]
fn dimensional_homes_teleport_race_failure_names_destination() {
    let mut plugin = plugin();
    let cmds = read(
        &mut plugin,
        command("home"),
        Some("v2|base,minecraft:overworld,10,70,20"),
    );
    let [Command::TeleportPlayer(teleport)] = cmds.as_slice() else {
        panic!("expected teleport")
    };
    let cmds = send(
        &mut plugin,
        Event::PlayerTeleportAnswered(solaris_plugin_sdk::events::PlayerTeleportAnswered {
            request: teleport.request.clone(),
            session: 7,
            position: teleport.position,
            outcome: PlayerTeleportOutcome::Refused(
                solaris_plugin_sdk::events::PlayerTeleportFailure::DimensionMismatch,
            ),
        }),
    );
    assert!(
        matches!(cmds.as_slice(), [Command::SendMessage(m)] if m.text.contains("minecraft:overworld") && m.text.contains("DimensionMismatch"))
    );
    assert!(plugin.backs.is_empty());
}
