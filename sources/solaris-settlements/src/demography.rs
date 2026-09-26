//! Durable household records, per-resident social projections, and the
//! demographic intents a later slice executes natively.
//!
//! A household is a package policy unit: it groups resident handles core
//! already owns and names at most one home POI. Health, position, inventory,
//! and equipment stay in core. Nothing here performs a native effect, and the
//! relocation identity decision is still open, so `RelocationIntent` records
//! the decision and its homes and assumes no mechanism.
//!
//! Every mutation either holds its invariants or returns a typed [`Refusal`];
//! a record is never repaired silently.

use std::collections::BTreeSet;

use super::*;

/// Distinct resident handles one household may hold.
pub(super) const MAX_HOUSEHOLD_MEMBERS: usize = 16;
/// Households one settlement may hold.
pub(super) const MAX_SETTLEMENT_HOUSEHOLDS: usize = 256;
/// Durable demographic intents one registry may hold.
pub(super) const MAX_DEMOGRAPHY_INTENTS: usize = 256;
/// Longest durable operation id core admits: 64 script-id bytes, so the
/// derived `-reserve` and `-spawn` ids must fit too.
pub(super) const MAX_OPERATION_ID_BYTES: usize = 64;

/// Whether one string fits core's durable operation-id alphabet: 1..64 bytes
/// of lowercase letters, digits, `_` and `-`, exactly as
/// `mc_script::validate_script_id_value` admits. A birth that records any
/// other shape could never issue its reserve or spawn natively.
fn valid_operation_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_OPERATION_ID_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}
/// Ticks from a recorded `age_origin` until a child counts as an adult for
/// military service: 18 calendar years of 12 Minecraft days at 20 TPS.
pub(super) const ADULT_AGE_TICKS: u64 = 18 * 12 * 24_000;
/// Whether one resident may take military service at one owner tick: a
/// recorded age origin admits service only once adulthood passes, while a
/// resident with no recorded origin is an adopted adult and stays admissible.
pub(super) fn is_adult_for_service(age_origin: Option<u64>, observed_tick: u64) -> bool {
    age_origin.is_none_or(|origin| observed_tick.saturating_sub(origin) >= ADULT_AGE_TICKS)
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Origin {
    /// Found by adopting residents a generated village already held.
    #[default]
    Adoption,
    /// Formed by a household that arrived from another community.
    Migration,
    /// Formed when one household divided.
    Split,
    /// Formed on the arrival of refugees.
    Refugee,
}

/// Why a mutation refused. Every mutation holds its invariants or returns one
/// of these; a record is never repaired silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    UnknownSettlement,
    UnknownResident,
    UnknownHousehold,
    UnknownIntent,
    StaleRevision,
    EmptyMembers,
    DuplicateMember,
    MemberLimit,
    HouseholdLimit,
    AlreadyInHousehold,
    NotAMember,
    SecondHome,
    HomeClaimed,
    HomeMismatch,
    HouseholdIdentityConflict,
    AgeOriginAlreadySet,
    InvalidIntent,
    IntentNotRecorded,
    IntentLimit,
    IntentConflict,
    InvalidTransition,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::UnknownSettlement => "Settlement not found.",
            Self::UnknownResident => "Resident not found in that settlement.",
            Self::UnknownHousehold => "Household not found.",
            Self::UnknownIntent => "Demographic intent not found.",
            Self::StaleRevision => "The household changed since it was read.",
            Self::EmptyMembers => "A household needs at least one resident.",
            Self::DuplicateMember => "A household names the same resident twice.",
            Self::MemberLimit => "Household member limit reached.",
            Self::HouseholdLimit => "Household limit reached for this settlement.",
            Self::AlreadyInHousehold => "That resident already belongs to a household.",
            Self::NotAMember => "That resident is not a member of the household.",
            Self::SecondHome => "A household holds one home; a new one needs its own decision.",
            Self::HomeClaimed => "Another household of this settlement claims that home.",
            Self::HomeMismatch => "The intent names a different home than the household holds.",
            Self::HouseholdIdentityConflict => "That household id already names another household.",
            Self::AgeOriginAlreadySet => "That resident's age origin is already recorded.",
            Self::InvalidIntent => "The intent is incomplete or inconsistent.",
            Self::IntentNotRecorded => "A new intent must start recorded.",
            Self::IntentLimit => "Demographic intent limit reached.",
            Self::IntentConflict => "That operation id already names another intent.",
            Self::InvalidTransition => "The intent already reached a final state.",
        })
    }
}

