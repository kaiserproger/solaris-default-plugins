use super::production::Journal;
use super::*;

type Records = BTreeMap<String, (String, u64)>;
fn seed(journal: &Journal) -> Records {
    use super::production_storage::{Loader, Mutation, Record, Step, Writer};
    let mut records = Records::new();
    let mut revision = 0;
    for (id, job) in &journal.jobs {
        let mut loader = Loader::all();
        while let Some(key) = loader.next() {
            let saved = records.get(key);
            loader
                .answer(Record {
                    value: saved.map(|r| r.0.clone()),
                    version: saved.map(|r| r.1),
                })
                .unwrap();
        }
        let mut writer = Writer::new(
            loader.finish().unwrap(),
            id,
            &serde_json::to_value(job).unwrap(),
        )
        .unwrap();
        let mut step = writer.next().unwrap();
        loop {
            step = match step {
                Step::Read(key) => {
                    let saved = records.get(&key);
                    writer
                        .answer_read(Record {
                            value: saved.map(|r| r.0.clone()),
                            version: saved.map(|r| r.1),
                        })
                        .unwrap()
                }
                Step::Batch { mutations, .. } => {
                    revision += 1;
                    for mutation in mutations {
                        if let Mutation::CompareAndSwap {
                            key,
                            expected_version,
                            value,
                        } = mutation
                        {
                            assert_eq!(records.get(&key).map(|r| r.1), expected_version);
                            records.insert(key, (value, revision));
                        }
                    }
                    writer.committed(revision).unwrap()
                }
                Step::Complete => break,
            };
        }
    }
    records
}
fn drive_storage(
    plugin: &mut Settlements,
    mut commands: Vec<Command>,
    records: &mut Records,
) -> Vec<Command> {
    loop {
        if let Some(read) = commands.iter().find_map(|command| match command {
            Command::StorageGet(read) => Some(read),
            _ => None,
        }) {
            let saved = records.get(&read.key);
            commands = plugin
                .production_get_answer(&StorageGetAnswered {
                    request: read.request.clone(),
                    outcome: StorageGetOutcome::Read(solaris_plugin_sdk::StorageRecord {
                        value: saved.map(|r| r.0.clone()),
                        version: saved.map(|r| r.1),
                    }),
                })
                .unwrap();
        } else if let Some(batch) = commands.iter().find_map(|command| match command {
            Command::StorageBatchCas(batch) => Some(batch),
            _ => None,
        }) {
            let revision = records.values().map(|r| r.1).max().unwrap_or(0) + 1;
            let mut changes = Vec::new();
            for mutation in &batch.mutations {
                let (key, deleted) = match mutation {
                    solaris_plugin_sdk::commands::StorageMutation::Cas(cas) => {
                        assert_eq!(records.get(&cas.key).map(|r| r.1), cas.expected_version);
                        records.insert(cas.key.clone(), (cas.value.clone(), revision));
                        (&cas.key, false)
                    }
                    solaris_plugin_sdk::commands::StorageMutation::Delete(delete) => {
                        assert_eq!(
                            records.get(&delete.key).map(|r| r.1),
                            delete.expected_version
                        );
                        records.remove(&delete.key);
                        (&delete.key, true)
                    }
                };
                changes.push(solaris_plugin_sdk::storage::StorageChange {
                    key: key.clone(),
                    deleted,
                });
            }
            changes.sort_by(|a, b| a.key.cmp(&b.key));
            commands = plugin
                .production_operation_answer(&OperationAnswered {
                    request: batch.request.clone(),
                    operation_id: Some(batch.operation_id.clone()),
                    outcome: OperationOutcome::Committed(
                        solaris_plugin_sdk::events::OperationCommitted {
                            revision,
                            payload: OperationPayload::StorageBatch(changes),
                        },
                    ),
                })
                .unwrap();
        } else {
            return commands;
        }
    }
}
fn recovered_plugin(tick: u64, records: &mut Records) -> (Settlements, Vec<Command>) {
    let mut plugin = Settlements::default();
    let commands = plugin.production_timer(tick);
    let mut commands = drive_storage(&mut plugin, commands, records);
    while plugin.production_snapshot().is_none() {
        let next = plugin.production_timer(tick);
        commands = drive_storage(&mut plugin, next, records);
    }
    (plugin, commands)
}
fn operation_request(commands: &[Command]) -> String {
    commands
        .iter()
        .find_map(|command| match command {
            Command::Operation(operation) => Some(operation.request.clone()),
            Command::OperationStatus(status) => Some(status.request.clone()),
            _ => None,
        })
        .expect("native read request")
}

