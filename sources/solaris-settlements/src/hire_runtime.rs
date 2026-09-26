//! Hiring is a registry CAS intent, one native world-journal item decision,
//! then a registry CAS finalization. On reload the same intent reissues the
//! byte-identical transfer; the host replays its receipt before checking fences.

use super::*;
use solaris_plugin_sdk::inventories::{
    InventoryExpectedRevision, OwnedInventorySnapshot, OwnedItemTransfer,
};
use solaris_plugin_sdk::{query_residents, transfer_owned_items};

#[derive(Clone, Copy, PartialEq, Eq)]
enum HireStage {
    Recover,
    Registry,
    Resident,
    Warehouse,
    Equipment,
    Carry,
    IntentWrite,
    Transfer,
    ResultingGear,
    FinalizeRead,
    FinalizeWrite,
    AbortRead,
    AbortWrite,
}

pub(super) struct PendingHire {
    request: String,
    stage: HireStage,
    session: u64,
    player: String,
    settlement: String,
    alias: String,
    handle: String,
    role: Option<hire::Role>,
    warehouse: String,
    registry: Option<SettlementRegistry>,
    version: Option<u64>,
    snapshots: Vec<OwnedInventorySnapshot>,
    intent: Option<hire::Intent>,
    resulting_revision: u64,
    /// The owner tick of the callback that started this hire: DEM-02 admits a
    /// child to military service only once it reaches adulthood.
    observed_tick: u64,
}

impl PendingHire {
    fn new(request: String, stage: HireStage) -> Self {
        Self {
            request,
            stage,
            session: 0,
            player: String::new(),
            settlement: String::new(),
            alias: String::new(),
            handle: String::new(),
            role: None,
            warehouse: String::new(),
            registry: None,
            version: None,
            snapshots: Vec::new(),
            intent: None,
            resulting_revision: 0,
            observed_tick: 0,
        }
    }

