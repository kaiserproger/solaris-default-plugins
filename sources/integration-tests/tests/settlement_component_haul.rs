//! CP-001 / CP-003: the shipped Settlements component creates directed cargo
//! moves the resident/warehouse owner executes.
//!
//! This target is ignored by ordinary core-only runs. The `settlement-cargo`
//! harness profile rebuilds the sibling component, byte-checks the deployed
//! package, and runs this test alongside the core's real chest-transfer cases.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use mc_plugin_host::{
    DeploymentConfig, DiscoveryMode, HostQueues, PlayerSessions, PluginLimits, start_deployment,
};
use mc_script::{
    PlayerCommandAdmission, ScriptBlockPosition, ScriptClientViewField,
    ScriptClientViewRequestKind, ScriptCommand, ScriptEvent, ScriptGameMode, ScriptHostileCategory,
    ScriptInventoryEndpoint, ScriptInventoryExpectedRevision, ScriptInventoryFence,
    ScriptInventoryItem, ScriptInventorySlot, ScriptOnlinePlayerSnapshot, ScriptOperation,
    ScriptOperationOutcome, ScriptOperationPayload, ScriptOrderMemberOutcome,
    ScriptOrderMemberState, ScriptOrderTarget, ScriptOwnedInventoryOperation,
    ScriptOwnedInventoryResult, ScriptOwnedInventorySnapshot, ScriptPlayerContext, ScriptPlayerId,
    ScriptResidentLifecycle, ScriptResidentOperation, ScriptResidentOrder,
    ScriptResidentOrderOperation, ScriptResidentOrderResult, ScriptResidentPois,
    ScriptResidentResult, ScriptResidentSnapshot, ScriptResidentWorkOrder,
    ScriptSettlementOperation, ScriptSettlementResult, ScriptWarehouseBinding,
    ScriptWarehouseSource,
};

const PACKAGE_ID: &str = "solaris-settlements";
const PLAYER: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
const SESSION: u64 = 7;
const STRUCTURE: &str = "warehouse-1";
const WAREHOUSE: &str = "warehouse:settlement:warehouse-1:0";
const RESIDENT: &str = "resident-1";

struct Sessions;

impl PlayerSessions for Sessions {
    fn session_of(&self, player: &str) -> Option<u64> {
        (player == PLAYER).then_some(SESSION)
    }
}

fn player_context() -> ScriptPlayerContext {
    ScriptPlayerContext::try_new(PLAYER, "Ada", false, 32.0, 64.0, -16.0)
        .expect("bounded settlement player context")
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

fn copy_package(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("create staged package directory");
    for entry in std::fs::read_dir(source).expect("read shipped package") {
        let entry = entry.expect("read shipped package entry");
        let target = destination.join(entry.file_name());
        let file_type = entry.file_type().expect("read shipped entry type");
        if file_type.is_dir() {
            copy_package(&entry.path(), &target);
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), target).expect("copy shipped package file");
        } else {
            panic!("shipped package contains a non-file non-directory entry");
        }
    }
}

fn deployment(root: &Path) -> DeploymentConfig {
    DeploymentConfig {
        root: root.to_path_buf(),
        mode: DiscoveryMode::Strict,
        expected: vec![PACKAGE_ID.to_owned()],
        grants: BTreeMap::from([(
            PACKAGE_ID.to_owned(),
            vec![
                "player_queries".to_owned(),
                "storage".to_owned(),
                "inventory_transfers".to_owned(),
                "persistent_residents".to_owned(),
                "resident_work".to_owned(),
                "resident_orders".to_owned(),
                "world_sites".to_owned(),
                "structure_operations".to_owned(),
            ],
        )]),
        require_grants: true,
        precommit_hooks: Vec::new(),
    }
}

fn bind_outcome() -> ScriptOperationOutcome {
    ScriptOperationOutcome::committed(
        4,
        ScriptOperationPayload::Settlement {
            result: Box::new(ScriptSettlementResult::Warehouse {
                binding: Box::new(ScriptWarehouseBinding::new(
                    WAREHOUSE.to_owned(),
                    ScriptWarehouseSource::Authored {
                        structure_id: STRUCTURE.to_owned(),
                        container_id: 0,
                    },
                    4,
                )),
            }),
        },
    )
    .expect("the core-owned warehouse result is valid")
}

/// A fresh component reads the durable hiring registry so it can replay any
/// transfer whose final CAS was interrupted.
async fn answer_empty_hire_registry(boundary: &mc_script::ScriptBoundary) {
    let read = boundary
        .recv_command()
        .await
        .expect("the component reads pending hires at startup");
    let admitted = boundary
        .accept_host_command(read)
        .expect("the component's owner-scoped registry read is admitted");
    assert_eq!(admitted.plugin_id(), PACKAGE_ID);
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "settlement-registry-v1"
    ));
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_get_result(None, None)
                .expect("an empty registry is a valid recovery answer"),
        )
        .expect("the component receives its recovery answer");
}

