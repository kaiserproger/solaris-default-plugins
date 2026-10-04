//! CP-060 siege/breach/salvage admission (structural; RULESET DECISION: victory
//! grants NO separate breach/salvage right).
//!
//! A breach or salvage effect needs BOTH a live war admission (Declared policy,
//! in-window tick, current revisions — see `war_policy::authorize`) AND an open
//! siege record for the same war and target. An ended (even victorious) or
//! revoked war admits nothing; salvage additionally needs a breached siege.
//! No separate siege authority is admitted: siege records open and close under
//! the declaring charter-holder, reusing war authority.
use super::war_policy::{EffectFacts, Permit, Policy, PoliticalFacts, Refusal, State};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum SiegeState {
    Open,
    Breached { at: u64, event: String },
    Lifted { at: u64, event: String },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SiegeRecord {
    pub war_id: String,
    pub target: String,
    pub siege_event: String,
    pub started_at: u64,
    pub state: SiegeState,
}
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.'))
}
impl SiegeRecord {
    /// Open a siege against a known lawful target of a live war. Opening is a
    /// record, not an effect: breach still needs `authorize_breach`.
    pub fn open(
        policy: &Policy,
        holder: &str,
        target: &str,
        siege_event: &str,
        tick: u64,
    ) -> Result<Self, Refusal> {
        policy.validate()?;
        if !matches!(policy.state, State::Declared) {
            return Err(Refusal::InvalidRecord);
        }
        if holder != policy.declarer {
            return Err(Refusal::NotCharterHolder);
        }
        if !id(target)
            || !id(siege_event)
            || tick < policy.declared_at
            || !policy.targets.iter().any(|t| t.id == target)
        {
            return Err(Refusal::InvalidRecord);
        }
        Ok(Self {
            war_id: policy.id.clone(),
            target: target.into(),
            siege_event: siege_event.into(),
            started_at: tick,
            state: SiegeState::Open,
        })
    }
    /// Record the breach. Replays preserve the original event; closing an
    /// already-terminal siege with a different event fails.
    pub fn close_breached(
        &mut self,
        holder: &str,
        policy: &Policy,
        event: &str,
        tick: u64,
    ) -> Result<(), Refusal> {
        if holder != policy.declarer {
            return Err(Refusal::NotCharterHolder);
        }
        if !id(event) || tick < self.started_at {
            return Err(Refusal::InvalidRecord);
        }
        match &self.state {
            SiegeState::Open => (),
            SiegeState::Breached { event: old, .. } if old == event => return Ok(()),
            _ => return Err(Refusal::SiegeNotOpen),
        }
        self.state = SiegeState::Breached {
            at: tick,
            event: event.into(),
        };
        Ok(())
    }
    /// Lift the siege without a breach. Same replay rules as breaching.
    pub fn lift(
        &mut self,
        holder: &str,
        policy: &Policy,
        event: &str,
        tick: u64,
    ) -> Result<(), Refusal> {
        if holder != policy.declarer {
            return Err(Refusal::NotCharterHolder);
        }
        if !id(event) || tick < self.started_at {
            return Err(Refusal::InvalidRecord);
        }
        match &self.state {
            SiegeState::Open => (),
            SiegeState::Lifted { event: old, .. } if old == event => return Ok(()),
            _ => return Err(Refusal::SiegeNotOpen),
        }
        self.state = SiegeState::Lifted {
            at: tick,
            event: event.into(),
        };
        Ok(())
    }
}
fn war_admission(
    policy: &Policy,
    politics: &PoliticalFacts,
    effect: &EffectFacts,
    siege: &SiegeRecord,
) -> Result<Permit, Refusal> {
    // Ended (even by victory) or revoked wars admit no breach/salvage: the war
    // predicate runs first, so its refusal wins over the siege pairing and
    // phase checks (unknown targets stay UnknownTarget, not ForbiddenTarget).
    let permit = super::war_policy::authorize(policy, politics, effect)?;
    if siege.war_id != policy.id || siege.target != effect.target {
        return Err(Refusal::ForbiddenTarget);
    }
    Ok(permit)
}
/// Admit one breach effect against an open siege of a live war.
pub(super) fn authorize_breach(
    policy: &Policy,
    politics: &PoliticalFacts,
    effect: &EffectFacts,
    siege: &SiegeRecord,
) -> Result<Permit, Refusal> {
    let permit = war_admission(policy, politics, effect, siege)?;
    if !matches!(siege.state, SiegeState::Open) {
        return Err(Refusal::SiegeNotOpen);
    }
    Ok(permit)
}
/// Admit one salvage effect. Salvage needs a recorded breach on top of the
/// same live-war admission; victory alone never opens it.
pub(super) fn authorize_salvage(
    policy: &Policy,
    politics: &PoliticalFacts,
    effect: &EffectFacts,
    siege: &SiegeRecord,
) -> Result<Permit, Refusal> {
    let permit = war_admission(policy, politics, effect, siege)?;
    if !matches!(siege.state, SiegeState::Breached { .. }) {
        return Err(Refusal::SiegeNotOpen);
    }
    Ok(permit)
}
