use serde::{Deserialize, Serialize};
use solaris_plugin_sdk::inventories;

/// The worker's equipment slots are the resident endpoint's canonical order,
/// not the player's inventory or entity packet slot order.
#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Role {
    Militia,
    Infantry,
    Spearman,
    Archer,
    Medic,
}

impl Role {
    pub(super) fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "militia" => Self::Militia,
            "infantry" => Self::Infantry,
            "spearman" => Self::Spearman,
            "archer" => Self::Archer,
            "medic" => Self::Medic,
            _ => return None,
        })
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Militia => "militia",
            Self::Infantry => "infantry",
            Self::Spearman => "spearman",
            Self::Archer => "archer",
            Self::Medic => "medic",
        }
    }

    pub(super) fn combat_capable(self) -> bool {
        self != Self::Medic
    }

    fn kit(self) -> &'static [KitItem] {
        match self {
            Self::Militia => &MILITIA,
            Self::Infantry => &INFANTRY,
            Self::Spearman => &SPEARMAN,
            Self::Archer => &ARCHER,
            Self::Medic => &MEDIC,
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub(super) enum Destination {
    Equipment(u8),
    Carry(u8),
}

#[derive(Clone, Copy)]
struct KitItem {
    item: &'static str,
    count: u32,
    destination: Destination,
}

const fn equipment(item: &'static str, slot: u8) -> KitItem {
    KitItem {
        item,
        count: 1,
        destination: Destination::Equipment(slot),
    }
}

const fn wage(slot: u8) -> KitItem {
    KitItem {
        item: "minecraft:emerald",
        count: 1,
        destination: Destination::Carry(slot),
    }
}

// Real stock, including the signing wage, comes from the employer's bound
// warehouse. No role definition creates an item or a new resident.
const MILITIA: [KitItem; 3] = [
    equipment("minecraft:iron_sword", 0),
    equipment("minecraft:leather_chestplate", 3),
    wage(0),
];
const INFANTRY: [KitItem; 4] = [
    equipment("minecraft:iron_sword", 0),
    equipment("minecraft:shield", 1),
    equipment("minecraft:iron_chestplate", 3),
    wage(0),
];
const SPEARMAN: [KitItem; 3] = [
    equipment("minecraft:trident", 0),
    equipment("minecraft:chainmail_chestplate", 3),
    wage(0),
];
const ARCHER: [KitItem; 4] = [
    equipment("minecraft:bow", 0),
    equipment("minecraft:leather_chestplate", 3),
    KitItem {
        item: "minecraft:arrow",
        count: 32,
        destination: Destination::Carry(0),
    },
    wage(1),
];
const MEDIC: [KitItem; 1] = [wage(0)];

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Transfer {
    pub source_slot: u8,
    pub destination: Destination,
    pub count: u32,
}

/// Only issued kit follows a departing soldier back to its original warehouse;
/// the signing emerald is already the resident's earned personal property.
#[derive(Clone, Deserialize, Serialize)]
pub(super) struct IssuedKit {
    pub(super) warehouse: String,
    pub(super) items: Vec<IssuedItem>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct IssuedItem {
    pub(super) item: String,
    pub(super) count: u32,
    pub(super) damage: Option<i32>,
    pub(super) enchantments: Vec<IssuedEnchantment>,
    pub(super) custom_name: Option<String>,
    pub(super) item_model: Option<String>,
    pub(super) destination: Destination,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(super) struct IssuedEnchantment {
    resource_id: String,
    level: i32,
}

impl IssuedItem {
    fn from_stock(item: &inventories::InventoryItem, count: u32, destination: Destination) -> Self {
        Self {
            item: item.resource_id.clone(),
            count,
            damage: item.damage,
            enchantments: item
                .enchantments
                .iter()
                .map(|enchantment| IssuedEnchantment {
                    resource_id: enchantment.resource_id.clone(),
                    level: enchantment.level,
                })
                .collect(),
            custom_name: item.custom_name.clone(),
            item_model: item.item_model.clone(),
            destination,
        }
    }

    pub(super) fn matches(&self, held: &inventories::InventoryItem) -> bool {
        held.resource_id == self.item
            && held.custom_name == self.custom_name
            && held.item_model == self.item_model
            && held.enchantments.len() == self.enchantments.len()
            && held
                .enchantments
                .iter()
                .zip(&self.enchantments)
                .all(|(current, issued)| {
                    current.resource_id == issued.resource_id && current.level == issued.level
                })
            && self
                .damage
                .is_none_or(|original| held.damage.is_some_and(|current| current >= original))
    }
}

pub(super) fn issued_kit(
    role: Role,
    warehouse: &str,
    stock: &inventories::OwnedInventorySnapshot,
    transfers: &[Transfer],
) -> Result<IssuedKit, &'static str> {
    let mut items: Vec<IssuedItem> = Vec::new();
    for transfer in transfers {
        let Some(requirement) = role
            .kit()
            .iter()
            .find(|item| item.destination == transfer.destination)
        else {
            return Err("The issued kit has an unknown destination.");
        };
        if requirement.item == "minecraft:emerald" {
            continue;
        }
        let Some(source) = stock
            .slots
            .iter()
            .find(|slot| slot.slot == transfer.source_slot)
            .and_then(|slot| slot.item.as_ref())
        else {
            return Err("The issued kit has no source stack.");
        };
        if let Some(existing) = items
            .iter_mut()
            .find(|item| item.destination == transfer.destination)
        {
            if !existing.matches(source) {
                return Err("The issued kit mixes incompatible stacks in one resident slot.");
            }
            existing.count += transfer.count;
        } else {
            items.push(IssuedItem::from_stock(
                source,
                transfer.count,
                transfer.destination,
            ));
        }
    }
    Ok(IssuedKit {
        warehouse: warehouse.to_owned(),
        items,
    })
}
#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Fence {
    pub revision: u64,
    pub snapshot_hash: String,
}

impl Fence {
    pub(super) fn from_snapshot(snapshot: &inventories::OwnedInventorySnapshot) -> Self {
        Self {
            revision: snapshot.fence.revision,
            snapshot_hash: snapshot.fence.snapshot_hash.clone(),
        }
    }

    pub(super) fn wire(&self) -> inventories::InventoryFence {
        inventories::InventoryFence {
            revision: self.revision,
            snapshot_hash: self.snapshot_hash.clone(),
        }
    }
}

/// Persist before issuing the native move. A reload reconstructs precisely the
/// same fenced request and operation id; core replays a committed receipt before
/// checking fences, so a lost answer cannot debit the warehouse twice.
#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Intent {
    pub employer: String,
    pub role: Role,
    pub warehouse: String,
    pub operation_id: String,
    pub transfers: Vec<Transfer>,
    pub issued: IssuedKit,
    pub warehouse_fence: Fence,
    pub equipment_fence: Fence,
    pub carry_fence: Fence,
}

