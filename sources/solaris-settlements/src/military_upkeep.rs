//! CP-069+ military upkeep: recurring wages, kit readiness and morale
//! accounting behind the slice-A/B envelope (RULESET DECISION only).
//!
//! Wages accrue only from simulation ticks through `commerce::Employment`:
//! real emerald stock, an explicit negotiated contract, and a bounded budget
//! pay for every period — service itself never mints pay. Kit readiness is
//! the `hire::plan` envelope: real warehouse stock, free resident slots, and
//! exact paper models with the signing wage kept separate. Morale is the
//! `morale::OperationalMorale` envelope: distinct physical observations
//! advance distinct categories, and rally needs physical arrival plus the
//! named officer. Desertion follows from unpaid wages or routed morale —
//! never from a timer alone — and never mints or reclaims items by itself.
//!
//! This module owns its own ledger records only; money stays in fenced native
//! inventories and kit items stay in real stock. Storage keys are disjoint:
//! `settlement-upkeep-v1:{unit}`.

use super::commerce::{Employment, Refusal as PayRefusal};
use super::hire::{Destination, Loadout, Role};
use super::morale::{MoraleObservation, MoralePhase, OperationalMorale};
use serde::{Deserialize, Serialize};
use solaris_plugin_sdk::npcs;
use std::collections::BTreeMap;
pub(super) const VALUE_BYTES: usize = 4096;
/// Periods of unpaid wages before a unit deserts. Proposal value.
pub(super) const DESERT_AFTER_UNPAID_PERIODS: u64 = 3;

fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.'))
}

pub(super) fn key(unit: &str) -> Result<String, PayRefusal> {
    if !id(unit) {
        return Err(PayRefusal::InvalidContract);
    }
    Ok(format!("settlement-upkeep-v1:{unit}"))
}

/// One serviced unit: recurring pay, issued kit lineage, and morale state.
#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Unit {
    pub id: String,
    pub role: String,
    pub loadout: Option<String>,
    pub resident: String,
    pub warehouse: String,
    pub employment: String,
    pub employment_record: Employment,
    pub kit_destinations: Vec<u8>,
    pub morale: OperationalMorale,
    pub deserted: bool,
    pub desert_event: Option<String>,
}

impl Unit {
    pub(super) fn validate(&self) -> Result<(), PayRefusal> {
        if !id(&self.id)
            || Role::parse(&self.role).is_none()
            || !id(&self.resident)
            || !id(&self.warehouse)
            || !id(&self.employment)
        {
            return Err(PayRefusal::InvalidContract);
        }
        let role = Role::parse(&self.role).ok_or(PayRefusal::InvalidContract)?;
        Loadout::parse(role, self.loadout.as_deref()).map_err(|_| PayRefusal::InvalidContract)?;
        if self.employment_record.wage == 0 || self.employment_record.period_ticks == 0 {
            return Err(PayRefusal::InvalidContract);
        }
        if self.kit_destinations.is_empty() || self.kit_destinations.len() > 16 {
            return Err(PayRefusal::InvalidContract);
        }
        Ok(())
    }

    pub(super) fn encode(&self) -> Result<String, PayRefusal> {
        self.validate()?;
        let value = serde_json::to_string(self).map_err(|_| PayRefusal::InvalidContract)?;
        if value.len() > VALUE_BYTES {
            return Err(PayRefusal::InvalidContract);
        }
        Ok(value)
    }

    pub(super) fn decode(value: &str) -> Result<Self, PayRefusal> {
        if value.len() > VALUE_BYTES {
            return Err(PayRefusal::InvalidContract);
        }
        let unit: Self = serde_json::from_str(value).map_err(|_| PayRefusal::InvalidContract)?;
        unit.validate()?;
        Ok(unit)
    }

    /// Periods currently unpaid: earned minus paid, in wage units. Zero wage
    /// is an invalid contract, never a free garrison.
    pub(super) fn unpaid_periods(&self) -> Result<u64, PayRefusal> {
        let unpaid = self
            .employment_record
            .earned
            .checked_sub(self.employment_record.paid)
            .ok_or(PayRefusal::InvalidContract)?;
        if self.employment_record.wage == 0 {
            return Err(PayRefusal::InvalidContract);
        }
        Ok(unpaid / u64::from(self.employment_record.wage))
    }