    fn endpoint(&self, stage: HireStage) -> InventoryEndpoint {
        match stage {
            HireStage::Warehouse => InventoryEndpoint::Warehouse(self.warehouse.clone()),
            HireStage::Equipment | HireStage::ResultingGear => {
                InventoryEndpoint::ResidentEquipment(self.handle.clone())
            }
            HireStage::Carry => InventoryEndpoint::ResidentCarry(self.handle.clone()),
            _ => unreachable!("inventory stage"),
        }
    }
}
impl Settlements {
    pub(super) fn hire(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        alias: &str,
        role: &str,
        observed_tick: u64,
    ) -> Vec<Command> {
        if self.pending_hire.is_some()
            || self.pending_dismiss.is_some()
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
        let Some(role) = hire::Role::parse(&role.to_ascii_lowercase()) else {
            return vec![message_session(
                session,
                "Role must be militia, infantry, spearman, archer, or medic.",
            )];
        };
        let Some(request) = self.next_storage_request("hire-registry") else {
            return vec![message_session(
                session,
                "Settlement records are unavailable.",
            )];
        };
        let mut pending = PendingHire::new(request.clone(), HireStage::Registry);
        pending.session = session;
        pending.player = player.to_owned();
        pending.settlement = settlement.to_owned();
        pending.alias = alias.to_owned();
        pending.role = Some(role);
        pending.observed_tick = observed_tick;
        self.pending_hire = Some(pending);
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    pub(super) fn recover_hires(&mut self) -> Vec<Command> {
        let Some(request) = self.next_storage_request("hire-recover") else {
            return Vec::new();
        };
        self.pending_hire = Some(PendingHire::new(request.clone(), HireStage::Recover));
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    fn hire_request(
        &mut self,
        pending: &mut PendingHire,
        stage: HireStage,
        prefix: &str,
    ) -> String {
        let request = self
            .next_storage_request(prefix)
            .expect("bounded storage request sequence");
        pending.stage = stage;
        pending.request = request.clone();
        request
    }

    fn hire_failure(&mut self, pending: PendingHire, reason: &str) -> Vec<Command> {
        log(
            LogLevel::Info,
            &format!(
                "{MARKER} hire-refused settlement={} resident={} reason={reason}",
                pending.settlement, pending.alias
            ),
        );
        if pending.session == 0 {
            Vec::new()
        } else {
            vec![message_session(pending.session, reason)]
        }
    }

    pub(super) fn hire_get_answer(&mut self, answer: &StorageGetAnswered) -> Option<Vec<Command>> {
        let mut pending = self.pending_hire.take()?;
        if pending.request != answer.request
            || !matches!(
                pending.stage,
                HireStage::Recover
                    | HireStage::Registry
                    | HireStage::FinalizeRead
                    | HireStage::AbortRead
            )
        {
            self.pending_hire = Some(pending);
            return None;
        }
        let StorageGetOutcome::Read(record) = &answer.outcome else {
            return Some(self.hire_failure(pending, "Settlement records are unavailable."));
        };
        let registry = match record.value.as_deref() {
            Some(value) => match serde_json::from_str::<SettlementRegistry>(value) {
                Ok(registry) => registry,
                Err(_) => {
                    return Some(self.hire_failure(pending, "Settlement records are unreadable."))
                }
            },
            None => SettlementRegistry::default(),
        };
        pending.version = record.version;
        Some(match pending.stage {
            HireStage::Recover => {
                let next = registry.settlements.iter().find_map(|(name, settlement)| {
                    settlement.residents.iter().find_map(|(alias, resident)| {
                        resident.hiring.as_ref().map(|intent| {
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
                    return Some(self.recover_demobs_from_read(&registry));
                };
                pending.settlement = settlement;
                pending.alias = alias;
                pending.handle = handle;
                pending.warehouse = intent.warehouse.clone();
                pending.role = Some(intent.role);
                pending.intent = Some(intent);
                self.issue_hire_transfer(pending)
            }
            HireStage::Registry => self.start_hire(pending, registry),
            HireStage::FinalizeRead | HireStage::AbortRead => {
                self.write_hire_result(pending, registry)
            }
            _ => unreachable!(),
        })
    }

    fn start_hire(
        &mut self,
        mut pending: PendingHire,
        registry: SettlementRegistry,
    ) -> Vec<Command> {
        let Some(settlement) = registry.settlements.get(&pending.settlement) else {
            return self.hire_failure(pending, "Settlement not found.");
        };
        if !can_build_settlement(settlement, &pending.player) {
            return self.hire_failure(pending, "Only the owner or a steward hires residents.");
        }
        if settlement.condition.as_deref() == Some("ruined") {
            return self.hire_failure(pending, "Restore this settlement before hiring.");
        }
        let Some(resident) = settlement.residents.get(&pending.alias) else {
            return self.hire_failure(pending, "Resident not found in this settlement.");
        };
        // DEM-02: a child serves only once it reaches adulthood; residents with
        // no recorded origin are adopted adults and stay admissible. Every hire
        // role takes the same path, and the route names no exception.
        if !demography::is_adult_for_service(resident.age_origin, pending.observed_tick) {
            return self.hire_failure(pending, "A child serves only once grown.");
        }
        if resident.role.is_some()
            || resident.hiring.is_some()
            || resident.capturing.is_some()
            || resident.captive_by.is_some()
            || registry.settlements.iter().any(|(_, record)| {
                record.residents.values().any(|other| {
                    other.handle == resident.handle
                        && (other.role.is_some()
                            || other.hiring.is_some()
                            || other.capturing.is_some()
                            || other.captive_by.is_some())
                })
            })
        {
            return self.hire_failure(pending, "Resident is already employed or hiring.");
        }
        let Some(binding) = self.warehouse.as_ref() else {
            return self.hire_failure(pending, "Bind this settlement's warehouse before hiring.");
        };
        let belongs = warehouse_belongs_to(settlement, binding);
        if !belongs {
            return self.hire_failure(
                pending,
                "The bound warehouse belongs to another settlement.",
            );
        }
        pending.handle = resident.handle.clone();
        pending.warehouse = binding.handle.clone();
        pending.registry = Some(registry);
        let request = self.hire_request(&mut pending, HireStage::Resident, "hire-resident");
        let handle = pending.handle.clone();
        self.pending_hire = Some(pending);
        vec![query_residents(&request, &[handle], None)]
    }

    pub(super) fn hire_operation_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        let mut pending = self.pending_hire.take()?;
        if pending.request != answer.request
            || !matches!(
                pending.stage,
                HireStage::Resident
                    | HireStage::Warehouse
                    | HireStage::Equipment
                    | HireStage::Carry
                    | HireStage::Transfer
                    | HireStage::ResultingGear
            )
        {
            self.pending_hire = Some(pending);
            return None;
        }
        Some(match pending.stage {
            HireStage::Resident => {
                let OperationOutcome::Committed(committed) = &answer.outcome else {
                    return Some(self.hire_failure(pending, "Resident is unavailable."));
                };
                let OperationPayload::Resident(ResidentResult::Page(page)) = &committed.payload
                else {
                    return Some(
                        self.hire_failure(pending, "Resident query returned an unexpected result."),
                    );
                };
                if page.residents.len() != 1
                    || page.residents[0].handle != pending.handle
                    || !matches!(
                        page.residents[0].lifecycle,
                        residents::ResidentLifecycle::AliveLoaded
                            | residents::ResidentLifecycle::AliveUnloaded
                    )
                {
                    return Some(
                        self.hire_failure(pending, "Resident is not alive and available."),
                    );
                }
                self.query_hire_inventory(pending, HireStage::Warehouse)
            }
            HireStage::Warehouse | HireStage::Equipment | HireStage::Carry => {
                let OperationOutcome::Committed(committed) = &answer.outcome else {
                    return Some(self.hire_failure(pending, "Hiring inventory is unavailable."));
                };
                let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
                    &committed.payload
                else {
                    return Some(
                        self.hire_failure(
                            pending,
                            "Hiring inventory returned an unexpected result.",
                        ),
                    );
                };
                if !same_inventory_endpoint(&snapshot.endpoint, &pending.endpoint(pending.stage)) {
                    return Some(
                        self.hire_failure(pending, "Hiring inventory returned the wrong endpoint."),
                    );
                }
                pending.snapshots.push(snapshot.clone());
                match pending.stage {
                    HireStage::Warehouse => {
                        self.query_hire_inventory(pending, HireStage::Equipment)
                    }
                    HireStage::Equipment => self.query_hire_inventory(pending, HireStage::Carry),
                    _ => self.persist_hire_intent(pending),
                }
            }
            HireStage::ResultingGear => {
                let OperationOutcome::Committed(committed) = &answer.outcome else {
                    return Some(self.hire_failure(
                        pending,
                        "Hiring gear cannot be read; inspect the pending intent.",
                    ));
                };
                let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
                    &committed.payload
                else {
                    return Some(self.hire_failure(
                        pending,
                        "Hiring gear returned an unexpected result; inspect the pending intent.",
                    ));
                };
                if !same_inventory_endpoint(&snapshot.endpoint, &pending.endpoint(pending.stage))
                    || snapshot.fence.revision < pending.resulting_revision
                {
                    return Some(self.hire_failure(
                        pending,
                        "Hiring gear revision is stale; inspect the pending intent.",
                    ));
                }
                pending.resulting_revision = snapshot.fence.revision;
                self.read_hire_result(pending, false)
            }
            HireStage::Transfer => {
                if answer.operation_id.as_deref()
                    != pending
                        .intent
                        .as_ref()
                        .map(|intent| intent.operation_id.as_str())
                {
                    self.pending_hire = Some(pending);
                    return Some(Vec::new());
                }
                let OperationOutcome::Committed(committed) = &answer.outcome else {
                    // No native decision was committed. Abort the intent under
                    // another CAS; a competing record write cannot turn a
                    // refused transfer into employment.
                    return Some(self.read_hire_result(pending, true));
                };
                let OperationPayload::OwnedInventory(InventoryResult::Transfer(fences)) =
                    &committed.payload
                else {
                    return Some(self.hire_failure(pending, "Hiring transfer returned an unexpected result; inspect the pending intent."));
                };
                let equipment = InventoryEndpoint::ResidentEquipment(pending.handle.clone());
                let carry = InventoryEndpoint::ResidentCarry(pending.handle.clone());
                let revision = fences
                    .iter()
                    .find(|fence| same_inventory_endpoint(&fence.endpoint, &equipment))
                    .or_else(|| {
                        fences
                            .iter()
                            .find(|fence| same_inventory_endpoint(&fence.endpoint, &carry))
                    })
                    .map(|fence| fence.fence.revision);
                let Some(revision) = revision else {
                    return Some(self.hire_failure(
                        pending,
                        "Hiring receipt omitted resident gear; inspect the pending intent.",
                    ));
                };
                pending.resulting_revision = revision;
                self.query_hire_inventory(pending, HireStage::ResultingGear)
            }
            _ => unreachable!(),
        })
    }

    fn query_hire_inventory(&mut self, mut pending: PendingHire, stage: HireStage) -> Vec<Command> {
        let request = self.hire_request(&mut pending, stage, "hire-inventory");
        let endpoint = pending.endpoint(stage);
        self.pending_hire = Some(pending);
        vec![query_owned_inventory(&request, endpoint, None)]
    }

    fn persist_hire_intent(&mut self, mut pending: PendingHire) -> Vec<Command> {
        let [warehouse, equipment, carry] = pending.snapshots.as_slice() else {
            unreachable!("three snapshots")
        };
        let role = pending.role.expect("chosen hiring role");
        let transfers = match hire::plan(role, warehouse, equipment, carry) {
            Ok(transfers) => transfers,
            Err(reason) => return self.hire_failure(pending, reason),
        };
        let issued = match hire::issued_kit(role, &pending.warehouse, warehouse, &transfers) {
            Ok(issued) => issued,
            Err(reason) => return self.hire_failure(pending, reason),
        };
        let Some(mut registry) = pending.registry.take() else {
            unreachable!("read registry")
        };
        let Some(next) = registry
            .hire_sequence
            .checked_add(1)
            .filter(|next| *next <= MAX_SAFE_REVISION)
        else {
            return self.hire_failure(pending, "No further hires can be recorded.");
        };
        registry.hire_sequence = next;
        let intent = hire::Intent {
            employer: pending.settlement.clone(),
            role,
            warehouse: pending.warehouse.clone(),
            operation_id: format!("hire-{next}"),
            transfers,
            issued,
            warehouse_fence: hire::Fence::from_snapshot(warehouse),
            equipment_fence: hire::Fence::from_snapshot(equipment),
            carry_fence: hire::Fence::from_snapshot(carry),
        };
        registry
            .settlements
            .get_mut(&pending.settlement)
            .expect("chosen settlement")
            .residents
            .get_mut(&pending.alias)
            .expect("chosen resident")
            .hiring = Some(intent.clone());
        let value = match serde_json::to_string(&registry) {
            Ok(value) => value,
            Err(_) => return self.hire_failure(pending, "Settlement records are unavailable."),
        };
        pending.intent = Some(intent);
        let request = self.hire_request(&mut pending, HireStage::IntentWrite, "hire-intent");
        self.pending_hire = Some(pending);
        vec![storage_cas(
            &request,
            SETTLEMENT_REGISTRY_KEY,
            self.pending_hire.as_ref().unwrap().version,
            value,
        )]
    }

    fn issue_hire_transfer(&mut self, mut pending: PendingHire) -> Vec<Command> {
        let intent = pending.intent.as_ref().expect("durable intent");
        let warehouse = InventoryEndpoint::Warehouse(intent.warehouse.clone());
        let equipment = InventoryEndpoint::ResidentEquipment(pending.handle.clone());
        let carry = InventoryEndpoint::ResidentCarry(pending.handle.clone());
        let transfers = intent
            .transfers
            .iter()
            .map(|transfer| {
                let (destination, destination_slot) = match transfer.destination {
                    hire::Destination::Equipment(slot) => (equipment.clone(), slot),
                    hire::Destination::Carry(slot) => (carry.clone(), slot),
                };
                OwnedItemTransfer {
                    source: warehouse.clone(),
                    source_slot: transfer.source_slot,
                    destination,
                    destination_slot,
                    count: transfer.count,
                }
            })
            .collect();
        let mut fences = vec![InventoryExpectedRevision {
            endpoint: warehouse,
            fence: intent.warehouse_fence.wire(),
        }];
        if intent
            .transfers
            .iter()
            .any(|transfer| matches!(transfer.destination, hire::Destination::Equipment(_)))
        {
            fences.push(InventoryExpectedRevision {
                endpoint: equipment,
                fence: intent.equipment_fence.wire(),
            });
        }
        if intent
            .transfers
            .iter()
            .any(|transfer| matches!(transfer.destination, hire::Destination::Carry(_)))
        {
            fences.push(InventoryExpectedRevision {
                endpoint: carry,
                fence: intent.carry_fence.wire(),
            });
        }
        let operation_id = intent.operation_id.clone();
        let request = self.hire_request(&mut pending, HireStage::Transfer, "hire-transfer");
        self.pending_hire = Some(pending);
        vec![transfer_owned_items(
            &request,
            &operation_id,
            0,
            transfers,
            fences,
        )]
    }

    fn read_hire_result(&mut self, mut pending: PendingHire, abort: bool) -> Vec<Command> {
        let request = self.hire_request(
            &mut pending,
            if abort {
                HireStage::AbortRead
            } else {
                HireStage::FinalizeRead
            },
            "hire-finalize-read",
        );
        self.pending_hire = Some(pending);
        vec![storage_get(&request, SETTLEMENT_REGISTRY_KEY)]
    }

    fn write_hire_result(
        &mut self,
        mut pending: PendingHire,
        mut registry: SettlementRegistry,
    ) -> Vec<Command> {
        let Some(resident) = registry
            .settlements
            .get_mut(&pending.settlement)
            .and_then(|record| record.residents.get_mut(&pending.alias))
        else {
            return self.hire_failure(
                pending,
                "Hiring resident record disappeared; inspect native receipt.",
            );
        };
        if resident.handle != pending.handle
            || resident
                .hiring
                .as_ref()
                .map(|intent| intent.operation_id.as_str())
                != pending
                    .intent
                    .as_ref()
                    .map(|intent| intent.operation_id.as_str())
        {
            return self.hire_failure(pending, "Hiring intent changed; inspect native receipt.");
        }
        let abort = pending.stage == HireStage::AbortRead;
        if !abort {
            resident.role = pending.role;
            resident.employer = Some(pending.settlement.clone());
            resident.work_revision = pending.resulting_revision;
            resident.issued = Some(
                pending
                    .intent
                    .as_ref()
                    .expect("durable hire intent")
                    .issued
                    .clone(),
            );
        }
        resident.hiring = None;
        let value = match serde_json::to_string(&registry) {
            Ok(value) => value,
            Err(_) => return self.hire_failure(pending, "Settlement records are unavailable."),
        };
        let request = self.hire_request(
            &mut pending,
            if abort {
                HireStage::AbortWrite
            } else {
                HireStage::FinalizeWrite
            },
            "hire-finalize",
        );
        self.pending_hire = Some(pending);
        vec![storage_cas(
            &request,
            SETTLEMENT_REGISTRY_KEY,
            self.pending_hire.as_ref().unwrap().version,
            value,
        )]
    }

    pub(super) fn hire_cas_answer(&mut self, answer: &StorageCasAnswered) -> Option<Vec<Command>> {
        let pending = self.pending_hire.take()?;
        if pending.request != answer.request
            || !matches!(
                pending.stage,
                HireStage::IntentWrite | HireStage::FinalizeWrite | HireStage::AbortWrite
            )
        {
            self.pending_hire = Some(pending);
            return None;
        }
        Some(match (pending.stage, &answer.outcome) {
            (HireStage::IntentWrite, StorageCasOutcome::Committed(_)) => {
                self.issue_hire_transfer(pending)
            }
            (HireStage::FinalizeWrite | HireStage::AbortWrite, StorageCasOutcome::Committed(_)) => {
                let aborted = pending.stage == HireStage::AbortWrite;
                let message = if aborted {
                    "Hiring refused; no stock or resident gear changed.".to_owned()
                } else {
                    format!(
                        "{} hired as {} for {}.",
                        pending.alias,
                        pending.role.expect("role").name(),
                        pending.settlement
                    )
                };
                log(
                    LogLevel::Info,
                    &format!(
                        "{MARKER} hire-{} settlement={} resident={} handle={}",
                        if aborted { "refused" } else { "committed" },
                        pending.settlement,
                        pending.alias,
                        pending.handle
                    ),
                );
                let mut commands = if pending.session == 0 {
                    Vec::new()
                } else {
                    vec![message_session(pending.session, message)]
                };
                commands.extend(self.recover_hires());
                commands
            }
            (HireStage::FinalizeWrite | HireStage::AbortWrite, StorageCasOutcome::Refused) => {
                let abort = pending.stage == HireStage::AbortWrite;
                self.read_hire_result(pending, abort)
            }
            _ => self.hire_failure(
                pending,
                "Settlement records changed; inspect the resident before retrying.",
            ),
        })
    }
}