/// Refuse absent stock, occupied role slots, or a kit that cannot fit into one
/// bounded native transfer *before* persisting an intent or touching a resident.
pub(super) fn plan(
    role: Role,
    warehouse: &inventories::OwnedInventorySnapshot,
    equipment: &inventories::OwnedInventorySnapshot,
    carry: &inventories::OwnedInventorySnapshot,
) -> Result<Vec<Transfer>, &'static str> {
    let mut available = [0u32; 54];
    for slot in &warehouse.slots {
        if let Some(item) = &slot.item {
            available[usize::from(slot.slot)] = item.count;
        }
    }
    let mut transfers = Vec::new();
    for requirement in role.kit() {
        let destination = match requirement.destination {
            Destination::Equipment(slot) => equipment.slots.iter().find(|entry| entry.slot == slot),
            Destination::Carry(slot) => carry.slots.iter().find(|entry| entry.slot == slot),
        };
        if !destination.is_some_and(|slot| slot.item.is_none()) {
            return Err("The resident's required gear or payment slot is occupied.");
        }
        let mut remaining = requirement.count;
        for slot in &warehouse.slots {
            if remaining == 0 {
                break;
            }
            if !slot
                .item
                .as_ref()
                .is_some_and(|item| item.resource_id == requirement.item)
            {
                continue;
            }
            let count = available[usize::from(slot.slot)].min(remaining);
            if count == 0 {
                continue;
            }
            if transfers.len() == 16 {
                return Err("The kit spans more than sixteen warehouse stacks.");
            }
            transfers.push(Transfer {
                source_slot: slot.slot,
                destination: requirement.destination,
                count,
            });
            available[usize::from(slot.slot)] -= count;
            remaining -= count;
        }
        if remaining != 0 {
            return Err("The warehouse is missing the requested kit or signing wage.");
        }
    }
    Ok(transfers)
}