    /// Desertion needs a real cause: sustained unpaid wages or routed morale.
    /// A timer, a window, or a lost battle alone never deserts a paid,
    /// steady unit. Desertion records the cause; it moves no items.
    pub(super) fn check_desertion(&mut self, event: &str) -> Result<bool, PayRefusal> {
        self.validate()?;
        if self.deserted {
            return Ok(false);
        }
        if !id(event) {
            return Err(PayRefusal::InvalidContract);
        }
        let unpaid = self.unpaid_periods()?;
        let routed = !self.morale.may_guard();
        if unpaid >= DESERT_AFTER_UNPAID_PERIODS || routed {
            self.deserted = true;
            self.desert_event = Some(event.into());
            return Ok(true);
        }
        Ok(false)
    }
}

#[derive(Default)]
pub(super) struct UpkeepLedger {
    units: BTreeMap<String, Unit>,
}

impl UpkeepLedger {
    /// Enlist one serviced unit. The employment id must be unique; a replay
    /// with the identical record is idempotent, a conflicting replay fails.
    pub(super) fn enlist(&mut self, unit: Unit) -> Result<(), PayRefusal> {
        unit.validate()?;
        let after = unit.encode()?;
        if let Some(previous) = self.units.get(&unit.employment) {
            let before = previous.encode()?;
            return if before == after {
                Ok(())
            } else {
                Err(PayRefusal::ConflictingReplay)
            };
        }
        // The whole ledger is byte-bounded: the future encoding must fit
        // VALUE_BYTES before the insert, not just the single record.
        let mut future: Vec<&Unit> = self.units.values().collect();
        future.push(&unit);
        let value = serde_json::to_string(&future).map_err(|_| PayRefusal::InvalidContract)?;
        if value.len() > VALUE_BYTES {
            return Err(PayRefusal::InvalidContract);
        }
        self.units.insert(unit.employment.clone(), unit);
        Ok(())
    }

    pub(super) fn get(&self, employment: &str) -> Option<&Unit> {
        self.units.get(employment)
    }

    pub(super) fn get_mut(&mut self, employment: &str) -> Option<&mut Unit> {
        self.units.get_mut(employment)
    }

    /// Accrue wages for every active unit from the simulation tick. Inactive
    /// or deserted units accrue nothing but keep earned personal property.
    /// Atomic: records accrue on clones first and the whole future ledger is
    /// bound-checked before any live record mutates, so a ledger at the
    /// 4096-byte edge refuses the tick instead of accruing halfway.
    pub(super) fn accrue_all(&mut self, tick: u64) -> Result<(), PayRefusal> {
        let mut future: Vec<Unit> = Vec::with_capacity(self.units.len());
        for unit in self.units.values() {
            let mut next = unit.clone();
            if !next.deserted && next.employment_record.active {
                next.employment_record.accrue(tick)?;
            }
            next.validate()?;
            next.encode()?;
            future.push(next);
        }
        let value = serde_json::to_string(&future).map_err(|_| PayRefusal::InvalidContract)?;
        if value.len() > VALUE_BYTES {
            return Err(PayRefusal::InvalidContract);
        }
        for unit in future {
            self.units.insert(unit.employment.clone(), unit);
        }
        Ok(())
    }
    pub(super) fn encode(&self) -> Result<String, PayRefusal> {
        let units: Vec<&Unit> = self.units.values().collect();
        let value = serde_json::to_string(&units).map_err(|_| PayRefusal::InvalidContract)?;
        if value.len() > VALUE_BYTES {
            return Err(PayRefusal::InvalidContract);
        }
        Ok(value)
    }