/// Exercise the shipped guest rather than substituting its Rust planner: it
/// reads a live resident, snapshots the real endpoint types, persists an
/// intent, and emits the zero-actor transfer before finalizing employment.
async fn exercise_shipped_component_hire(boundary: &mc_script::ScriptBoundary) {
    let registry = serde_json::json!({
        "settlements": {
            "oak_hollow": {
                "owner": PLAYER,
                "size_class": "small",
                "revision": 1,
                "buildings": {
                    "warehouse_1": {
                        "purpose": "solaris:warehouse",
                        "origin": { "origin": "structure", "structure_id": STRUCTURE }
                    }
                },
                "residents": {
                    "resident_1": { "handle": RESIDENT, "work_revision": 3 }
                }
            }
        }
    })
    .to_string();
    assert_eq!(
        boundary.try_enqueue_player_command_with_context(
            ScriptPlayerId::new(SESSION),
            player_context(),
            "settlement hire oak_hollow resident_1 militia"
        ),
        Ok(PlayerCommandAdmission::Enqueued)
    );
    let read = boundary
        .recv_command()
        .await
        .expect("hire reads the registry");
    let admitted = boundary
        .accept_host_command(read)
        .expect("hire registry read is admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "settlement-registry-v1"
    ));
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_get_result(Some(&registry), Some(5))
                .unwrap(),
        )
        .expect("component sees its civilian roster");

    let query = boundary
        .recv_command()
        .await
        .expect("hire checks resident lifecycle");
    let admitted = boundary
        .accept_host_command(query)
        .expect("resident query is admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::Operation { request }
            if matches!(
                request.operation(),
                ScriptOperation::Resident {
                    operation: ScriptResidentOperation::Query { handles, .. }
                } if handles == &[RESIDENT.to_owned()]
            )
    ));
    let resident = ScriptResidentSnapshot::new(
        RESIDENT.to_owned(),
        PLAYER.to_owned(),
        ScriptResidentLifecycle::AliveUnloaded,
        3,
        None,
        ScriptResidentPois::default(),
        None,
    );
    let outcome = ScriptOperationOutcome::committed(
        3,
        ScriptOperationPayload::Resident {
            result: Box::new(ScriptResidentResult::Page {
                residents: vec![resident],
                cursor: None,
            }),
        },
    )
    .unwrap();
    boundary
        .try_enqueue_event(admitted.operation_result(outcome).unwrap())
        .expect("real guest sees live resident Page through the new WIT mapping");

    let warehouse = ScriptInventoryEndpoint::Warehouse {
        handle: WAREHOUSE.to_owned(),
    };
    let equipment = ScriptInventoryEndpoint::ResidentEquipment {
        handle: RESIDENT.to_owned(),
    };
    let carry = ScriptInventoryEndpoint::ResidentCarry {
        handle: RESIDENT.to_owned(),
    };
    for (endpoint, size) in [(&warehouse, 27u8), (&equipment, 6), (&carry, 8)] {
        let query = boundary
            .recv_command()
            .await
            .expect("hire reads owned inventory");
        let admitted = boundary
            .accept_host_command(query)
            .expect("inventory query is admitted");
        assert!(matches!(
            admitted.request(),
            ScriptCommand::Operation { request }
                if matches!(
                    request.operation(),
                    ScriptOperation::Inventory {
                        operation: ScriptOwnedInventoryOperation::Query { endpoint: actual, .. }
                    } if actual == endpoint
                )
        ));
        let slots = (0..size)
            .map(|slot| {
                let item = if endpoint == &warehouse {
                    let resource = match slot {
                        0 => Some("minecraft:iron_sword"),
                        1 => Some("minecraft:leather_chestplate"),
                        2 => Some("minecraft:emerald"),
                        _ => None,
                    };
                    resource.map(|resource| {
                        ScriptInventoryItem::new(
                            resource.to_owned(),
                            1,
                            None,
                            Vec::new(),
                            None,
                            None,
                        )
                    })
                } else {
                    None
                };
                ScriptInventorySlot::new(slot, item)
            })
            .collect();
        let snapshot = ScriptOwnedInventorySnapshot::new(
            endpoint.clone(),
            ScriptInventoryFence::try_new(4, "a".repeat(64)).unwrap(),
            slots,
        );
        let outcome = ScriptOperationOutcome::committed(
            4,
            ScriptOperationPayload::OwnedInventory {
                result: Box::new(ScriptOwnedInventoryResult::Snapshot {
                    inventory: snapshot,
                }),
            },
        )
        .unwrap();
        boundary
            .try_enqueue_event(admitted.operation_result(outcome).unwrap())
            .expect("real guest reads kit, equipment and payment slots");
    }
    let save = boundary
        .recv_command()
        .await
        .expect("hire persists intent first");
    let admitted = boundary
        .accept_host_command(save)
        .expect("intent CAS is admitted");
    let ScriptCommand::PluginStorageCompareAndSwap { request } = admitted.request() else {
        panic!("hire must save its intent before any item move");
    };
    assert_eq!(request.key(), "settlement-registry-v1");
    assert_eq!(request.expected_version(), Some(5));
    let intent: serde_json::Value = serde_json::from_str(request.value()).unwrap();
    let resident = &intent["settlements"]["oak_hollow"]["residents"]["resident_1"];
    assert_eq!(resident["handle"], RESIDENT);
    assert_eq!(resident["hiring"]["operation_id"], "hire-1");
    assert!(
        resident["role"].is_null(),
        "no role before the native commit"
    );
    let intent_value = request.value().to_owned();
    boundary
        .try_enqueue_event(ScriptEvent::player_left(
            ScriptPlayerId::new(SESSION),
            "disconnected",
        ))
        .expect("a disconnected requester does not cancel durable hiring");
    boundary
        .try_enqueue_event(admitted.plugin_storage_cas_result(true, Some(6)).unwrap())
        .expect("native storage commits the hiring intent");

    let move_items = boundary
        .recv_command()
        .await
        .expect("guest issues kit move");
    let admitted = boundary
        .accept_host_command(move_items)
        .expect("warehouse and resident transfer is admitted without a player");
    let ScriptCommand::Operation { request } = admitted.request() else {
        panic!("a durable inventory transfer must follow the intent CAS");
    };
    assert!(matches!(
        request.operation(),
        ScriptOperation::Inventory {
            operation: ScriptOwnedInventoryOperation::Transfer {
                operation_id,
                actor_id: 0,
                transfers,
                expected_revisions,
            }
        } if operation_id == "hire-1"
            && transfers.len() == 3
            && expected_revisions.len() == 3
            && transfers[0].source == warehouse
            && transfers[0].destination == equipment
            && transfers[0].destination_slot == 0
            && transfers[1].destination_slot == 3
            && transfers[2].destination == carry
            && transfers[2].destination_slot == 0
    ));
    let result = ScriptOperationOutcome::committed(
        8,
        ScriptOperationPayload::OwnedInventory {
            result: Box::new(ScriptOwnedInventoryResult::Transfer {
                inventories: vec![
                    ScriptInventoryExpectedRevision::new(
                        warehouse,
                        ScriptInventoryFence::try_new(4, "b".repeat(64)).unwrap(),
                    ),
                    ScriptInventoryExpectedRevision::new(
                        equipment.clone(),
                        ScriptInventoryFence::try_new(8, "c".repeat(64)).unwrap(),
                    ),
                    ScriptInventoryExpectedRevision::new(
                        carry,
                        ScriptInventoryFence::try_new(8, "d".repeat(64)).unwrap(),
                    ),
                ],
            }),
        },
    )
    .unwrap();
    boundary
        .try_enqueue_event(admitted.operation_result(result).unwrap())
        .expect("core returns the committed gear and signing wage receipt");
    let current_gear = boundary
        .recv_command()
        .await
        .expect("hire checks current gear after native work resumes");
    let admitted = boundary
        .accept_host_command(current_gear)
        .expect("current gear query admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::Operation { request }
            if matches!(
                request.operation(),
                ScriptOperation::Inventory {
                    operation: ScriptOwnedInventoryOperation::Query { endpoint, .. }
                } if endpoint == &equipment
            )
    ));
    let snapshot = ScriptOwnedInventorySnapshot::new(
        equipment,
        ScriptInventoryFence::try_new(10, "e".repeat(64)).unwrap(),
        (0..6)
            .map(|slot| ScriptInventorySlot::new(slot, None))
            .collect(),
    );
    let outcome = ScriptOperationOutcome::committed(
        10,
        ScriptOperationPayload::OwnedInventory {
            result: Box::new(ScriptOwnedInventoryResult::Snapshot {
                inventory: snapshot,
            }),
        },
    )
    .unwrap();
    boundary
        .try_enqueue_event(admitted.operation_result(outcome).unwrap())
        .expect("guest uses the current post-resume resident revision");
    let read = boundary
        .recv_command()
        .await
        .expect("hire re-reads before finalization");
    let admitted = boundary
        .accept_host_command(read)
        .expect("registry re-read admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "settlement-registry-v1"
    ));
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_get_result(Some(&intent_value), Some(6))
                .unwrap(),
        )
        .expect("guest sees its durable pending intent");
    let finish = boundary.recv_command().await.expect("hire finalizes role");
    let admitted = boundary
        .accept_host_command(finish)
        .expect("final CAS admitted");
    let ScriptCommand::PluginStorageCompareAndSwap { request } = admitted.request() else {
        panic!("role must be finalized with a second CAS");
    };
    assert_eq!(request.expected_version(), Some(6));
    let finished: serde_json::Value = serde_json::from_str(request.value()).unwrap();
    let resident = &finished["settlements"]["oak_hollow"]["residents"]["resident_1"];
    assert_eq!(resident["role"], "militia");
    assert_eq!(resident["employer"], "oak_hollow");
    assert_eq!(resident["handle"], RESIDENT);
    assert_eq!(resident["work_revision"], 10);
    assert_eq!(resident["issued"]["warehouse"], WAREHOUSE);
    assert_eq!(resident["issued"]["items"].as_array().unwrap().len(), 2);
    assert!(
        resident["issued"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|piece| piece["item"] != "minecraft:emerald")
    );
    assert!(resident["hiring"].is_null());
    let finished_value = request.value().to_owned();
    boundary
        .try_enqueue_event(admitted.plugin_storage_cas_result(true, Some(7)).unwrap())
        .expect("final hire role is durable");
    let message = boundary.recv_command().await.expect("hire confirmation");
    let admitted = boundary
        .accept_host_command(message)
        .expect("hire chat admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::SendChatMessage { message, .. } if message.contains("hired as militia")
    ));
    let recovery = boundary.recv_command().await.expect("scan remaining hires");
    let admitted = boundary
        .accept_host_command(recovery)
        .expect("post-hire scan admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "settlement-registry-v1"
    ));
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_get_result(Some(&finished_value), Some(7))
                .unwrap(),
        )
        .expect("no pending intent survives finalization");
}

async fn read_dismiss_registry(boundary: &mc_script::ScriptBoundary, value: &str, version: u64) {
    let command = boundary
        .recv_command()
        .await
        .expect("dismissal reads its durable roster");
    let admitted = boundary
        .accept_host_command(command)
        .expect("registry read admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "settlement-registry-v1"
    ));
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_get_result(Some(value), Some(version))
                .unwrap(),
        )
        .expect("the saved roster reaches the component");
}

async fn save_dismiss_registry(boundary: &mc_script::ScriptBoundary, version: u64) -> String {
    let command = boundary
        .recv_command()
        .await
        .expect("dismissal saves its next durable step");
    let admitted = boundary
        .accept_host_command(command)
        .expect("roster CAS admitted");
    let ScriptCommand::PluginStorageCompareAndSwap { request } = admitted.request() else {
        panic!("demobilization must persist its next step before physical work");
    };
    assert_eq!(request.key(), "settlement-registry-v1");
    assert_eq!(request.expected_version(), Some(version));
    let value = request.value().to_owned();
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_cas_result(true, Some(version + 1))
                .unwrap(),
        )
        .expect("durable demobilization step committed");
    value
}

