//! Fenced service exit: stop military goals, return only issued kit through
//! the physical warehouse, then restore the same resident to civilian work.

use super::*;
use solaris_plugin_sdk::inventories::{
    InventoryExpectedRevision, OwnedInventorySnapshot, OwnedItemTransfer,
};
use solaris_plugin_sdk::{demobilize_resident, query_residents, transfer_owned_items};

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Intent {
    pub(super) issued: hire::IssuedKit,
    pub(super) sequence: u64,
    pub(super) attempt: u32,
    pub(super) step: Step,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Step {
    Stop {
        revision: u64,
    },
    Return {
        revision: u64,
        transfers: Vec<ReturnTransfer>,
        warehouse_fence: hire::Fence,
        equipment_fence: Option<hire::Fence>,
        carry_fence: Option<hire::Fence>,
    },
    Finish {
        revision: u64,
    },
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct ReturnTransfer {
    source: hire::Destination,
    destination_slot: u8,
    count: u32,
}

impl Intent {
    fn operation_id(&self) -> String {
        match self.step {
            Step::Stop { .. } => format!("dismiss-stop-{}", self.sequence),
            Step::Return { .. } => format!("dismiss-return-{}-{}", self.sequence, self.attempt),
            Step::Finish { .. } => format!("dismiss-finish-{}", self.sequence),
        }
    }
}

/// The existing resident endpoint slots name issued kit, not all belongings.
/// Missing/consumed equipment is not fabricated; personal replacements remain
/// on the resident. An unavailable empty warehouse slot refuses the *whole*
/// transfer before any item changes owner.
fn plan_return(
    issued: &hire::IssuedKit,
    warehouse: &OwnedInventorySnapshot,
    equipment: &OwnedInventorySnapshot,
    carry: &OwnedInventorySnapshot,
) -> Result<Vec<ReturnTransfer>, &'static str> {
    let mut free = warehouse
        .slots
        .iter()
        .filter(|slot| slot.item.is_none())
        .map(|slot| slot.slot);
    let mut transfers = Vec::new();
    for item in &issued.items {
        let (source, slot) = match item.destination {
            hire::Destination::Equipment(slot) => (equipment, slot),
            hire::Destination::Carry(slot) => (carry, slot),
        };
        let Some(held) = source
            .slots
            .iter()
            .find(|entry| entry.slot == slot)
            .and_then(|entry| entry.item.as_ref())
            .filter(|held| item.matches(held))
        else {
            continue;
        };
        let Some(destination_slot) = free.next() else {
            return Err(
                "No warehouse space for the issued equipment; resident stays demobilizing.",
            );
        };
        transfers.push(ReturnTransfer {
            source: item.destination,
            destination_slot,
            count: held.count.min(item.count),
        });
    }
    Ok(transfers)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Recover,
    Registry,
    InitialResident,
    InitialFence,
    IntentWrite,
    Stop,
    Warehouse,
    Equipment,
    Carry,
    StepWrite,
    Return,
    ResultingGear,
    Finish,
    FinalizeRead,
    FinalizeWrite,
    StepRead,
}

pub(super) struct PendingDismiss {
    request: String,
    stage: Stage,
    session: u64,
    player: String,
    settlement: String,
    alias: String,
    handle: String,
    version: Option<u64>,
    registry: Option<SettlementRegistry>,
    intent: Option<Intent>,
    snapshots: Vec<OwnedInventorySnapshot>,
    resulting_revision: u64,
    retry_used: bool,
}

impl PendingDismiss {
    fn new(request: String, stage: Stage) -> Self {
        Self {
            request,
            stage,
            session: 0,
            player: String::new(),
            settlement: String::new(),
            alias: String::new(),
            handle: String::new(),
            version: None,
            registry: None,
            intent: None,
            snapshots: Vec::new(),
            resulting_revision: 0,
            retry_used: false,
        }
    }

    fn endpoint(&self, stage: Stage) -> InventoryEndpoint {
        match stage {
            Stage::InitialFence | Stage::ResultingGear | Stage::Equipment => {
                InventoryEndpoint::ResidentEquipment(self.handle.clone())
            }
            Stage::Warehouse => InventoryEndpoint::Warehouse(
                self.intent
                    .as_ref()
                    .expect("durable intent")
                    .issued
                    .warehouse
                    .clone(),
            ),
            Stage::Carry => InventoryEndpoint::ResidentCarry(self.handle.clone()),
            _ => unreachable!("inventory stage"),
        }
    }
}

impl Settlements {
    pub(super) fn dismiss(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        alias: &str,
    ) -> Vec<Command> {
        if self.pending_dismiss.is_some()
            || self.pending_hire.is_some()
            || self.pending_registry_read.is_some()
            || self.pending_registry_write.is_some()
        {
            return vec![message_session(
                session,
                "Another settlement record update is pending.",
            )];
        }
        if !valid_settlement_name(settlement) || !valid_settlement_name(alias) {
            return vec![message_session(
                session,
                "Pass an existing settlement and resident alias.",
            )];
        }
        let Some(request) = self.next_storage_request("dismiss-registry") else {
            return vec![message_session(
                session,
                "Settlement records are unavailable.",
            )];
        };
        let mut pending = PendingDismiss::new(request.clone(), Stage::Registry);
        pending.session = session;
        pending.player = player.to_owned();
        pending.settlement = settlement.to_owned();
        pending.alias = alias.to_owned();
        self.pending_dismiss = Some(pending);
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    pub(super) fn recover_demobs(&mut self) -> Vec<Command> {
        let Some(request) = self.next_storage_request("dismiss-recover") else {
            return Vec::new();
        };
        self.pending_dismiss = Some(PendingDismiss::new(request.clone(), Stage::Recover));
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    fn dismiss_request(
        &mut self,
        pending: &mut PendingDismiss,
        stage: Stage,
        prefix: &str,
    ) -> String {
        let request = self
            .next_storage_request(prefix)
            .expect("bounded request sequence");
        pending.stage = stage;
        pending.request = request.clone();
        request
    }

    fn dismiss_failure(&mut self, pending: PendingDismiss, reason: &str) -> Vec<Command> {
        log(
            LogLevel::Info,
            &format!(
                "{MARKER} dismiss-refused settlement={} resident={} reason={reason}",
                pending.settlement, pending.alias,
            ),
        );
        if pending.session != 0 {
            return vec![message_session(pending.session, reason)];
        }
        // Startup recovery: one resident the native endpoint refuses (it can
        // have died) must not starve the rest of the chain. Drop only that
        // candidate from the in-memory snapshot the chain already holds and
        // resume; the durable dismissal intent and its gear are never touched.
        if let Some(mut registry) = pending.registry.clone() {
            let removed = registry
                .settlements
                .get_mut(&pending.settlement)
                .and_then(|settlement| settlement.residents.get_mut(&pending.alias))
                .is_some_and(|resident| resident.dismissing.take().is_some());
            if removed {
                return self.resume_demobs_from_registry(pending, &registry);
            }
        }
        Vec::new()
    }

    fn dismiss_inventory(&mut self, mut pending: PendingDismiss, stage: Stage) -> Vec<Command> {
        let endpoint = pending.endpoint(stage);
        let request = self.dismiss_request(&mut pending, stage, "dismiss-inventory");
        self.pending_dismiss = Some(pending);
        vec![query_owned_inventory(&request, endpoint, None)]
    }

    pub(super) fn dismiss_get_answer(
        &mut self,
        answer: &StorageGetAnswered,
    ) -> Option<Vec<Command>> {
        let mut pending = self.pending_dismiss.take()?;
        if pending.request != answer.request
            || !matches!(
                pending.stage,
                Stage::Recover | Stage::Registry | Stage::StepRead | Stage::FinalizeRead
            )
        {
            self.pending_dismiss = Some(pending);
            return None;
        }
        let StorageGetOutcome::Read(record) = &answer.outcome else {
            return Some(self.dismiss_failure(pending, "Settlement records are unavailable."));
        };
        let registry = match record.value.as_deref() {
            Some(value) => match serde_json::from_str::<SettlementRegistry>(value) {
                Ok(registry) => registry,
                Err(_) => {
                    return Some(
                        self.dismiss_failure(pending, "Settlement records are unreadable."),
                    )
                }
            },
            None => SettlementRegistry::default(),
        };
        pending.version = record.version;
        Some(match pending.stage {
            Stage::Recover => self.resume_demobs_from_registry(pending, &registry),
            Stage::Registry => self.start_dismiss(pending, registry),
            Stage::FinalizeRead => self.finalize_dismiss(pending, registry),
            Stage::StepRead => {
                let Some(resident) = registry
                    .settlements
                    .get(&pending.settlement)
                    .and_then(|settlement| settlement.residents.get(&pending.alias))
                else {
                    return Some(
                        self.dismiss_failure(pending, "Dismissal resident record disappeared."),
                    );
                };
                let Some(current) = &resident.dismissing else {
                    return Some(self.dismiss_failure(pending, "Dismissal intent disappeared."));
                };
                let desired = pending.intent.as_ref().expect("desired next step");
                if resident.handle != pending.handle
                    || current.sequence != desired.sequence
                    || !matches!(
                        (&current.step, &desired.step),
                        (Step::Stop { .. }, Step::Return { .. } | Step::Finish { .. })
                            | (
                                Step::Return { .. },
                                Step::Return { .. } | Step::Finish { .. }
                            )
                    )
                {
                    return Some(self.dismiss_failure(
                        pending,
                        "Dismissal intent changed; inspect the resident.",
                    ));
                }
                pending.registry = Some(registry);
                self.persist_dismiss_step(pending, Stage::StepWrite)
            }
            _ => unreachable!(),
        })
    }
    pub(super) fn recover_demobs_from_read(
        &mut self,
        registry: &SettlementRegistry,
    ) -> Vec<Command> {
        self.resume_demobs_from_registry(
            PendingDismiss::new(String::new(), Stage::Recover),
            registry,
        )
    }

    fn resume_demobs_from_registry(
        &mut self,
        mut pending: PendingDismiss,
        registry: &SettlementRegistry,
    ) -> Vec<Command> {
        let next = registry.settlements.iter().find_map(|(name, settlement)| {
            settlement.residents.iter().find_map(|(alias, resident)| {
                resident.dismissing.as_ref().map(|intent| {
                    (
                        name.clone(),
                        alias.clone(),
                        resident.handle.clone(),
                        intent.clone(),
                    )
                })
            })
        });
        let Some((settlement, alias, handle, intent)) = next else {
            return self.recover_captures_from_read(registry);
        };
        pending.settlement = settlement;
        pending.alias = alias;
        pending.handle = handle;
        pending.intent = Some(intent);
        // Keep the snapshot startup recovery is chaining off, so a refusal for
        // this resident can continue with the others without a second read.
        pending.registry = Some(registry.clone());
        self.issue_dismiss_step(pending)
    }

    fn start_dismiss(
        &mut self,
        mut pending: PendingDismiss,
        registry: SettlementRegistry,
    ) -> Vec<Command> {
        let Some(settlement) = registry.settlements.get(&pending.settlement) else {
            return self.dismiss_failure(pending, "Settlement not found.");
        };
        if !can_build_settlement(settlement, &pending.player) {
            return self
                .dismiss_failure(pending, "Only the owner or a steward dismisses soldiers.");
        }
        let Some(resident) = settlement.residents.get(&pending.alias) else {
            return self.dismiss_failure(pending, "Resident not found in this settlement.");
        };
        if resident.role.is_none()
            || resident.employer.as_deref() != Some(pending.settlement.as_str())
        {
            return self.dismiss_failure(pending, "Resident is not serving this settlement.");
        }
        pending.handle = resident.handle.clone();
        if let Some(intent) = &resident.dismissing {
            pending.intent = Some(intent.clone());
            return self.issue_dismiss_step(pending);
        }
        if resident.issued.is_none() || resident.hiring.is_some() {
            return self.dismiss_failure(
                pending,
                "Issued kit provenance is unavailable; resident stays employed.",
            );
        }
        pending.registry = Some(registry);
        let request =
            self.dismiss_request(&mut pending, Stage::InitialResident, "dismiss-resident");
        let handle = pending.handle.clone();
        self.pending_dismiss = Some(pending);
        vec![query_residents(&request, &[handle], None)]
    }

    fn persist_dismiss_step(&mut self, mut pending: PendingDismiss, stage: Stage) -> Vec<Command> {
        let mut registry = pending.registry.take().expect("read registry");
        let Some(resident) = registry
            .settlements
            .get_mut(&pending.settlement)
            .and_then(|settlement| settlement.residents.get_mut(&pending.alias))
        else {
            return self.dismiss_failure(pending, "Resident record disappeared during dismissal.");
        };
        if resident.handle != pending.handle {
            return self.dismiss_failure(pending, "Resident identity changed during dismissal.");
        }
        resident.dismissing = pending.intent.clone();
        let Ok(value) = serde_json::to_string(&registry) else {
            return self.dismiss_failure(pending, "Settlement records are unavailable.");
        };
        let request = self.dismiss_request(&mut pending, stage, "dismiss-intent");
        let version = pending.version;
        self.pending_dismiss = Some(pending);
        vec![storage_cas(
            &request,
            SETTLEMENT_REGISTRY_KEY,
            version,
            value,
        )]
    }

    fn issue_dismiss_step(&mut self, mut pending: PendingDismiss) -> Vec<Command> {
        let intent = pending.intent.clone().expect("durable dismissal intent");
        let operation_id = intent.operation_id();
        let command = match &intent.step {
            Step::Stop { revision } => {
                let request = self.dismiss_request(&mut pending, Stage::Stop, "dismiss-stop");
                demobilize_resident(&request, &operation_id, &pending.handle, *revision)
            }
            Step::Finish { revision } => {
                let request = self.dismiss_request(&mut pending, Stage::Finish, "dismiss-finish");
                demobilize_resident(&request, &operation_id, &pending.handle, *revision)
            }
            Step::Return {
                transfers,
                warehouse_fence,
                equipment_fence,
                carry_fence,
                ..
            } => {
                let warehouse = InventoryEndpoint::Warehouse(intent.issued.warehouse.clone());
                let equipment = InventoryEndpoint::ResidentEquipment(pending.handle.clone());
                let carry = InventoryEndpoint::ResidentCarry(pending.handle.clone());
                let transfers = transfers
                    .iter()
                    .map(|transfer| {
                        let (source, source_slot) = match transfer.source {
                            hire::Destination::Equipment(slot) => (equipment.clone(), slot),
                            hire::Destination::Carry(slot) => (carry.clone(), slot),
                        };
                        OwnedItemTransfer {
                            source,
                            source_slot,
                            destination: warehouse.clone(),
                            destination_slot: transfer.destination_slot,
                            count: transfer.count,
                        }
                    })
                    .collect();
                let mut fences = vec![InventoryExpectedRevision {
                    endpoint: warehouse,
                    fence: warehouse_fence.wire(),
                }];
                if let Some(fence) = equipment_fence {
                    fences.push(InventoryExpectedRevision {
                        endpoint: equipment,
                        fence: fence.wire(),
                    });
                }
                if let Some(fence) = carry_fence {
                    fences.push(InventoryExpectedRevision {
                        endpoint: carry,
                        fence: fence.wire(),
                    });
                }
                let request = self.dismiss_request(&mut pending, Stage::Return, "dismiss-return");
                transfer_owned_items(&request, &operation_id, 0, transfers, fences)
            }
        };
        self.pending_dismiss = Some(pending);
        vec![command]
    }

    fn read_dismiss_registry(&mut self, mut pending: PendingDismiss, stage: Stage) -> Vec<Command> {
        let request = self.dismiss_request(&mut pending, stage, "dismiss-registry-read");
        self.pending_dismiss = Some(pending);
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    fn plan_dismiss_return(&mut self, mut pending: PendingDismiss) -> Vec<Command> {
        let [warehouse, equipment, carry] = pending.snapshots.as_slice() else {
            unreachable!("three inventory snapshots");
        };
        let intent = pending.intent.as_mut().expect("durable intent");
        let transfers = match plan_return(&intent.issued, warehouse, equipment, carry) {
            Ok(transfers) => transfers,
            Err(reason) => return self.dismiss_failure(pending, reason),
        };
        let revision = match intent.step {
            Step::Stop { .. } => pending.resulting_revision,
            Step::Return { revision, .. } => revision,
            Step::Finish { .. } => unreachable!("already civilian"),
        };
        if transfers.is_empty() {
            intent.step = Step::Finish {
                revision: equipment.fence.revision,
            };
        } else {
            if matches!(intent.step, Step::Return { .. }) {
                let Some(attempt) = intent.attempt.checked_add(1) else {
                    return self.dismiss_failure(pending, "Dismissal transfer attempts exhausted.");
                };
                intent.attempt = attempt;
            }
            intent.step = Step::Return {
                revision,
                equipment_fence: transfers
                    .iter()
                    .any(|transfer| matches!(transfer.source, hire::Destination::Equipment(_)))
                    .then(|| hire::Fence::from_snapshot(equipment)),
                carry_fence: transfers
                    .iter()
                    .any(|transfer| matches!(transfer.source, hire::Destination::Carry(_)))
                    .then(|| hire::Fence::from_snapshot(carry)),
                warehouse_fence: hire::Fence::from_snapshot(warehouse),
                transfers,
            };
        }
        self.read_dismiss_registry(pending, Stage::StepRead)
    }

    pub(super) fn dismiss_operation_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let mut pending = self.pending_dismiss.take()?;
        if pending.request != answer.request
            || !matches!(
                pending.stage,
                Stage::InitialResident
                    | Stage::InitialFence
                    | Stage::Stop
                    | Stage::Warehouse
                    | Stage::Equipment
                    | Stage::Carry
                    | Stage::Return
                    | Stage::ResultingGear
                    | Stage::Finish
            )
        {
            self.pending_dismiss = Some(pending);
            return None;
        }
        Some(match pending.stage {
            Stage::InitialResident => {
                let OperationOutcome::Committed(committed) = &answer.outcome else {
                    return Some(self.dismiss_failure(pending, "Resident is unavailable."));
                };
                let OperationPayload::Resident(ResidentResult::Page(page)) = &committed.payload
                else {
                    return Some(self.dismiss_failure(
                        pending,
                        "Resident query returned an unexpected result.",
                    ));
                };
                let [snapshot] = page.residents.as_slice() else {
                    return Some(
                        self.dismiss_failure(pending, "Resident is not alive and loaded."),
                    );
                };
                if snapshot.handle != pending.handle
                    || snapshot.lifecycle != residents::ResidentLifecycle::AliveLoaded
                {
                    return Some(
                        self.dismiss_failure(pending, "Resident is not alive and loaded."),
                    );
                }
                self.dismiss_inventory(pending, Stage::InitialFence)
            }
            Stage::InitialFence | Stage::ResultingGear => {
                let OperationOutcome::Committed(committed) = &answer.outcome else {
                    return Some(
                        self.dismiss_failure(pending, "Resident equipment is unavailable."),
                    );
                };
                let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
                    &committed.payload
                else {
                    return Some(self.dismiss_failure(
                        pending,
                        "Resident equipment query returned an unexpected result.",
                    ));
                };
                if !same_inventory_endpoint(&snapshot.endpoint, &pending.endpoint(pending.stage)) {
                    return Some(self.dismiss_failure(
                        pending,
                        "Resident equipment query returned the wrong endpoint.",
                    ));
                }
                if pending.stage == Stage::ResultingGear {
                    pending.intent.as_mut().expect("durable return intent").step = Step::Finish {
                        revision: snapshot.fence.revision,
                    };
                    return Some(self.read_dismiss_registry(pending, Stage::StepRead));
                }
                let mut registry = pending.registry.take().expect("initial registry");
                let Some(sequence) = registry
                    .dismiss_sequence
                    .checked_add(1)
                    .filter(|next| *next <= MAX_SAFE_REVISION)
                else {
                    return Some(
                        self.dismiss_failure(pending, "No further dismissals can be recorded."),
                    );
                };
                registry.dismiss_sequence = sequence;
                let issued = registry.settlements[&pending.settlement].residents[&pending.alias]
                    .issued
                    .clone()
                    .expect("issued kit checked before query");
                pending.registry = Some(registry);
                pending.intent = Some(Intent {
                    issued,
                    sequence,
                    attempt: 0,
                    step: Step::Stop {
                        revision: snapshot.fence.revision,
                    },
                });
                self.persist_dismiss_step(pending, Stage::IntentWrite)
            }
            Stage::Warehouse | Stage::Equipment | Stage::Carry => {
                let OperationOutcome::Committed(committed) = &answer.outcome else {
                    return Some(self.dismiss_failure(
                        pending,
                        "Issued gear or its warehouse is unavailable; resident stays demobilizing.",
                    ));
                };
                let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
                    &committed.payload
                else {
                    return Some(self.dismiss_failure(
                        pending,
                        "Dismissal inventory returned an unexpected result.",
                    ));
                };
                if !same_inventory_endpoint(&snapshot.endpoint, &pending.endpoint(pending.stage)) {
                    return Some(self.dismiss_failure(
                        pending,
                        "Dismissal inventory returned the wrong endpoint.",
                    ));
                }
                pending.snapshots.push(snapshot.clone());
                match pending.stage {
                    Stage::Warehouse => self.dismiss_inventory(pending, Stage::Equipment),
                    Stage::Equipment => self.dismiss_inventory(pending, Stage::Carry),
                    _ => self.plan_dismiss_return(pending),
                }
            }
            Stage::Stop | Stage::Finish => {
                if answer.operation_id.as_deref()
                    != pending
                        .intent
                        .as_ref()
                        .map(|intent| intent.operation_id())
                        .as_deref()
                {
                    self.pending_dismiss = Some(pending);
                    return Some(Vec::new());
                }
                let OperationOutcome::Committed(committed) = &answer.outcome else {
                    return Some(self.dismiss_failure(
                        pending,
                        "Resident cannot be demobilized; inspect native status.",
                    ));
                };
                let OperationPayload::ResidentOrder(ResidentOrderResult::Demobilized(resident)) =
                    &committed.payload
                else {
                    return Some(self.dismiss_failure(
                        pending,
                        "Demobilization returned an unexpected result.",
                    ));
                };
                if resident.handle != pending.handle {
                    return Some(self.dismiss_failure(
                        pending,
                        "Demobilization answered for another resident.",
                    ));
                }
                match pending.stage {
                    Stage::Stop if resident.state == residents::DemobilizeState::Demobilizing => {
                        pending.resulting_revision = resident.revision;
                        self.dismiss_inventory(pending, Stage::Warehouse)
                    }
                    Stage::Finish if resident.state == residents::DemobilizeState::Civilian => {
                        pending.resulting_revision = resident.revision;
                        self.read_dismiss_registry(pending, Stage::FinalizeRead)
                    }
                    _ => self.dismiss_failure(
                        pending,
                        "Demobilization did not reach the expected state.",
                    ),
                }
            }
            Stage::Return => {
                if answer.operation_id.as_deref()
                    != pending
                        .intent
                        .as_ref()
                        .map(|intent| intent.operation_id())
                        .as_deref()
                {
                    self.pending_dismiss = Some(pending);
                    return Some(Vec::new());
                }
                let OperationOutcome::Committed(committed) = &answer.outcome else {
                    if pending.session != 0 && !pending.retry_used {
                        pending.retry_used = true;
                        pending.snapshots.clear();
                        return Some(self.dismiss_inventory(pending, Stage::Warehouse));
                    }
                    return Some(self.dismiss_failure(
                        pending,
                        "Issued-gear return was refused; resident stays demobilizing.",
                    ));
                };
                let OperationPayload::OwnedInventory(InventoryResult::Transfer(fences)) =
                    &committed.payload
                else {
                    return Some(self.dismiss_failure(
                        pending,
                        "Issued-gear receipt was unexpected; inspect the pending intent.",
                    ));
                };
                let equipment = InventoryEndpoint::ResidentEquipment(pending.handle.clone());
                let carry = InventoryEndpoint::ResidentCarry(pending.handle.clone());
                if !fences.iter().any(|fence| {
                    same_inventory_endpoint(&fence.endpoint, &equipment)
                        || same_inventory_endpoint(&fence.endpoint, &carry)
                }) {
                    return Some(self.dismiss_failure(
                        pending,
                        "Issued-gear receipt omitted the resident fence.",
                    ));
                }
                self.dismiss_inventory(pending, Stage::ResultingGear)
            }
            _ => unreachable!(),
        })
    }

    fn finalize_dismiss(
        &mut self,
        mut pending: PendingDismiss,
        mut registry: SettlementRegistry,
    ) -> Vec<Command> {
        let Some(resident) = registry
            .settlements
            .get_mut(&pending.settlement)
            .and_then(|settlement| settlement.residents.get_mut(&pending.alias))
        else {
            return self.dismiss_failure(pending, "Demobilized resident record disappeared.");
        };
        if resident.handle != pending.handle
            || resident.dismissing.as_ref().map(Intent::operation_id)
                != pending.intent.as_ref().map(Intent::operation_id)
        {
            return self.dismiss_failure(
                pending,
                "Demobilization intent changed; inspect the resident.",
            );
        }
        resident.role = None;
        resident.employer = None;
        resident.issued = None;
        resident.dismissing = None;
        resident.work_revision = pending.resulting_revision;
        let Ok(value) = serde_json::to_string(&registry) else {
            return self.dismiss_failure(pending, "Settlement records are unavailable.");
        };
        let request = self.dismiss_request(&mut pending, Stage::FinalizeWrite, "dismiss-finalize");
        let version = pending.version;
        self.pending_dismiss = Some(pending);
        vec![storage_cas(
            &request,
            SETTLEMENT_REGISTRY_KEY,
            version,
            value,
        )]
    }

    pub(super) fn dismiss_cas_answer(
        &mut self,
        answer: &StorageCasAnswered,
    ) -> Option<Vec<Command>> {
        let pending = self.pending_dismiss.take()?;
        if pending.request != answer.request
            || !matches!(
                pending.stage,
                Stage::IntentWrite | Stage::StepWrite | Stage::FinalizeWrite
            )
        {
            self.pending_dismiss = Some(pending);
            return None;
        }
        Some(match answer.outcome {
            StorageCasOutcome::Committed(_) if pending.stage == Stage::FinalizeWrite => {
                let mut commands = if pending.session == 0 {
                    Vec::new()
                } else {
                    vec![message_session(pending.session, "Resident returned to civilian work; issued gear returned to its warehouse.")]
                };
                commands.extend(self.recover_demobs());
                commands
            }
            StorageCasOutcome::Committed(_) => self.issue_dismiss_step(pending),
            StorageCasOutcome::Refused | StorageCasOutcome::Failed(_) => self.dismiss_failure(
                pending,
                "Settlement record changed; retry dismissal to resume its durable intent.",
            ),
        })
    }
}