    pub(super) fn decode(value: &str) -> Result<Self, PayRefusal> {
        if value.len() > VALUE_BYTES {
            return Err(PayRefusal::InvalidContract);
        }
        let units: Vec<Unit> =
            serde_json::from_str(value).map_err(|_| PayRefusal::InvalidContract)?;
        let mut ledger = Self::default();
        for unit in units {
            unit.validate()?;
            if ledger.units.insert(unit.employment.clone(), unit).is_some() {
                return Err(PayRefusal::InvalidContract);
            }
        }
        Ok(ledger)
    }
}

pub(super) fn check_kit_readiness(
    role: Role,
    loadout: Option<Loadout>,
    planned: &[super::hire::Transfer],
    issued: &super::hire::IssuedKit,
) -> Result<(), &'static str> {
    // Readiness binds the issued kit to the role/loadout destination by
    // destination, mirroring the frozen hire tables: the weapon at
    // Equipment(0) (archers: bow; longsword infantry: longsword), the pavise
    // at Equipment(1) where the role carries one, mail at Equipment(3), and
    // archer arrows at Carry(0). The signing emerald is earned property,
    // never kit: `issued_kit` skips it, so the wage transfer (Carry(0) for
    // every role, Carry(1) for archers whose arrows share Carry(0)) has no
    // issued counterpart. Medics plan the wage only and issue nothing.
    let gear: &[(Destination, &str, Option<&str>, u32)] = match (role, loadout) {
        (Role::Militia, None) => &[
            (
                Destination::Equipment(0),
                "minecraft:paper",
                Some(super::military_catalog::ARMING_SWORD),
                1,
            ),
            (
                Destination::Equipment(3),
                "minecraft:paper",
                Some(super::military_catalog::MAIL_HAUBERK),
                1,
            ),
        ],
        (Role::Infantry, None) => &[
            (
                Destination::Equipment(0),
                "minecraft:paper",
                Some(super::military_catalog::ARMING_SWORD),
                1,
            ),
            (
                Destination::Equipment(1),
                "minecraft:paper",
                Some(super::military_catalog::PAVISE),
                1,
            ),
            (
                Destination::Equipment(3),
                "minecraft:paper",
                Some(super::military_catalog::MAIL_HAUBERK),
                1,
            ),
        ],
        (Role::Infantry, Some(Loadout::Longsword)) => &[
            (
                Destination::Equipment(0),
                "minecraft:paper",
                Some(super::military_catalog::LONGSWORD),
                1,
            ),
            (
                Destination::Equipment(3),
                "minecraft:paper",
                Some(super::military_catalog::MAIL_HAUBERK),
                1,
            ),
        ],
        (Role::Spearman, None) => &[
            (
                Destination::Equipment(0),
                "minecraft:paper",
                Some(super::military_catalog::SPEAR),
                1,
            ),
            (
                Destination::Equipment(3),
                "minecraft:paper",
                Some(super::military_catalog::MAIL_HAUBERK),
                1,
            ),
        ],
        (Role::Archer, None) => &[
            (
                Destination::Equipment(0),
                "minecraft:paper",
                Some(super::military_catalog::WAR_BOW),
                1,
            ),
            (
                Destination::Equipment(3),
                "minecraft:paper",
                Some(super::military_catalog::MAIL_HAUBERK),
                1,
            ),
            (Destination::Carry(0), "minecraft:arrow", None, 32),
        ],
        (Role::Medic, None) => &[],
        _ => return Err("kit readiness rejects an inadmissible role and loadout"),
    };
    if planned.is_empty() || planned.len() > 16 {
        return Err("kit readiness needs a bounded non-empty plan");
    }
    let wage_slot = if role == Role::Archer {
        Destination::Carry(1)
    } else {
        Destination::Carry(0)
    };
    // Distinct gear destinations in the plan must be exactly the role's.
    let mut planned_gear: Vec<Destination> = planned
        .iter()
        .map(|transfer| transfer.destination)
        .filter(|destination| *destination != wage_slot)
        .collect();
    planned_gear.sort_by_key(|destination| match destination {
        Destination::Equipment(slot) => (0u8, *slot),
        Destination::Carry(slot) => (1u8, *slot),
    });
    planned_gear.dedup();
    let mut expected: Vec<Destination> = gear.iter().map(|(slot, _, _, _)| *slot).collect();
    expected.sort_by_key(|destination| match destination {
        Destination::Equipment(slot) => (0u8, *slot),
        Destination::Carry(slot) => (1u8, *slot),
    });
    if planned_gear != expected {
        return Err("issued kit does not match the role kit destinations");
    }
    // Split-stack plans merge into one issued item per destination
    // (`issued_kit` merges same-destination stacks): quantities must sum.
    if issued.items.len() != gear.len() {
        return Err("issued kit carries an unplanned item");
    }
    for (destination, resource, model, count) in gear {
        let Some(item) = issued
            .items
            .iter()
            .find(|item| &item.destination == destination)
        else {
            return Err("issued kit does not cover the planned destination once");
        };
        if item.item != *resource || item.item_model.as_deref() != *model {
            return Err("issued kit substitutes a non-paper model");
        }
        let planned_count: u32 = planned
            .iter()
            .filter(|transfer| &transfer.destination == destination)
            .map(|transfer| transfer.count)
            .sum();
        // The issued quantity must equal the summed planned quantity for
        // the destination AND the role table quantity: split stacks merge,
        // but short issues and over-issues both fail.
        if item.count != planned_count || item.count != *count {
            return Err("issued kit quantity does not match the plan");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::commerce::Employment;
    use super::*;

    fn employment() -> Employment {
        Employment {
            owner: "owner-1".into(),
            worker: "resident-1".into(),
            treasury: "container:minecraft:overworld:0:64:0".into(),
            wage: 3,
            period_ticks: 100,
            next_due: 100,
            budget: 90,
            earned: 0,
            paid: 0,
            active: true,
        }
    }

    fn unit() -> Unit {
        Unit {
            id: "unit-1".into(),
            role: "militia".into(),
            loadout: None,
            resident: "resident-1".into(),
            warehouse: "container:minecraft:overworld:0:64:0".into(),
            employment: "employment-1".into(),
            employment_record: employment(),
            kit_destinations: vec![0, 3],
            morale: OperationalMorale::new(20.0, npcs::BlockPosition { x: 2, y: 64, z: 0 }, None),
            deserted: false,
            desert_event: None,
        }
    }

    fn observe(unit: &mut Unit, losses: usize, flanked: bool) {
        unit.morale.advance(MoraleObservation {
            health: 20.0,
            flanked,
            new_losses: losses,
            officer_present: false,
            officer_lost: false,
            at_rally: false,
        });
    }

    #[test]
    fn wages_accrue_only_from_ticks_and_never_mint() {
        let mut ledger = UpkeepLedger::default();
        ledger.enlist(unit()).unwrap();
        // Replay with the identical record is idempotent.
        ledger.enlist(unit()).unwrap();
        let mut conflicting = unit();
        conflicting.employment_record.wage = 5;
        assert_eq!(
            ledger.enlist(conflicting),
            Err(PayRefusal::ConflictingReplay)
        );
        // No tick reached: nothing accrues, nothing is payable.
        ledger.accrue_all(50).unwrap();
        assert_eq!(
            ledger
                .get("employment-1")
                .unwrap()
                .unpaid_periods()
                .unwrap(),
            0
        );
        // Three periods accrue exactly three wages from the tick, not from
        // service: no tick, no pay.
        ledger.accrue_all(350).unwrap();
        let record = &ledger.get("employment-1").unwrap().employment_record;
        assert_eq!(record.earned, 9);
        assert_eq!(record.next_due, 400);
        // Paying needs a real accepted transfer receipt: the ledger records
        // the payment, it never moves money itself.
        assert_eq!(record.payable().unwrap(), 9);
        ledger
            .get_mut("employment-1")
            .unwrap()
            .employment_record
            .accept_payment(9)
            .unwrap();
        assert_eq!(
            ledger
                .get("employment-1")
                .unwrap()
                .unpaid_periods()
                .unwrap(),
            0
        );
        // Overpaying past earned or budget fails; earned property is kept.
        assert!(ledger
            .get_mut("employment-1")
            .unwrap()
            .employment_record
            .accept_payment(1)
            .is_err());
    }

    #[test]
    fn paid_steady_units_never_desert_but_unpaid_or_routed_ones_do() {
        let mut paid = unit();
        observe(&mut paid, 0, false);
        assert_eq!(paid.check_desertion("desert-1"), Ok(false));
        assert!(!paid.deserted);

        // Three unpaid periods: desertion with a recorded cause.
        let mut unpaid = unit();
        unpaid.employment_record.earned = 9;
        assert_eq!(unpaid.unpaid_periods().unwrap(), 3);
        assert_eq!(unpaid.check_desertion("desert-pay"), Ok(true));
        assert_eq!(unpaid.desert_event.as_deref(), Some("desert-pay"));
        // Desertion records only: earned property stays, accrual stops.
        assert_eq!(unpaid.employment_record.earned, 9);
        assert_eq!(unpaid.check_desertion("desert-again"), Ok(false));

        // Routed morale deserts even a fully paid unit; shaken does not.
        let mut routed = unit();
        observe(&mut routed, 1, false);
        assert_eq!(routed.morale.observed_state().0, MoralePhase::Shaken);
        assert_eq!(routed.check_desertion("desert-early"), Ok(false));
        observe(&mut routed, 1, true);
        observe(&mut routed, 1, false);
        assert!(!routed.morale.may_guard(), "routing blocks guard duty");
        assert_eq!(routed.check_desertion("desert-routed"), Ok(true));
    }

    #[test]
    fn surrender_is_terminal_and_deserts_without_new_losses() {
        let mut unit = unit();
        unit.morale.surrender();
        assert_eq!(unit.morale.observed_state().0, MoralePhase::Surrendered);
        assert_eq!(unit.check_desertion("desert-surrender"), Ok(true));
    }

    #[test]
    fn restart_roundtrips_ledger_and_invalid_contracts_fail() {
        let mut ledger = UpkeepLedger::default();
        ledger.enlist(unit()).unwrap();
        ledger.accrue_all(250).unwrap();
        let text = ledger.encode().unwrap();
        let ledger2 = UpkeepLedger::decode(&text).unwrap();
        assert_eq!(
            ledger2
                .get("employment-1")
                .unwrap()
                .employment_record
                .earned,
            6
        );
        let mut bad = unit();
        bad.role = "warlord".into();
        assert_eq!(ledger.enlist(bad), Err(PayRefusal::InvalidContract));
        let mut free = unit();
        free.employment_record.wage = 0;
        assert_eq!(ledger.enlist(free), Err(PayRefusal::InvalidContract));
        assert!(Unit::decode(&"x".repeat(VALUE_BYTES + 1)).is_err());
    }

    #[test]
    fn saturated_ledger_refuses_enlist_and_accrue_atomically() {
        // Fill the ledger near the byte bound with distinct units.
        let mut ledger = UpkeepLedger::default();
        let mut rank = 0;
        while ledger.encode().unwrap().len() < VALUE_BYTES - 8 {
            let mut next = unit();
            next.id = format!("unit-{rank}");
            next.employment = format!("employment-{rank}");
            next.resident = format!("resident-{rank}");
            if ledger.enlist(next).is_err() {
                break;
            }
            rank += 1;
            assert!(rank < 32, "test ledger must saturate within 32 units");
        }
        assert!(rank > 0, "test ledger admits at least one unit");
        let before = ledger.encode().unwrap();
        // The next distinct unit no longer fits: refused, ledger unchanged,
        // replay still available for the admitted ones.
        let mut extra = unit();
        extra.id = "unit-extra".into();
        extra.employment = "employment-extra".into();
        extra.resident = "resident-extra".into();
        assert_eq!(ledger.enlist(extra), Err(PayRefusal::InvalidContract));
        assert_eq!(ledger.encode().unwrap(), before);
        // Force an arithmetic overflow on every unit with a far tick: the
        // periods multiplication overflows, so accrue fails. Atomicity
        // requires the whole tick to refuse with no unit mutated.
        let before = ledger.encode().unwrap();
        let earned: Vec<u64> = ledger
            .units
            .values()
            .map(|unit| unit.employment_record.earned)
            .collect();
        let dues: Vec<u64> = ledger
            .units
            .values()
            .map(|unit| unit.employment_record.next_due)
            .collect();
        assert_eq!(ledger.accrue_all(u64::MAX), Err(PayRefusal::Overflow));
        assert_eq!(ledger.encode().unwrap(), before);
        assert_eq!(
            ledger
                .units
                .values()
                .map(|unit| unit.employment_record.earned)
                .collect::<Vec<_>>(),
            earned
        );
        assert_eq!(
            ledger
                .units
                .values()
                .map(|unit| unit.employment_record.next_due)
                .collect::<Vec<_>>(),
            dues
        );
    }

    #[test]
    fn accrue_all_is_atomic_when_a_later_unit_overflows() {
        // Two due units; the second overflows its periods multiplication.
        // The first must not accrue: partial ticks never commit.
        let mut ledger = UpkeepLedger::default();
        let mut first = unit();
        first.employment = "employment-first".into();
        first.id = "unit-first".into();
        first.resident = "resident-first".into();
        first.employment_record.next_due = 100;
        ledger.enlist(first).unwrap();
        let mut second = unit();
        second.employment = "employment-second".into();
        second.id = "unit-second".into();
        second.resident = "resident-second".into();
        second.employment_record.next_due = 100;
        second.employment_record.wage = u32::MAX;
        second.employment_record.period_ticks = u64::MAX;
        ledger.enlist(second).unwrap();
        let before = ledger.encode().unwrap();
        assert_eq!(ledger.accrue_all(u64::MAX), Err(PayRefusal::Overflow));
        assert_eq!(ledger.encode().unwrap(), before);
        assert_eq!(
            ledger
                .get("employment-first")
                .unwrap()
                .employment_record
                .earned,
            0
        );
    }

    #[test]
    fn kit_never_substitutes_vanilla_models() {
        use super::super::hire::IssuedItem;
        use super::super::military_catalog;
        let kit = super::super::hire::IssuedKit {
            warehouse: "container:minecraft:overworld:0:64:0".into(),
            items: vec![
                IssuedItem {
                    item: "minecraft:paper".into(),
                    count: 1,
                    damage: None,
                    enchantments: vec![],
                    custom_name: None,
                    item_model: Some(military_catalog::ARMING_SWORD.into()),
                    max_damage: Some(320),
                    custom_data: None,
                    stew_effects: vec![],
                    destination: Destination::Equipment(0),
                },
                IssuedItem {
                    item: "minecraft:paper".into(),
                    count: 1,
                    damage: None,
                    enchantments: vec![],
                    custom_name: None,
                    item_model: Some(military_catalog::MAIL_HAUBERK.into()),
                    max_damage: None,
                    custom_data: None,
                    stew_effects: vec![],
                    destination: Destination::Equipment(3),
                },
            ],
        };
        // The plan always carries the signing wage (Carry(0)) alongside gear;
        // `issued_kit` skips emerald, so readiness compares gear only.
        let plan = vec![
            super::super::hire::Transfer {
                source_slot: 0,
                destination: Destination::Equipment(0),
                count: 1,
            },
            super::super::hire::Transfer {
                source_slot: 1,
                destination: Destination::Equipment(3),
                count: 1,
            },
            super::super::hire::Transfer {
                source_slot: 5,
                destination: Destination::Carry(0),
                count: 1,
            },
        ];
        let mut vanilla = kit.clone();
        vanilla.items[0].item = "minecraft:iron_sword".into();
        vanilla.items[0].item_model = None;
        assert!(check_kit_readiness(Role::Militia, None, &plan, &vanilla).is_err());
        // Wrong role model refused: a spear is not militia kit.
        let mut spear = kit.clone();
        spear.items[0].item_model = Some(military_catalog::SPEAR.into());
        assert!(check_kit_readiness(Role::Militia, None, &plan, &spear).is_err());
        // The same steel is a spearman weapon only with a spearman plan.
        let spear_plan = vec![
            super::super::hire::Transfer {
                source_slot: 0,
                destination: Destination::Equipment(0),
                count: 1,
            },
            super::super::hire::Transfer {
                source_slot: 1,
                destination: Destination::Equipment(3),
                count: 1,
            },
            super::super::hire::Transfer {
                source_slot: 5,
                destination: Destination::Carry(0),
                count: 1,
            },
        ];
        assert!(check_kit_readiness(Role::Spearman, None, &spear_plan, &spear).is_ok());
        // Unplanned extra item refused even with a valid model.
        let mut extra = kit.clone();
        extra.items.push(IssuedItem {
            item: "minecraft:paper".into(),
            count: 1,
            damage: None,
            enchantments: vec![],
            custom_name: None,
            item_model: Some(military_catalog::MAIL_HAUBERK.into()),
            max_damage: None,
            custom_data: None,
            stew_effects: vec![],
            destination: Destination::Equipment(3),
        });
        assert!(check_kit_readiness(Role::Militia, None, &plan, &extra).is_err());
        // Medics plan the signing wage only, and `issued_kit` skips emerald:
        // the frozen hire outputs for a medic are a wage-only plan with an
        // empty issue, and that combination is ready for medics only.
        let empty = super::super::hire::IssuedKit {
            warehouse: "container:minecraft:overworld:0:64:0".into(),
            items: vec![],
        };
        let wage_plan = vec![super::super::hire::Transfer {
            source_slot: 2,
            destination: Destination::Carry(0),
            count: 1,
        }];
        assert!(check_kit_readiness(Role::Medic, None, &wage_plan, &empty).is_ok());
        assert!(check_kit_readiness(Role::Militia, None, &wage_plan, &empty).is_err());
        // Archers plan arrows at Carry(0) plus the wage at Carry(1): arrows
        // are issued gear, the wage is skipped like everywhere else.
        let archer_kit = super::super::hire::IssuedKit {
            warehouse: "container:minecraft:overworld:0:64:0".into(),
            items: vec![
                IssuedItem {
                    item: "minecraft:paper".into(),
                    count: 1,
                    damage: None,
                    enchantments: vec![],
                    custom_name: None,
                    item_model: Some(military_catalog::WAR_BOW.into()),
                    max_damage: None,
                    custom_data: None,
                    stew_effects: vec![],
                    destination: Destination::Equipment(0),
                },
                IssuedItem {
                    item: "minecraft:arrow".into(),
                    count: 32,
                    damage: None,
                    enchantments: vec![],
                    custom_name: None,
                    item_model: None,
                    max_damage: None,
                    custom_data: None,
                    stew_effects: vec![],
                    destination: Destination::Carry(0),
                },
                IssuedItem {
                    item: "minecraft:paper".into(),
                    count: 1,
                    damage: None,
                    enchantments: vec![],
                    custom_name: None,
                    item_model: Some(military_catalog::MAIL_HAUBERK.into()),
                    max_damage: None,
                    custom_data: None,
                    stew_effects: vec![],
                    destination: Destination::Equipment(3),
                },
            ],
        };
        let archer_plan = vec![
            super::super::hire::Transfer {
                source_slot: 0,
                destination: Destination::Equipment(0),
                count: 1,
            },
            super::super::hire::Transfer {
                source_slot: 2,
                destination: Destination::Carry(0),
                count: 32,
            },
            super::super::hire::Transfer {
                source_slot: 3,
                destination: Destination::Equipment(3),
                count: 1,
            },
            super::super::hire::Transfer {
                source_slot: 4,
                destination: Destination::Carry(1),
                count: 1,
            },
        ];
        assert!(check_kit_readiness(Role::Archer, None, &archer_plan, &archer_kit).is_ok());
        // A substituted wage in place of arrows is refused.
        let mut no_arrows = archer_kit.clone();
        no_arrows.items[1].item = "minecraft:emerald".into();
        assert!(check_kit_readiness(Role::Archer, None, &archer_plan, &no_arrows).is_err());
    }
}