async fn answer_dismiss_equipment_fence(
    boundary: &mc_script::ScriptBoundary,
    revision: u64,
    stock: &[(u8, &str)],
) {
    let command = boundary
        .recv_command()
        .await
        .expect("dismissal checks current equipment fence");
    let admitted = boundary
        .accept_host_command(command)
        .expect("equipment query admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::Operation { request }
            if matches!(request.operation(), ScriptOperation::Inventory {
                operation: ScriptOwnedInventoryOperation::Query {
                    endpoint: ScriptInventoryEndpoint::ResidentEquipment { handle }, ..
                }
            } if handle == RESIDENT)
    ));
    let snapshot = ScriptOwnedInventorySnapshot::new(
        ScriptInventoryEndpoint::ResidentEquipment {
            handle: RESIDENT.to_owned(),
        },
        ScriptInventoryFence::try_new(revision, "e".repeat(64)).unwrap(),
        (0..6)
            .map(|slot| {
                ScriptInventorySlot::new(
                    slot,
                    stock
                        .iter()
                        .find(|(where_, _)| *where_ == slot)
                        .map(|(_, id)| {
                            ScriptInventoryItem::new(
                                (*id).to_owned(),
                                1,
                                None,
                                Vec::new(),
                                None,
                                None,
                            )
                        }),
                )
            })
            .collect(),
    );
    let result = ScriptOperationOutcome::committed(
        revision,
        ScriptOperationPayload::OwnedInventory {
            result: Box::new(ScriptOwnedInventoryResult::Snapshot {
                inventory: snapshot,
            }),
        },
    )
    .unwrap();
    boundary
        .try_enqueue_event(admitted.operation_result(result).unwrap())
        .expect("authoritative resident gear fence reaches the guest");
}

#[tokio::test]
#[ignore = "requires the explicit first-party settlement component integration gate"]
async fn shipped_dismissal_returns_issued_gear_then_restores_same_civilian() {
    let staged = tempfile::tempdir().expect("temporary deployment root");
    copy_package(
        &plugins_root().join(PACKAGE_ID),
        &staged.path().join(PACKAGE_ID),
    );
    let limits = PluginLimits::default();
    let packages = mc_plugin_host::discover(&deployment(staged.path()), &limits)
        .expect("shipped dismissal package admitted")
        .into_packages();
    let host = start_deployment(packages, limits, HostQueues::default(), Arc::new(Sessions))
        .expect("shipped component starts");
    let boundary = host.boundary().clone();
    macro_rules! next {
        ($stage:literal) => {
            tokio::time::timeout(std::time::Duration::from_secs(20), boundary.recv_command())
                .await
                .expect(concat!("guest timed out at ", $stage))
                .expect(concat!("guest command channel closed at ", $stage))
        };
    }

    let startup = next!("dismissal startup counter");
    let admitted = boundary
        .accept_host_command(startup)
        .expect("counter read admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "operation-counter"
    ));
    boundary
        .try_enqueue_event(admitted.plugin_storage_get_result(None, None).unwrap())
        .expect("guest receives its counter");
    answer_empty_hire_registry(&boundary).await;

    assert_eq!(
        boundary.try_enqueue_player_command_with_context(
            ScriptPlayerId::new(SESSION),
            player_context(),
            "settlement dismiss oak_hollow soldier"
        ),
        Ok(PlayerCommandAdmission::Enqueued)
    );
    let mut registry = serde_json::json!({
        "settlements": {
            "oak_hollow": {
                "owner": PLAYER, "size_class": "small", "revision": 1,
                "residents": {
                    "soldier": {
                        "handle": RESIDENT, "work_revision": 4, "role": "militia",
                        "employer": "oak_hollow",
                        "issued": {
                            "warehouse": WAREHOUSE,
                            "items": [
                                {"item": "minecraft:iron_sword", "count": 1,
                                 "damage": null, "enchantments": [],
                                 "custom_name": null, "item_model": null,
                                 "destination": {"Equipment": 0}},
                                {"item": "minecraft:leather_chestplate", "count": 1,
                                 "damage": null, "enchantments": [],
                                 "custom_name": null, "item_model": null,
                                 "destination": {"Equipment": 3}}
                            ]
                        }
                    }
                }
            }
        }
    })
    .to_string();
    read_dismiss_registry(&boundary, &registry, 5).await;
    let query = next!("dismissal resident query");
    let admitted = boundary
        .accept_host_command(query)
        .expect("resident query admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::Operation { request }
            if matches!(request.operation(), ScriptOperation::Resident {
                operation: ScriptResidentOperation::Query { handles, .. }
            } if handles == &[RESIDENT.to_owned()])
    ));
    let resident = ScriptResidentSnapshot::new(
        RESIDENT.to_owned(),
        PLAYER.to_owned(),
        ScriptResidentLifecycle::AliveLoaded,
        4,
        None,
        ScriptResidentPois::default(),
        Some(mc_script::ScriptResidentLoadedState::new(
            mc_script::ScriptPosition::try_new(0.0, 64.0, 0.0).unwrap(),
            20.0,
            Vec::new(),
        )),
    );
    let result = ScriptOperationOutcome::committed(
        4,
        ScriptOperationPayload::Resident {
            result: Box::new(ScriptResidentResult::Page {
                residents: vec![resident],
                cursor: None,
            }),
        },
    )
    .unwrap();
    boundary
        .try_enqueue_event(admitted.operation_result(result).unwrap())
        .expect("guest sees a live soldier");
    answer_dismiss_equipment_fence(
        &boundary,
        11,
        &[
            (0, "minecraft:iron_sword"),
            (3, "minecraft:leather_chestplate"),
        ],
    )
    .await;
    registry = save_dismiss_registry(&boundary, 5).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&registry).unwrap()["settlements"]["oak_hollow"]
            ["residents"]["soldier"]["dismissing"]["step"]["kind"],
        "stop"
    );

    let stop = next!("native stop order");
    let admitted = boundary
        .accept_host_command(stop)
        .expect("demobilization admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::Operation { request }
            if matches!(request.operation(), ScriptOperation::ResidentOrder {
                operation: ScriptResidentOrderOperation::Demobilize {
                    operation_id, handle, expected_revision
                }
            } if operation_id == "dismiss-stop-1" && handle == RESIDENT && *expected_revision == 11)
    ));
    let result = ScriptOperationOutcome::committed(
        12,
        ScriptOperationPayload::ResidentOrder {
            result: Box::new(ScriptResidentOrderResult::Demobilized {
                resident: Box::new(mc_script::ScriptDemobilizeResult::new(
                    RESIDENT.to_owned(),
                    mc_script::ScriptDemobilizeState::Demobilizing,
                    Some(mc_script::ScriptWorkPauseReason::Interrupted),
                    Vec::new(),
                    12,
                )),
            }),
        },
    )
    .unwrap();
    boundary
        .try_enqueue_event(admitted.operation_result(result).unwrap())
        .expect("military goals stop before items move");

    let warehouse = ScriptInventoryEndpoint::Warehouse {
        handle: WAREHOUSE.to_owned(),
    };
    let equipment = ScriptInventoryEndpoint::ResidentEquipment {
        handle: RESIDENT.to_owned(),
    };
    let carry = ScriptInventoryEndpoint::ResidentCarry {
        handle: RESIDENT.to_owned(),
    };
    for (endpoint, size) in [
        (warehouse.clone(), 27u8),
        (equipment.clone(), 6),
        (carry.clone(), 8),
    ] {
        let query = next!("dismissal inventory query");
        let admitted = boundary
            .accept_host_command(query)
            .expect("inventory query admitted");
        assert!(matches!(
            admitted.request(),
            ScriptCommand::Operation { request }
                if matches!(request.operation(), ScriptOperation::Inventory {
                    operation: ScriptOwnedInventoryOperation::Query { endpoint: actual, .. }
                } if actual == &endpoint)
        ));
        let slots = (0..size)
            .map(|slot| {
                let resource = if endpoint == equipment {
                    match slot {
                        0 => Some("minecraft:iron_sword"),
                        3 => Some("minecraft:leather_chestplate"),
                        _ => None,
                    }
                } else if endpoint == carry && slot == 0 {
                    Some("minecraft:emerald")
                } else {
                    None
                };
                ScriptInventorySlot::new(
                    slot,
                    resource.map(|id| {
                        ScriptInventoryItem::new(id.to_owned(), 1, None, Vec::new(), None, None)
                    }),
                )
            })
            .collect();
        let revision = if endpoint == warehouse { 11 } else { 12 };
        let snapshot = ScriptOwnedInventorySnapshot::new(
            endpoint,
            ScriptInventoryFence::try_new(revision, "a".repeat(64)).unwrap(),
            slots,
        );
        let result = ScriptOperationOutcome::committed(
            revision,
            ScriptOperationPayload::OwnedInventory {
                result: Box::new(ScriptOwnedInventoryResult::Snapshot {
                    inventory: snapshot,
                }),
            },
        )
        .unwrap();
        boundary
            .try_enqueue_event(admitted.operation_result(result).unwrap())
            .expect("guest sees issued equipment and personal wage");
    }
    read_dismiss_registry(&boundary, &registry, 6).await;
    registry = save_dismiss_registry(&boundary, 6).await;
    let planned: serde_json::Value = serde_json::from_str(&registry).unwrap();
    assert_eq!(
        planned["settlements"]["oak_hollow"]["residents"]["soldier"]["dismissing"]["step"]["kind"],
        "return"
    );

    let transfer = next!("native issued-gear return");
    let admitted = boundary
        .accept_host_command(transfer)
        .expect("return transfer admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::Operation { request }
            if matches!(request.operation(), ScriptOperation::Inventory {
                operation: ScriptOwnedInventoryOperation::Transfer {
                    operation_id, actor_id: 0, transfers, expected_revisions
                }
            } if operation_id == "dismiss-return-1-0"
                && transfers.len() == 2
                && transfers.iter().all(|move_| move_.source == equipment
                    && move_.destination == warehouse)
                && expected_revisions.len() == 2)
    ));
    let result = ScriptOperationOutcome::committed(
        13,
        ScriptOperationPayload::OwnedInventory {
            result: Box::new(ScriptOwnedInventoryResult::Transfer {
                inventories: vec![
                    ScriptInventoryExpectedRevision::new(
                        warehouse,
                        ScriptInventoryFence::try_new(11, "b".repeat(64)).unwrap(),
                    ),
                    ScriptInventoryExpectedRevision::new(
                        equipment,
                        ScriptInventoryFence::try_new(13, "c".repeat(64)).unwrap(),
                    ),
                ],
            }),
        },
    )
    .unwrap();
    boundary
        .try_enqueue_event(admitted.operation_result(result).unwrap())
        .expect("core returns committed gear transfer receipt");
    answer_dismiss_equipment_fence(&boundary, 14, &[]).await;
    read_dismiss_registry(&boundary, &registry, 7).await;
    registry = save_dismiss_registry(&boundary, 7).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&registry).unwrap()["settlements"]["oak_hollow"]
            ["residents"]["soldier"]["dismissing"]["step"]["revision"],
        14
    );

    let finish = next!("native civilian transition");
    let admitted = boundary
        .accept_host_command(finish)
        .expect("civilian transition admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::Operation { request }
            if matches!(request.operation(), ScriptOperation::ResidentOrder {
                operation: ScriptResidentOrderOperation::Demobilize {
                    operation_id, handle, expected_revision
                }
            } if operation_id == "dismiss-finish-1" && handle == RESIDENT && *expected_revision == 14)
    ));
    let result = ScriptOperationOutcome::committed(
        15,
        ScriptOperationPayload::ResidentOrder {
            result: Box::new(ScriptResidentOrderResult::Demobilized {
                resident: Box::new(mc_script::ScriptDemobilizeResult::new(
                    RESIDENT.to_owned(),
                    mc_script::ScriptDemobilizeState::Civilian,
                    None,
                    Vec::new(),
                    15,
                )),
            }),
        },
    )
    .unwrap();
    boundary
        .try_enqueue_event(admitted.operation_result(result).unwrap())
        .expect("resident returns to civilian");
    read_dismiss_registry(&boundary, &registry, 8).await;
    registry = save_dismiss_registry(&boundary, 8).await;
    let civilian: serde_json::Value = serde_json::from_str(&registry).unwrap();
    let soldier = &civilian["settlements"]["oak_hollow"]["residents"]["soldier"];
    assert_eq!(soldier["handle"], RESIDENT);
    assert_eq!(soldier["work_revision"], 15);
    assert!(soldier["role"].is_null() && soldier["employer"].is_null());
    assert!(soldier["issued"].is_null() && soldier["dismissing"].is_null());
    let chat = next!("dismissal confirmation");
    let admitted = boundary
        .accept_host_command(chat)
        .expect("confirmation admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::SendChatMessage { message, .. }
            if message.contains("returned to civilian work")
    ));
    read_dismiss_registry(&boundary, &registry, 9).await;
    host.stop();
}

