use super::*;
use solaris_plugin_sdk::events::{StorageCasAnswered, StorageGetAnswered, ZoneCommandAnswered};
use solaris_plugin_sdk::StorageRecord;

const PLAYER: &str = "11111111-2222-3333-4444-555555555555";
const OVERWORLD: &str = "minecraft:overworld";
const NETHER: &str = "minecraft:the_nether";

fn send(towns: &mut Towns, event: Event) -> Vec<Command> {
    towns
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

fn command(dimension: &str, action: &str) -> Event {
    Event::CommandInvoked(CommandInvoked {
        player: PLAYER.into(),
        session: 1,
        username: "Ada".into(),
        operator: false,
        position: types::Position {
            x: 32.0,
            y: 64.0,
            z: -16.0,
        },
        dimension: dimension.into(),
        name: "town".into(),
        arguments: action.split_whitespace().map(str::to_owned).collect(),
        raw_arguments: action.into(),
    })
}

fn load(value: Option<String>) -> (Towns, Vec<Command>) {
    let config = Config::new(include_str!("../../../solaris-towns/config.toml"));
    let mut towns = Towns::default();
    let init = towns
        .init(
            &config,
            &InitContext {
                plugin_id: "solaris-towns".into(),
                api_version: "0.8.0".into(),
                world_fingerprint: String::new(),
            },
        )
        .unwrap();
    let Command::StorageGet(get) = &init[0] else {
        panic!("expected get")
    };
    let events = send(
        &mut towns,
        Event::StorageGetAnswered(StorageGetAnswered {
            request: get.request.clone(),
            outcome: StorageGetOutcome::Read(StorageRecord {
                version: value.as_ref().map(|_| 1),
                value,
            }),
        }),
    );
    (towns, events)
}

fn commit(towns: &mut Towns, commands: Vec<Command>, version: u64) -> (String, Vec<Command>) {
    let [Command::StorageCas(cas)] = commands.as_slice() else {
        panic!("expected CAS: {commands:?}")
    };
    let value = cas.value.clone();
    let result = send(
        towns,
        Event::StorageCasAnswered(StorageCasAnswered {
            request: cas.request.clone(),
            outcome: StorageCasOutcome::Committed(version),
        }),
    );
    (value, result)
}

fn applied(towns: &mut Towns, zone: String) {
    send(
        towns,
        Event::ZoneCommandAnswered(ZoneCommandAnswered {
            zone,
            outcome: ZoneCommandOutcome::Applied,
        }),
    );
}

fn zone(commands: &[Command], dimension: &str) -> String {
    let [Command::UpsertProtectedZone(zone)] = commands else {
        panic!("expected zone: {commands:?}")
    };
    assert_eq!(zone.dimension, dimension);
    assert_eq!(
        (
            zone.minimum.x,
            zone.minimum.z,
            zone.maximum.x,
            zone.maximum.z
        ),
        (32.0, -16.0, 47.0, -1.0)
    );
    zone.zone.clone()
}

#[test]
fn dimensional_claims_event_path_roundtrip_and_unclaim() {
    let (mut towns, _) = load(None);
    let cmds = send(&mut towns, command(OVERWORLD, "create oak"));
    commit(&mut towns, cmds, 1);
    let cmds = send(&mut towns, command(OVERWORLD, "claim"));
    let (_, cmds) = commit(&mut towns, cmds, 2);
    let overworld_zone = zone(&cmds, OVERWORLD);
    applied(&mut towns, overworld_zone.clone());
    let cmds = send(&mut towns, command(NETHER, "claim"));
    let (saved, cmds) = commit(&mut towns, cmds, 3);
    let nether_zone = zone(&cmds, NETHER);
    assert_ne!(overworld_zone, nether_zone);
    applied(&mut towns, nether_zone.clone());
    let info = send(&mut towns, command(NETHER, "info"));
    let [Command::SendMessage(message)] = info.as_slice() else {
        panic!("expected info")
    };
    assert!(message.text.contains(OVERWORLD) && message.text.contains(NETHER));

    let (mut reloaded, zones) = load(Some(saved));
    assert_eq!(zones.len(), 2);
    assert!(zones.iter().any(|c| matches!(c, Command::UpsertProtectedZone(z) if z.zone == overworld_zone && z.dimension == OVERWORLD)));
    assert!(zones.iter().any(|c| matches!(c, Command::UpsertProtectedZone(z) if z.zone == nether_zone && z.dimension == NETHER)));
    applied(&mut reloaded, overworld_zone.clone());
    applied(&mut reloaded, nether_zone.clone());
    let duplicate = send(&mut reloaded, command(OVERWORLD, "claim"));
    assert!(
        matches!(duplicate.as_slice(), [Command::SendMessage(m)] if m.text.contains("claimed"))
    );
    let cmds = send(&mut reloaded, command(NETHER, "unclaim"));
    let (_, cmds) = commit(&mut reloaded, cmds, 4);
    assert!(matches!(cmds.as_slice(), [Command::RemoveZone(z)] if z.zone == nether_zone));
    applied(&mut reloaded, nether_zone);
    let cmds = send(&mut reloaded, command(NETHER, "unclaim"));
    assert!(
        matches!(cmds.as_slice(), [Command::SendMessage(m)] if m.text.contains("does not claim"))
    );
    let cmds = send(&mut reloaded, command(OVERWORLD, "unclaim"));
    let (_, cmds) = commit(&mut reloaded, cmds, 5);
    assert!(matches!(cmds.as_slice(), [Command::RemoveZone(z)] if z.zone == overworld_zone));
}

#[test]
fn dimensional_claims_legacy_records_always_reload_in_overworld() {
    for version in ["v1", "v2"] {
        let fields = if version == "v1" { "" } else { ",," };
        let saved = format!("{version}|oak,{PLAYER},{}:leader,2:-1{fields}", PLAYER);
        let config = Config::new(&format!(
            "{}\ndimension = \"{NETHER}\"\n",
            include_str!("../../../solaris-towns/config.toml")
        ));
        let mut towns = Towns::default();
        let init = towns
            .init(
                &config,
                &InitContext {
                    plugin_id: "solaris-towns".into(),
                    api_version: "0.8.0".into(),
                    world_fingerprint: String::new(),
                },
            )
            .unwrap();
        let Command::StorageGet(get) = &init[0] else {
            panic!("expected get")
        };
        let cmds = send(
            &mut towns,
            Event::StorageGetAnswered(StorageGetAnswered {
                request: get.request.clone(),
                outcome: StorageGetOutcome::Read(StorageRecord {
                    value: Some(saved),
                    version: Some(1),
                }),
            }),
        );
        let id = zone(&cmds, OVERWORLD);
        assert_eq!(id, "town-oak-p2-n1");
        applied(&mut towns, id);
        let cmds = send(&mut towns, command(NETHER, "claim"));
        let (encoded, cmds) = commit(&mut towns, cmds, 2);
        let id = zone(&cmds, NETHER);
        applied(&mut towns, id);
        let (_, restored) = load(Some(encoded));
        assert_eq!(restored.len(), 2);
        assert!(restored
            .iter()
            .any(|c| matches!(c, Command::UpsertProtectedZone(z) if z.dimension == OVERWORLD)));
        assert!(restored
            .iter()
            .any(|c| matches!(c, Command::UpsertProtectedZone(z) if z.dimension == NETHER)));
    }
}
