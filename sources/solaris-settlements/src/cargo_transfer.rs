//! Ordinary owner-authorized cargo movement, independent of NPC work admission.
use super::*;
use solaris_plugin_sdk::inventories::{
    InventoryExpectedRevision, OwnedInventorySnapshot, OwnedItemTransfer,
};

#[derive(Default)]
pub(super) struct Runtime {
    pending: Option<Pending>,
}
struct Pending {
    request: String,
    session: u64,
    source: InventoryEndpoint,
    destination: InventoryEndpoint,
    resource: String,
    count: u32,
    source_image: Option<OwnedInventorySnapshot>,
    guard: caravan::StorageGuard,
}
impl Runtime {
    pub(super) fn busy(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn reset(&mut self) {
        self.pending = None;
    }
}
impl Settlements {
    pub(super) fn handle_cargo_command(
        &mut self,
        invoked: &CommandInvoked,
    ) -> Option<Vec<Command>> {
        if invoked.name != "settlement"
            || invoked.arguments.first().map(String::as_str) != Some("transfer-cargo")
        {
            return None;
        }
        let args: Vec<_> = invoked.arguments.iter().map(String::as_str).collect();
        let [_, settlement, alias, direction, resource, count] = args.as_slice() else {
            return Some(vec![message_session(
                invoked.session,
                "Usage: transfer-cargo <settlement> <resident-alias> <in|out> <item-id> <count>",
            )]);
        };
        let Some(count) = count
            .parse::<u32>()
            .ok()
            .filter(|count| (1..=4096).contains(count))
        else {
            return Some(vec![message_session(
                invoked.session,
                "Cargo count must be 1..4096 actual items.",
            )]);
        };
        if !valid_settlement_name(settlement)
            || !valid_identity(alias)
            || !valid_resource_id(resource)
            || !matches!(*direction, "in" | "out")
        {
            return Some(vec![message_session(
                invoked.session,
                "Pass a known settlement/resident, in|out and a canonical item id.",
            )]);
        }
        Some(self.begin_registry_read(RegistryRead::Cargo {
            session: invoked.session,
            player: invoked.player.clone(),
            settlement: (*settlement).to_owned(),
            alias: (*alias).to_owned(),
            inward: *direction == "in",
            resource: (*resource).to_owned(),
            count,
        }))
    }

    pub(super) fn cargo_registry(
        &mut self,
        session: u64,
        player: &str,
        settlement: &str,
        alias: &str,
        inward: bool,
        resource: String,
        count: u32,
        registry: &SettlementRegistry,
        registry_version: Option<u64>,
    ) -> Vec<Command> {
        let Some(record) = registry
            .settlements
            .get(settlement)
            .filter(|record| can_build_settlement(record, player))
        else {
            return vec![message_session(
                session,
                "Cargo refused: current owner or steward rights required.",
            )];
        };
        let Some(resident) = record.residents.get(alias).filter(|resident| {
            resident.captive_by.is_none()
                && resident.capturing.is_none()
                && resident.capture_recovery.is_none()
                && resident.hiring.is_none()
                && resident.dismissing.is_none()
        }) else {
            return vec![message_session(
                session,
                "Cargo refused: resident is unknown, captive or changing service/custody.",
            )];
        };
        let Some(warehouse) = self
            .warehouse
            .as_ref()
            .filter(|warehouse| warehouse_belongs_to(record, warehouse))
            .and_then(|warehouse| warehouse_endpoint(&warehouse.handle))
        else {
            return vec![message_session(
                session,
                "Cargo refused: bind this settlement's physical warehouse first.",
            )];
        };
        let carry = InventoryEndpoint::NpcCarry(resident.handle.clone());
        let (source, destination) = if inward {
            (warehouse, carry)
        } else {
            (carry, warehouse)
        };
        let Ok((key, Some(version))) = self.registry_scope_fence(registry_version, settlement)
        else {
            return vec![message_session(
                session,
                "Cargo current ownership fence unavailable; no transfer.",
            )];
        };
        if version == 0 {
            return vec![message_session(
                session,
                "Cargo current ownership fence unavailable; no transfer.",
            )];
        }
        let Some(request) = self.next_storage_request("cargo-source") else {
            return vec![message_session(session, "Cargo query counter exhausted.")];
        };
        let command = query_owned_inventory(&request, source.clone(), None);
        self.cargo_runtime.pending = Some(Pending {
            request,
            session,
            source,
            destination,
            resource,
            count,
            source_image: None,
            guard: caravan::StorageGuard { key, version },
        });
        vec![command]
    }