/// One household: distinct member handles core already owns, at most one home
/// POI, and the policy refs the household runs under.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct Household {
    #[serde(default)]
    pub(super) settlement: String,
    #[serde(default)]
    pub(super) origin: Origin,
    /// Bumped by every accepted membership, home, or policy change.
    #[serde(default)]
    pub(super) revision: u64,
    /// Distinct member handles, ordered so a save is stable.
    #[serde(default)]
    pub(super) members: BTreeSet<String>,
    /// The one home POI this household holds, when it holds one.
    #[serde(default)]
    pub(super) home: Option<String>,
    /// Needs profile ref.
    #[serde(default)]
    pub(super) needs: Option<String>,
    /// Social policy ref.
    #[serde(default)]
    pub(super) policy: Option<String>,
}

/// Whether a durable formation created the household or found it already there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Formation {
    Created,
    Replayed,
}

/// Whether removing a member left the household standing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Removal {
    Updated,
    Retired,
}

/// Where a migrating household comes from.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Provenance {
    /// Another community this registry already models.
    #[default]
    OtherCommunity,
    /// A flow outside the modelled communities.
    ExternalFlow,
    /// A refugee arrival.
    Refugee,
}

/// How far an intent got. A restart resumes from exactly this state.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum IntentState {
    /// Written before any native effect: only this state may still issue one.
    /// Core operations stay idempotent under their operation id, so a restart
    /// reissues the same native request until its authoritative result is
    /// projected; only `Committed` or `Refused` closes the intent.
    #[default]
    Recorded,
    /// The authoritative result was projected into the records.
    Committed,
    /// Refused: nothing was applied and nothing will be.
    Refused,
}

/// A household is admitted from another community or an external flow.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct MigrationIntent {
    #[serde(default)]
    pub(super) household: String,
    #[serde(default)]
    pub(super) provenance: Provenance,
    #[serde(default)]
    pub(super) target_settlement: String,
    #[serde(default)]
    pub(super) expected_site_revision: u64,
    #[serde(default)]
    pub(super) state: IntentState,
}

/// A household moves between two settlements. Core has no owned transition for
/// this yet, so the intent records the decision and its homes, not a mechanism.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct RelocationIntent {
    #[serde(default)]
    pub(super) household: String,
    #[serde(default)]
    pub(super) from: String,
    #[serde(default)]
    pub(super) to: String,
    /// The home the household holds now, when it holds one.
    #[serde(default)]
    pub(super) old_home: Option<String>,
    /// The reservation token, or POI id, of the home being taken.
    #[serde(default)]
    pub(super) new_home_token: String,
    #[serde(default)]
    pub(super) state: IntentState,
}

/// One child, through one reserve plus one spawn operation.
///
/// The plan is immutable: the home the child takes, the revision it was read
/// at, the child alias and birth tick, and the stable intent, reserve and
/// spawn operation ids minted once with the intent. The alias and all three
/// ids derive from the settlement's resident sequence, which the opening CAS
/// reserves by bumping it: a refused reserve or spawn leaves a gap, never a
/// reused alias or operation id, so a retry can never collide with the earlier
/// core fingerprint. No token or handle is stored: before the reserve there is
/// no token yet, and after a restart the same reserve replays its receipt with
/// the same token, then the same spawn replays its snapshot. One closing CAS
/// records the child and the intent.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct BirthIntent {
    #[serde(default)]
    pub(super) household: String,
    #[serde(default)]
    pub(super) parents: Vec<String>,
    /// The site holding the free home the child takes.
    #[serde(default)]
    pub(super) site_id: String,
    /// The free home point of interest the child takes.
    #[serde(default)]
    pub(super) poi_id: String,
    /// The site revision the home was read at.
    #[serde(default)]
    pub(super) expected_site_revision: u64,
    /// The roster alias the child takes in the household's settlement.
    #[serde(default)]
    pub(super) child_alias: String,
    /// The absolute owner tick the child's age counts from.
    #[serde(default)]
    pub(super) birth_tick: u64,
    /// Stable reserve operation id, minted once with the intent.
    #[serde(default)]
    pub(super) reserve_operation_id: String,
    /// Stable spawn operation id, minted once with the intent.
    #[serde(default)]
    pub(super) spawn_operation_id: String,
    #[serde(default)]
    pub(super) state: IntentState,
}

/// One durable demographic intent, stored under its operation id.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Intent {
    Migration(MigrationIntent),
    Relocation(RelocationIntent),
    Birth(BirthIntent),
}

impl MigrationIntent {
    fn plan(&self) -> (&str, Provenance, &str, u64) {
        (
            &self.household,
            self.provenance,
            &self.target_settlement,
            self.expected_site_revision,
        )
    }
}

impl RelocationIntent {
    fn plan(&self) -> (&str, &str, &str, Option<&str>, &str) {
        (
            &self.household,
            &self.from,
            &self.to,
            self.old_home.as_deref(),
            &self.new_home_token,
        )
    }
}

impl BirthIntent {
    fn plan(&self) -> (&str, &[String], &str, &str, u64, &str, u64, &str, &str) {
        (
            &self.household,
            &self.parents,
            &self.site_id,
            &self.poi_id,
            self.expected_site_revision,
            &self.child_alias,
            self.birth_tick,
            &self.reserve_operation_id,
            &self.spawn_operation_id,
        )
    }
}

