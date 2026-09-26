use super::*;
use solaris_plugin_sdk::domain_operations::DomainOperation;
use solaris_plugin_sdk::residents::ResidentOrderOperation;

fn read(
    plugin: &mut Settlements,
    commands: &[Command],
    registry: &SettlementRegistry,
    version: u64,
) -> Vec<Command> {
    let [Command::StorageGet(get)] = commands else {
        panic!("registry read required")
    };
    plugin.storage_get(&StorageGetAnswered {
        request: get.request.clone(),
        outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
            value: Some(serde_json::to_string(registry).unwrap()),
            version: Some(version),
        }),
    })
}

fn commit(
    plugin: &mut Settlements,
    commands: &[Command],
    registry: &mut SettlementRegistry,
    version: &mut u64,
) -> Vec<Command> {
    let [Command::StorageCas(cas)] = commands else {
        panic!("registry CAS required")
    };
    assert_eq!(cas.expected_version, Some(*version));
    *registry = serde_json::from_str(&cas.value).unwrap();
    *version += 1;
    plugin.storage_cas(&StorageCasAnswered {
        request: cas.request.clone(),
        outcome: StorageCasOutcome::Committed(*version),
    })
}

fn armies() -> SettlementRegistry {
    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "attacker".to_owned(), "oak_hollow", "small").unwrap();
    register_settlement(&mut registry, "defender".to_owned(), "pine_haven", "small").unwrap();
    record_settlement_resident(&mut registry, "oak_hollow", "guard", "resident-guard", 4).unwrap();
    record_settlement_resident(&mut registry, "pine_haven", "soldier", "resident-victim", 7)
        .unwrap();
    let guard = &mut registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .get_mut("guard")
        .unwrap();
    guard.role = Some(hire::Role::Infantry);
    guard.employer = Some("oak_hollow".to_owned());
    let victim = &mut registry
        .settlements
        .get_mut("pine_haven")
        .unwrap()
        .residents
        .get_mut("soldier")
        .unwrap();
    victim.role = Some(hire::Role::Militia);
    victim.employer = Some("pine_haven".to_owned());
    victim.issued = Some(hire::IssuedKit {
        warehouse: "pine-warehouse".to_owned(),
        items: vec![hire::IssuedItem {
            item: "minecraft:iron_sword".to_owned(),
            count: 1,
            damage: None,
            enchantments: vec![],
            custom_name: Some("Personal blade".to_owned()),
            item_model: None,
            destination: hire::Destination::Equipment(0),
        }],
    });
    registry
}

#[test]
fn restart_after_native_capture_replays_one_claim_and_keeps_original_identity_and_gear() {
    let mut registry = armies();
    let mut version = 3;
    let mut plugin = Settlements::default();
    let start = plugin.capture(
        17,
        "attacker",
        "oak_hollow",
        "guard",
        "pine_haven",
        "soldier",
        "7",
    );
    let intent = read(&mut plugin, &start, &registry, version);
    let native = commit(&mut plugin, &intent, &mut registry, &mut version);
    let victim = &registry.settlements["pine_haven"].residents["soldier"];
    assert!(
        victim.role.is_none(),
        "the captured identity leaves its former army before native decision"
    );
    assert!(victim.capturing.is_some());
    assert_eq!(
        victim.issued.as_ref().unwrap().items[0]
            .custom_name
            .as_deref(),
        Some("Personal blade")
    );
    let [Command::Operation(first)] = native.as_slice() else {
        panic!("capture must use native resident authority")
    };
    let DomainOperation::ResidentOrder(ResidentOrderOperation::Capture(capture)) = &first.operation
    else {
        panic!("capture must be a native resident order")
    };
    assert_eq!(capture.handle, "resident-victim");
    assert_eq!(capture.custodian, "resident-guard");
    assert_eq!(capture.expected_revision, 7);
    let operation_id = capture.operation_id.clone();

    // The core may already have committed before the guest receives its answer.
    // A fresh instance recovers through production startup: the hiring read
    // chains hiring, dismissal and capture replay off one registry snapshot.
    let mut reopened = Settlements::default();
    let recover = reopened.recover_hires();
    let replay = read(&mut reopened, &recover, &registry, version);
    let [Command::Operation(replay)] = replay.as_slice() else {
        panic!("same fenced decision must replay")
    };
    let DomainOperation::ResidentOrder(ResidentOrderOperation::Capture(reissued)) =
        &replay.operation
    else {
        panic!("native capture replay")
    };
    assert_eq!(reissued.operation_id, operation_id);
    assert_eq!(reissued.handle, capture.handle);
    let finalize_read = reopened.answered(&OperationAnswered {
        request: replay.request.clone(),
        operation_id: Some(operation_id),
        outcome: OperationOutcome::Committed(OperationCommitted {
            revision: 11,
            payload: OperationPayload::ResidentOrder(ResidentOrderResult::Captured(
                residents::CaptureResult {
                    handle: "resident-victim".to_owned(),
                    custodian: "resident-guard".to_owned(),
                    revision: 11,
                },
            )),
        }),
    });
    let finalize = read(&mut reopened, &finalize_read, &registry, version);
    let next = commit(&mut reopened, &finalize, &mut registry, &mut version);
    assert!(
        matches!(next.as_slice(), [Command::StorageGet(_)]),
        "resume other pending captures"
    );
    let victim = &registry.settlements["pine_haven"].residents["soldier"];
    assert_eq!(victim.handle, "resident-victim");
    assert_eq!(
        victim.work_revision, 7,
        "capture does not rewrite the separate resident work fence"
    );
    assert!(victim.role.is_none() && victim.capturing.is_none());
    assert_eq!(victim.captive_by.as_deref(), Some("oak_hollow"));
    assert_eq!(
        victim.issued.as_ref().unwrap().items[0]
            .custom_name
            .as_deref(),
        Some("Personal blade")
    );
    assert!(!registry.settlements["oak_hollow"]
        .residents
        .values()
        .any(|other| other.handle == victim.handle));
    let attempted_hire = reopened.hire(17, "defender", "pine_haven", "soldier", "militia", 0);
    assert!(matches!(
        read(&mut reopened, &attempted_hire, &registry, version).as_slice(),
        [Command::SendMessage(_)]
    ));
}

#[test]
fn refused_native_capture_restores_original_army_without_claiming_custody() {
    let mut registry = armies();
    let mut version = 3;
    let mut plugin = Settlements::default();
    let start = plugin.capture(
        17,
        "attacker",
        "oak_hollow",
        "guard",
        "pine_haven",
        "soldier",
        "7",
    );
    let intent = read(&mut plugin, &start, &registry, version);
    let native = commit(&mut plugin, &intent, &mut registry, &mut version);
    let [Command::Operation(native)] = native.as_slice() else {
        panic!("native request")
    };
    let rejected = plugin.answered(&OperationAnswered {
        request: native.request.clone(),
        operation_id: Some("capture-1".to_owned()),
        outcome: OperationOutcome::Refused(solaris_plugin_sdk::events::OperationRefused {
            reason: OperationFailure::InvalidRequest,
            payload: OperationPayload::None,
        }),
    });
    let abort = read(&mut plugin, &rejected, &registry, version);
    commit(&mut plugin, &abort, &mut registry, &mut version);
    let victim = &registry.settlements["pine_haven"].residents["soldier"];
    assert!(victim.role == Some(hire::Role::Militia));
    assert!(victim.capturing.is_none() && victim.captive_by.is_none());
    assert_eq!(victim.handle, "resident-victim");
}
