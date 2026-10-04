//! CP-060 siege/breach/salvage admission tests (structural; no runtime claim).
use super::siege_policy::*;
use super::war_policy::*;

fn war() -> (Policy, PoliticalFacts, EffectFacts) {
    let attacker = Side {
        house: "house-a".into(),
        realm: Some("realm-a".into()),
    };
    let defender = Side {
        house: "house-b".into(),
        realm: Some("realm-b".into()),
    };
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
            key: key(&policy.id).unwrap(),
            version: 31,
        },
        admitted_revision: 1,
        admitted_politics_revision: 7,
    };
    (policy, politics, effect)
}

#[test]
fn only_the_declaring_holder_opens_and_closes_sieges() {
    let (policy, _, _) = war();
    let siege = SiegeRecord::open(
        &policy,
        "holder-a",
        "known-gate",
        "siege-1",
        policy.grace_end,
    )
    .unwrap();
    assert!(matches!(siege.state, SiegeState::Open));
    assert_eq!(
        SiegeRecord::open(
            &policy,
            "holder-b",
            "known-gate",
            "siege-1",
            policy.grace_end
        ),
        Err(Refusal::NotCharterHolder)
    );
    assert_eq!(
        SiegeRecord::open(
            &policy,
            "holder-a",
            "unseen-stock",
            "siege-1",
            policy.grace_end
        ),
        Err(Refusal::InvalidRecord)
    );
    let mut siege = siege;
    assert_eq!(
        siege.close_breached("holder-b", &policy, "breach-1", policy.grace_end + 1),
        Err(Refusal::NotCharterHolder)
    );
    siege
        .close_breached("holder-a", &policy, "breach-1", policy.grace_end + 1)
        .unwrap();
    let replay_revision = siege.clone();
    siege
        .close_breached("holder-a", &policy, "breach-1", policy.grace_end + 2)
        .unwrap();
    assert_eq!(siege, replay_revision);
    assert_eq!(
        siege.close_breached("holder-a", &policy, "breach-2", policy.grace_end + 3),
        Err(Refusal::SiegeNotOpen)
    );
    assert_eq!(
        siege.lift("holder-a", &policy, "lift-1", policy.grace_end + 4),
        Err(Refusal::SiegeNotOpen)
    );
}

#[test]
fn breach_needs_open_siege_of_a_live_war() {
    let (policy, politics, effect) = war();
    let siege = SiegeRecord::open(
        &policy,
        "holder-a",
        "known-gate",
        "siege-1",
        policy.grace_end,
    )
    .unwrap();
    assert!(authorize_breach(&policy, &politics, &effect, &siege).is_ok());
    // A foreign war/target pairing never reaches the siege.
    let mut foreign = siege.clone();
    foreign.war_id = "other-war".into();
    assert_eq!(
        authorize_breach(&policy, &politics, &effect, &foreign),
        Err(Refusal::ForbiddenTarget)
    );
    let mut other_target = effect.clone();
    other_target.target = "other-gate".into();
    assert_eq!(
        authorize_breach(&policy, &politics, &other_target, &siege),
        Err(Refusal::UnknownTarget)
    );
    // A lifted siege admits no breach.
    let mut lifted = siege.clone();
    lifted
        .lift("holder-a", &policy, "lift-1", policy.grace_end + 1)
        .unwrap();
    assert_eq!(
        authorize_breach(&policy, &politics, &effect, &lifted),
        Err(Refusal::SiegeNotOpen)
    );
    // Peace stops breaches even through an open siege: the war refusal wins.
    let (mut ended, politics, effect) = war();
    ended
        .end("holder-a", "peace-1", ended.grace_end + 1, Outcome::Peace)
        .unwrap();
    assert_eq!(
        authorize_breach(&ended, &politics, &effect, &siege),
        Err(Refusal::Ended)
    );
}

#[test]
fn salvage_needs_a_recorded_breach_and_a_live_war() {
    let (policy, politics, effect) = war();
    let mut siege = SiegeRecord::open(
        &policy,
        "holder-a",
        "known-gate",
        "siege-1",
        policy.grace_end,
    )
    .unwrap();
    // Victory alone opens nothing: the siege is still merely open.
    assert_eq!(
        authorize_salvage(&policy, &politics, &effect, &siege),
        Err(Refusal::SiegeNotOpen)
    );
    siege
        .close_breached("holder-a", &policy, "breach-1", policy.grace_end + 1)
        .unwrap();
    assert!(authorize_salvage(&policy, &politics, &effect, &siege).is_ok());
    // Revocation stops salvage through a breached siege.
    let (mut revoked, politics, effect) = war();
    revoked
        .revoke("holder-a", "revoke-1", revoked.grace_end + 1)
        .unwrap();
    assert_eq!(
        authorize_salvage(&revoked, &politics, &effect, &siege),
        Err(Refusal::Revoked)
    );
}