impl Intent {
    /// The household every variant names.
    pub(super) fn household(&self) -> &str {
        match self {
            Self::Migration(intent) => &intent.household,
            Self::Relocation(intent) => &intent.household,
            Self::Birth(intent) => &intent.household,
        }
    }

    pub(super) fn state(&self) -> IntentState {
        match self {
            Self::Migration(intent) => intent.state,
            Self::Relocation(intent) => intent.state,
            Self::Birth(intent) => intent.state,
        }
    }

    fn set_state(&mut self, state: IntentState) {
        match self {
            Self::Migration(intent) => intent.state = state,
            Self::Relocation(intent) => intent.state = state,
            Self::Birth(intent) => intent.state = state,
        }
    }

    /// The intent without its progress: two records with the same plan are the
    /// same operation replayed, whatever state each one reached.
    fn same_plan(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Migration(left), Self::Migration(right)) => left.plan() == right.plan(),
            (Self::Relocation(left), Self::Relocation(right)) => left.plan() == right.plan(),
            (Self::Birth(left), Self::Birth(right)) => left.plan() == right.plan(),
            _ => false,
        }
    }
}

fn household_of<'a>(
    registry: &'a SettlementRegistry,
    household_id: &str,
) -> Result<&'a Household, Refusal> {
    registry
        .households
        .get(household_id)
        .ok_or(Refusal::UnknownHousehold)
}

fn household_mut<'a>(
    registry: &'a mut SettlementRegistry,
    household_id: &str,
) -> Result<&'a mut Household, Refusal> {
    registry
        .households
        .get_mut(household_id)
        .ok_or(Refusal::UnknownHousehold)
}

fn households_of<'a>(
    households: &'a BTreeMap<String, Household>,
    settlement: &'a str,
) -> impl Iterator<Item = &'a Household> + 'a {
    households
        .values()
        .filter(move |household| household.settlement == settlement)
}

/// Every alias that names this handle is the same person, so they share the
/// one household ref.
fn residents_named<'a>(
    record: &'a SettlementRecord,
    handle: &'a str,
) -> impl Iterator<Item = &'a SettlementResident> + 'a {
    record
        .residents
        .values()
        .filter(move |resident| resident.handle == handle)
}

/// Whether the settlement records this handle, and whether one of its records
/// already belongs to a household. A household never names a person the
/// registry cannot resolve.
fn resident_is_bound(
    settlements: &BTreeMap<String, SettlementRecord>,
    settlement: &str,
    handle: &str,
) -> Result<bool, Refusal> {
    let Some(record) = settlements.get(settlement) else {
        return Err(Refusal::UnknownSettlement);
    };
    let mut found = false;
    let mut bound = false;
    for resident in residents_named(record, handle) {
        found = true;
        bound |= resident.household.is_some();
    }
    if !found {
        return Err(Refusal::UnknownResident);
    }
    Ok(bound)
}

fn bind_member(
    settlements: &mut BTreeMap<String, SettlementRecord>,
    settlement: &str,
    handle: &str,
    bound: Option<&str>,
) -> Result<(), Refusal> {
    let Some(record) = settlements.get_mut(settlement) else {
        return Err(Refusal::UnknownSettlement);
    };
    let mut found = false;
    for resident in record.residents.values_mut() {
        if resident.handle == handle {
            found = true;
            resident.household = bound.map(str::to_owned);
        }
    }
    if found {
        Ok(())
    } else {
        Err(Refusal::UnknownResident)
    }
}

/// Clear every resident ref that names this household, so a retired household
/// leaves no member behind.
fn clear_member_refs(registry: &mut SettlementRegistry, household_id: &str) {
    for record in registry.settlements.values_mut() {
        for resident in record.residents.values_mut() {
            if resident.household.as_deref() == Some(household_id) {
                resident.household = None;
            }
        }
    }
}

/// Open recorded births holding slots: terminal intents hold nothing.
fn open_births<'a>(registry: &'a SettlementRegistry) -> impl Iterator<Item = &'a BirthIntent> + 'a {
    registry.demography_intents.values().filter_map(|intent| {
        let Intent::Birth(birth) = intent else {
            return None;
        };
        (birth.state == IntentState::Recorded).then_some(birth)
    })
}

/// Open births of one household: they already hold its member slots.
fn household_births<'a>(
    registry: &'a SettlementRegistry,
    household_id: &'a str,
) -> impl Iterator<Item = &'a BirthIntent> + 'a {
    open_births(registry).filter(move |birth| birth.household == household_id)
}

/// Open births whose holder lives in one settlement: they already hold its
/// roster slots and their aliases.
fn settlement_births<'a>(
    registry: &'a SettlementRegistry,
    settlement: &'a str,
) -> impl Iterator<Item = &'a BirthIntent> + 'a {
    open_births(registry).filter(move |birth| {
        registry
            .households
            .get(&birth.household)
            .is_some_and(|holder| holder.settlement == settlement)
    })
}

