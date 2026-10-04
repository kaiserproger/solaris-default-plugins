//! CP-066 artillery service behind the slice-A/B envelope (RULESET DECISION only).
//!
//! One admitted chain: crew present, cargo available, gun reloaded, aim inside
//! the proposal sector, live war admission (`war_policy::authorize`), then
//! breach admission (`siege_policy`) against an open siege of the same war and
//! target. Salvage needs the recorded breach on top of the same live
//! admission; victory alone never opens it.
//!
//! This module owns no war/siege records: it only reads the frozen
//! `war_policy`/`siege_policy` predicates through their public seam and never
//! issues a physical effect. Physical damage execution (SIEGE-07 blocks/LOS,
//! REC-10 salvage state) stays with the native gate and is explicitly
//! unproven here. Storage keys are disjoint from the war envelope:
//! `settlement-artillery-v1:{gun}` and `...:{gun}/shots`.
//!
//! Numbers below (3 crew, ±20°/0–15° sector, 5% misfire, 1 charge + 1 ball
//! per shot) are INHERITED, NOT SOURCED: they mirror the pre-existing
//! sibling `artillery.rs` proposal values (CREW_REQUIRED, YAW_LIMIT_DEGREES,
//! PITCH_MIN/MAX_DEGREES, MISFIRE_PER_100, check_shot_cargo 1+1) so the two
//! modules agree. HONESTY GAP: no CP-066/067/069 owner-decision section
//! exists in either repo's OWNER_DECISIONS (checked
//! docs/settlements/OWNER_DECISIONS.md and IMPLEMENTATION_SPEC GUN-01–10,
//! which name crew/cargo/sector/misfire gates but no numeric values), so
//! these numbers are UNVERIFIED PROPOSALS, not admitted balance. Firing
//! additionally requires a deployed gun (GUN-02/09) in this module; sibling
//! `artillery.rs` custody allows warehouse fire and is the looser one.

use super::siege_policy::{authorize_breach, authorize_salvage, SiegeRecord};
use super::war_policy::{
    authorize, EffectFacts, JointOutcome, Outcome, Policy, PoliticalFacts, Refusal,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(super) const VALUE_BYTES: usize = 4096;
/// Three distinct crew handles serve one gun; a short crew stops the shot.
/// Inherited proposal value, unverified (see module header).
pub(super) const CREW_REQUIRED: usize = 3;
/// Proposal firing sector (gameplay units, absolute degrees). Inherited,
/// unverified (see module header).
pub(super) const YAW_LIMIT_DEGREES: f64 = 20.0;
pub(super) const PITCH_MIN_DEGREES: f64 = 0.0;
pub(super) const PITCH_MAX_DEGREES: f64 = 15.0;
/// Proposal misfire rate: 5 of every 100 seeded decisions misfire. Seeded by
/// the saved operation id, so a misfire can never be re-rolled into a free
/// shot (GUN-07). Both outcomes consume the same cargo (REC-09). Inherited,
/// unverified (see module header).
pub(super) const MISFIRE_PER_100: u64 = 5;

fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.'))
}

pub(super) fn key(gun: &str) -> Result<String, Refusal> {
    if !id(gun) {
        return Err(Refusal::InvalidRecord);
    }
    Ok(format!("settlement-artillery-v1:{gun}"))
}

pub(super) fn ledger_key(gun: &str) -> Result<String, Refusal> {
    if !id(gun) {
        return Err(Refusal::InvalidRecord);
    }
    Ok(format!("settlement-artillery-v1:{gun}/shots"))
}

/// One gun has exactly one custody at a time: transport and deploy move it,
/// never duplicate it (GUN-02). Capture changes the controlling owner; it
/// never mints a second gun for the winner (GUN-09).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum GunLocation {
    Warehouse(String),
    Deployed { x: i32, y: i32, z: i32 },
    Captured { by: String },
}

/// Durable gun state: cargo, reload and custody. One authored charge moves into
/// the gun on reload; one ball is consumed by every decided shot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Gun {
    pub id: String,
    pub crew: Vec<String>,
    pub charges: u32,
    pub balls: u32,
    pub loaded: bool,
    pub location: GunLocation,
}

