//! CP-058's sole war/destruction policy (RULESET DECISION: council Astra + GLM).
//!
//! Admitted values; no other policy defaults live here:
//! charter-holder declares with a delivered reason; grace is exactly 72000
//! simulation ticks; at most 8 windows, each at most 72000 ticks, pauses of at
//! least 12000 ticks, absolute `[start, end)` ticks; at most 8 lawful known
//! targets; one record of at most 4096 UTF-8 bytes; Private/Neutral/Unknown
//! protection refused; 0 new damage/destruction commits after peace/revoke;
//! victory ONLY by committed peace/surrender with a recorded outcome (window
//! expiry without an outcome stops effects and names no winner); automatic
//! contribution is 0 (voluntary payment only via a real inventory transfer);
//! victory grants no separate breach/salvage right (see `siege_policy`);
//! `AdmittedConflictContinues`: logout gives no immunity for an already
//! admitted lawful conflict in the current window, but admits no new offline
//! targets.
//!
//! A predicate is not a physical permission: every accepted effect must
//! conditionally commit against both durable versions.
use serde::{Deserialize, Serialize};

pub(super) const VALUE_BYTES: usize = 4096;
pub(super) const MAX_TARGETS: usize = 8;
pub(super) const MAX_WINDOWS: usize = 8;
/// Exact grace between declaration and the first admissible tick.
pub(super) const GRACE_TICKS: u64 = 72_000;
/// Longest single `[start, end)` window.
pub(super) const MAX_WINDOW_TICKS: u64 = 72_000;
/// Shortest pause between two consecutive windows.
pub(super) const MIN_PAUSE_TICKS: u64 = 12_000;
/// Automatic war contribution: always zero by decision. Any non-zero payment
/// moves only through a real inventory transfer, which this module never issues.
pub(super) const AUTO_CONTRIBUTION: u64 = 0;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Side {
    pub house: String,
    pub realm: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Window {
    pub start: u64,
    pub end: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum OfflineProtection {
    Protected,
    AdmittedConflictContinues,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Protection {
    Unprotected,
    Private,
    Neutral,
    OfflineOwner,
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Target {
    pub id: String,
    pub owner: Side,
    pub provenance: String,
    pub observed_at: u64,
    pub delivered_at: u64,
    pub lawful_basis: String,
}
/// How a war ended. There is no auto-victory: holding points, outlasting a
/// window, or going offline never produces an outcome on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Outcome {
    Peace,
    Surrender,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum State {
    Declared,
    Ended {
        at: u64,
        event: String,
        outcome: Outcome,
    },
    Revoked {
        at: u64,
        event: String,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Policy {
    pub id: String,
    pub revision: u64,
    /// An explicit admitted political ruleset record, never a compiled default.
    pub ruleset: String,
    pub attacker: Side,
    pub defender: Side,
    pub goal: String,
    /// Charter-holder identity that declared this war.
    pub declarer: String,
    /// Delivered lawful reason record both sides can cite.
    pub cause: String,
    pub declaration_event: String,
    pub declared_at: u64,
    pub grace_end: u64,
    pub windows: Vec<Window>,
    pub offline: OfflineProtection,
    pub targets: Vec<Target>,
    pub state: State,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Version {
    pub key: String,
    pub version: u64,
}
/// Explicit bounded/revocable mandate from the charter-holder. Holder-rootedness
/// is checked through `PoliticalFacts::holder` at authorize time; the mandate
/// itself binds the acting principal to one war and one politics revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Delegation {
    pub principal: String,
    pub war_id: String,
    pub revision: u64,
    pub revoked: bool,
}
/// Constructed from the current CP-061 records, not from command/UI fields.
#[derive(Clone, Debug)]
pub(super) struct PoliticalFacts {
    pub revision: u64,
    pub storage: Vec<Version>,
    pub actor: String,
    pub actor_side: Option<Side>,
    pub target_side: Option<Side>,
    /// Current charter-authorized holder; `None` means vacant/unknown.
    pub holder: Option<String>,
    pub war_id: Option<String>,
    pub lawful_goal: Option<String>,
    pub delegation: Option<Delegation>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    Player,
    Npc,
    Projectile,
    NativeWork,
    Explosion,
    Fire,
    Fluid,
    Mutation,
}
#[derive(Clone, Debug)]
pub(super) struct EffectFacts {
    pub event: String,
    pub actor: String,
    pub target: String,
    pub tick: u64,
    pub source: Source,
    pub protection: Protection,
    pub policy: Version,
    pub admitted_revision: u64,
    pub admitted_politics_revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Permit {
    pub event: String,
    pub actor: String,
    pub target: String,
    pub policy_revision: u64,
    pub politics_revision: u64,
    /// The native decision must validate these together with its physical facts.
    pub guards: Vec<Version>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    InvalidRecord,
    UnknownSide,
    WrongSide,
    UnknownTarget,
    ForbiddenTarget,
    Protected,
    Grace,
    WindowClosed,
    Ended,
    Revoked,
    StalePolicy,
    StalePolitics,
    DelegationRevoked,
    UnknownActor,
    NotCharterHolder,
    SiegeNotOpen,
    NativeConditionalGuardRequired,
}
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.'))
}
fn side(value: &Side) -> bool {
    id(&value.house) && value.realm.as_deref().is_none_or(id)
}
pub(super) fn key(war: &str) -> Result<String, Refusal> {
    if !id(war) {
        return Err(Refusal::InvalidRecord);
    }
    Ok(format!("settlement-war-v1:{war}"))
}
impl Policy {
    pub fn validate(&self) -> Result<(), Refusal> {
        if !id(&self.id)
            || self.revision == 0
            || !id(&self.ruleset)
            || !id(&self.goal)
            || !id(&self.declarer)
            || !id(&self.cause)
            || !id(&self.declaration_event)
            || !side(&self.attacker)
            || !side(&self.defender)
            || self.attacker.house == self.defender.house
            || (self.attacker.realm.is_some() && self.attacker.realm == self.defender.realm)
            || self.declared_at.checked_add(GRACE_TICKS) != Some(self.grace_end)
            || self.targets.is_empty()
            || self.targets.len() > MAX_TARGETS
            || self.windows.is_empty()
            || self.windows.len() > MAX_WINDOWS
        {
            return Err(Refusal::InvalidRecord);
        }
        let mut end = self.grace_end;
        let mut first = true;
        for window in &self.windows {
            let duration = window
                .end
                .checked_sub(window.start)
                .ok_or(Refusal::InvalidRecord)?;
            if duration == 0 || duration > MAX_WINDOW_TICKS {
                return Err(Refusal::InvalidRecord);
            }
            if window.start < end {
                return Err(Refusal::InvalidRecord);
            }
            if !first && window.start - end < MIN_PAUSE_TICKS {
                return Err(Refusal::InvalidRecord);
            }
            first = false;
            end = window.end;
        }
        for (index, target) in self.targets.iter().enumerate() {
            if !id(&target.id)
                || !id(&target.provenance)
                || !id(&target.lawful_basis)
                || (target.owner != self.defender && target.owner != self.attacker)
                || target.observed_at > target.delivered_at
                || target.delivered_at > self.declared_at
                || self.targets[..index].iter().any(|t| t.id == target.id)
            {
                return Err(Refusal::InvalidRecord);
            }
        }
        let terminal_ok = match &self.state {
            State::Declared => true,
            State::Ended { at, event, .. } | State::Revoked { at, event } => {
                *at >= self.declared_at && id(event)
            }
        };
        if !terminal_ok {
            return Err(Refusal::InvalidRecord);
        }
        Ok(())
    }
    pub fn encode(&self) -> Result<String, Refusal> {
        self.validate()?;
        let value = serde_json::to_string(self).map_err(|_| Refusal::InvalidRecord)?;
        if value.len() > VALUE_BYTES {
            return Err(Refusal::InvalidRecord);
        }
        Ok(value)
    }
    pub fn decode(value: &str) -> Result<Self, Refusal> {
        if value.len() > VALUE_BYTES {
            return Err(Refusal::InvalidRecord);
        }
        let policy: Self = serde_json::from_str(value).map_err(|_| Refusal::InvalidRecord)?;
        policy.validate()?;
        Ok(policy)
    }
    /// Commit peace or surrender. Only the declaring charter-holder ends the
    /// war; replays preserve the original event and outcome, and changing
    /// either on a terminal record fails. Takes effect on the next commit:
    /// already-committed receipts stay valid, new effects are refused.
    pub fn end(
        &mut self,
        holder: &str,
        event: &str,
        tick: u64,
        outcome: Outcome,
    ) -> Result<(), Refusal> {
        self.validate()?;
        if holder != self.declarer {
            return Err(Refusal::NotCharterHolder);
        }
        if !id(event) || tick < self.declared_at {
            return Err(Refusal::InvalidRecord);
        }
        match &self.state {
            State::Declared => (),
            State::Ended {
                event: old,
                outcome: old_outcome,
                ..
            } if old == event && *old_outcome == outcome => return Ok(()),
            State::Ended { .. } => return Err(Refusal::Ended),
            State::Revoked { .. } => return Err(Refusal::Revoked),
        }
        self.revision = self.revision.checked_add(1).ok_or(Refusal::InvalidRecord)?;
        self.state = State::Ended {
            at: tick,
            event: event.into(),
            outcome,
        };
        Ok(())
    }
    /// Revoke the war. Only the declaring charter-holder revokes; revocation
    /// stops new effects but never cancels already-committed receipts.
    pub fn revoke(&mut self, holder: &str, event: &str, tick: u64) -> Result<(), Refusal> {
        self.validate()?;
        if holder != self.declare_holder() {
            return Err(Refusal::NotCharterHolder);
        }
        if !id(event) || tick < self.declared_at {
            return Err(Refusal::InvalidRecord);
        }
        match &self.state {
            State::Declared => (),
            State::Revoked { event: old, .. } if old == event => return Ok(()),
            State::Revoked { .. } => return Err(Refusal::Revoked),
            State::Ended { .. } => return Err(Refusal::Ended),
        }
        self.revision = self.revision.checked_add(1).ok_or(Refusal::InvalidRecord)?;
        self.state = State::Revoked {
            at: tick,
            event: event.into(),
        };
        Ok(())
    }
    fn declare_holder(&self) -> &str {
        &self.declarer
    }
}
/// Automatic war contribution. Decided zero: holding, outlasting, or winning a
/// war never debits anyone. Voluntary payments move only through real
/// inventory transfers outside this module.
pub(super) fn auto_contribution(_policy: &Policy, _politics: &PoliticalFacts) -> u64 {
    AUTO_CONTRIBUTION
}
/// Recorded joint outcome for CP-063: a committed peace/surrender agreement
/// plus the terminal siege record. Window expiry, a missing agreement, or a
/// siege that closed before the agreement yield no winner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct JointOutcome {
    pub event: String,
    pub outcome: Outcome,
    pub at: u64,
    pub siege_event: String,
    pub siege_at: u64,
}
pub(super) fn joint_outcome(
    policy: &Policy,
    siege_closed: Option<(&str, u64)>,
) -> Option<JointOutcome> {
    let State::Ended { at, event, outcome } = &policy.state else {
        return None;
    };
    let (siege_event, siege_at) = siege_closed?;
    if !id(siege_event) || siege_at < *at {
        return None;
    }
    Some(JointOutcome {
        event: event.clone(),
        outcome: *outcome,
        at: *at,
        siege_event: siege_event.into(),
        siege_at,
    })
}
/// Identical predicate for direct player, NPC proxy, projectile, work and vanilla
/// causal mutations. Unknown causal actors never acquire another actor's rights.
pub(super) fn authorize(
    policy: &Policy,
    politics: &PoliticalFacts,
    effect: &EffectFacts,
) -> Result<Permit, Refusal> {
    policy.validate()?;
    if effect.policy.key != key(&policy.id)?
        || politics.storage.is_empty()
        || politics.storage.len() >= 32
        || effect.policy.version == 0
        || politics.storage.iter().enumerate().any(|(index, guard)| {
            guard.key.is_empty()
                || guard.key.len() > 128
                || guard.version == 0
                || guard.key == effect.policy.key
                || politics.storage[..index]
                    .iter()
                    .any(|old| old.key == guard.key)
        })
        || !id(&effect.event)
    {
        return Err(Refusal::InvalidRecord);
    }
    match policy.state {
        State::Declared => (),
        State::Ended { .. } => return Err(Refusal::Ended),
        State::Revoked { .. } => return Err(Refusal::Revoked),
    }
    if policy.revision != effect.admitted_revision {
        return Err(Refusal::StalePolicy);
    }
    if politics.revision != effect.admitted_politics_revision {
        return Err(Refusal::StalePolitics);
    }
    if !id(&effect.actor) || effect.actor != politics.actor {
        return Err(Refusal::UnknownActor);
    }
    if politics.holder.as_deref() != Some(policy.declarer.as_str()) {
        return Err(Refusal::NotCharterHolder);
    }
    let (Some(actor), Some(target)) = (&politics.actor_side, &politics.target_side) else {
        return Err(Refusal::UnknownSide);
    };
    if !((actor == &policy.attacker && target == &policy.defender)
        || (actor == &policy.defender && target == &policy.attacker))
    {
        return Err(Refusal::WrongSide);
    }
    if politics.war_id.as_deref() != Some(policy.id.as_str())
        || politics.lawful_goal.as_deref() != Some(policy.goal.as_str())
    {
        return Err(Refusal::ForbiddenTarget);
    }
    let Some(known) = policy
        .targets
        .iter()
        .find(|target| target.id == effect.target)
    else {
        return Err(Refusal::UnknownTarget);
    };
    if &known.owner != target || known.delivered_at > effect.tick {
        return Err(Refusal::ForbiddenTarget);
    }
    let Some(delegation) = &politics.delegation else {
        return Err(Refusal::DelegationRevoked);
    };
    if delegation.revoked
        || delegation.principal != effect.actor
        || delegation.war_id != policy.id
        || delegation.revision != politics.revision
    {
        return Err(Refusal::DelegationRevoked);
    }
    let protection_ok = match effect.protection {
        Protection::Unprotected => true,
        Protection::OfflineOwner => policy.offline == OfflineProtection::AdmittedConflictContinues,
        Protection::Private | Protection::Neutral | Protection::Unknown => false,
    };
    if !protection_ok {
        return Err(Refusal::Protected);
    }
    if effect.tick < policy.grace_end {
        return Err(Refusal::Grace);
    }
    if !policy
        .windows
        .iter()
        .any(|w| effect.tick >= w.start && effect.tick < w.end)
    {
        return Err(Refusal::WindowClosed);
    }
    // Source deliberately cannot select a weaker policy, including vanilla fire/fluid.
    let _ = effect.source;
    let mut guards = Vec::with_capacity(politics.storage.len() + 1);
    guards.push(effect.policy.clone());
    guards.extend(politics.storage.iter().cloned());
    Ok(Permit {
        event: effect.event.clone(),
        actor: effect.actor.clone(),
        target: effect.target.clone(),
        policy_revision: policy.revision,
        politics_revision: politics.revision,
        guards,
    })
}
/// Until a native owner consumes both guards atomically this is a diagnostic,
/// not a permission cache. Consumers must not issue an unguarded physical effect.
pub(super) fn require_native_guard(_permit: &Permit) -> Result<(), Refusal> {
    Err(Refusal::NativeConditionalGuardRequired)
}