/// Whether one settlement holds an open birth under one roster alias, and how
/// many open births it holds. Computed before any mutable borrow: a new roster
/// record must refuse a held alias and a held roster slot.
pub(super) fn settlement_birth_reservation(
    registry: &SettlementRegistry,
    settlement: &str,
    resident_name: &str,
) -> (usize, bool) {
    let mut count = 0;
    let mut alias_held = false;
    for birth in settlement_births(registry, settlement) {
        count += 1;
        alias_held |= birth.child_alias == resident_name;
    }
    (count, alias_held)
}

/// The distinct member set a formation names, or the bound it breaks.
fn distinct_members(members: &[impl AsRef<str>]) -> Result<BTreeSet<String>, Refusal> {
    if members.is_empty() {
        return Err(Refusal::EmptyMembers);
    }
    if members.len() > MAX_HOUSEHOLD_MEMBERS {
        return Err(Refusal::MemberLimit);
    }
    let mut distinct = BTreeSet::new();
    for member in members {
        if !distinct.insert(member.as_ref().to_owned()) {
            return Err(Refusal::DuplicateMember);
        }
    }
    Ok(distinct)
}

/// The invariants a formation must satisfy, checked before anything changes.
fn validate_formation(
    registry: &SettlementRegistry,
    settlement: &str,
    members: &BTreeSet<String>,
) -> Result<(), Refusal> {
    if !registry.settlements.contains_key(settlement) {
        return Err(Refusal::UnknownSettlement);
    }
    for member in members {
        if resident_is_bound(&registry.settlements, settlement, member)? {
            return Err(Refusal::AlreadyInHousehold);
        }
    }
    if households_of(&registry.households, settlement).count() >= MAX_SETTLEMENT_HOUSEHOLDS {
        return Err(Refusal::HouseholdLimit);
    }
    Ok(())
}

fn household_id(sequence: u64) -> String {
    format!("household-{sequence}")
}

/// Found a household now, allocating the next registry household id. The id is
/// allocated only by a formation that holds, so a refused formation leaves the
/// registry untouched and one id never names two member sets.
pub(super) fn found_household(
    registry: &mut SettlementRegistry,
    settlement: &str,
    origin: Origin,
    members: &[impl AsRef<str>],
) -> Result<String, Refusal> {
    let sequence = registry
        .household_sequence
        .checked_add(1)
        .ok_or(Refusal::HouseholdLimit)?;
    let household_id = household_id(sequence);
    form_household(registry, &household_id, settlement, origin, members)?;
    registry.household_sequence = sequence;
    Ok(household_id)
}

/// Form a household under the id a durable formation already names.
///
/// The id either is new — created here — or already names exactly this
/// settlement, origin, and member set, which is the same household replayed
/// after an unclear CAS answer. A different household under a used id is
/// refused, so one id never names two member sets.
pub(super) fn form_household(
    registry: &mut SettlementRegistry,
    household_id: &str,
    settlement: &str,
    origin: Origin,
    members: &[impl AsRef<str>],
) -> Result<Formation, Refusal> {
    let distinct = distinct_members(members)?;
    if let Some(existing) = registry.households.get(household_id) {
        let replayed = existing.settlement == settlement
            && existing.origin == origin
            && existing.members == distinct;
        return if replayed {
            Ok(Formation::Replayed)
        } else {
            Err(Refusal::HouseholdIdentityConflict)
        };
    }
    validate_formation(registry, settlement, &distinct)?;
    {
        let SettlementRegistry { settlements, .. } = &mut *registry;
        for member in &distinct {
            bind_member(settlements, settlement, member, Some(household_id))?;
        }
    }
    registry.households.insert(
        household_id.to_owned(),
        Household {
            settlement: settlement.to_owned(),
            origin,
            revision: 1,
            members: distinct,
            home: None,
            needs: None,
            policy: None,
        },
    );
    Ok(Formation::Created)
}

/// Add a resident to an existing household.
pub(super) fn add_member(
    registry: &mut SettlementRegistry,
    household_id: &str,
    handle: &str,
    expected_revision: u64,
) -> Result<(), Refusal> {
    {
        let household = household_of(registry, household_id)?;
        if household.revision != expected_revision {
            return Err(Refusal::StaleRevision);
        }
        if household.members.contains(handle) {
            return Err(Refusal::DuplicateMember);
        }
        // Open recorded births already hold their member slots: an unrelated
        // arrival over the last slot refuses, so a recorded birth still fits.
        if household
            .members
            .len()
            .saturating_add(household_births(registry, household_id).count())
            >= MAX_HOUSEHOLD_MEMBERS
        {
            return Err(Refusal::MemberLimit);
        }
        if resident_is_bound(&registry.settlements, &household.settlement, handle)? {
            return Err(Refusal::AlreadyInHousehold);
        }
    }
    {
        let SettlementRegistry {
            settlements,
            households,
            ..
        } = &mut *registry;
        let settlement = households
            .get(household_id)
            .ok_or(Refusal::UnknownHousehold)?
            .settlement
            .as_str();
        bind_member(settlements, settlement, handle, Some(household_id))?;
    }
    let household = household_mut(registry, household_id)?;
    household.members.insert(handle.to_owned());
    household.revision = household.revision.saturating_add(1);
    Ok(())
}