#[tokio::test]
#[ignore = "requires the explicit first-party settlement component integration gate"]
async fn shipped_guard_uses_server_issued_target_fences_across_hold_and_attack() {
    let staged = tempfile::tempdir().expect("temporary deployment root");
    copy_package(
        &plugins_root().join(PACKAGE_ID),
        &staged.path().join(PACKAGE_ID),
    );
    let limits = PluginLimits::default();
    let packages = mc_plugin_host::discover(&deployment(staged.path()), &limits)
        .expect("shipped guard component is admitted")
        .into_packages();
    let host = start_deployment(packages, limits, HostQueues::default(), Arc::new(Sessions))
        .expect("shipped guard component starts");
    let boundary = host.boundary().clone();
    macro_rules! next {
        ($stage:literal) => {
            tokio::time::timeout(std::time::Duration::from_secs(20), boundary.recv_command())
                .await
                .expect(concat!("guest timed out at ", $stage))
                .expect(concat!("guest command channel closed at ", $stage))
        };
    }

    let startup = next!("counter read");
    let admitted = boundary
        .accept_host_command(startup)
        .expect("counter admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "operation-counter"
    ));
    boundary
        .try_enqueue_event(admitted.plugin_storage_get_result(None, None).unwrap())
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        answer_empty_hire_registry(&boundary),
    )
    .await
    .expect("guest timed out at hiring registry read");

    assert_eq!(
        boundary.try_enqueue_player_command_with_context(
            ScriptPlayerId::new(SESSION),
            player_context(),
            "settlement defend oak_hollow resident_1 0 4 64 4 1 64 4",
        ),
        Ok(PlayerCommandAdmission::Enqueued)
    );
    let read = next!("guard registry read");
    let admitted = boundary
        .accept_host_command(read)
        .expect("registry read admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "settlement-registry-v1"
    ));
    let roster = serde_json::json!({
        "settlements": {
            "oak_hollow": {
                "owner": PLAYER,
                "size_class": "small",
                "revision": 1,
                "buildings": {},
                "residents": {
                    "resident_1": {
                        "handle": RESIDENT,
                        "role": "archer",
                        "employer": "oak_hollow"
                    }
                }
            }
        }
    })
    .to_string();
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_get_result(Some(&roster), Some(1))
                .unwrap(),
        )
        .unwrap();
    let counter = next!("hold counter reservation");
    let admitted = boundary
        .accept_host_command(counter)
        .expect("counter admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageCompareAndSwap { request }
            if request.key() == "operation-counter" && request.expected_version().is_none()
    ));
    boundary
        .try_enqueue_event(admitted.plugin_storage_cas_result(true, Some(1)).unwrap())
        .unwrap();
    let hold = next!("native hold order");
    let admitted = boundary
        .accept_host_command(hold)
        .expect("hold order admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::Operation { request }
            if matches!(
                request.operation(),
                ScriptOperation::ResidentOrder {
                    operation: ScriptResidentOrderOperation::IssueOrder {
                        expected_order_revisions,
                        order: ScriptResidentOrder::Hold { anchor, .. },
                        ..
                    }
                } if expected_order_revisions == &[0]
                    && anchor == &ScriptBlockPosition::new(4, 64, 4)
            )
    ));
    let target = ScriptOrderTarget::new(
        "issued-hostile".to_owned(),
        0,
        29,
        ScriptHostileCategory::Hostile,
        ScriptBlockPosition::new(6, 64, 4),
    );
    let hold_result = ScriptOperationOutcome::committed(
        9,
        ScriptOperationPayload::ResidentOrder {
            result: Box::new(ScriptResidentOrderResult::Order {
                order_revision: 9,
                members: vec![ScriptOrderMemberOutcome::new(
                    RESIDENT.to_owned(),
                    ScriptOrderMemberState::Applied,
                    Some(0),
                    vec![target],
                )],
                combat: Vec::new(),
            }),
        },
    )
    .unwrap();
    boundary
        .try_enqueue_event(admitted.operation_result(hold_result).unwrap())
        .unwrap();
    let counter = next!("attack counter reservation");
    let admitted = boundary
        .accept_host_command(counter)
        .expect("counter admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageCompareAndSwap { request }
            if request.key() == "operation-counter" && request.expected_version() == Some(1)
    ));
    boundary
        .try_enqueue_event(admitted.plugin_storage_cas_result(true, Some(2)).unwrap())
        .unwrap();
    let attack = next!("native attack order");
    let admitted = boundary
        .accept_host_command(attack)
        .expect("attack order admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::Operation { request }
            if matches!(
                request.operation(),
                ScriptOperation::ResidentOrder {
                    operation: ScriptResidentOrderOperation::IssueOrder {
                        expected_order_revisions,
                        order: ScriptResidentOrder::Attack { targets, policy },
                        ..
                    }
                } if expected_order_revisions == &[9]
                    && targets.len() == 1
                    && targets[0].target_ref == "issued-hostile"
                    && targets[0].policy_revision == 0
                    && targets[0].expires_revision == 29
                    && policy.permitted == [ScriptHostileCategory::Hostile]
                    && policy.allies == [RESIDENT]
                    && policy.rally == Some(ScriptBlockPosition::new(1, 64, 4))
            )
    ));
    let attack_result = ScriptOperationOutcome::committed(
        10,
        ScriptOperationPayload::ResidentOrder {
            result: Box::new(ScriptResidentOrderResult::Order {
                order_revision: 10,
                members: vec![ScriptOrderMemberOutcome::new(
                    RESIDENT.to_owned(),
                    ScriptOrderMemberState::Applied,
                    Some(0),
                    Vec::new(),
                )],
                combat: Vec::new(),
            }),
        },
    )
    .unwrap();
    boundary
        .try_enqueue_event(admitted.operation_result(attack_result).unwrap())
        .unwrap();
    let message = next!("guard order report");
    let admitted = boundary
        .accept_host_command(message)
        .expect("report admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::SendChatMessage { message, .. }
            if message.contains("Order revision 10")
    ));
    host.stop();
}

