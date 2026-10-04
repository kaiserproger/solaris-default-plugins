//! Player-facing physical supply and checkpoint-qualified caravan escort contracts.
//! Terms describe real stock and allocated native coins; projections confer no rights.
use super::caravan::{Container, Contract as Caravan, Stack, StorageGuard};
use super::payment_mandate::{Blob, Reservation};
use serde::{Deserialize, Serialize};
use solaris_plugin_sdk::{events, inventories, npc_routes};

// Sixteen native scope guards, less registry, caravan store and offer index.
pub(super) const MAX_OFFERS: usize = 13;
pub(super) const MAX_LEGS: usize = 16;
pub(super) const ITEM_BUDGET: u32 = 4096;
pub(super) const INDEX_KEY: &str = "player-contract-index-v1";
pub(super) fn key(id: &str) -> String {
    format!("player-contract-v1/{id}")
}
pub(super) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Index {
    pub ids: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Coins {
    pub slot: u8,
    pub count: u32,
    pub stack: Stack,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Need {
    Supply {
        resource: String,
        target: u32,
    },
    Escort {
        shipment: String,
        carrier: String,
        source: Container,
        item: String,
        quantity: u32,
        radius: u32,
        sample_ticks: u64,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Terms {
    pub id: String,
    pub owner: String,
    pub stock: Container,
    pub need: Need,
    pub reward: u32,
    pub coins: Vec<Coins>,
    pub reserve_operation: String,
    pub stock_fence: inventories::InventoryFence,
    pub guards: Vec<StorageGuard>,
}
impl Terms {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !valid_id(&self.id)
            || self.owner.is_empty()
            || self.reward == 0
            || self.reward > ITEM_BUDGET
            || self.coins.is_empty()
            || self.coins.len() >= MAX_LEGS
            || self
                .coins
                .iter()
                .any(|c| c.count == 0 || c.stack.resource != "minecraft:emerald")
            || self.coins.iter().map(|c| u64::from(c.count)).sum::<u64>() != u64::from(self.reward)
            || self.guards.is_empty()
            || self.guards.iter().any(|g| g.version == 0)
        {
            return Err("invalid physical contract budget or authority");
        }
        match &self.need {
            Need::Supply { resource, target } => {
                if resource.is_empty()
                    || resource == "minecraft:emerald"
                    || *target == 0
                    || target
                        .checked_add(self.reward)
                        .is_none_or(|n| n > ITEM_BUDGET)
                {
                    return Err("supply target and reward must fit one native exchange; currency is not its own stock deficit");
                }
            }
            Need::Escort {
                shipment,
                carrier,
                source,
                item,
                quantity,
                radius,
                sample_ticks,
            } => {
                if shipment.is_empty()
                    || carrier.is_empty()
                    || source == &self.stock
                    || item.is_empty()
                    || *quantity == 0
                    || *radius == 0
                    || *radius > 128
                    || *sample_ticks == 0
                    || sample_ticks.checked_mul(2).is_none()
                {
                    return Err("explicit genuine shipment and bounded checkpoint terms required");
                }
            }
        }
        Ok(())
    }
    pub fn plan(&self) -> inventories::InventoryResourcePlan {
        inventories::InventoryResourcePlan {
            portions: vec![inventories::InventoryWorkPortion {
                work_units: 1,
                materials: vec![inventories::InventoryMaterial {
                    resource_id: "minecraft:emerald".into(),
                    quantity: u64::from(self.reward),
                }],
            }],
        }
    }
    pub fn remaining(
        &self,
        stock: &inventories::OwnedInventorySnapshot,
    ) -> Result<u32, &'static str> {
        if !same_endpoint(&stock.endpoint, &self.stock.endpoint()) {
            return Err("not original physical stock");
        }
        let Need::Supply { resource, target } = &self.need else {
            return Err("not a stock-deficit contract");
        };
        let count = stock
            .slots
            .iter()
            .filter_map(|s| s.item.as_ref())
            .filter(|i| &i.resource_id == resource)
            .map(|i| u64::from(i.count))
            .sum::<u64>();
        Ok(u64::from(*target).saturating_sub(count) as u32)
    }
    pub fn matches_caravan(&self, c: &Caravan) -> bool {
        let Need::Escort {
            shipment,
            carrier,
            source,
            item,
            quantity,
            ..
        } = &self.need
        else {
            return false;
        };
        c.party.id == *shipment
            && c.owner() == self.owner
            && c.carrier == *carrier
            && c.source == *source
            && c.destination == self.stock
            && c.item == *item
            && c.quantity == *quantity
            && c.death_ref.is_none()
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Participant {
    pub player: String,
    pub session: u64,
}
#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub(super) enum State {
    ReserveIntent,
    Offered,
    Claimed,
    PayoutIntent,
    Paid,
    ReleaseIntent,
    Closed,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Escort {
    pub route_ref: String,
    pub entity_uuid: String,
    pub epoch: u64,
    pub started: bool,
    pub geometry_hash: String,
    pub manifest: Blob,
    pub load_operation: String,
    pub segment: u16,
    pub cursor: u16,
    pub native_ticks: u64,
    pub samples: u64,
    pub progress_samples: u64,
    pub last_tick: u64,
    pub next_due: u64,
    pub arrival: Option<String>,
    pub checkpoint: Blob,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Head {
    pub id: String,
    pub owner: String,
    pub terms: Blob,
    pub public: Offer,
    pub state: State,
    pub participant: Option<Participant>,
    pub reservation: Option<Reservation>,
    pub escort: Option<Escort>,
    pub intent: Option<Blob>,
    pub receipt: Option<Blob>,
    pub release_operation: Option<String>,
    pub blocked: Option<String>,
    pub attempt: u64,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Offer {
    pub id: String,
    pub owner: String,
    pub location: Container,
    pub need: Need,
    pub fixed_emerald_reward: u32,
}
#[derive(Serialize)]
pub(super) struct Projection<'a> {
    pub offer: &'a Offer,
    pub state: State,
    /// Only this actor's claim and receipt references; never foreign inventories.
    pub own_claim: Option<&'a Participant>,
    pub own_receipt: Option<&'a Blob>,
    pub own_checkpoint: Option<&'a Blob>,
    pub blocked: Option<&'a str>,
}
impl Head {
    pub fn projection(&self, player: &str) -> Option<Projection<'_>> {
        let own = self.participant.as_ref().filter(|p| p.player == player);
        if self.state != State::Offered && own.is_none() && self.owner != player {
            return None;
        }
        Some(Projection {
            offer: &self.public,
            state: self.state,
            own_claim: own,
            own_receipt: own.and(self.receipt.as_ref()),
            own_checkpoint: own.and(self.escort.as_ref().map(|e| &e.checkpoint)),
            blocked: if own.is_some() || self.owner == player {
                self.blocked.as_deref()
            } else {
                None
            },
        })
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Intent {
    pub operation: inventories::InventoryConsumeReservation,
    pub participant: Participant,
    pub guards: Vec<StorageGuard>,
    pub before: Vec<inventories::OwnedInventorySnapshot>,
    pub supplied: u32,
    pub delivery: Option<serde_json::Value>,
}
#[derive(Serialize)]
pub(super) struct Checkpoint<'a> {
    pub player: &'a events::PlayerSnapshot,
    pub route: &'a npc_routes::NpcRouteStatus,
    pub simulation_tick: u64,
    pub previous: Option<&'a Blob>,
}
pub(super) fn same_endpoint(
    a: &inventories::InventoryEndpoint,
    b: &inventories::InventoryEndpoint,
) -> bool {
    match (a, b) {
        (
            inventories::InventoryEndpoint::PlayerInventory(a),
            inventories::InventoryEndpoint::PlayerInventory(b),
        ) => a == b,
        (
            inventories::InventoryEndpoint::Container(a),
            inventories::InventoryEndpoint::Container(b),
        ) => {
            a.dimension == b.dimension
                && a.position.x == b.position.x
                && a.position.y == b.position.y
                && a.position.z == b.position.z
        }
        _ => false,
    }
}
pub(super) fn expected(
    s: &inventories::OwnedInventorySnapshot,
) -> inventories::InventoryExpectedRevision {
    inventories::InventoryExpectedRevision {
        endpoint: s.endpoint.clone(),
        fence: s.fence.clone(),
    }
}
pub(super) fn allocate_coins(
    stock: &inventories::OwnedInventorySnapshot,
    reward: u32,
) -> Result<Vec<Coins>, &'static str> {
    let mut left = reward;
    let mut coins = Vec::new();
    for slot in &stock.slots {
        if left == 0 {
            break;
        }
        let Some(item) = slot
            .item
            .as_ref()
            .filter(|i| i.resource_id == "minecraft:emerald")
        else {
            continue;
        };
        let count = item.count.min(left);
        coins.push(Coins {
            slot: slot.slot,
            count,
            stack: Stack::from_native(item),
        });
        left -= count;
    }
    if left != 0 || coins.len() >= MAX_LEGS {
        return Err("actual allocated coin budget cannot fit native exchange");
    }
    Ok(coins)
}
pub(super) fn observe_escort(
    terms: &Terms,
    participant: &Participant,
    caravan: &Caravan,
    route: &npc_routes::NpcRouteStatus,
    player: &events::PlayerSnapshot,
    now: u64,
    previous: Option<&Escort>,
    checkpoint: Blob,
) -> Result<Escort, &'static str> {
    let Need::Escort {
        carrier,
        radius,
        sample_ticks,
        ..
    } = &terms.need
    else {
        return Err("not escort terms");
    };
    if !terms.matches_caravan(caravan)
        || player.player != participant.player
        || player.session != participant.session
        || route.handle != *carrier
        || caravan.carrier_uuid.as_deref() != Some(route.entity_uuid.as_str())
        || caravan.route_ref.as_deref() != Some(route.route_ref.as_str())
        || !matches!(
            route.state,
            npc_routes::NpcRouteState::Prepared
                | npc_routes::NpcRouteState::Running
                | npc_routes::NpcRouteState::Arrived
        )
        || route.death_cargo.is_some()
    {
        return Err("original genuine route, carrier, live player or shipment unavailable");
    }
    let pose = route
        .pose
        .as_ref()
        .ok_or("original native carrier pose unavailable")?;
    if pose.dimension != player.dimension
        || pose.dimension != terms.stock.dimension
        || ![
            pose.x,
            pose.y,
            pose.z,
            player.position.x,
            player.position.y,
            player.position.z,
        ]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err("escort observation dimension or pose mismatch");
    }
    let distance = (pose.x - player.position.x).powi(2)
        + (pose.y - player.position.y).powi(2)
        + (pose.z - player.position.z).powi(2);
    if distance > f64::from(*radius).powi(2) {
        return Err("player missed actual declared proximity checkpoint");
    }
    let manifest = caravan
        .manifest
        .as_ref()
        .ok_or("original physical load manifest unavailable")?;
    let manifest_blob = Blob::encode(manifest)?.0;
    let load = caravan
        .receipts
        .iter()
        .find(|r| r.effect == super::caravan::Effect::Load && r.native_revision.is_some())
        .ok_or("genuine committed load receipt unavailable")?;
    let arrival = route.arrival.as_ref().map(|a| a.receipt_ref.clone());
    let started = !matches!(route.state, npc_routes::NpcRouteState::Prepared);
    if started
        && (!caravan.started
            || !caravan.receipts.iter().any(|receipt| {
                receipt.effect == super::caravan::Effect::Start
                    && receipt.native_revision.is_some()
                    && receipt.epoch == route.executor_epoch
            }))
    {
        return Err("genuine original native start receipt unavailable");
    }
    let (samples, progress_samples) = if let Some(last) = previous {
        if route.route_ref != last.route_ref
            || route.entity_uuid != last.entity_uuid
            || (last.started && route.executor_epoch != last.epoch)
            || (last.started && !started)
            || route.geometry_hash != last.geometry_hash
            || manifest_blob != last.manifest
            || load.operation != last.load_operation
            || now < last.next_due
            || now > last.next_due.saturating_add(*sample_ticks)
            || (route.segment, route.cursor) < (last.segment, last.cursor)
            || route.native_ticks < last.native_ticks
        {
            return Err(
                "escort original identity, epoch, causal checkpoint or sampling deadline changed",
            );
        }
        (
            last.samples
                .checked_add(1)
                .ok_or("checkpoint count exhausted")?,
            last.progress_samples
                .checked_add(u64::from(
                    route.native_ticks > last.native_ticks
                        && (route.segment, route.cursor) > (last.segment, last.cursor),
                ))
                .ok_or("progress count exhausted")?,
        )
    } else {
        if !matches!(route.state, npc_routes::NpcRouteState::Prepared)
            || route.segment != 0
            || route.cursor != 0
            || arrival.is_some()
        {
            return Err("escort must be accepted at original route's first native checkpoint before progress");
        }
        (1, 0)
    };
    if matches!(route.state, npc_routes::NpcRouteState::Arrived)
        && route.arrival.as_ref().is_none_or(|a| {
            a.executor_epoch != route.executor_epoch || a.dimension != player.dimension
        })
    {
        return Err("native arrival witness absent or not original epoch");
    }
    Ok(Escort {
        route_ref: route.route_ref.clone(),
        entity_uuid: route.entity_uuid.clone(),
        epoch: route.executor_epoch,
        started,
        geometry_hash: route.geometry_hash.clone(),
        manifest: manifest_blob,
        load_operation: load.operation.clone(),
        segment: route.segment,
        cursor: route.cursor,
        native_ticks: route.native_ticks,
        samples,
        progress_samples,
        last_tick: now,
        next_due: now
            .checked_add(*sample_ticks)
            .ok_or("simulation deadline exhausted")?,
        arrival,
        checkpoint,
    })
}
pub(super) fn escort_delivery<'a>(
    terms: &Terms,
    escort: &Escort,
    caravan: &'a Caravan,
) -> Result<super::caravan::GrossDelivery<'a>, &'static str> {
    let gross = caravan
        .gross_delivery()
        .ok_or("no genuine committed gross delivery")?;
    if !terms.matches_caravan(caravan)
        || escort.samples < 2
        || escort.progress_samples == 0
        || escort.arrival.as_deref() != Some(gross.arrival_ref)
        || escort.route_ref != gross.route_ref
        || escort.entity_uuid != gross.entity_uuid
        || escort.epoch != gross.executor_epoch
        || escort.manifest != Blob::encode(gross.manifest)?.0
        || escort.load_operation != gross.load.operation
    {
        return Err("gross delivery does not close original causally observed escort");
    }
    Ok(gross)
}
pub(super) fn validate_paid_receipt(
    terms: &Terms,
    intent: &Intent,
    result: &inventories::InventoryReservedTransfer,
) -> Result<(), &'static str> {
    let r = &result.reservation;
    if r.reservation_ref != intent.operation.reservation_ref
        || !same_endpoint(&r.endpoint, &terms.stock.endpoint())
        || r.receipt_watermark != 1
        || r.quantities.len() != 1
        || r.released
        || r.quantities[0].resource_id != "minecraft:emerald"
        || r.quantities[0].reserved != u64::from(terms.reward)
        || r.quantities[0].consumed != u64::from(terms.reward)
        || r.quantities[0].remaining != 0
        || r.quantities[0].returned != 0
        || result.inventories.len() != 2
    {
        return Err("native budget consumption receipt mismatch");
    }
    let mut expected_images = intent.before.clone();
    for leg in &intent.operation.transfers {
        let source = expected_images
            .iter()
            .position(|s| same_endpoint(&s.endpoint, &leg.source))
            .ok_or("source image missing")?;
        let destination = expected_images
            .iter()
            .position(|s| same_endpoint(&s.endpoint, &leg.destination))
            .ok_or("destination image missing")?;
        let source_slot = expected_images[source]
            .slots
            .iter()
            .position(|s| s.slot == leg.source_slot)
            .ok_or("source slot missing")?;
        let destination_slot = expected_images[destination]
            .slots
            .iter()
            .position(|s| s.slot == leg.destination_slot)
            .ok_or("destination slot missing")?;
        let mut item = expected_images[source].slots[source_slot]
            .item
            .clone()
            .ok_or("physical source item missing")?;
        if item.count < leg.count {
            return Err("physical source quantity mismatch");
        }
        let source_item = &mut expected_images[source].slots[source_slot].item;
        if item.count == leg.count {
            *source_item = None;
        } else {
            source_item.as_mut().expect("present").count -= leg.count;
        }
        item.count = leg.count;
        let dest = &mut expected_images[destination].slots[destination_slot].item;
        if let Some(existing) = dest {
            if !Stack::from_native(&item).matches(existing) {
                return Err("native component placement mismatch");
            }
            existing.count = existing
                .count
                .checked_add(leg.count)
                .ok_or("native item count overflow")?;
        } else {
            *dest = Some(item);
        }
    }
    for before in &expected_images {
        let after = result
            .inventories
            .iter()
            .find(|s| same_endpoint(&s.endpoint, &before.endpoint))
            .ok_or("participant after-image missing")?;
        if after.fence.revision <= before.fence.revision
            || after.fence.snapshot_hash.len() != 64
            || serde_json::to_vec(&after.slots).map_err(|_| "after-image encoding refused")?
                != serde_json::to_vec(&before.slots)
                    .map_err(|_| "expected image encoding refused")?
        {
            return Err("full canonical native after-image disagrees with immutable exchange");
        }
    }
    if matches!(terms.need, Need::Supply { .. })
        && terms.remaining(
            result
                .inventories
                .iter()
                .find(|s| same_endpoint(&s.endpoint, &terms.stock.endpoint()))
                .ok_or("stock after-image missing")?,
        )? != 0
    {
        return Err("actual physical stock deficit not closed");
    }
    Ok(())
}