/// Remove a resident from a household. Core death is authoritative: a guardian
/// is always removed, even the last one. A household left with no members but
/// an open birth intent stays as its vacant holder, keeping the intent and its
/// reserved capacity; without one it retires and every ref naming it clears.
pub(super) fn remove_member(
    registry: &mut SettlementRegistry,
    household_id: &str,
    handle: &str,
    expected_revision: u64,
) -> Result<Removal, Refusal> {
    let vacant = {
        let household = household_of(registry, household_id)?;
        if household.revision != expected_revision {
            return Err(Refusal::StaleRevision);
        }
        if !household.members.contains(handle) {
            return Err(Refusal::NotAMember);
        }
        household.members.len() == 1
            && registry
                .demography_intents
                .values()
                .any(|intent| matches!(intent, Intent::Birth(birth) if birth.household == household_id && birth.state == IntentState::Recorded))
    };
    if vacant {
        {
            let SettlementRegistry {
                settlements,
                households,
                ..
            } = &mut *registry;
            let settlement = households
                .get(household_id)
                .ok_or(Refusal::UnknownHousehold)?
                .settlement
                .as_str();
            bind_member(settlements, settlement, handle, None)?;
        }
        let household = household_mut(registry, household_id)?;
        household.members.remove(handle);
        household.revision = household.revision.saturating_add(1);
        return Ok(Removal::Updated);
    }
    let last_member = {
        let household = household_of(registry, household_id)?;
        household.members.len() == 1
    };
    if last_member {
        registry.households.remove(household_id);
        clear_member_refs(registry, household_id);
        return Ok(Removal::Retired);
    }
    {
        let SettlementRegistry {
            settlements,
            households,
            ..
        } = &mut *registry;
        let settlement = households
            .get(household_id)
            .ok_or(Refusal::UnknownHousehold)?
            .settlement
            .as_str();
        bind_member(settlements, settlement, handle, None)?;
    }
    let household = household_mut(registry, household_id)?;
    household.members.remove(handle);
    household.revision = household.revision.saturating_add(1);
    Ok(Removal::Updated)
}

/// Project one committed birth into the records: register the child under its
/// alias with its age origin, bind it to the household, and close the intent.
/// Replaying the same child alias and handle is the same projection, never a
/// second child; any conflicting record is refused, never repaired.
pub(super) fn commit_birth(
    registry: &mut SettlementRegistry,
    operation_id: &str,
    child_handle: &str,
) -> Result<IntentState, Refusal> {
    let intent = registry
        .demography_intents
        .get(operation_id)
        .ok_or(Refusal::UnknownIntent)?
        .clone();
    let Intent::Birth(birth) = intent else {
        return Err(Refusal::InvalidIntent);
    };
    if birth.state != IntentState::Recorded {
        return Ok(birth.state);
    }
    let household_id = birth.household.clone();
    // Validate everything before changing anything: a refusal leaves the
    // registry untouched, and only the exact replay shape falls through.
    let replayed = {
        let household = household_of(registry, &household_id)?;
        let Some(record) = registry.settlements.get(&household.settlement) else {
            return Err(Refusal::UnknownSettlement);
        };
        if record.residents.len() >= MAX_SETTLEMENT_RESIDENTS
            && !record.residents.contains_key(&birth.child_alias)
        {
            return Err(Refusal::HouseholdLimit);
        }
        if household.members.len() >= MAX_HOUSEHOLD_MEMBERS
            && !household.members.contains(child_handle)
        {
            return Err(Refusal::MemberLimit);
        }
        match record.residents.get(&birth.child_alias) {
            Some(existing)
                if existing.handle == child_handle
                    && existing.household.as_deref() == Some(household_id.as_str())
                    && existing.age_origin == Some(birth.birth_tick) =>
            {
                true
            }
            Some(_) => return Err(Refusal::IntentConflict),
            None => {
                if residents_named(record, child_handle).next().is_some() {
                    return Err(Refusal::IntentConflict);
                }
                false
            }
        }
    };
    if !replayed {
        let settlement = household_of(registry, &household_id)?.settlement.clone();
        let record = registry
            .settlements
            .get_mut(&settlement)
            .ok_or(Refusal::UnknownSettlement)?;
        record.residents.insert(
            birth.child_alias.clone(),
            SettlementResident {
                handle: child_handle.to_owned(),
                work_revision: 0,
                role: None,
                employer: None,
                hiring: None,
                issued: None,
                dismissing: None,
                capturing: None,
                captive_by: None,
                household: Some(household_id.clone()),
                age_origin: Some(birth.birth_tick),
            },
        );
        record.revision = record.revision.saturating_add(1);
        let household = household_mut(registry, &household_id)?;
        household.members.insert(child_handle.to_owned());
        household.revision = household.revision.saturating_add(1);
    }
    let intent = registry
        .demography_intents
        .get_mut(operation_id)
        .ok_or(Refusal::UnknownIntent)?;
    intent.set_state(IntentState::Committed);
    Ok(IntentState::Committed)
}
/// Give a household its one home. The same home again is the same decision;
/// a second home is the relocation slice's decision and is refused here.
pub(super) fn set_home(
    registry: &mut SettlementRegistry,
    household_id: &str,
    poi_id: &str,
    expected_revision: u64,
) -> Result<(), Refusal> {
    {
        let household = household_of(registry, household_id)?;
        if household.revision != expected_revision {
            return Err(Refusal::StaleRevision);
        }
        match household.home.as_deref() {
            Some(home) if home == poi_id => return Ok(()),
            Some(_) => return Err(Refusal::SecondHome),
            None => {}
        }
        let claimed = households_of(&registry.households, &household.settlement)
            .any(|other| other.home.as_deref() == Some(poi_id));
        if claimed {
            return Err(Refusal::HomeClaimed);
        }
    }
    let household = household_mut(registry, household_id)?;
    household.home = Some(poi_id.to_owned());
    household.revision = household.revision.saturating_add(1);
    Ok(())
}