impl Gun {
    pub(super) fn validate(&self) -> Result<(), Refusal> {
        if !id(&self.id) {
            return Err(Refusal::InvalidRecord);
        }
        match &self.location {
            GunLocation::Warehouse(handle) => {
                if !id(handle) {
                    return Err(Refusal::InvalidRecord);
                }
            }
            GunLocation::Deployed { x, y, z } => {
                if [*x, *y, *z]
                    .iter()
                    .any(|axis| axis.unsigned_abs() > 30_000_000)
                    || !(-64..=320).contains(y)
                {
                    return Err(Refusal::InvalidRecord);
                }
            }
            GunLocation::Captured { by } => {
                if !id(by) {
                    return Err(Refusal::InvalidRecord);
                }
            }
        }
        Ok(())
    }

    pub(super) fn encode(&self) -> Result<String, Refusal> {
        self.validate()?;
        let value = serde_json::to_string(self).map_err(|_| Refusal::InvalidRecord)?;
        if value.len() > VALUE_BYTES {
            return Err(Refusal::InvalidRecord);
        }
        Ok(value)
    }

    pub(super) fn decode(value: &str) -> Result<Self, Refusal> {
        if value.len() > VALUE_BYTES {
            return Err(Refusal::InvalidRecord);
        }
        let gun: Self = serde_json::from_str(value).map_err(|_| Refusal::InvalidRecord)?;
        gun.validate()?;
        Ok(gun)
    }

    /// Deploy moves the single gun from its warehouse; it is never copied.
    pub(super) fn deploy(&mut self, x: i32, y: i32, z: i32) -> Result<(), Refusal> {
        self.validate()?;
        if !matches!(self.location, GunLocation::Warehouse(_)) {
            return Err(Refusal::InvalidRecord);
        }
        if [x, y, z]
            .iter()
            .any(|axis| axis.unsigned_abs() > 30_000_000)
            || !(-64..=320).contains(&y)
        {
            return Err(Refusal::InvalidRecord);
        }
        self.location = GunLocation::Deployed { x, y, z };
        Ok(())
    }

    pub(super) fn capture(&mut self, by: &str) -> Result<(), Refusal> {
        self.validate()?;
        if !id(by) {
            return Err(Refusal::InvalidRecord);
        }
        self.location = GunLocation::Captured { by: by.into() };
        Ok(())
    }

    /// Move one authored charge into the gun. Refuses without a charge and
    /// never double-loads.
    pub(super) fn reload(&mut self) -> Result<(), Refusal> {
        self.validate()?;
        if self.loaded {
            return Err(Refusal::InvalidRecord);
        }
        if self.charges == 0 {
            return Err(Refusal::InvalidRecord);
        }
        self.charges -= 1;
        self.loaded = true;
        Ok(())
    }
}