#[tokio::test]
#[ignore = "requires the explicit first-party settlement component integration gate"]
async fn shipped_squad_marches_and_cancels_one_fenced_roster() {
    let staged = tempfile::tempdir().expect("temporary deployment root");
    copy_package(
        &plugins_root().join(PACKAGE_ID),
        &staged.path().join(PACKAGE_ID),
    );
    let limits = PluginLimits::default();
    let packages = mc_plugin_host::discover(&deployment(staged.path()), &limits)
        .expect("shipped squad component is admitted")
        .into_packages();
    let host = start_deployment(packages, limits, HostQueues::default(), Arc::new(Sessions))
        .expect("shipped squad component starts");
    let boundary = host.boundary().clone();
    macro_rules! next {
        ($stage:literal) => {
            tokio::time::timeout(std::time::Duration::from_secs(20), boundary.recv_command())
                .await
                .expect(concat!("guest timed out at ", $stage))
                .expect(concat!("guest command channel closed at ", $stage))
        };
    }
    let startup = next!("squad counter read");
    let admitted = boundary
        .accept_host_command(startup)
        .expect("counter admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "operation-counter"
    ));
    boundary
        .try_enqueue_event(admitted.plugin_storage_get_result(None, None).unwrap())
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        answer_empty_hire_registry(&boundary),
    )
    .await
    .expect("guest timed out at squad hiring registry read");
    let roster = serde_json::json!({
        "settlements": {
            "oak_hollow": {
                "owner": PLAYER,
                "size_class": "small",
                "revision": 1,
                "residents": {
                    "archer": {"handle": RESIDENT, "role": "archer", "employer": "oak_hollow"},
                    "spear": {"handle": "resident-2", "role": "spearman", "employer": "oak_hollow"}
                }
            }
        }
    })
    .to_string();
    assert_eq!(
        boundary.try_enqueue_player_command_with_context(
            ScriptPlayerId::new(SESSION),
            player_context(),
            "settlement march oak_hollow spear:5,archer:0 13 64 8",
        ),
        Ok(PlayerCommandAdmission::Enqueued)
    );
    let read = next!("squad registry read");
    let admitted = boundary
        .accept_host_command(read)
        .expect("registry read admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "settlement-registry-v1"
    ));
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_get_result(Some(&roster), Some(1))
                .unwrap(),
        )
        .unwrap();
    let counter = next!("squad counter reservation");
    let admitted = boundary
        .accept_host_command(counter)
        .expect("counter admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageCompareAndSwap { request }
            if request.key() == "operation-counter" && request.expected_version().is_none()
    ));
    boundary
        .try_enqueue_event(admitted.plugin_storage_cas_result(true, Some(1)).unwrap())
        .unwrap();
    let order = next!("native squad order");
    let admitted = boundary
        .accept_host_command(order)
        .expect("squad order admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::Operation { request }
            if matches!(
                request.operation(),
                ScriptOperation::ResidentOrder {
                    operation: ScriptResidentOrderOperation::IssueOrder {
                        handles,
                        expected_order_revisions,
                        order: ScriptResidentOrder::Move { anchor, .. },
                        ..
                    }
                } if handles == &[RESIDENT, "resident-2"]
                    && expected_order_revisions == &[0, 5]
                    && anchor == &ScriptBlockPosition::new(13, 64, 8)
            )
    ));
    let outcome = ScriptOperationOutcome::committed(
        9,
        ScriptOperationPayload::ResidentOrder {
            result: Box::new(ScriptResidentOrderResult::Order {
                order_revision: 9,
                members: vec![
                    ScriptOrderMemberOutcome::new(
                        RESIDENT.to_owned(),
                        ScriptOrderMemberState::Applied,
                        Some(0),
                        Vec::new(),
                    ),
                    ScriptOrderMemberOutcome::new(
                        "resident-2".to_owned(),
                        ScriptOrderMemberState::BlockedRoute,
                        None,
                        Vec::new(),
                    ),
                ],
                combat: Vec::new(),
            }),
        },
    )
    .unwrap();
    boundary
        .try_enqueue_event(admitted.operation_result(outcome).unwrap())
        .unwrap();
    let report = next!("squad movement report");
    let admitted = boundary
        .accept_host_command(report)
        .expect("movement report admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::SendChatMessage { message, .. }
            if message.contains("1/2 applied") && message.contains("resident-2")
    ));

    assert_eq!(
        boundary.try_enqueue_player_command_with_context(
            ScriptPlayerId::new(SESSION),
            player_context(),
            "settlement halt oak_hollow archer:9,spear:9",
        ),
        Ok(PlayerCommandAdmission::Enqueued)
    );
    let read = next!("halt registry read");
    let admitted = boundary
        .accept_host_command(read)
        .expect("halt registry read admitted");
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_get_result(Some(&roster), Some(1))
                .unwrap(),
        )
        .unwrap();
    let counter = next!("halt counter reservation");
    let admitted = boundary
        .accept_host_command(counter)
        .expect("halt counter admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageCompareAndSwap { request }
            if request.key() == "operation-counter" && request.expected_version() == Some(1)
    ));
    boundary
        .try_enqueue_event(admitted.plugin_storage_cas_result(true, Some(2)).unwrap())
        .unwrap();
    let cancel = next!("native squad cancellation");
    let admitted = boundary
        .accept_host_command(cancel)
        .expect("squad cancellation admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::Operation { request }
            if matches!(
                request.operation(),
                ScriptOperation::ResidentOrder {
                    operation: ScriptResidentOrderOperation::CancelOrder {
                        handles,
                        expected_order_revisions,
                        ..
                    }
                } if handles == &[RESIDENT, "resident-2"]
                    && expected_order_revisions == &[9, 9]
            )
    ));
    let outcome = ScriptOperationOutcome::committed(
        10,
        ScriptOperationPayload::ResidentOrder {
            result: Box::new(ScriptResidentOrderResult::OrderCancelled {
                order_revision: 10,
                members: vec![
                    ScriptOrderMemberOutcome::new(
                        RESIDENT.to_owned(),
                        ScriptOrderMemberState::Applied,
                        Some(0),
                        Vec::new(),
                    ),
                    ScriptOrderMemberOutcome::new(
                        "resident-2".to_owned(),
                        ScriptOrderMemberState::Applied,
                        Some(1),
                        Vec::new(),
                    ),
                ],
            }),
        },
    )
    .unwrap();
    boundary
        .try_enqueue_event(admitted.operation_result(outcome).unwrap())
        .unwrap();
    let report = next!("squad cancellation report");
    let admitted = boundary
        .accept_host_command(report)
        .expect("cancellation report admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::SendChatMessage { message, .. }
            if message.contains("halted") && message.contains("2/2 applied")
    ));
    host.stop();
}