/// Point a household at its needs profile and social policy.
pub(super) fn set_policy(
    registry: &mut SettlementRegistry,
    household_id: &str,
    needs: Option<&str>,
    policy: Option<&str>,
    expected_revision: u64,
) -> Result<(), Refusal> {
    let household = household_mut(registry, household_id)?;
    if household.revision != expected_revision {
        return Err(Refusal::StaleRevision);
    }
    household.needs = needs.map(str::to_owned);
    household.policy = policy.map(str::to_owned);
    household.revision = household.revision.saturating_add(1);
    Ok(())
}

/// Record a resident's absolute owner tick of origin. The tick is immutable: a
/// different tick for the same resident is refused, never overwritten, so a
/// lazily derived age stays consistent across restarts.
pub(super) fn record_age_origin(
    registry: &mut SettlementRegistry,
    settlement: &str,
    handle: &str,
    tick: u64,
) -> Result<(), Refusal> {
    let SettlementRegistry { settlements, .. } = &mut *registry;
    let Some(record) = settlements.get_mut(settlement) else {
        return Err(Refusal::UnknownSettlement);
    };
    let mut found = false;
    let mut conflicting = false;
    for resident in residents_named(record, handle) {
        found = true;
        conflicting |= resident.age_origin.is_some_and(|origin| origin != tick);
    }
    if !found {
        return Err(Refusal::UnknownResident);
    }
    if conflicting {
        return Err(Refusal::AgeOriginAlreadySet);
    }
    for resident in record.residents.values_mut() {
        if resident.handle == handle {
            resident.age_origin = Some(tick);
        }
    }
    Ok(())
}