#[test]
fn restored_deadline_uses_simulation_time_not_observation_count() {
    let mut journal = Journal::default();
    journal
        .insert(super::production_tests::intent("prod-cake"))
        .unwrap();
    let mut records = seed(&journal);
    let (mut plugin, waiting) = recovered_plugin(119, &mut records);
    assert!(matches!(waiting.as_slice(),[Command::ScheduleTimer(timer)] if timer.delay_ticks==1));
    for _ in 0..3 {
        let commands = plugin.production_timer(119);
        let waiting = drive_storage(&mut plugin, commands, &mut records);
        assert!(
            matches!(waiting.as_slice(),[Command::ScheduleTimer(timer)] if timer.delay_ticks==1)
        );
        assert!(plugin.production_snapshot().unwrap().jobs["prod-cake"]
            .outcome
            .is_none());
    }
    let commands = plugin.production_timer(120);
    let due = drive_storage(&mut plugin, commands, &mut records);
    assert!(
        matches!(due.as_slice(),[Command::OperationStatus(status)] if status.operation_id=="prod-cake")
    );
}

#[test]
fn unusable_live_manual_or_authored_station_pauses_durably_without_material_work() {
    for manual in [true, false] {
        let mut original = super::production_tests::intent("prod-refused-station");
        if !manual {
            original.station.worksite_id = None;
        }
        let mut journal = Journal::default();
        journal.insert(original.clone()).unwrap();
        let mut other = super::production_tests::intent("prod-later");
        other.worker = "worker-2".to_owned();
        other.due_tick = 200;
        journal.insert(other.clone()).unwrap();
        let mut registry = SettlementRegistry::default();
        register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
        record_settlement_resident(&mut registry, "oak_hollow", "resident_1", "worker-1", 7)
            .unwrap();
        record_settlement_certificate(
            &mut registry,
            "oak_hollow",
            "bakery",
            "workshop",
            if manual {
                BuildingOrigin::Certificate {
                    certificate_id: "site-1".to_owned(),
                }
            } else {
                BuildingOrigin::Structure {
                    structure_id: "site-1".to_owned(),
                }
            },
            3,
            Vec::new(),
        )
        .unwrap();
        let building = registry
            .settlements
            .get_mut("oak_hollow")
            .unwrap()
            .buildings
            .get_mut("bakery")
            .unwrap();
        building.dimension = Some("minecraft:overworld".to_owned());
        building.station_positions = vec![StoredPosition { x: 12, y: 64, z: 9 }];
        building.worksite_id = original.station.worksite_id.clone();
        let mut records = seed(&journal);
        let (mut plugin, status) = recovered_plugin(120, &mut records);
        let authorize = plugin
            .production_operation_answer(&OperationAnswered {
                request: operation_request(&status),
                operation_id: Some(original.operation_id.clone()),
                outcome: OperationOutcome::Refused(solaris_plugin_sdk::events::OperationRefused {
                    reason: OperationFailure::NotFound,
                    payload: OperationPayload::None,
                }),
            })
            .unwrap();
        let check = plugin.test_registry_read(&registry);
        let points = vec![world_structures::WorksitePointFailure {
            at: world_structures::BlockPosition { x: 12, y: 64, z: 9 },
            reason: world_structures::WorksiteFailure::MissingWorkstation,
        }];
        let payload = if manual {
            WorldStructureResult::WorksiteCheck(world_structures::WorksiteCheck {
                worksite_id: "site-1".to_owned(),
                world_revision: 99,
                valid: false,
                blocked_points: points,
            })
        } else {
            WorldStructureResult::StructureCheck(world_structures::StructureCheck {
                structure_id: "site-1".to_owned(),
                plugin_id: "solaris-settlements".to_owned(),
                world_revision: 99,
                valid: false,
                blocked_points: points,
            })
        };
        let receipt_read = plugin
            .production_operation_answer(&OperationAnswered {
                request: operation_request(&check),
                operation_id: None,
                outcome: OperationOutcome::Committed(
                    solaris_plugin_sdk::events::OperationCommitted {
                        revision: 99,
                        payload: OperationPayload::WorldStructure(payload),
                    },
                ),
            })
            .unwrap();
        assert!(receipt_read
            .iter()
            .all(|command| !matches!(command, Command::Operation(_))));
        let _ = drive_storage(&mut plugin, receipt_read, &mut records);
        let projected = plugin
            .production_snapshot()
            .expect("complete durable projection");
        assert_eq!(
            projected.jobs["prod-later"].intent, other,
            "publishing one naturally scoped job must preserve every unrelated durable intent"
        );
        let job = &projected.jobs[&original.operation_id];
        assert!(
            job.outcome.is_none(),
            "no terminal refusal or invented native receipt"
        );
        assert_eq!(
            job.intent, original,
            "original identity, quality and fences remain intact"
        );
        let pause = job
            .pause
            .as_ref()
            .expect("durable recoverable station pause");
        assert_eq!(pause.retry_tick, 140);
        let reason = &pause.reason;
        assert!(
            reason.contains("missing-workstation") && reason.contains("12,64,9"),
            "actionable physical refusal: {reason}"
        );
        assert!(projected
            .exact_practice("oak_hollow", &original.operation_id)
            .is_none());
        let (restored, _) = recovered_plugin(139, &mut records);
        let durable = restored.production_snapshot().unwrap();
        assert_eq!(durable.jobs[&original.operation_id], *job);
        assert!(
            durable.due(139).is_none(),
            "pause retry is a simulation deadline"
        );
        assert_eq!(
            durable.due(140).unwrap().intent,
            original,
            "restart restores the same project, not a replacement that can debit again"
        );
    }
}