#[tokio::test]
#[ignore = "requires the explicit first-party settlement component integration gate"]
async fn shipped_component_emits_admitted_resident_warehouse_cargo_moves() {
    let staged = tempfile::tempdir().expect("temporary deployment root");
    copy_package(
        &plugins_root().join(PACKAGE_ID),
        &staged.path().join(PACKAGE_ID),
    );

    let limits = PluginLimits::default();
    let packages = mc_plugin_host::discover(&deployment(staged.path()), &limits)
        .expect("the full shipped package is a strict component deployment")
        .into_packages();
    let host = start_deployment(packages, limits, HostQueues::default(), Arc::new(Sessions))
        .expect("the shipped component starts");
    let boundary = host.boundary().clone();

    assert_eq!(
        boundary.player_command_roots(),
        vec!["settlement".to_owned()],
        "the production command root comes from the shipped package"
    );
    assert_eq!(
        boundary.try_enqueue_player_command_with_context(
            ScriptPlayerId::new(SESSION),
            player_context(),
            "settlement bind warehouse-1 0",
        ),
        Ok(PlayerCommandAdmission::Enqueued),
        "the bind command reaches the real component through the production boundary"
    );
    let storage_read = boundary
        .recv_command()
        .await
        .expect("the component reads its durable counter before issuing work");
    let admitted = boundary
        .accept_host_command(storage_read)
        .expect("the server admits the component's durable read");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "operation-counter"
    ));
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_get_result(None, None)
                .expect("the empty counter is a valid core storage result"),
        )
        .expect("the core storage result reaches the actual component");
    answer_empty_hire_registry(&boundary).await;
    let counter_write = boundary
        .recv_command()
        .await
        .expect("the component durably allocates its bind operation id");
    let admitted = boundary
        .accept_host_command(counter_write)
        .expect("the server admits the component's durable counter write");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageCompareAndSwap { request }
            if request.key() == "operation-counter" && request.expected_version().is_none()
    ));
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_cas_result(true, Some(1))
                .expect("the core records the component's counter"),
        )
        .expect("the counter result reaches the actual component");
    let bind = boundary
        .recv_command()
        .await
        .expect("the component asks core to bind its warehouse");
    let admitted = boundary
        .accept_host_command(bind)
        .expect("the server admits the component's bind request");
    let ScriptCommand::Operation { request } = admitted.request() else {
        panic!(
            "expected the component bind operation, saw {:?}",
            admitted.request()
        );
    };
    assert!(matches!(
        request.operation(),
        ScriptOperation::Settlement {
            operation: ScriptSettlementOperation::BindWarehouse {
                structure_id,
                container_id: 0,
                ..
            },
        } if structure_id == STRUCTURE
    ));
    boundary
        .try_enqueue_event(
            admitted
                .operation_result(bind_outcome())
                .expect("the core answer is correlated to the admitted bind"),
        )
        .expect("the core warehouse answer is delivered back to the component");
    let bound = boundary
        .recv_command()
        .await
        .expect("the component reports its accepted binding");
    let admitted_bound = boundary
        .accept_host_command(bound)
        .expect("the host admits the component's binding report");
    assert!(matches!(
        admitted_bound.request(),
        ScriptCommand::SendChatMessage { message, .. } if message == "Warehouse bound."
    ));

    assert_eq!(
        boundary.try_enqueue_player_command_with_context(
            ScriptPlayerId::new(SESSION),
            player_context(),
            "settlement haul resident-1 3 minecraft:birch_log",
        ),
        Ok(PlayerCommandAdmission::Enqueued),
        "the haul command reaches the same running component"
    );
    let counter_write = boundary
        .recv_command()
        .await
        .expect("the component durably allocates its haul operation id");
    let admitted = boundary
        .accept_host_command(counter_write)
        .expect("the server admits the component's haul counter write");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageCompareAndSwap { request }
            if request.key() == "operation-counter" && request.expected_version() == Some(1)
    ));
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_cas_result(true, Some(2))
                .expect("the core records the component's haul counter"),
        )
        .expect("the counter result reaches the actual component");
    let haul = boundary
        .recv_command()
        .await
        .expect("the component emits its haul operation");
    let admitted = boundary
        .accept_host_command(haul)
        .expect("the existing owner admits the component's haul operation");
    let ScriptCommand::Operation { request } = admitted.request() else {
        panic!(
            "expected the component haul operation, saw {:?}",
            admitted.request()
        );
    };
    assert!(matches!(
        request.operation(),
        ScriptOperation::ResidentOrder {
            operation: ScriptResidentOrderOperation::AssignWork {
                handle,
                work: ScriptResidentWorkOrder::Haul {
                    source: ScriptInventoryEndpoint::ResidentCarry { handle: source },
                    destination: ScriptInventoryEndpoint::Warehouse { handle: destination },
                    item: Some(item),
                },
                expected_revision: 3,
                ..
            },
        } if handle == RESIDENT
            && source == RESIDENT
            && destination == WAREHOUSE
            && item == "minecraft:birch_log"
    ));

    assert_eq!(
        boundary.try_enqueue_player_command_with_context(
            ScriptPlayerId::new(SESSION),
            player_context(),
            "settlement issue resident-1 3 carry minecraft:birch_log",
        ),
        Ok(PlayerCommandAdmission::Enqueued),
        "the issue command reaches the same running component"
    );
    let counter_write = boundary
        .recv_command()
        .await
        .expect("the component durably allocates its withdrawal operation id");
    let admitted = boundary
        .accept_host_command(counter_write)
        .expect("the server admits the component's withdrawal counter write");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageCompareAndSwap { request }
            if request.key() == "operation-counter" && request.expected_version() == Some(2)
    ));
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_cas_result(true, Some(3))
                .expect("the core records the component's withdrawal counter"),
        )
        .expect("the counter result reaches the actual component");
    let withdrawal = boundary
        .recv_command()
        .await
        .expect("the component emits its withdrawal operation");
    let admitted = boundary
        .accept_host_command(withdrawal)
        .expect("the existing owner admits the component's withdrawal operation");
    let ScriptCommand::Operation { request } = admitted.request() else {
        panic!(
            "expected the component withdrawal operation, saw {:?}",
            admitted.request()
        );
    };
    assert!(matches!(
        request.operation(),
        ScriptOperation::ResidentOrder {
            operation: ScriptResidentOrderOperation::AssignWork {
                handle,
                work: ScriptResidentWorkOrder::Haul {
                    source: ScriptInventoryEndpoint::Warehouse { handle: source },
                    destination: ScriptInventoryEndpoint::ResidentCarry { handle: destination },
                    item: Some(item),
                },
                expected_revision: 3,
                ..
            },
        } if handle == RESIDENT
            && source == WAREHOUSE
            && destination == RESIDENT
            && item == "minecraft:birch_log"
    ));

    exercise_shipped_component_hire(&boundary).await;
    host.stop();
}