/// Mint one birth plan and record its intent, before any native effect.
///
/// The `operation_id` names the stable demographic decision - the notification
/// the birth answers - so a repeated notification finds the same intent. The
/// child alias and the reserve and spawn operation ids derive from the
/// settlement's resident sequence, which this call reserves by bumping it in
/// the same change: a refused reserve or spawn leaves a gap, never a reused
/// alias or operation id, so a retry can never collide with the earlier core
/// fingerprint. Settlement names are bounded package ids, so the minted names
/// stay in every contract bound, and the per-settlement sequence keeps two
/// settlements from minting the same alias. No token or handle is stored:
/// before the reserve there is no token yet, and after a restart the same
/// reserve replays its receipt with the same token, then the same spawn
/// replays its snapshot. One closing CAS records the child and the intent.
pub(super) fn record_birth(
    registry: &mut SettlementRegistry,
    operation_id: &str,
    household_id: &str,
    parents: &[String],
    site_id: &str,
    poi_id: &str,
    expected_site_revision: u64,
    birth_tick: u64,
) -> Result<BirthIntent, Refusal> {
    if !valid_operation_id(operation_id)
        || format!("{operation_id}-reserve").len() > MAX_OPERATION_ID_BYTES
        || format!("{operation_id}-spawn").len() > MAX_OPERATION_ID_BYTES
    {
        return Err(Refusal::InvalidIntent);
    }
    // A repeated notification finds the same persisted intent first, even when
    // its household retired since: the sequence must not advance twice for one
    // decision. Every caller-owned field is compared, so the same id with
    // another plan is a conflict, never a silent replay.
    if let Some(existing) = registry.demography_intents.get(operation_id) {
        let Intent::Birth(birth) = existing.clone() else {
            return Err(Refusal::IntentConflict);
        };
        if birth.household == household_id
            && birth.parents == parents
            && birth.site_id == site_id
            && birth.poi_id == poi_id
            && birth.expected_site_revision == expected_site_revision
            && birth.birth_tick == birth_tick
            && birth.reserve_operation_id == format!("{operation_id}-reserve")
            && birth.spawn_operation_id == format!("{operation_id}-spawn")
        {
            return Ok(birth);
        }
        return Err(Refusal::IntentConflict);
    }
    let household = household_of(registry, household_id)?.clone();
    let Some(record) = registry.settlements.get(&household.settlement) else {
        return Err(Refusal::UnknownSettlement);
    };
    let next = record
        .resident_sequence
        .checked_add(1)
        .ok_or(Refusal::HouseholdLimit)?;
    let intent = Intent::Birth(BirthIntent {
        household: household_id.to_owned(),
        parents: parents.to_vec(),
        site_id: site_id.to_owned(),
        poi_id: poi_id.to_owned(),
        expected_site_revision,
        child_alias: format!("resident_{next}"),
        birth_tick,
        reserve_operation_id: format!("{operation_id}-reserve"),
        spawn_operation_id: format!("{operation_id}-spawn"),
        state: IntentState::Recorded,
    });
    validate_intent(registry, &intent)?;
    if registry.demography_intents.len() >= MAX_DEMOGRAPHY_INTENTS {
        return Err(Refusal::IntentLimit);
    }
    // Only the fully validated change mutates: bump the sequence and insert
    // the intent together, so any refusal leaves the registry untouched.
    let record = registry
        .settlements
        .get_mut(&household.settlement)
        .ok_or(Refusal::UnknownSettlement)?;
    record.resident_sequence = next;
    registry
        .demography_intents
        .insert(operation_id.to_owned(), intent.clone());
    let Intent::Birth(birth) = intent else {
        unreachable!("just-recorded birth intent");
    };
    Ok(birth)
}

/// Record a demographic intent under its operation id, before any native
/// effect. Replaying the same operation id with the same plan returns the state
/// that intent reached and changes nothing; the same id with a different plan
/// is refused, so one id never names two intents.
///
/// A birth is never recorded here: it needs its reserved alias and operation
/// ids, so only [`record_birth`] records one. A birth reaching this path is a
/// caller bug, refused before it can write an intent without reserved identity.
pub(super) fn record_intent(
    registry: &mut SettlementRegistry,
    operation_id: &str,
    intent: Intent,
) -> Result<IntentState, Refusal> {
    if matches!(intent, Intent::Birth(_)) {
        return Err(Refusal::InvalidIntent);
    }
    if let Some(existing) = registry.demography_intents.get(operation_id) {
        return if existing.same_plan(&intent) {
            Ok(existing.state())
        } else {
            Err(Refusal::IntentConflict)
        };
    }
    if intent.state() != IntentState::Recorded {
        return Err(Refusal::IntentNotRecorded);
    }
    validate_intent(registry, &intent)?;
    if registry.demography_intents.len() >= MAX_DEMOGRAPHY_INTENTS {
        return Err(Refusal::IntentLimit);
    }
    registry
        .demography_intents
        .insert(operation_id.to_owned(), intent);
    Ok(IntentState::Recorded)
}