    pub(super) fn cargo_query_answer(
        &mut self,
        answer: &OperationAnswered,
    ) -> Option<Vec<Command>> {
        if !self
            .cargo_runtime
            .pending
            .as_ref()
            .is_some_and(|pending| pending.request == answer.request)
        {
            return None;
        }
        if answer.operation_id.is_some() {
            return Some(Vec::new());
        }
        let mut pending = self.cargo_runtime.pending.take()?;
        let OperationOutcome::Committed(committed) = &answer.outcome else {
            return Some(vec![message_session(
                pending.session,
                format!("Cargo inventory read refused: {:?}.", answer.outcome),
            )]);
        };
        let OperationPayload::OwnedInventory(InventoryResult::Snapshot(snapshot)) =
            &committed.payload
        else {
            return Some(vec![message_session(
                pending.session,
                "Cargo inventory read returned an unexpected result.",
            )]);
        };
        let expected = if pending.source_image.is_some() {
            &pending.destination
        } else {
            &pending.source
        };
        if !same_inventory_endpoint(&snapshot.endpoint, expected) {
            return Some(vec![message_session(
                pending.session,
                "Cargo inventory read returned another endpoint.",
            )]);
        }
        if pending.source_image.is_none() {
            pending.source_image = Some(snapshot.clone());
            let Some(request) = self.next_storage_request("cargo-destination") else {
                return Some(vec![message_session(
                    pending.session,
                    "Cargo query counter exhausted.",
                )]);
            };
            pending.request = request.clone();
            let command = query_owned_inventory(&request, pending.destination.clone(), None);
            self.cargo_runtime.pending = Some(pending);
            return Some(vec![command]);
        }
        let source = pending.source_image.as_ref().expect("read source image");
        let transfers = match plan(source, snapshot, &pending.resource, pending.count) {
            Ok(transfers) => transfers,
            Err(reason) => return Some(vec![message_session(pending.session, reason)]),
        };
        let mut fences = vec![
            InventoryExpectedRevision {
                endpoint: source.endpoint.clone(),
                fence: source.fence.clone(),
            },
            InventoryExpectedRevision {
                endpoint: snapshot.endpoint.clone(),
                fence: snapshot.fence.clone(),
            },
        ];
        // Native endpoint canonical order is container before NPC carry.
        if matches!(fences[0].endpoint, InventoryEndpoint::NpcCarry(_)) {
            fences.swap(0, 1);
        }
        Some(self.submit(OperationIntent::CargoTransfer {
            session: pending.session,
            transfers,
            fences,
            guard: pending.guard,
        }))
    }

    pub(super) fn cargo_transfer_answer(
        &mut self,
        session: u64,
        outcome: &OperationOutcome,
    ) -> Vec<Command> {
        match outcome {
            OperationOutcome::Committed(committed)
                if matches!(
                    committed.payload,
                    OperationPayload::OwnedInventory(InventoryResult::Transfer(_))
                ) =>
            {
                vec![message_session(
                    session,
                    format!(
                        "Cargo transfer committed native={}",
                        serde_json::json!(committed.payload)
                    ),
                )]
            }
            OperationOutcome::Refused(refused) => vec![message_session(
                session,
                format!("Cargo transfer refused: {:?}.", refused.reason),
            )],
            _ => vec![message_session(
                session,
                "Cargo transfer returned an unexpected result; no completion claimed.",
            )],
        }
    }
}

/// Empty slots avoid guessing native stack capacities or component compatibility.
/// Each piece is no larger than an already valid native source stack; native
/// authority still checks the exact components, capacity, reservations and fences.
fn plan(
    source: &OwnedInventorySnapshot,
    destination: &OwnedInventorySnapshot,
    resource: &str,
    count: u32,
) -> Result<Vec<OwnedItemTransfer>, &'static str> {
    let mut targets = destination.slots.iter().filter(|slot| slot.item.is_none());
    let mut remaining = count;
    let mut transfers = Vec::new();
    for slot in &source.slots {
        let Some(item) = slot
            .item
            .as_ref()
            .filter(|item| item.resource_id == resource)
        else {
            continue;
        };
        if remaining == 0 {
            break;
        }
        let Some(target) = targets.next() else {
            return Err("Cargo transfer needs empty destination slots; no native stack capacity is guessed.");
        };
        if transfers.len() == 16 {
            return Err("Cargo transfer exceeds the native sixteen-move bound; choose a smaller exact quantity.");
        }
        let moved = remaining.min(item.count);
        transfers.push(OwnedItemTransfer {
            source: source.endpoint.clone(),
            source_slot: slot.slot,
            destination: destination.endpoint.clone(),
            destination_slot: target.slot,
            count: moved,
        });
        remaining -= moved;
    }
    if remaining > 0 {
        return Err("Cargo source lacks the exact requested physical quantity.");
    }
    Ok(transfers)
}