#[tokio::test]
#[ignore = "requires the explicit first-party settlement component integration gate"]
async fn shipped_component_key_request_opens_authoritative_settlement_view() {
    let staged = tempfile::tempdir().expect("temporary deployment root");
    copy_package(
        &plugins_root().join(PACKAGE_ID),
        &staged.path().join(PACKAGE_ID),
    );
    let limits = PluginLimits::default();
    let packages = mc_plugin_host::discover(&deployment(staged.path()), &limits)
        .expect("the full shipped package is admitted with its view bundle")
        .into_packages();
    let host = start_deployment(packages, limits, HostQueues::default(), Arc::new(Sessions))
        .expect("the first-party component starts");
    let boundary = host.boundary().clone();

    let startup = tokio::time::timeout(std::time::Duration::from_secs(20), boundary.recv_command())
        .await
        .expect("guest counter read was not emitted")
        .expect("guest counter read channel closed");
    let admitted = boundary
        .accept_host_command(startup)
        .expect("counter read admitted");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "operation-counter"
    ));
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_get_result(Some("0"), Some(1))
                .unwrap(),
        )
        .expect("the real guest receives its counter");

    answer_empty_hire_registry(&boundary).await;
    boundary
        .try_enqueue_event(
            ScriptEvent::loader_view_request(
                PACKAGE_ID,
                ScriptPlayerId::new(SESSION),
                ScriptClientViewRequestKind::Settlement,
            )
            .expect("a targeted key request is bounded"),
        )
        .expect("the real guest receives a key request");
    let player_query =
        tokio::time::timeout(std::time::Duration::from_secs(20), boundary.recv_command())
            .await
            .expect("guest player query was not emitted")
            .expect("guest player query channel closed");
    let admitted = boundary
        .accept_host_command(player_query)
        .expect("the package declares and is granted player_queries");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::ListOnlinePlayers { .. }
    ));
    boundary
        .try_enqueue_event(
            admitted
                .online_players_result(
                    vec![
                        ScriptOnlinePlayerSnapshot::try_new(
                            ScriptPlayerId::new(SESSION),
                            player_context(),
                            "minecraft:overworld",
                        )
                        .unwrap(),
                    ],
                    false,
                )
                .unwrap(),
        )
        .expect("the guest receives the exact-session identity");

    let registry_read =
        tokio::time::timeout(std::time::Duration::from_secs(20), boundary.recv_command())
            .await
            .expect("guest settlement registry read was not emitted")
            .expect("guest registry channel closed");
    let admitted = boundary
        .accept_host_command(registry_read)
        .expect("the guest can read its durable registry");
    assert!(matches!(
        admitted.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "settlement-registry-v1"
    ));
    let registry = format!(
        "{{\"settlements\":{{\"oak_hollow\":{{\"owner\":\"{PLAYER}\",\"size_class\":\"small\",\"revision\":1}}}}}}"
    );
    boundary
        .try_enqueue_event(
            admitted
                .plugin_storage_get_result(Some(&registry), Some(1))
                .unwrap(),
        )
        .expect("the real guest receives its durable settlement");
    let open = tokio::time::timeout(std::time::Duration::from_secs(20), boundary.recv_command())
        .await
        .expect("the real guest did not open the settlement view")
        .expect("guest view channel closed");
    let admitted = boundary
        .accept_host_command(open)
        .expect("the WIT-converted, owner-scoped view is admitted");
    let ScriptCommand::OpenClientView { request } = admitted.request() else {
        panic!("expected the settlement view, saw {:?}", admitted.request());
    };
    assert_eq!(request.player_id(), ScriptPlayerId::new(SESSION));
    assert_eq!(request.owned_view_id(), "solaris-settlements:overview");
    assert_eq!(request.model().page(), 0);
    assert!(
        request.model().actions().iter().any(|action| {
            action.action_id() == "solaris-settlements:refresh" && action.enabled()
        })
    );
    boundary
        .try_enqueue_event(
            ScriptEvent::client_view_opened(
                PACKAGE_ID,
                request.request_id(),
                ScriptPlayerId::new(SESSION),
                Some("solaris:view-1".to_owned()),
                Some(1),
                None,
            )
            .unwrap(),
        )
        .expect("the guest receives an acknowledged view instance");
    let fields = || {
        vec![
            ScriptClientViewField::try_text("building", String::new()).unwrap(),
            ScriptClientViewField::try_selected("rotation", "90").unwrap(),
            ScriptClientViewField::try_number("x", 32.0).unwrap(),
            ScriptClientViewField::try_number("y", 64.0).unwrap(),
            ScriptClientViewField::try_number("z", -16.0).unwrap(),
        ]
    };
    boundary
        .try_enqueue_event(
            ScriptEvent::loader_view_action(
                PACKAGE_ID,
                ScriptPlayerId::new(SESSION),
                "solaris:view-1",
                1,
                "solaris-settlements:preview_gate",
                1,
                fields(),
                None,
            )
            .unwrap(),
        )
        .expect("owner-admitted preview action reaches the real guest");
    let select = tokio::time::timeout(std::time::Duration::from_secs(20), boundary.recv_command())
        .await
        .expect("real guest did not request a selection")
        .expect("guest selection command channel closed");
    let admitted = boundary
        .accept_host_command(select)
        .expect("WIT selection command is admitted");
    let ScriptCommand::BeginClientSelection {
        request_id,
        view_revision,
        action_id,
        constraints,
        ..
    } = admitted.request()
    else {
        panic!("expected begin selection, saw {:?}", admitted.request());
    };
    assert_eq!(*view_revision, 1);
    assert_eq!(action_id, "solaris-settlements:project_gate");
    assert_eq!(constraints.range_limit(), 128);
    let request_id = request_id.clone();
    boundary
        .try_enqueue_event(
            ScriptEvent::client_selection_started(
                PACKAGE_ID,
                request_id,
                ScriptPlayerId::new(SESSION),
                Some("solaris:selection-1".to_owned()),
                Some(1200),
                None,
            )
            .unwrap(),
        )
        .expect("guest receives server-issued selection context");
    let present = tokio::time::timeout(std::time::Duration::from_secs(20), boundary.recv_command())
        .await
        .expect("guest did not present selected gate")
        .expect("guest view-present channel closed");
    let admitted = boundary
        .accept_host_command(present)
        .expect("WIT view marker is admitted");
    let ScriptCommand::PresentClientView { request: presented } = admitted.request() else {
        panic!("expected selected preview, saw {:?}", admitted.request());
    };
    assert_eq!(presented.expected_revision(), 1);
    assert!(presented.model().markers().iter().any(|marker| {
        marker.marker_id() == "placement_90"
            && marker.selection_token() == Some("solaris:selection-1")
    }));
    host.stop();
}