/// Whether a restart may still issue the native effect of this intent: only a
/// recorded intent may, since a restart reissues the same idempotent native
/// request until the authoritative result commits or refuses it.
pub(super) fn may_issue_native(
    registry: &SettlementRegistry,
    operation_id: &str,
) -> Result<bool, Refusal> {
    let intent = registry
        .demography_intents
        .get(operation_id)
        .ok_or(Refusal::UnknownIntent)?;
    Ok(intent.state() == IntentState::Recorded)
}
/// Move an intent to its next state. A final intent never moves again, and
/// re-asserting the current state is a no-op, so a replayed callback resumes
/// the same intent exactly once.
pub(super) fn advance_intent(
    registry: &mut SettlementRegistry,
    operation_id: &str,
    next: IntentState,
) -> Result<IntentState, Refusal> {
    let intent = registry
        .demography_intents
        .get_mut(operation_id)
        .ok_or(Refusal::UnknownIntent)?;
    let current = intent.state();
    if current == next {
        return Ok(current);
    }
    // A birth reaches Committed only through its projection: the generic path
    // cannot know the spawned handle, so it would mark the intent done without
    // registering any child. A native refusal still closes through Refused.
    if next == IntentState::Committed && matches!(intent, Intent::Birth(_)) {
        return Err(Refusal::InvalidTransition);
    }
    let allowed = matches!(
        (current, next),
        (IntentState::Recorded, IntentState::Committed)
            | (IntentState::Recorded, IntentState::Refused)
    );
    if !allowed {
        return Err(Refusal::InvalidTransition);
    }
    // A refused birth releases the capacity its opening CAS reserved: a vacant
    // holder with no remaining open birth intent retires, clearing every ref
    // that named it.
    if next == IntentState::Refused {
        if let Intent::Birth(birth) = intent.clone() {
            let household_id = birth.household.clone();
            intent.set_state(next);
            let vacant = registry.households.get(&household_id).is_some_and(|household| {
                household.members.is_empty()
                    && !registry.demography_intents.values().any(|other| {
                        matches!(other, Intent::Birth(sibling) if sibling.household == household_id && sibling.state == IntentState::Recorded)
                    })
            });
            if vacant {
                registry.households.remove(&household_id);
                clear_member_refs(registry, &household_id);
            }
            return Ok(next);
        }
    }
    intent.set_state(next);
    Ok(next)
}
/// An intent names a household this registry holds, and only facts those
/// records already carry: a migration into another settlement, a relocation
/// away from the household's own home, a birth of one or two distinct parents.
fn validate_intent(registry: &SettlementRegistry, intent: &Intent) -> Result<(), Refusal> {
    let household = household_of(registry, intent.household())?;
    match intent {
        Intent::Migration(migration) => {
            if migration.target_settlement == household.settlement
                || !registry
                    .settlements
                    .contains_key(&migration.target_settlement)
            {
                return Err(Refusal::InvalidIntent);
            }
        }
        Intent::Relocation(relocation) => {
            if relocation.from != household.settlement
                || relocation.to == relocation.from
                || relocation.new_home_token.is_empty()
                || !registry.settlements.contains_key(&relocation.to)
            {
                return Err(Refusal::InvalidIntent);
            }
            if relocation.old_home != household.home {
                return Err(Refusal::HomeMismatch);
            }
        }
        Intent::Birth(birth) => {
            if birth.parents.is_empty()
                || birth.parents.len() > 2
                || birth.site_id.is_empty()
                || birth.poi_id.is_empty()
                || birth.child_alias.is_empty()
                || birth.reserve_operation_id.is_empty()
                || birth.spawn_operation_id.is_empty()
            {
                return Err(Refusal::InvalidIntent);
            }
            let household_id = intent.household();
            let Some(record) = registry.settlements.get(&household.settlement) else {
                return Err(Refusal::UnknownSettlement);
            };
            let mut distinct = BTreeSet::new();
            for parent in &birth.parents {
                if !distinct.insert(parent.as_str()) {
                    return Err(Refusal::InvalidIntent);
                }
                // Admission needs a living guardian of this household: the parent
                // handle must resolve to a roster record carrying this household.
                // Death after record time is a separate transition the vacant
                // holder test covers, never a weaker admission.
                let mut found = false;
                let mut of_house = false;
                for resident in residents_named(record, parent) {
                    found = true;
                    of_house |= resident.household.as_deref() == Some(household_id);
                }
                if !found {
                    return Err(Refusal::UnknownResident);
                }
                if !of_house {
                    return Err(Refusal::InvalidIntent);
                }
            }
            // The projection must fit when the intent is recorded, never after
            // the native spawn: open recorded births already hold their slots,
            // so a second intent over the last slot refuses here instead of
            // orphaning a second child in core. Terminal intents hold nothing.
            // Counts stay scoped - a birth in another household or settlement
            // never consumes this household's member slot or this settlement's
            // roster slot - and aliases are settlement-local.
            let household_births = registry.demography_intents.values().filter(|other| {
                matches!(other, Intent::Birth(sibling) if sibling.state == IntentState::Recorded && sibling.household == birth.household)
            }).count();
            let settlement_births = registry
                .demography_intents
                .values()
                .filter(|other| {
                    let Intent::Birth(sibling) = other else {
                        return false;
                    };
                    if sibling.state != IntentState::Recorded {
                        return false;
                    }
                    registry
                        .households
                        .get(&sibling.household)
                        .is_some_and(|holder| holder.settlement == household.settlement)
                })
                .count();
            if household.members.len().saturating_add(household_births) >= MAX_HOUSEHOLD_MEMBERS {
                return Err(Refusal::MemberLimit);
            }
            if record.residents.len().saturating_add(settlement_births) >= MAX_SETTLEMENT_RESIDENTS
            {
                return Err(Refusal::HouseholdLimit);
            }
            if record.residents.contains_key(&birth.child_alias)
                || registry.demography_intents.values().any(|other| {
                    let Intent::Birth(sibling) = other else {
                        return false;
                    };
                    if sibling.state != IntentState::Recorded
                        || sibling.child_alias != birth.child_alias
                    {
                        return false;
                    }
                    registry
                        .households
                        .get(&sibling.household)
                        .is_some_and(|holder| holder.settlement == household.settlement)
                })
            {
                return Err(Refusal::IntentConflict);
            }
        }
    }
    Ok(())
}
