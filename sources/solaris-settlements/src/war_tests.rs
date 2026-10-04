//! CP-058 policy unit tests (RULESET DECISION only; no runtime acceptance).
use super::war_policy::*;

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

fn fixtures() -> (Policy, PoliticalFacts, EffectFacts) {
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
        windows: vec![
            Window {
                start: grace_end,
                end: grace_end + 1_000,
            },
            Window {
                start: grace_end + 1_000 + MIN_PAUSE_TICKS,
                end: grace_end + 1_000 + MIN_PAUSE_TICKS + 2_000,
            },
        ],
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
        tick: grace_end,
        source: Source::Player,
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
fn grace_is_exactly_72000_ticks() {
    assert_eq!(GRACE_TICKS, 72_000);
    let (policy, _, _) = fixtures();
    assert_eq!(policy.grace_end - policy.declared_at, 72_000);
    for delta in [0, 1, 71_999, 72_001, 100_000] {
        let mut shifted = policy.clone();
        shifted.grace_end = shifted.declared_at + delta;
        // Keep windows consistent with the shifted grace so only grace is tested.
        let shift = shifted.grace_end as i64 - policy.grace_end as i64;
        for window in &mut shifted.windows {
            window.start = window.start.saturating_add_signed(shift);
            window.end = window.end.saturating_add_signed(shift);
        }
        if delta == 72_000 {
            assert!(shifted.validate().is_ok());
        } else {
            assert_eq!(
                shifted.validate(),
                Err(Refusal::InvalidRecord),
                "delta {delta}"
            );
        }
    }
}

#[test]
fn all_sources_obey_exact_grace_and_half_open_windows() {
    let (policy, politics, mut effect) = fixtures();
    let grace_end = policy.grace_end;
    let w1_end = policy.windows[0].end;
    let w2 = policy.windows[1].clone();
    for source in [
        Source::Player,
        Source::Npc,
        Source::Projectile,
        Source::NativeWork,
        Source::Explosion,
        Source::Fire,
        Source::Fluid,
        Source::Mutation,
    ] {
        effect.source = source;
        for (tick, expected) in [
            (grace_end - 1, Some(Refusal::Grace)),
            (grace_end, None),
            (w1_end - 1, None),
            (w1_end, Some(Refusal::WindowClosed)),
            (w2.start - 1, Some(Refusal::WindowClosed)),
            (w2.start, None),
            (w2.end - 1, None),
            (w2.end, Some(Refusal::WindowClosed)),
        ] {
            effect.tick = tick;
            assert_eq!(
                authorize(&policy, &politics, &effect).err(),
                expected,
                "{source:?} at {tick}"
            );
        }
    }
}

#[test]
fn window_shape_guards_hold() {
    let (policy, _, _) = fixtures();
    // Nine windows refused.
    let mut invalid = policy.clone();
    while invalid.windows.len() <= MAX_WINDOWS {
        invalid.windows.push(Window {
            start: invalid.windows.last().unwrap().end + MIN_PAUSE_TICKS,
            end: invalid.windows.last().unwrap().end + MIN_PAUSE_TICKS + 10,
        });
    }
    assert_eq!(invalid.windows.len(), MAX_WINDOWS + 1);
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    // A window longer than 72000 refused.
    invalid = policy.clone();
    invalid.windows[0].end = invalid.windows[0].start + MAX_WINDOW_TICKS + 1;
    invalid.windows[1].start = invalid.windows[0].end + MIN_PAUSE_TICKS;
    invalid.windows[1].end = invalid.windows[1].start + 10;
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    // A pause shorter than 12000 refused.
    invalid = policy.clone();
    invalid.windows[1].start = invalid.windows[0].end + MIN_PAUSE_TICKS - 1;
    invalid.windows[1].end = invalid.windows[1].start + 10;
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    // Overlapping the grace boundary refused.
    invalid = policy.clone();
    invalid.windows[0].start = invalid.grace_end - 1;
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    // Empty and zero-length windows refused.
    invalid = policy.clone();
    invalid.windows.clear();
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    invalid = policy.clone();
    invalid.windows[1].end = invalid.windows[1].start;
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    // Exactly 8 windows of exactly 72000 with exactly 12000 pauses admit.
    let mut full = policy.clone();
    full.windows.clear();
    let mut start = full.grace_end;
    for _ in 0..MAX_WINDOWS {
        full.windows.push(Window {
            start,
            end: start + MAX_WINDOW_TICKS,
        });
        start += MAX_WINDOW_TICKS + MIN_PAUSE_TICKS;
    }
    assert!(full.validate().is_ok());
}

#[test]
fn targets_bound_and_lawful() {
    let (policy, _, _) = fixtures();
    let mut invalid = policy.clone();
    invalid.targets.clear();
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    invalid = policy.clone();
    while invalid.targets.len() <= MAX_TARGETS {
        let mut extra = invalid.targets[0].clone();
        extra.id = format!("extra-{}", invalid.targets.len());
        invalid.targets.push(extra);
    }
    assert_eq!(invalid.targets.len(), MAX_TARGETS + 1);
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    // Duplicate ids, late delivery, and foreign owners are not lawful.
    invalid = policy.clone();
    invalid.targets.push(invalid.targets[0].clone());
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    invalid = policy.clone();
    invalid.targets[0].delivered_at = policy.declared_at + 1;
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    invalid = policy.clone();
    invalid.targets[0].observed_at = invalid.targets[0].delivered_at + 1;
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    invalid = policy.clone();
    invalid.targets[0].owner = Side {
        house: "house-c".into(),
        realm: None,
    };
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    // Unknown and friendly-side effects fail closed.
    let (_, politics, effect) = fixtures();
    let mut unknown = politics.clone();
    unknown.actor_side = None;
    assert_eq!(
        authorize(&policy, &unknown, &effect),
        Err(Refusal::UnknownSide)
    );
    unknown = politics.clone();
    unknown.target_side = unknown.actor_side.clone();
    assert_eq!(
        authorize(&policy, &unknown, &effect),
        Err(Refusal::WrongSide)
    );
    let mut hidden = effect.clone();
    hidden.target = "unseen-stock-or-person".into();
    assert_eq!(
        authorize(&policy, &politics, &hidden),
        Err(Refusal::UnknownTarget)
    );
    unknown = politics.clone();
    unknown.war_id = Some("other-war".into());
    assert_eq!(
        authorize(&policy, &unknown, &effect),
        Err(Refusal::ForbiddenTarget)
    );
}

#[test]
fn record_size_bound_is_4096_bytes() {
    assert_eq!(VALUE_BYTES, 4096);
    let (policy, _, _) = fixtures();
    assert!(policy.encode().unwrap().len() <= VALUE_BYTES);
    let mut big = policy.clone();
    big.cause = "c".repeat(VALUE_BYTES);
    assert_eq!(big.encode(), Err(Refusal::InvalidRecord));
    let oversized = "x".repeat(VALUE_BYTES + 1);
    assert_eq!(Policy::decode(&oversized), Err(Refusal::InvalidRecord));
    // A tampered ruleset id or missing declarer/cause never validates.
    for broken in [
        {
            let mut p = policy.clone();
            p.ruleset.clear();
            p
        },
        {
            let mut p = policy.clone();
            p.declarer.clear();
            p
        },
        {
            let mut p = policy.clone();
            p.cause.clear();
            p
        },
    ] {
        assert_eq!(broken.validate(), Err(Refusal::InvalidRecord));
    }
}

#[test]
fn only_the_declaring_charter_holder_acts() {
    let (policy, mut politics, effect) = fixtures();
    // A stale or vacant holder closes the effect path.
    politics.holder = Some("holder-b".into());
    assert_eq!(
        authorize(&policy, &politics, &effect),
        Err(Refusal::NotCharterHolder)
    );
    politics.holder = None;
    assert_eq!(
        authorize(&policy, &politics, &effect),
        Err(Refusal::NotCharterHolder)
    );
    // Ending and revoking need the declaring holder too.
    let mut ended = policy.clone();
    assert_eq!(
        ended.end("holder-b", "peace-1", policy.grace_end, Outcome::Peace),
        Err(Refusal::NotCharterHolder)
    );
    let mut revoked = policy.clone();
    assert_eq!(
        revoked.revoke("holder-b", "revoke-1", policy.grace_end),
        Err(Refusal::NotCharterHolder)
    );
}

#[test]
fn peace_and_surrender_end_with_recorded_outcome() {
    for outcome in [Outcome::Peace, Outcome::Surrender] {
        let (mut policy, politics, effect) = fixtures();
        let tick = policy.grace_end + 5;
        let event = if outcome == Outcome::Peace {
            "peace-original"
        } else {
            "surrender-original"
        };
        policy.end("holder-a", event, tick, outcome).unwrap();
        let restart = Policy::decode(&policy.encode().unwrap()).unwrap();
        assert_eq!(authorize(&restart, &politics, &effect), Err(Refusal::Ended));
        // Replay preserves the original event, tick, and outcome.
        let mut replay = restart.clone();
        replay.end("holder-a", event, tick + 3, outcome).unwrap();
        assert_eq!(replay, restart);
        // A different event or outcome on the terminal record fails.
        assert_eq!(
            replay.end("holder-a", "second-peace", tick + 3, outcome),
            Err(Refusal::Ended)
        );
        assert_eq!(
            replay.end(
                "holder-a",
                event,
                tick + 3,
                if outcome == Outcome::Peace {
                    Outcome::Surrender
                } else {
                    Outcome::Peace
                }
            ),
            Err(Refusal::Ended)
        );
        assert_eq!(effect.event, "original-native-effect");
    }
}

#[test]
fn revoke_stops_new_effects_without_reissuing() {
    let (mut policy, politics, mut effect) = fixtures();
    effect.source = Source::NativeWork;
    policy
        .revoke("holder-a", "revocation-original", policy.grace_end + 1)
        .unwrap();
    let mut restart = Policy::decode(&policy.encode().unwrap()).unwrap();
    assert_eq!(
        authorize(&restart, &politics, &effect),
        Err(Refusal::Revoked)
    );
    let revision = restart.revision;
    restart
        .revoke("holder-a", "revocation-original", policy.grace_end + 4)
        .unwrap();
    assert_eq!(restart.revision, revision);
    // Revoking an ended war, or ending a revoked one, fails without new state.
    assert_eq!(
        restart.end(
            "holder-a",
            "peace-late",
            policy.grace_end + 5,
            Outcome::Peace
        ),
        Err(Refusal::Revoked)
    );
    assert_eq!(effect.event, "original-native-effect");
}

#[test]
fn window_expiry_without_outcome_names_no_winner() {
    let (policy, _, _) = fixtures();
    // A live war past its last window stops effects but records no outcome.
    assert_eq!(joint_outcome(&policy, None), None);
    assert_eq!(
        joint_outcome(&policy, Some(("siege-1", policy.grace_end))),
        None
    );
    let (mut ended, _, _) = fixtures();
    ended
        .end("holder-a", "peace-1", ended.grace_end + 1, Outcome::Peace)
        .unwrap();
    // Peace alone is not a joint outcome: CP-063 needs the terminal siege too.
    assert_eq!(joint_outcome(&ended, None), None);
    // A siege that closed before the agreement cannot back it.
    let ended_at = match &ended.state {
        State::Ended { at, .. } => *at,
        _ => unreachable!(),
    };
    assert_eq!(
        joint_outcome(&ended, Some(("breach-1", ended_at - 1))),
        None
    );
    let joint = joint_outcome(&ended, Some(("breach-1", ended_at))).unwrap();
    assert_eq!(joint.event, "peace-1");
    assert_eq!(joint.outcome, Outcome::Peace);
    assert_eq!(joint.at, ended_at);
    assert_eq!(joint.siege_event, "breach-1");
}

#[test]
fn auto_contribution_is_zero() {
    assert_eq!(AUTO_CONTRIBUTION, 0);
    let (policy, politics, _) = fixtures();
    assert_eq!(auto_contribution(&policy, &politics), 0);
}

#[test]
fn offline_private_neutral_unknown_never_bypass() {
    let (mut policy, politics, mut effect) = fixtures();
    for protection in [
        Protection::Private,
        Protection::Neutral,
        Protection::Unknown,
        Protection::OfflineOwner,
    ] {
        effect.protection = protection.clone();
        assert_eq!(
            authorize(&policy, &politics, &effect),
            Err(Refusal::Protected),
            "{protection:?} under Protected"
        );
    }
    // AdmittedConflictContinues covers the already-admitted lawful conflict in
    // the current window — and nothing else.
    policy.offline = OfflineProtection::AdmittedConflictContinues;
    effect.protection = Protection::OfflineOwner;
    assert!(authorize(&policy, &politics, &effect).is_ok());
    for protection in [
        Protection::Private,
        Protection::Neutral,
        Protection::Unknown,
    ] {
        effect.protection = protection.clone();
        assert_eq!(
            authorize(&policy, &politics, &effect),
            Err(Refusal::Protected),
            "{protection:?} under AdmittedConflictContinues"
        );
    }
    // No new offline targets: an unknown target stays unknown offline.
    let mut hidden = effect.clone();
    hidden.protection = Protection::OfflineOwner;
    hidden.target = "unseen-stock-or-person".into();
    assert_eq!(
        authorize(&policy, &politics, &hidden),
        Err(Refusal::UnknownTarget)
    );
}

#[test]
fn revoked_delegation_or_changed_rights_refuses_old_admission() {
    let (mut policy, mut politics, mut effect) = fixtures();
    let original = authorize(&policy, &politics, &effect).unwrap();
    assert_eq!(original.guards[0].version, 31);
    assert_eq!(original.guards[1].version, 37);
    politics.revision += 1;
    politics.storage[0].version += 1;
    assert_eq!(
        authorize(&policy, &politics, &effect),
        Err(Refusal::StalePolitics)
    );
    effect.admitted_politics_revision = politics.revision;
    let delegation = politics.delegation.as_mut().unwrap();
    delegation.revision = politics.revision;
    delegation.revoked = true;
    assert_eq!(
        authorize(&policy, &politics, &effect),
        Err(Refusal::DelegationRevoked)
    );
    policy.revision += 1;
    assert_eq!(
        authorize(&policy, &politics, &effect),
        Err(Refusal::StalePolicy)
    );
    assert_eq!(
        require_native_guard(&original),
        Err(Refusal::NativeConditionalGuardRequired),
        "a previously valid predicate is not a native commit fence"
    );
}

#[test]
fn malformed_admission_never_creates_implicit_window_or_knowledge() {
    let (policy, _, _) = fixtures();
    let mut invalid = policy.clone();
    invalid.windows.clear();
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    invalid = policy.clone();
    invalid.attacker.house.clone_from(&invalid.defender.house);
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    invalid = policy.clone();
    invalid.targets[0].delivered_at = policy.declared_at + 1;
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    invalid = policy.clone();
    invalid.targets.push(invalid.targets[0].clone());
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    invalid = policy.clone();
    invalid.windows[1].start = invalid.windows[0].end + MIN_PAUSE_TICKS - 1;
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
    invalid = policy;
    invalid.revision = 0;
    assert_eq!(invalid.validate(), Err(Refusal::InvalidRecord));
}