#[test]
#[ignore = "requires the explicit first-party settlement component integration gate"]
fn shipped_gate_previews_match_the_catalog_and_every_staged_block_across_chunk_edges() {
    let report = mc_data::blocks::load_blocks_report(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../solaris/data/vanilla/reports/blocks.json"),
    )
    .expect("local vanilla report");
    let registry = mc_world::BlockRegistry::from_report(&report).expect("block registry");
    let package = plugins_root().join(PACKAGE_ID);
    let path = package.join("structures/palisade_gate.toml");
    let catalog = mc_worldgen::BlueprintCatalog::from_files(
        &registry,
        "solaris",
        &[(
            path.file_name().unwrap().to_string_lossy().into_owned(),
            std::fs::read_to_string(&path).expect("authored gate"),
        )],
    )
    .expect("validated authored gate");
    let blueprint = catalog
        .get("solaris:palisade_gate")
        .expect("gate blueprint");
    let archive =
        std::fs::File::open(package.join("client/settlements.zip")).expect("shipped bundle");
    let mut archive = zip::ZipArchive::new(archive).expect("verified ZIP");
    let index: serde_json::Value = serde_json::from_reader(
        archive
            .by_name("solaris-client.json")
            .expect("bundle index"),
    )
    .expect("schema-2 index");
    let previews = index["world_previews"]
        .as_array()
        .expect("preview declarations");
    assert_eq!(previews.len(), 4, "all quarter turns are selectable");
    let origin = [15, 70, -16];
    for (turn, preview) in previews.iter().enumerate() {
        let degrees = (turn * 90) as u16;
        assert_eq!(preview["blueprint_id"], blueprint.id());
        assert_eq!(
            preview["content_hash"],
            blueprint.content_hash(),
            "reject stale preview"
        );
        assert_eq!(preview["rotation"], turn);
        let size = blueprint.size();
        assert_eq!(
            [
                preview["size_x"].as_i64(),
                preview["size_y"].as_i64(),
                preview["size_z"].as_i64()
            ],
            [
                Some(i64::from(size[0])),
                Some(i64::from(size[1])),
                Some(i64::from(size[2]))
            ],
        );
        let instance = mc_worldgen::BlueprintInstance::new(
            Arc::clone(blueprint),
            mc_worldgen::QuarterTurn::from_degrees(degrees).expect("quarter turn"),
            origin,
        );
        let placed = |blocks: Vec<mc_worldgen::PlacedBlock>| {
            blocks
                .into_iter()
                .map(|block| {
                    (
                        block.pos,
                        registry
                            .by_id(block.state)
                            .expect("registered block state")
                            .block
                            .id
                            .as_str()
                            .to_owned(),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        };
        let actual = placed(instance.placed_blocks());
        let stages = placed(
            blueprint
                .stages()
                .iter()
                .flat_map(|stage| instance.placed_stage_blocks(stage))
                .collect(),
        );
        assert_eq!(
            actual, stages,
            "preview must cover committed construction stages"
        );
        let expected = preview["blocks"]
            .as_array()
            .expect("preview cells")
            .iter()
            .map(|block| {
                let x = block["x"].as_i64().unwrap() as i32;
                let y = block["y"].as_i64().unwrap() as i32;
                let z = block["z"].as_i64().unwrap() as i32;
                let offset = instance.turn().rotate_offset([x, y, z], size);
                (
                    [
                        origin[0] + offset[0],
                        origin[1] + offset[1],
                        origin[2] + offset[2],
                    ],
                    block["block_id"].as_str().unwrap().to_owned(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            expected, actual,
            "preview geometry must match native placement at {degrees} degrees"
        );
        assert!(
            expected.keys().any(|at| at[0] >= 16),
            "footprint crosses the chunk edge"
        );
    }
}

#[tokio::test]
#[ignore = "requires the explicit first-party two-component integration gate"]
async fn independent_audit_and_loader_backed_settlements_share_one_deployment() {
    const AUDIT: &str = "solaris-audit";
    async fn load_saved_records(boundary: &mc_script::ScriptBoundary, audit_value: &str) {
        let mut seen_audit = false;
        let mut seen_registry = false;
        let mut seen_settlements = false;
        for _ in 0..3 {
            let command = boundary.recv_command().await.expect("guest startup read");
            let admitted = boundary
                .accept_host_command(command)
                .expect("guest startup read is admitted");
            let (value, version) = match admitted.plugin_id() {
                AUDIT => {
                    assert!(!seen_audit, "audit reads its state once");
                    assert!(matches!(
                        admitted.request(),
                        ScriptCommand::PluginStorageGet { request } if request.key() == "actions-v1"
                    ));
                    seen_audit = true;
                    (Some(audit_value), Some(1))
                }
                PACKAGE_ID => match admitted.request() {
                    ScriptCommand::PluginStorageGet { request }
                        if request.key() == "operation-counter" =>
                    {
                        assert!(!seen_settlements, "settlements reads its counter once");
                        seen_settlements = true;
                        (None, None)
                    }
                    ScriptCommand::PluginStorageGet { request }
                        if request.key() == "settlement-registry-v1" =>
                    {
                        assert!(!seen_registry, "settlements recovers hires once");
                        seen_registry = true;
                        (None, None)
                    }
                    other => panic!("unexpected settlement startup read: {other:?}"),
                },
                other => panic!("unexpected guest storage read: {other}"),
            };
            boundary
                .try_enqueue_event(
                    admitted
                        .plugin_storage_get_result(value, version)
                        .expect("owner-scoped durable read"),
                )
                .expect("guest receives its own stored value");
        }
        assert!(seen_audit && seen_settlements && seen_registry);
    }
    let staged = tempfile::tempdir().expect("temporary deployment root");
    for id in [AUDIT, PACKAGE_ID] {
        copy_package(&plugins_root().join(id), &staged.path().join(id));
    }
    let config = DeploymentConfig {
        root: staged.path().to_path_buf(),
        mode: DiscoveryMode::Strict,
        expected: vec![AUDIT.to_owned(), PACKAGE_ID.to_owned()],
        grants: BTreeMap::from([
            (AUDIT.to_owned(), vec!["storage".to_owned()]),
            (
                PACKAGE_ID.to_owned(),
                vec![
                    "player_queries".to_owned(),
                    "storage".to_owned(),
                    "inventory_transfers".to_owned(),
                    "persistent_residents".to_owned(),
                    "resident_work".to_owned(),
                    "resident_orders".to_owned(),
                    "world_sites".to_owned(),
                    "structure_operations".to_owned(),
                ],
            ),
        ]),
        require_grants: true,
        precommit_hooks: Vec::new(),
    };
    let limits = PluginLimits {
        epoch_ticks_per_call: 64,
        ..PluginLimits::default()
    };
    let checked = mc_plugin_host::check_deployment(&config, &limits)
        .expect("both independent shipped components check");
    assert_eq!(checked.client_bundles().len(), 1);
    assert_eq!(checked.client_bundles()[0].owner_plugin_id(), PACKAGE_ID);

    let packages = || {
        mc_plugin_host::discover(&config, &limits)
            .expect("strict deployment discovers both packages")
            .into_packages()
    };
    let host = start_deployment(
        packages(),
        limits,
        HostQueues::default(),
        Arc::new(Sessions),
    )
    .expect("both shipped components start together");
    let boundary = host.boundary().clone();
    assert_eq!(
        boundary.player_command_roots(),
        vec!["audit".to_owned(), "settlement".to_owned()]
    );
    assert_eq!(host.client_bundles().len(), 1);

    let audit_read = boundary.recv_command().await.expect("audit startup read");
    let audit_read = boundary
        .accept_host_command(audit_read)
        .expect("audit startup storage request is admitted");
    assert_eq!(audit_read.plugin_id(), AUDIT);
    assert!(matches!(
        audit_read.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "actions-v1"
    ));
    boundary
        .try_enqueue_event(
            audit_read
                .plugin_storage_get_result(None, None)
                .expect("empty persistent audit store"),
        )
        .expect("startup read result reaches audit");
    let settlement_read = boundary
        .recv_command()
        .await
        .expect("settlement startup read");
    let settlement_read = boundary
        .accept_host_command(settlement_read)
        .expect("settlement startup storage request is admitted");
    assert_eq!(settlement_read.plugin_id(), PACKAGE_ID);
    assert!(matches!(
        settlement_read.request(),
        ScriptCommand::PluginStorageGet { request } if request.key() == "operation-counter"
    ));
    boundary
        .try_enqueue_event(
            settlement_read
                .plugin_storage_get_result(None, None)
                .expect("empty persistent settlement counter"),
        )
        .expect("startup read result reaches settlements");
    answer_empty_hire_registry(&boundary).await;
    boundary
        .try_enqueue_event(
            ScriptEvent::try_player_block_placed_with_context(
                ScriptPlayerId::new(SESSION),
                player_context(),
                "minecraft:overworld",
                "minecraft:stone",
                11,
                64,
                -7,
                ScriptGameMode::Survival,
            )
            .expect("committed block placement snapshot"),
        )
        .expect("audit observes committed world action");
    let audit_write = boundary
        .recv_command()
        .await
        .expect("audit persists the action");
    let audit_write = boundary
        .accept_host_command(audit_write)
        .expect("audit storage mutation is admitted");
    assert_eq!(audit_write.plugin_id(), AUDIT);
    let ScriptCommand::PluginStorageCompareAndSwap { request } = audit_write.request() else {
        panic!("audit must persist its recorded action");
    };
    assert_eq!(request.key(), "actions-v1");
    assert_eq!(request.expected_version(), None);
    let audit_value = request.value().to_owned();
    assert!(audit_value.contains("minecraft:stone"));
    boundary
        .try_enqueue_event(
            audit_write
                .plugin_storage_cas_result(true, Some(1))
                .expect("durable audit commit result"),
        )
        .expect("audit observes its committed storage mutation");

    let replaced = host.reload(packages()).await.expect("both guests reload");
    assert_eq!(replaced.loaded_packages, 2);
    assert_eq!(replaced.replaced.len(), 2);
    assert_eq!(
        boundary.player_command_roots(),
        vec!["audit".to_owned(), "settlement".to_owned()]
    );
    load_saved_records(&boundary, &audit_value).await;
    host.stop();

    let restarted = start_deployment(
        packages(),
        limits,
        HostQueues::default(),
        Arc::new(Sessions),
    )
    .expect("both shipped components restart");
    assert_eq!(restarted.boundary().player_command_roots().len(), 2);
    let restarted_boundary = restarted.boundary();
    load_saved_records(restarted_boundary, &audit_value).await;
    restarted_boundary
        .try_enqueue_event(
            ScriptEvent::try_player_block_placed_with_context(
                ScriptPlayerId::new(SESSION),
                player_context(),
                "minecraft:overworld",
                "minecraft:dirt",
                22,
                64,
                -8,
                ScriptGameMode::Survival,
            )
            .expect("post-restart committed action"),
        )
        .expect("restarted audit sees committed world action");
    let recovered = restarted_boundary
        .accept_host_command(
            restarted_boundary
                .recv_command()
                .await
                .expect("recovered history"),
        )
        .expect("restarted audit mutation is admitted");
    assert_eq!(recovered.plugin_id(), AUDIT);
    assert!(matches!(
        recovered.request(),
        ScriptCommand::PluginStorageCompareAndSwap { request }
            if request.expected_version() == Some(1)
                && request.value().contains("minecraft:stone")
                && request.value().contains("minecraft:dirt")
    ));
    assert_eq!(
        restarted_boundary.try_enqueue_player_command_with_context(
            ScriptPlayerId::new(SESSION),
            ScriptPlayerContext::try_new(PLAYER, "Ada", true, 32.0, 64.0, -16.0)
                .expect("operator context"),
            "audit",
        ),
        Ok(PlayerCommandAdmission::Enqueued)
    );
    let status = restarted_boundary
        .accept_host_command(
            restarted_boundary
                .recv_command()
                .await
                .expect("audit status"),
        )
        .expect("recovered audit status is admitted");
    assert_eq!(status.plugin_id(), AUDIT);
    assert!(matches!(
        status.request(),
        ScriptCommand::SendChatMessage { message, .. }
            if message.contains("minecraft:stone") && message.contains("11,64,-7")
    ));
    restarted.stop();

    std::fs::remove_dir_all(staged.path().join(AUDIT)).expect("disable staged audit package");
    let mut settlements_only = config;
    settlements_only.expected = vec![PACKAGE_ID.to_owned()];
    settlements_only.grants.remove(AUDIT);
    let remaining = mc_plugin_host::discover(&settlements_only, &limits)
        .expect("disabled audit does not change settlements")
        .into_packages();
    let host = start_deployment(remaining, limits, HostQueues::default(), Arc::new(Sessions))
        .expect("remaining component starts without a server rebuild");
    assert_eq!(host.boundary().player_command_roots(), vec!["settlement"]);
    assert_eq!(host.client_bundles().len(), 1);
    host.stop();
}