/// Three distinct crew handles serve one gun; a short crew stops the shot with
/// a war-envelope refusal instead of firing shorthanded.
pub(super) fn check_crew(crew: &[String]) -> Result<(), Refusal> {
    let mut distinct: Vec<&str> = Vec::new();
    for handle in crew {
        if !id(handle) {
            return Err(Refusal::UnknownActor);
        }
        if !distinct.contains(&handle.as_str()) {
            distinct.push(handle.as_str());
        }
    }
    if distinct.len() < CREW_REQUIRED {
        return Err(Refusal::UnknownActor);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Aim {
    pub yaw_degrees: f64,
    pub pitch_degrees: f64,
}

/// A rejected aim never authorizes a shot outside the sector (GUN-05).
pub(super) fn check_aim(aim: Aim) -> Result<(), Refusal> {
    if !aim.yaw_degrees.is_finite() || !aim.pitch_degrees.is_finite() {
        return Err(Refusal::ForbiddenTarget);
    }
    if aim.yaw_degrees.abs() > YAW_LIMIT_DEGREES
        || aim.pitch_degrees < PITCH_MIN_DEGREES
        || aim.pitch_degrees > PITCH_MAX_DEGREES
    {
        return Err(Refusal::ForbiddenTarget);
    }
    Ok(())
}

/// Deterministic misfire roll from the saved operation id: retry/restart
/// cannot re-roll it into a free shot.
pub(super) fn misfire_roll(operation: &str) -> bool {
    let seed = operation.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    seed % 100 < MISFIRE_PER_100
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ShotOutcome {
    Fired,
    Misfire,
}

/// Durable shot decision (REC-09): the first decision for an operation id is
/// recorded with its full intent; a duplicate delivery replays it without a
/// second consumption, and a conflicting replay of the same id is refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ShotRecord {
    pub operation: String,
    pub gun: String,
    pub target: String,
    pub tick: u64,
    pub outcome: ShotOutcome,
}

#[derive(Default)]
pub(super) struct ShotLedger {
    shots: BTreeMap<String, ShotRecord>,
}

impl ShotLedger {
    pub(super) fn decide(&mut self, record: ShotRecord) -> Result<(ShotOutcome, bool), Refusal> {
        if !id(&record.operation) || !id(&record.gun) || !id(&record.target) {
            return Err(Refusal::InvalidRecord);
        }
        if let Some(previous) = self.shots.get(&record.operation) {
            if previous == &record {
                return Ok((previous.outcome, true));
            }
            return Err(Refusal::InvalidRecord);
        }
        // REC-09 durability is byte-bounded: the journal must still fit
        // VALUE_BYTES with the new record. The future encoding is checked
        // before any mutation, so a saturated journal refuses without
        // recording and the caller consumes no cargo.
        let mut future = self.shots.clone();
        future.insert(record.operation.clone(), record.clone());
        let records: Vec<&ShotRecord> = future.values().collect();
        let value = serde_json::to_string(&records).map_err(|_| Refusal::InvalidRecord)?;
        if value.len() > VALUE_BYTES {
            return Err(Refusal::InvalidRecord);
        }
        self.shots.insert(record.operation.clone(), record.clone());
        Ok((record.outcome, false))
    }

    pub(super) fn decided(&self, operation: &str) -> Option<&ShotRecord> {
        self.shots.get(operation)
    }

    pub(super) fn encode(&self) -> Result<String, Refusal> {
        let records: Vec<&ShotRecord> = self.shots.values().collect();
        let value = serde_json::to_string(&records).map_err(|_| Refusal::InvalidRecord)?;
        if value.len() > VALUE_BYTES {
            return Err(Refusal::InvalidRecord);
        }
        Ok(value)
    }

    pub(super) fn decode(value: &str) -> Result<Self, Refusal> {
        if value.len() > VALUE_BYTES {
            return Err(Refusal::InvalidRecord);
        }
        let records: Vec<ShotRecord> =
            serde_json::from_str(value).map_err(|_| Refusal::InvalidRecord)?;
        let mut ledger = Self::default();
        for record in records {
            if !id(&record.operation) || !id(&record.gun) || !id(&record.target) {
                return Err(Refusal::InvalidRecord);
            }
            if ledger
                .shots
                .insert(record.operation.clone(), record)
                .is_some()
            {
                return Err(Refusal::InvalidRecord);
            }
        }
        Ok(ledger)
    }
}

/// One admitted firing: crew, reload, ball, aim, live war admission plus breach
/// admission against the open siege, then the durable decision. A misfire
/// consumes the same cargo and records the same decision but admits no breach:
/// there is no projectile, so there is no damage effect to authorize.
#[allow(clippy::too_many_arguments)]
pub(super) fn decide_fire(
    gun: &mut Gun,
    ledger: &mut ShotLedger,
    operation: &str,
    aim: Aim,
    tick: u64,
    policy: &Policy,
    politics: &PoliticalFacts,
    effect: &EffectFacts,
    siege: &SiegeRecord,
) -> Result<(ShotRecord, bool), Refusal> {
    gun.validate()?;
    if !id(operation) {
        return Err(Refusal::InvalidRecord);
    }
    if effect.target != siege.target || siege.war_id != policy.id {
        return Err(Refusal::ForbiddenTarget);
    }
    // Duplicate delivery replays the recorded decision: no second consumption,
    // no re-admission, no crew/cargo re-check. A same-id record with a
    // different gun/target/tick is a conflicting replay and is refused.
    if let Some(previous) = ledger.decided(operation) {
        if previous.gun != gun.id || previous.target != effect.target || previous.tick != tick {
            return Err(Refusal::InvalidRecord);
        }
        return Ok((previous.clone(), true));
    }
    // Fresh shots check predicates, then authorize, then record: a refused war
    // admits nothing, burns no operation id, and keeps the loaded charge.
    // Only a deployed gun fires (GUN-02/09): a warehouse or captured gun
    // reloads but never shoots without deploy.
    if !matches!(gun.location, GunLocation::Deployed { .. }) {
        return Err(Refusal::InvalidRecord);
    }
    check_crew(&gun.crew)?;
    if !gun.loaded {
        return Err(Refusal::InvalidRecord);
    }
    if gun.balls == 0 {
        return Err(Refusal::InvalidRecord);
    }
    check_aim(aim)?;
    authorize(policy, politics, effect)?;
    // The seeded outcome is fixed before admission, so restart cannot re-roll.
    let outcome = if misfire_roll(operation) {
        ShotOutcome::Misfire
    } else {
        ShotOutcome::Fired
    };
    if outcome == ShotOutcome::Fired {
        authorize_breach(policy, politics, effect, siege)?;
    }
    let record = ShotRecord {
        operation: operation.into(),
        gun: gun.id.clone(),
        target: effect.target.clone(),
        tick,
        outcome,
    };
    let (stored, replay) = ledger.decide(record)?;
    debug_assert!(!replay);
    debug_assert_eq!(stored, outcome);
    gun.balls -= 1;
    gun.loaded = false;
    let record = ledger
        .decided(operation)
        .cloned()
        .ok_or(Refusal::InvalidRecord)?;
    Ok((record, false))
}

/// Admit one salvage effect after the breach is recorded. Victory alone never
/// opens salvage: the same live-war admission plus the breached siege apply.
pub(super) fn admit_salvage(
    policy: &Policy,
    politics: &PoliticalFacts,
    effect: &EffectFacts,
    siege: &SiegeRecord,
) -> Result<(), Refusal> {
    authorize_salvage(policy, politics, effect, siege)?;
    Ok(())
}

/// Recorded joint outcome for CP-063: a committed peace/surrender plus the
/// terminal siege record. Window expiry, a missing agreement, or a siege that
/// closed before the agreement yield no winner.
pub(super) fn campaign_outcome(
    policy: &Policy,
    siege_closed: Option<(&str, u64)>,
) -> Option<JointOutcome> {
    super::war_policy::joint_outcome(policy, siege_closed)
}

/// Automatic war contribution is decided zero: holding, outlasting, or winning
/// never debits anyone. Voluntary payments move only through real inventory
/// transfers outside this module.
pub(super) fn campaign_contribution(policy: &Policy, politics: &PoliticalFacts) -> u64 {
    super::war_policy::auto_contribution(policy, politics)
}

/// Dismantle returns the listed surviving components only: strictly less metal
/// than the build cost, no labor refund, never a whole second gun (GUN-09).
/// Proposal values.
pub(super) fn dismantle_returns() -> Vec<(&'static str, u32)> {
    vec![("minecraft:iron_ingot", 2), ("minecraft:oak_planks", 2)]
}

pub(super) fn dismantle_conserves(build_iron_ingots: u32) -> bool {
    let returned: u32 = dismantle_returns()
        .iter()
        .filter(|(item, _)| *item == "minecraft:iron_ingot")
        .map(|(_, count)| *count)
        .sum();
    returned < build_iron_ingots
        && !dismantle_returns()
            .iter()
            .any(|(item, _)| *item == "solaris-settlements:field_gun")
}

#[allow(dead_code)]
fn outcome_is_peace(outcome: Outcome) -> bool {
    matches!(outcome, Outcome::Peace)
}

#[cfg(test)]
mod tests {
    use super::super::war_policy::{
        key as war_key, Delegation, OfflineProtection, Protection, Side, Source, State, Target,
        Version, Window, GRACE_TICKS,
    };
    use super::*;

    fn sides() -> (Side, Side) {
        (
            Side {
                house: "house-a".into(),
                realm: Some("realm-a".into()),
            },
            Side {
                house: "house-b".into(),
                realm: Some("realm-b".into()),
            },
        )
    }

    fn fixtures() -> (Policy, PoliticalFacts, EffectFacts, SiegeRecord) {
        let (attacker, defender) = sides();
        let declared_at = 1_000_000;
        let grace_end = declared_at + GRACE_TICKS;
        let policy = Policy {
            id: "war-a-b".into(),
            revision: 1,
            ruleset: "campaign-war-r1".into(),
            attacker: attacker.clone(),
            defender: defender.clone(),
            goal: "recover-control".into(),
            declarer: "holder-a".into(),
            cause: "delivered-cause-1".into(),
            declaration_event: "declaration-1".into(),
            declared_at,
            grace_end,
            windows: vec![Window {
                start: grace_end,
                end: grace_end + 10_000,
            }],
            offline: OfflineProtection::Protected,
            state: State::Declared,
            targets: vec![Target {
                id: "known-gate".into(),
                owner: defender.clone(),
                provenance: "delivered-scout-report".into(),
                observed_at: declared_at - 5,
                delivered_at: declared_at - 2,
                lawful_basis: "recover-control".into(),
            }],
        };
        let politics = PoliticalFacts {
            revision: 7,
            storage: vec![Version {
                key: "political-v1/title/original".into(),
                version: 37,
            }],
            actor: "original-principal".into(),
            actor_side: Some(attacker),
            target_side: Some(defender),
            holder: Some("holder-a".into()),
            war_id: Some(policy.id.clone()),
            lawful_goal: Some(policy.goal.clone()),
            delegation: Some(Delegation {
                principal: "original-principal".into(),
                war_id: policy.id.clone(),
                revision: 7,
                revoked: false,
            }),
        };
        let effect = EffectFacts {
            event: "original-native-effect".into(),
            actor: politics.actor.clone(),
            target: "known-gate".into(),
            tick: grace_end + 5,
            source: Source::NativeWork,
            protection: Protection::Unprotected,
            policy: Version {
                key: war_key(&policy.id).expect("fixture war id valid"),
                version: 31,
            },
            admitted_revision: 1,
            admitted_politics_revision: 7,
        };
        let siege = SiegeRecord::open(&policy, "holder-a", "known-gate", "siege-1", grace_end + 1)
            .expect("fixture siege opens");
        (policy, politics, effect, siege)
    }

    fn fixture_gun() -> Gun {
        Gun {
            id: "gun-1".into(),
            crew: vec!["crew-a".into(), "crew-b".into(), "crew-c".into()],
            charges: 2,
            balls: 2,
            loaded: false,
            location: GunLocation::Warehouse("warehouse-1".into()),
        }
    }

    fn aim() -> Aim {
        Aim {
            yaw_degrees: 5.0,
            pitch_degrees: 8.0,
        }
    }

    fn firing_operation() -> String {
        (0..1000)
            .map(|candidate| format!("shot-{candidate}"))
            .find(|id| !misfire_roll(id))
            .expect("a firing operation id exists")
    }

    fn misfiring_operation() -> String {
        (0..1000)
            .map(|candidate| format!("shot-{candidate}"))
            .find(|id| misfire_roll(id))
            .expect("a misfiring operation id exists")
    }

    #[test]
    fn full_chain_fires_then_salvages_after_breach() {
        let (policy, politics, effect, mut siege) = fixtures();
        let mut gun = fixture_gun();
        gun.deploy(10, 64, -4).unwrap();
        gun.reload().unwrap();
        let mut ledger = ShotLedger::default();
        let operation = firing_operation();
        let (record, replay) = decide_fire(
            &mut gun,
            &mut ledger,
            &operation,
            aim(),
            effect.tick,
            &policy,
            &politics,
            &effect,
            &siege,
        )
        .expect("admitted chain fires");
        assert!(!replay);
        assert_eq!(record.outcome, ShotOutcome::Fired);
        assert_eq!(gun.charges, 1, "reload consumed exactly one charge");
        assert_eq!(gun.balls, 1, "the shot consumed exactly one ball");
        assert!(!gun.loaded, "the gun is unloaded after the shot");
        // Duplicate delivery replays without a second consumption.
        let (again, replay) = decide_fire(
            &mut gun,
            &mut ledger,
            &operation,
            aim(),
            effect.tick,
            &policy,
            &politics,
            &effect,
            &siege,
        )
        .expect("duplicate delivery replays");
        assert!(replay);
        assert_eq!(again.outcome, ShotOutcome::Fired);
        assert_eq!((gun.charges, gun.balls), (1, 1));
        // Salvage is refused before the breach is recorded ...
        assert_eq!(
            admit_salvage(&policy, &politics, &effect, &siege),
            Err(Refusal::SiegeNotOpen)
        );
        siege
            .close_breached("holder-a", &policy, "breach-1", effect.tick + 1)
            .unwrap();
        admit_salvage(&policy, &politics, &effect, &siege).expect("breach opens salvage");
    }

    #[test]
    fn misfire_consumes_cargo_but_admits_no_breach() {
        let (policy, politics, effect, siege) = fixtures();
        let mut gun = fixture_gun();
        gun.deploy(10, 64, -4).unwrap();
        gun.reload().unwrap();
        let mut ledger = ShotLedger::default();
        let operation = misfiring_operation();
        let (record, _) = decide_fire(
            &mut gun,
            &mut ledger,
            &operation,
            aim(),
            effect.tick,
            &policy,
            &politics,
            &effect,
            &siege,
        )
        .expect("misfire still decides");
        assert_eq!(record.outcome, ShotOutcome::Misfire);
        assert_eq!((gun.charges, gun.balls), (1, 1));
        assert!(!gun.loaded);
        // The misfire record replays deterministically: restart cannot re-roll.
        let decoded = ShotLedger::decode(&ledger.encode().unwrap()).unwrap();
        assert_eq!(
            decoded.decided(&operation).unwrap().outcome,
            ShotOutcome::Misfire
        );
    }

    #[test]
    fn crew_cargo_reload_and_aim_refusals_fire_nothing() {
        let (policy, politics, effect, siege) = fixtures();
        let mut ledger = ShotLedger::default();
        // Short crew.
        let mut gun = fixture_gun();
        gun.crew.pop();
        gun.deploy(10, 64, -4).unwrap();
        gun.reload().unwrap();
        assert_eq!(
            decide_fire(
                &mut gun,
                &mut ledger,
                "shot-crew",
                aim(),
                effect.tick,
                &policy,
                &politics,
                &effect,
                &siege
            ),
            Err(Refusal::UnknownActor)
        );
        // Loaded but never deployed: warehouse guns never fire (GUN-02/09).
        let mut gun = fixture_gun();
        gun.reload().unwrap();
        assert_eq!(
            decide_fire(
                &mut gun,
                &mut ledger,
                "shot-warehouse",
                aim(),
                effect.tick,
                &policy,
                &politics,
                &effect,
                &siege
            ),
            Err(Refusal::InvalidRecord)
        );
        assert!(ledger.decided("shot-warehouse").is_none());
        assert!(gun.loaded, "undeployed guns keep their loaded charge");
        // Captured guns never fire for the new owner without a fresh deploy.
        let mut gun = fixture_gun();
        gun.deploy(10, 64, -4).unwrap();
        gun.capture("house-b").unwrap();
        gun.reload().unwrap();
        assert_eq!(
            decide_fire(
                &mut gun,
                &mut ledger,
                "shot-captured",
                aim(),
                effect.tick,
                &policy,
                &politics,
                &effect,
                &siege
            ),
            Err(Refusal::InvalidRecord)
        );
        assert!(ledger.decided("shot-captured").is_none());
        // No reload (a firing-seeded id, so only the reload refusal shows).
        let mut gun = fixture_gun();
        gun.deploy(10, 64, -4).unwrap();
        assert_eq!(
            decide_fire(
                &mut gun,
                &mut ledger,
                "shot-0",
                aim(),
                effect.tick,
                &policy,
                &politics,
                &effect,
                &siege
            ),
            Err(Refusal::InvalidRecord)
        );
        assert!(ledger.decided("shot-0").is_none());
        // Empty ball cargo.
        let mut gun = fixture_gun();
        gun.deploy(10, 64, -4).unwrap();
        gun.reload().unwrap();
        gun.balls = 0;
        assert_eq!(
            decide_fire(
                &mut gun,
                &mut ledger,
                "shot-ball",
                aim(),
                effect.tick,
                &policy,
                &politics,
                &effect,
                &siege
            ),
            Err(Refusal::InvalidRecord)
        );
        // Aim outside the sector.
        let mut gun = fixture_gun();
        gun.deploy(10, 64, -4).unwrap();
        gun.reload().unwrap();
        assert_eq!(
            decide_fire(
                &mut gun,
                &mut ledger,
                "shot-aim",
                Aim {
                    yaw_degrees: 45.0,
                    pitch_degrees: 8.0,
                },
                effect.tick,
                &policy,
                &politics,
                &effect,
                &siege
            ),
            Err(Refusal::ForbiddenTarget)
        );
        // Reload without a charge refuses before any state changes.
        let mut gun = fixture_gun();
        gun.charges = 0;
        assert!(gun.reload().is_err());
        assert!(!gun.loaded && gun.charges == 0);
    }

    #[test]
    fn revoked_war_and_closed_window_admit_nothing_and_burn_no_id() {
        let (mut policy, politics, effect, siege) = fixtures();
        let mut gun = fixture_gun();
        gun.deploy(10, 64, -4).unwrap();
        gun.reload().unwrap();
        let mut ledger = ShotLedger::default();
        policy
            .revoke("holder-a", "revoke-1", policy.declared_at + 1)
            .unwrap();
        assert_eq!(
            decide_fire(
                &mut gun,
                &mut ledger,
                "shot-revoked",
                aim(),
                effect.tick,
                &policy,
                &politics,
                &effect,
                &siege
            ),
            Err(Refusal::Revoked)
        );
        assert!(ledger.decided("shot-revoked").is_none());
        assert_eq!((gun.charges, gun.balls), (1, 2));
        assert!(gun.loaded, "refused shots keep the loaded charge");

        let (policy, politics, mut effect, siege) = fixtures();
        effect.tick = policy.grace_end + 50_000;
        let mut gun = fixture_gun();
        gun.deploy(10, 64, -4).unwrap();
        gun.reload().unwrap();
        assert_eq!(
            decide_fire(
                &mut gun,
                &mut ledger,
                "shot-window",
                aim(),
                effect.tick,
                &policy,
                &politics,
                &effect,
                &siege
            ),
            Err(Refusal::WindowClosed)
        );
        assert!(ledger.decided("shot-window").is_none());
    }

    #[test]
    fn conflicting_replay_and_restart_roundtrip() {
        let (policy, politics, effect, siege) = fixtures();
        let mut gun = fixture_gun();
        gun.deploy(10, 64, -4).unwrap();
        gun.reload().unwrap();
        let mut ledger = ShotLedger::default();
        let (first, _) = decide_fire(
            &mut gun,
            &mut ledger,
            "shot-restart",
            aim(),
            effect.tick,
            &policy,
            &politics,
            &effect,
            &siege,
        )
        .expect("first decision records");
        // Same id, different target: conflicting replay refused outright.
        let mut other = effect.clone();
        other.target = "other-gate".into();
        assert_eq!(
            decide_fire(
                &mut gun,
                &mut ledger,
                "shot-restart",
                aim(),
                effect.tick,
                &policy,
                &politics,
                &other,
                &siege
            ),
            Err(Refusal::ForbiddenTarget)
        );
        // Restart replays the ledger and the gun exactly.
        let gun_text = gun.encode().unwrap();
        let ledger_text = ledger.encode().unwrap();
        let gun2 = Gun::decode(&gun_text).unwrap();
        let ledger2 = ShotLedger::decode(&ledger_text).unwrap();
        assert_eq!(gun2, gun);
        assert_eq!(ledger2.decided("shot-restart").unwrap(), &first);
        // Deploy moves the single gun; capture changes the owner only.
        let mut fresh = fixture_gun();
        fresh.deploy(3, 70, 3).unwrap();
        fresh.capture("house-b").unwrap();
        assert!(matches!(fresh.location, GunLocation::Captured { .. }));
        assert!(dismantle_conserves(4));
        assert!(!dismantle_returns()
            .iter()
            .any(|(item, _)| *item == "solaris-settlements:field_gun"));
    }
    #[test]
    fn saturated_journal_refuses_new_shots_without_consuming_cargo() {
        let (policy, politics, effect, siege) = fixtures();
        let mut gun = fixture_gun();
        gun.deploy(10, 64, -4).unwrap();
        gun.reload().unwrap();
        let mut ledger = ShotLedger::default();
        // Fill the journal near the byte bound through the public decide
        // path, so every admitted record carries a real war admission.
        let mut rank = 0;
        while ledger.encode().unwrap().len() < VALUE_BYTES - 60 {
            let id = format!("fill-{rank}");
            let record = ShotRecord {
                operation: id,
                gun: gun.id.clone(),
                target: effect.target.clone(),
                tick: effect.tick,
                outcome: ShotOutcome::Fired,
            };
            ledger.decide(record).unwrap();
            rank += 1;
            assert!(rank < 64, "test journal must saturate within 64 shots");
        }
        let balls = gun.balls;
        let loaded = gun.loaded;
        let journal = ledger.encode().unwrap();
        // The next admitted shot no longer fits: a long operation id makes
        // the future journal encoding exceed VALUE_BYTES, so the shot is
        // refused, both structures unchanged, recorded decisions replay.
        // ("shot-overflow" is firing-seeded; the long id shares its prefix.)
        assert!(!misfire_roll("shot-overflow"));
        let overflow = format!("shot-overflow-{}", "o".repeat(48));
        assert_eq!(
            decide_fire(
                &mut gun,
                &mut ledger,
                &overflow,
                aim(),
                effect.tick,
                &policy,
                &politics,
                &effect,
                &siege
            ),
            Err(Refusal::InvalidRecord)
        );
        assert!(ledger.decided(&overflow).is_none());
        assert_eq!((gun.balls, gun.loaded), (balls, loaded));
        assert_eq!(ledger.encode().unwrap(), journal);
        let (again, replay) = decide_fire(
            &mut gun,
            &mut ledger,
            "fill-0",
            aim(),
            effect.tick,
            &policy,
            &politics,
            &effect,
            &siege,
        )
        .expect("recorded decisions still replay on a saturated journal");
        assert!(replay);
        assert_eq!(again.outcome, ShotOutcome::Fired);
        assert_eq!(ledger.encode().unwrap(), journal);
    }

    #[test]
    fn joint_outcome_needs_peace_and_terminal_siege() {
        let (mut policy, politics, _, _) = fixtures();
        assert!(campaign_outcome(&policy, Some(("breach-1", policy.grace_end + 9))).is_none());
        policy
            .end("holder-a", "peace-1", policy.grace_end + 8, Outcome::Peace)
            .unwrap();
        // A siege that closed before the agreement cannot back it.
        assert!(campaign_outcome(&policy, Some(("breach-1", policy.grace_end + 7))).is_none());
        let joint = campaign_outcome(&policy, Some(("breach-1", policy.grace_end + 9)))
            .expect("peace plus terminal siege is a joint outcome");
        assert_eq!(joint.siege_event, "breach-1");
        assert_eq!(campaign_contribution(&policy, &politics), 0);
    }
}
