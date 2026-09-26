use super::*;
use std::collections::BTreeSet;

use crate::demography::{
    add_member, advance_intent, commit_birth, form_household, found_household, may_issue_native,
    record_age_origin, record_birth, record_intent, remove_member, set_home, set_policy,
    BirthIntent, Formation, Intent, IntentState, MigrationIntent, Origin, Provenance, Refusal,
    RelocationIntent, Removal, MAX_DEMOGRAPHY_INTENTS, MAX_HOUSEHOLD_MEMBERS,
    MAX_SETTLEMENT_HOUSEHOLDS,
};

fn resident(handle: &str) -> SettlementResident {
    SettlementResident {
        handle: handle.to_owned(),
        work_revision: 0,
        role: None,
        employer: None,
        hiring: None,
        issued: None,
        dismissing: None,
        capturing: None,
        captive_by: None,
        household: None,
        age_origin: None,
    }
}

fn households() -> SettlementRegistry {
    let mut registry = SettlementRegistry::default();
    register_settlement(&mut registry, "owner".to_owned(), "oak_hollow", "small").unwrap();
    register_settlement(&mut registry, "owner".to_owned(), "pine_ridge", "small").unwrap();
    for (alias, handle) in [
        ("ada", "resident-handle-1"),
        ("bram", "resident-handle-2"),
        ("cora", "resident-handle-3"),
    ] {
        record_settlement_resident(&mut registry, "oak_hollow", alias, handle, 0).unwrap();
    }
    record_settlement_resident(&mut registry, "pine_ridge", "dara", "resident-handle-4", 0)
        .unwrap();
    registry
}

/// The durable ref a resident record carries, which is the authority a
/// household membership is read from.
fn bound_household(
    registry: &SettlementRegistry,
    settlement: &str,
    handle: &str,
) -> Option<String> {
    registry.settlements[settlement]
        .residents
        .values()
        .find(|resident| resident.handle == handle)
        .expect("recorded resident")
        .household
        .clone()
}

fn recorded_age_origin(
    registry: &SettlementRegistry,
    settlement: &str,
    handle: &str,
) -> Option<u64> {
    registry.settlements[settlement]
        .residents
        .values()
        .find(|resident| resident.handle == handle)
        .expect("recorded resident")
        .age_origin
}

#[test]
fn household_formation_binds_distinct_members_and_retires_when_empty() {
    let mut registry = households();
    let household = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1", "resident-handle-2"],
    )
    .unwrap();

    assert_eq!(household, "household-1");
    assert_eq!(registry.household_sequence, 1);
    let record = &registry.households[&household];
    assert_eq!(record.settlement, "oak_hollow");
    assert_eq!(record.origin, Origin::Adoption);
    assert_eq!(record.revision, 1);
    assert_eq!(
        record.members,
        BTreeSet::from([
            "resident-handle-1".to_owned(),
            "resident-handle-2".to_owned()
        ])
    );
    assert!(record.home.is_none() && record.needs.is_none() && record.policy.is_none());
    // The resident records carry the ref, so one person belongs to one household.
    assert_eq!(
        bound_household(&registry, "oak_hollow", "resident-handle-1"),
        Some(household.clone())
    );
    assert_eq!(
        bound_household(&registry, "oak_hollow", "resident-handle-2"),
        Some(household.clone())
    );

    // Formation refuses a repeated handle, an already bound resident, an
    // unknown resident, no member at all, and an unknown settlement.
    assert_eq!(
        found_household(
            &mut registry,
            "oak_hollow",
            Origin::Split,
            &["resident-handle-3", "resident-handle-3"]
        ),
        Err(Refusal::DuplicateMember)
    );
    assert_eq!(
        found_household(
            &mut registry,
            "oak_hollow",
            Origin::Split,
            &["resident-handle-1"]
        ),
        Err(Refusal::AlreadyInHousehold)
    );
    assert_eq!(
        found_household(
            &mut registry,
            "oak_hollow",
            Origin::Split,
            &["resident-handle-9"]
        ),
        Err(Refusal::UnknownResident)
    );
    assert_eq!(
        found_household(&mut registry, "oak_hollow", Origin::Split, &[] as &[&str]),
        Err(Refusal::EmptyMembers)
    );
    assert_eq!(
        found_household(
            &mut registry,
            "nowhere",
            Origin::Split,
            &["resident-handle-3"]
        ),
        Err(Refusal::UnknownSettlement)
    );
    // No refusal consumed a household id.
    let second = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Split,
        &["resident-handle-3"],
    )
    .unwrap();
    assert_eq!(second, "household-2");
    assert_eq!(registry.household_sequence, 2);

    // Membership refuses a duplicate, a resident of another household, and an
    // unknown resident; every accepted change moves the revision.
    assert_eq!(
        add_member(&mut registry, &household, "resident-handle-1", 1),
        Err(Refusal::DuplicateMember)
    );
    assert_eq!(
        add_member(&mut registry, &second, "resident-handle-1", 1),
        Err(Refusal::AlreadyInHousehold)
    );
    assert_eq!(
        add_member(&mut registry, &second, "resident-handle-9", 1),
        Err(Refusal::UnknownResident)
    );
    assert_eq!(
        remove_member(&mut registry, &second, "resident-handle-9", 1),
        Err(Refusal::NotAMember)
    );
    remove_member(&mut registry, &household, "resident-handle-2", 1).unwrap();
    assert_eq!(registry.households[&household].revision, 2);
    assert_eq!(
        bound_household(&registry, "oak_hollow", "resident-handle-2"),
        None
    );
    assert_eq!(
        registry.households[&household].members,
        BTreeSet::from(["resident-handle-1".to_owned()])
    );
    set_policy(
        &mut registry,
        &household,
        Some("needs.basic"),
        Some("policy.household"),
        2,
    )
    .unwrap();
    assert_eq!(
        registry.households[&household].needs.as_deref(),
        Some("needs.basic")
    );
    assert_eq!(
        registry.households[&household].policy.as_deref(),
        Some("policy.household")
    );

    // The last removal retires the household and releases its member.
    assert_eq!(
        remove_member(&mut registry, &second, "resident-handle-3", 1).unwrap(),
        Removal::Retired
    );
    assert!(!registry.households.contains_key(&second));
    assert_eq!(
        bound_household(&registry, "oak_hollow", "resident-handle-3"),
        None
    );
    assert_eq!(
        add_member(&mut registry, &second, "resident-handle-3", 0),
        Err(Refusal::UnknownHousehold)
    );
}

#[test]
fn a_dead_last_guardian_leaves_a_vacant_holder_until_its_birth_settles() {
    let mut registry = households();
    let household = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();
    let birth = record_birth(
        &mut registry,
        "birth-vacant",
        &household,
        &["resident-handle-1".to_owned()],
        "site-1",
        "home-poi-9",
        3,
        4_000,
    )
    .expect("one household parent records one birth");

    // Core death is authoritative: the last guardian leaves even with an open
    // birth, and the household stays vacant as that intent's holder.
    let revision = registry.households[&household].revision;
    assert_eq!(
        remove_member(&mut registry, &household, "resident-handle-1", revision).unwrap(),
        Removal::Updated
    );
    assert!(registry.households[&household].members.is_empty());
    assert_eq!(may_issue_native(&registry, "birth-vacant"), Ok(true));

    // The committed birth fills the vacant holder with its recorded child.
    assert_eq!(
        commit_birth(&mut registry, "birth-vacant", "resident-handle-9"),
        Ok(IntentState::Committed)
    );
    assert_eq!(
        registry.settlements["oak_hollow"].residents[&birth.child_alias]
            .household
            .as_deref(),
        Some(household.as_str())
    );
    assert_eq!(
        registry.households[&household].members,
        BTreeSet::from(["resident-handle-9".to_owned()])
    );

    // Without an open birth the same death retires the household instead.
    let lone = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Split,
        &["resident-handle-2"],
    )
    .unwrap();
    let revision = registry.households[&lone].revision;
    assert_eq!(
        remove_member(&mut registry, &lone, "resident-handle-2", revision).unwrap(),
        Removal::Retired
    );
    assert!(!registry.households.contains_key(&lone));
}

#[test]
fn one_household_id_never_names_two_member_sets() {
    let mut registry = households();
    let members = [
        "resident-handle-1".to_owned(),
        "resident-handle-2".to_owned(),
    ];
    assert_eq!(
        form_household(
            &mut registry,
            "household-1",
            "oak_hollow",
            Origin::Adoption,
            &members
        ),
        Ok(Formation::Created)
    );
    // The same durable formation replayed after an unclear CAS answer is the
    // same household, not a second one.
    assert_eq!(
        form_household(
            &mut registry,
            "household-1",
            "oak_hollow",
            Origin::Adoption,
            &members
        ),
        Ok(Formation::Replayed)
    );
    assert_eq!(registry.households.len(), 1);
    assert_eq!(registry.households["household-1"].revision, 1);

    // A used id naming any other member set, settlement, or origin is refused
    // and leaves the household it already names untouched.
    let other = [
        "resident-handle-1".to_owned(),
        "resident-handle-3".to_owned(),
    ];
    assert_eq!(
        form_household(
            &mut registry,
            "household-1",
            "oak_hollow",
            Origin::Adoption,
            &other
        ),
        Err(Refusal::HouseholdIdentityConflict)
    );
    assert_eq!(
        form_household(
            &mut registry,
            "household-1",
            "pine_ridge",
            Origin::Split,
            &members
        ),
        Err(Refusal::HouseholdIdentityConflict)
    );
    assert_eq!(registry.households.len(), 1);
    assert_eq!(
        registry.households["household-1"].members,
        BTreeSet::from(members.clone())
    );
    assert_eq!(
        bound_household(&registry, "oak_hollow", "resident-handle-3"),
        None
    );
}

#[test]
fn a_household_holds_one_home_and_its_settlement_owns_the_claim() {
    let mut registry = households();
    let first = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();
    let second = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Split,
        &["resident-handle-2"],
    )
    .unwrap();

    set_home(&mut registry, &first, "home-poi-1", 1).unwrap();
    assert_eq!(
        registry.households[&first].home.as_deref(),
        Some("home-poi-1")
    );
    assert_eq!(registry.households[&first].revision, 2);
    // The same home again is the same decision, not a second one.
    set_home(&mut registry, &first, "home-poi-1", 2).unwrap();
    assert_eq!(registry.households[&first].revision, 2);
    // A second home needs the relocation decision this slice does not make.
    assert_eq!(
        set_home(&mut registry, &first, "home-poi-2", 2),
        Err(Refusal::SecondHome)
    );
    assert_eq!(
        registry.households[&first].home.as_deref(),
        Some("home-poi-1")
    );
    assert_eq!(registry.households[&first].revision, 2);
    // Two households of one settlement never claim the same home.
    assert_eq!(
        set_home(&mut registry, &second, "home-poi-1", 1),
        Err(Refusal::HomeClaimed)
    );
    assert!(registry.households[&second].home.is_none());
    // The claim is per settlement: another settlement may name the same POI.
    let elsewhere = found_household(
        &mut registry,
        "pine_ridge",
        Origin::Refugee,
        &["resident-handle-4"],
    )
    .unwrap();
    set_home(&mut registry, &elsewhere, "home-poi-1", 1).unwrap();
    assert_eq!(
        registry.households[&elsewhere].home.as_deref(),
        Some("home-poi-1")
    );
}

#[test]
fn member_household_and_intent_bounds_refuse_instead_of_growing() {
    let mut registry = households();
    {
        let record = registry.settlements.get_mut("oak_hollow").unwrap();
        for index in 0..=MAX_SETTLEMENT_HOUSEHOLDS + MAX_HOUSEHOLD_MEMBERS {
            record.residents.insert(
                format!("crowd_{index}"),
                resident(&format!("crowd-handle-{index}")),
            );
        }
    }
    let full: Vec<String> = (0..MAX_HOUSEHOLD_MEMBERS)
        .map(|index| format!("crowd-handle-{index}"))
        .collect();
    let household = found_household(&mut registry, "oak_hollow", Origin::Adoption, &full).unwrap();
    assert_eq!(
        registry.households[&household].members.len(),
        MAX_HOUSEHOLD_MEMBERS
    );

    // One more member refuses through both the addition and the formation.
    let extra = format!("crowd-handle-{MAX_HOUSEHOLD_MEMBERS}");
    assert_eq!(
        add_member(&mut registry, &household, &extra, 1),
        Err(Refusal::MemberLimit)
    );
    let mut too_many = full.clone();
    too_many.push(extra.clone());
    assert_eq!(
        found_household(&mut registry, "oak_hollow", Origin::Split, &too_many),
        Err(Refusal::MemberLimit)
    );

    // Every further household of the settlement refuses at the household bound.
    let mut index = MAX_HOUSEHOLD_MEMBERS;
    while registry.households.len() < MAX_SETTLEMENT_HOUSEHOLDS {
        found_household(
            &mut registry,
            "oak_hollow",
            Origin::Adoption,
            &[format!("crowd-handle-{index}")],
        )
        .unwrap();
        index += 1;
    }
    assert_eq!(registry.households.len(), MAX_SETTLEMENT_HOUSEHOLDS);
    let overflow = format!(
        "crowd-handle-{}",
        MAX_SETTLEMENT_HOUSEHOLDS + MAX_HOUSEHOLD_MEMBERS
    );
    assert_eq!(
        found_household(&mut registry, "oak_hollow", Origin::Adoption, &[overflow]),
        Err(Refusal::HouseholdLimit)
    );

    // The durable intent history is bounded too.
    let mut intents = households();
    let household = found_household(
        &mut intents,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();
    for index in 0..MAX_DEMOGRAPHY_INTENTS {
        let intent = Intent::Migration(MigrationIntent {
            household: household.clone(),
            provenance: Provenance::ExternalFlow,
            target_settlement: "pine_ridge".to_owned(),
            expected_site_revision: index as u64,
            state: IntentState::Recorded,
        });
        record_intent(&mut intents, &format!("migrate-{index}"), intent).unwrap();
    }
    let overflow = Intent::Migration(MigrationIntent {
        household,
        provenance: Provenance::ExternalFlow,
        target_settlement: "pine_ridge".to_owned(),
        expected_site_revision: MAX_DEMOGRAPHY_INTENTS as u64,
        state: IntentState::Recorded,
    });
    assert_eq!(
        record_intent(&mut intents, "migrate-overflow", overflow),
        Err(Refusal::IntentLimit)
    );
}

#[test]
fn a_stale_revision_changes_nothing() {
    let mut registry = households();
    let household = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();
    let frozen = serde_json::to_string(&registry).unwrap();

    assert_eq!(
        add_member(&mut registry, &household, "resident-handle-2", 0),
        Err(Refusal::StaleRevision)
    );
    assert_eq!(
        remove_member(&mut registry, &household, "resident-handle-1", 7),
        Err(Refusal::StaleRevision)
    );
    assert_eq!(
        set_home(&mut registry, &household, "home-poi-1", 0),
        Err(Refusal::StaleRevision)
    );
    assert_eq!(
        set_policy(&mut registry, &household, Some("needs.basic"), None, 0),
        Err(Refusal::StaleRevision)
    );
    assert_eq!(serde_json::to_string(&registry).unwrap(), frozen);

    // The revision the caller read still applies.
    add_member(&mut registry, &household, "resident-handle-2", 1).unwrap();
    assert_eq!(registry.households[&household].revision, 2);
    assert_eq!(
        bound_household(&registry, "oak_hollow", "resident-handle-2").as_deref(),
        Some(household.as_str())
    );
}

#[test]
fn a_replayed_intent_applies_no_second_effect() {
    let mut registry = households();
    let household = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();
    set_home(&mut registry, &household, "home-poi-1", 1).unwrap();
    let relocation = Intent::Relocation(RelocationIntent {
        household: household.clone(),
        from: "oak_hollow".to_owned(),
        to: "pine_ridge".to_owned(),
        old_home: Some("home-poi-1".to_owned()),
        new_home_token: "site-token-9".to_owned(),
        state: IntentState::Recorded,
    });

    assert_eq!(
        record_intent(&mut registry, "move-1", relocation.clone()),
        Ok(IntentState::Recorded)
    );
    assert_eq!(may_issue_native(&registry, "move-1"), Ok(true));
    let recorded = serde_json::to_string(&registry).unwrap();

    // The same operation id and plan replayed after an unclear answer changes
    // nothing, and a restart reissues the same idempotent native request until
    // its authoritative result is projected: the intent stays recorded.
    assert_eq!(
        record_intent(&mut registry, "move-1", relocation.clone()),
        Ok(IntentState::Recorded)
    );
    assert_eq!(serde_json::to_string(&registry).unwrap(), recorded);
    assert_eq!(may_issue_native(&registry, "move-1"), Ok(true));

    // The same id with another plan is a conflict, not a second intent.
    let conflicting = Intent::Relocation(RelocationIntent {
        household: household.clone(),
        from: "oak_hollow".to_owned(),
        to: "pine_ridge".to_owned(),
        old_home: Some("home-poi-1".to_owned()),
        new_home_token: "site-token-10".to_owned(),
        state: IntentState::Recorded,
    });
    assert_eq!(
        record_intent(&mut registry, "move-1", conflicting),
        Err(Refusal::IntentConflict)
    );
    assert_eq!(serde_json::to_string(&registry).unwrap(), recorded);

    // A final intent never moves again, and re-asserting its state is a no-op.
    assert_eq!(
        advance_intent(&mut registry, "move-1", IntentState::Committed),
        Ok(IntentState::Committed)
    );
    assert_eq!(
        advance_intent(&mut registry, "move-1", IntentState::Refused),
        Err(Refusal::InvalidTransition)
    );
    assert_eq!(
        advance_intent(&mut registry, "move-1", IntentState::Committed),
        Ok(IntentState::Committed)
    );
    assert_eq!(
        advance_intent(&mut registry, "move-2", IntentState::Refused),
        Err(Refusal::UnknownIntent)
    );
    // Committing the intent moved the household nowhere: it records the
    // decision, and no slice executes it yet.
    assert_eq!(registry.households[&household].settlement, "oak_hollow");
    assert_eq!(
        registry.households[&household].home.as_deref(),
        Some("home-poi-1")
    );
}

#[test]
fn an_intent_is_validated_against_the_records_it_names() {
    let mut registry = households();
    let household = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();

    // A migration into the settlement the household already lives in is none.
    assert_eq!(
        record_intent(
            &mut registry,
            "migrate-same",
            Intent::Migration(MigrationIntent {
                household: household.clone(),
                provenance: Provenance::OtherCommunity,
                target_settlement: "oak_hollow".to_owned(),
                expected_site_revision: 3,
                state: IntentState::Recorded,
            })
        ),
        Err(Refusal::InvalidIntent)
    );
    assert_eq!(
        record_intent(
            &mut registry,
            "migrate-elsewhere",
            Intent::Migration(MigrationIntent {
                household: household.clone(),
                provenance: Provenance::Refugee,
                target_settlement: "pine_ridge".to_owned(),
                expected_site_revision: 3,
                state: IntentState::Recorded,
            })
        ),
        Ok(IntentState::Recorded)
    );
    // A household the registry does not hold refuses before any native effect.
    assert_eq!(
        record_birth(
            &mut registry,
            "birth-unknown",
            "household-9",
            &["resident-handle-1".to_owned()],
            "site-1",
            "home-poi-9",
            3,
            4_000,
        ),
        Err(Refusal::UnknownHousehold)
    );
    assert_eq!(
        record_intent(
            &mut registry,
            "move-home",
            Intent::Relocation(RelocationIntent {
                household: household.clone(),
                from: "oak_hollow".to_owned(),
                to: "pine_ridge".to_owned(),
                old_home: Some("home-poi-1".to_owned()),
                new_home_token: "site-token-4".to_owned(),
                state: IntentState::Recorded,
            })
        ),
        Err(Refusal::HomeMismatch)
    );
    // A birth needs one or two distinct parents of this household.
    assert_eq!(
        record_birth(
            &mut registry,
            "birth-twice",
            &household,
            &[
                "resident-handle-1".to_owned(),
                "resident-handle-1".to_owned(),
            ],
            "site-1",
            "home-poi-9",
            3,
            4_000,
        ),
        Err(Refusal::InvalidIntent)
    );
    // A parent of another settlement is not this household's parent.
    assert_eq!(
        record_birth(
            &mut registry,
            "birth-unrelated",
            &household,
            &["resident-handle-4".to_owned()],
            "site-1",
            "home-poi-9",
            3,
            4_000,
        ),
        Err(Refusal::UnknownResident)
    );
    let birth = record_birth(
        &mut registry,
        "birth-ok",
        &household,
        &["resident-handle-1".to_owned()],
        "site-1",
        "home-poi-9",
        3,
        4_000,
    )
    .expect("one household parent records one birth");
    assert_eq!(birth.state, IntentState::Recorded);
    assert_eq!(may_issue_native(&registry, "birth-ok"), Ok(true));
    // A birth never records through the generic path: it needs its reserved
    // alias and operation ids.
    assert_eq!(
        record_intent(
            &mut registry,
            "birth-late",
            Intent::Birth(BirthIntent {
                household: household.clone(),
                parents: vec!["resident-handle-1".to_owned()],
                site_id: "site-1".to_owned(),
                poi_id: "home-poi-9".to_owned(),
                expected_site_revision: 3,
                child_alias: "newborn".to_owned(),
                birth_tick: 4_000,
                reserve_operation_id: "birth-reserve-1".to_owned(),
                spawn_operation_id: "birth-spawn-1".to_owned(),
                state: IntentState::Recorded,
            })
        ),
        Err(Refusal::InvalidIntent)
    );
}

#[test]
fn an_age_origin_is_recorded_once_as_an_absolute_tick() {
    let mut registry = households();
    record_age_origin(&mut registry, "oak_hollow", "resident-handle-1", 4_000).unwrap();
    assert_eq!(
        recorded_age_origin(&registry, "oak_hollow", "resident-handle-1"),
        Some(4_000)
    );
    // Replaying the same tick is the same fact; a different one is refused.
    record_age_origin(&mut registry, "oak_hollow", "resident-handle-1", 4_000).unwrap();
    assert_eq!(
        record_age_origin(&mut registry, "oak_hollow", "resident-handle-1", 4_001),
        Err(Refusal::AgeOriginAlreadySet)
    );
    assert_eq!(
        recorded_age_origin(&registry, "oak_hollow", "resident-handle-1"),
        Some(4_000)
    );
    assert_eq!(
        record_age_origin(&mut registry, "pine_ridge", "resident-handle-1", 4_000),
        Err(Refusal::UnknownResident)
    );
    assert_eq!(
        record_age_origin(&mut registry, "nowhere", "resident-handle-1", 4_000),
        Err(Refusal::UnknownSettlement)
    );
}

#[test]
fn an_old_registry_without_households_loads_and_accepts_one() {
    // The shape a save written before this slice has: no household fields.
    let old = concat!(
        r#"{"settlements":{"oak_hollow":{"owner":"owner","size_class":"small","revision":1,"#,
        r#""residents":{"ada":{"handle":"resident-handle-1"}}}}}"#
    );
    let mut registry: SettlementRegistry = serde_json::from_str(old).unwrap();
    assert_eq!(registry.household_sequence, 0);
    assert!(registry.households.is_empty() && registry.demography_intents.is_empty());
    let ada = &registry.settlements["oak_hollow"].residents["ada"];
    assert!(ada.household.is_none() && ada.age_origin.is_none());

    // The loaded registry accepts a household, and the new fields round-trip.
    let household = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();
    assert_eq!(household, "household-1");
    let round_trip: SettlementRegistry =
        serde_json::from_str(&serde_json::to_string(&registry).unwrap()).unwrap();
    assert_eq!(round_trip.households, registry.households);
    assert_eq!(round_trip.household_sequence, 1);
    assert_eq!(
        round_trip.settlements["oak_hollow"].residents["ada"]
            .household
            .as_deref(),
        Some("household-1")
    );
}

#[test]
fn a_committed_birth_registers_one_child_and_replays_as_the_same_child() {
    let mut registry = households();
    let household = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();
    let birth = record_birth(
        &mut registry,
        "birth-1",
        &household,
        &["resident-handle-1".to_owned()],
        "site-1",
        "home-poi-9",
        3,
        4_000,
    )
    .expect("one household parent records one birth");
    let frozen = serde_json::to_string(&registry).unwrap();
    assert_eq!(
        commit_birth(&mut registry, "birth-1", "resident-handle-9"),
        Ok(IntentState::Committed)
    );
    assert_birth_evidence(
        &registry,
        "oak_hollow",
        &household,
        &birth.child_alias,
        "resident-handle-9",
        4_000,
        "birth-1",
    );

    // Replaying the same child alias and handle closes nothing twice: the
    // roster, the household and the intent are already there.
    let replayed = serde_json::to_string(&registry).unwrap();
    assert_eq!(
        commit_birth(&mut registry, "birth-1", "resident-handle-9"),
        Ok(IntentState::Committed)
    );
    assert_eq!(serde_json::to_string(&registry).unwrap(), replayed);

    // A conflicting record under the same alias is refused, never repaired.
    assert_eq!(
        commit_birth(&mut registry, "birth-1", "resident-handle-8"),
        Ok(IntentState::Committed)
    );
    assert_eq!(serde_json::to_string(&registry).unwrap(), replayed);
    assert_ne!(frozen, replayed);
}

#[test]
fn a_replayed_refused_birth_returns_its_state_after_its_household_retired() {
    let mut registry = households();
    let household = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();
    let birth = record_birth(
        &mut registry,
        "birth-doomed",
        &household,
        &["resident-handle-1".to_owned()],
        "site-1",
        "home-poi-9",
        3,
        4_000,
    )
    .expect("one household parent records one birth");

    // The guardian dies, the household stays vacant, then the native effect
    // refuses and the vacant holder retires with it.
    let revision = registry.households[&household].revision;
    assert_eq!(
        remove_member(&mut registry, &household, "resident-handle-1", revision).unwrap(),
        Removal::Updated
    );
    assert_eq!(
        advance_intent(&mut registry, "birth-doomed", IntentState::Refused),
        Ok(IntentState::Refused)
    );
    assert!(!registry.households.contains_key(&household));

    // The same notification replays the persisted refusal, not a new intent:
    // no household, no sequence advance, no second child.
    let frozen = serde_json::to_string(&registry).unwrap();
    let replayed = record_birth(
        &mut registry,
        "birth-doomed",
        &household,
        &["resident-handle-1".to_owned()],
        "site-1",
        "home-poi-9",
        3,
        4_000,
    )
    .expect("a refused decision replays its intent");
    assert_eq!(replayed.child_alias, birth.child_alias);
    assert_eq!(replayed.state, IntentState::Refused);
    assert_eq!(may_issue_native(&registry, "birth-doomed"), Ok(false));
    assert_eq!(serde_json::to_string(&registry).unwrap(), frozen);
}

#[test]
fn a_second_open_birth_over_the_last_slot_refuses_before_any_native_effect() {
    let mut registry = households();
    // One household with room for exactly one more member.
    let mut members = Vec::new();
    for index in 0..MAX_HOUSEHOLD_MEMBERS - 1 {
        let handle = format!("crowd-handle-{index}");
        registry
            .settlements
            .get_mut("oak_hollow")
            .unwrap()
            .residents
            .insert(format!("crowd_{index}"), resident(&handle));
        members.push(handle);
    }
    let member_refs: Vec<&str> = members.iter().map(String::as_str).collect();
    let household =
        found_household(&mut registry, "oak_hollow", Origin::Adoption, &member_refs).unwrap();
    record_birth(
        &mut registry,
        "birth-first",
        &household,
        &[members[0].clone()],
        "site-1",
        "home-poi-9",
        3,
        4_000,
    )
    .expect("one free slot records one birth");
    assert_eq!(may_issue_native(&registry, "birth-first"), Ok(true));
    let frozen = serde_json::to_string(&registry).unwrap();
    assert_eq!(
        record_birth(
            &mut registry,
            "birth-second",
            &household,
            &[members[0].clone()],
            "site-1",
            "home-poi-9",
            3,
            4_000,
        ),
        Err(Refusal::MemberLimit)
    );
    assert_eq!(serde_json::to_string(&registry).unwrap(), frozen);
    assert_eq!(
        may_issue_native(&registry, "birth-second"),
        Err(Refusal::UnknownIntent)
    );
}

#[test]
fn a_birth_never_commits_through_the_generic_path() {
    let mut registry = households();
    let household = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();
    record_birth(
        &mut registry,
        "birth-generic",
        &household,
        &["resident-handle-1".to_owned()],
        "site-1",
        "home-poi-9",
        3,
        4_000,
    )
    .expect("one household parent records one birth");
    let frozen = serde_json::to_string(&registry).unwrap();

    // The generic path cannot know the spawned handle, so it refuses: only
    // the projection commits a birth, and the intent stays recorded.
    assert_eq!(
        advance_intent(&mut registry, "birth-generic", IntentState::Committed),
        Err(Refusal::InvalidTransition)
    );
    assert_eq!(serde_json::to_string(&registry).unwrap(), frozen);
    assert_eq!(may_issue_native(&registry, "birth-generic"), Ok(true));
}

#[test]
fn an_open_birth_elsewhere_never_consumes_this_household_slot() {
    let mut registry = households();
    // One household with room for exactly one more member, and a second
    // household in another settlement holding an open birth of its own.
    let mut members = Vec::new();
    for index in 0..MAX_HOUSEHOLD_MEMBERS - 1 {
        let handle = format!("crowd-handle-{index}");
        registry
            .settlements
            .get_mut("oak_hollow")
            .unwrap()
            .residents
            .insert(format!("crowd_{index}"), resident(&handle));
        members.push(handle);
    }
    let member_refs: Vec<&str> = members.iter().map(String::as_str).collect();
    let household =
        found_household(&mut registry, "oak_hollow", Origin::Adoption, &member_refs).unwrap();
    let elsewhere = found_household(
        &mut registry,
        "pine_ridge",
        Origin::Refugee,
        &["resident-handle-4"],
    )
    .unwrap();
    record_birth(
        &mut registry,
        "birth-elsewhere",
        &elsewhere,
        &["resident-handle-4".to_owned()],
        "site-9",
        "home-poi-1",
        1,
        4_000,
    )
    .expect("another settlement records its own birth");

    // This household still has its last slot: the foreign birth holds
    // nothing here, and this birth still records and projects.
    let birth = record_birth(
        &mut registry,
        "birth-here",
        &household,
        &[members[0].clone()],
        "site-1",
        "home-poi-9",
        3,
        4_000,
    )
    .expect("a foreign birth holds no slot here");
    assert_eq!(
        commit_birth(&mut registry, "birth-here", "resident-handle-9"),
        Ok(IntentState::Committed)
    );
    assert!(registry.households[&household]
        .members
        .contains("resident-handle-9"));
    assert!(registry.settlements["oak_hollow"]
        .residents
        .contains_key(&birth.child_alias));
}

#[test]
fn an_unrelated_arrival_over_a_held_slot_refuses_and_the_birth_still_fits() {
    let mut registry = households();
    // One household with room for exactly one more member, then one open
    // birth holding that last slot.
    let mut members = Vec::new();
    for index in 0..MAX_HOUSEHOLD_MEMBERS - 1 {
        let handle = format!("crowd-handle-{index}");
        registry
            .settlements
            .get_mut("oak_hollow")
            .unwrap()
            .residents
            .insert(format!("crowd_{index}"), resident(&handle));
        members.push(handle);
    }
    let member_refs: Vec<&str> = members.iter().map(String::as_str).collect();
    let household =
        found_household(&mut registry, "oak_hollow", Origin::Adoption, &member_refs).unwrap();
    record_birth(
        &mut registry,
        "birth-held",
        &household,
        &[members[0].clone()],
        "site-1",
        "home-poi-9",
        3,
        4_000,
    )
    .expect("one free slot records one birth");
    registry
        .settlements
        .get_mut("oak_hollow")
        .unwrap()
        .residents
        .insert("latecomer".to_owned(), resident("resident-late"));

    // An unrelated member arrival over the held slot refuses, while the held
    // birth still projects into the slot it reserved.
    let revision = registry.households[&household].revision;
    assert_eq!(
        add_member(&mut registry, &household, "resident-late", revision),
        Err(Refusal::MemberLimit)
    );
    assert_eq!(
        commit_birth(&mut registry, "birth-held", "resident-handle-9"),
        Ok(IntentState::Committed)
    );
    assert!(registry.households[&household]
        .members
        .contains("resident-handle-9"));
}

#[test]
fn a_full_household_refuses_the_birth_before_any_native_effect() {
    let mut registry = households();
    let household = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();
    // Fill the household so the projection cannot fit one more member.
    for index in 0..MAX_HOUSEHOLD_MEMBERS {
        let handle = format!("crowd-handle-{index}");
        registry
            .settlements
            .get_mut("oak_hollow")
            .unwrap()
            .residents
            .insert(format!("crowd_{index}"), resident(&handle));
        if index > 0 {
            let revision = registry.households[&household].revision;
            add_member(&mut registry, &household, &handle, revision).unwrap();
        }
    }
    let frozen = serde_json::to_string(&registry).unwrap();
    assert_eq!(
        record_birth(
            &mut registry,
            "birth-full",
            &household,
            &["resident-handle-1".to_owned()],
            "site-1",
            "home-poi-9",
            3,
            4_000,
        ),
        Err(Refusal::MemberLimit)
    );
    assert_eq!(serde_json::to_string(&registry).unwrap(), frozen);
    assert_eq!(
        may_issue_native(&registry, "birth-full"),
        Err(Refusal::UnknownIntent)
    );
}

/// The package-side evidence one committed birth must leave: the roster holds
/// the child alias with its handle, household ref and age origin, the
/// household holds the handle, and the intent closed. Core items and POI
/// occupancy stay core evidence: the committed reserve receipt answers the
/// token and the site query answers `Occupied`, asserted by the native suites.
fn assert_birth_evidence(
    registry: &SettlementRegistry,
    settlement: &str,
    household_id: &str,
    child_alias: &str,
    child_handle: &str,
    birth_tick: u64,
    operation_id: &str,
) {
    let child = &registry.settlements[settlement].residents[child_alias];
    assert_eq!(child.handle, child_handle);
    assert_eq!(child.household.as_deref(), Some(household_id));
    assert_eq!(child.age_origin, Some(birth_tick));
    assert!(registry.households[household_id]
        .members
        .contains(child_handle));
    assert_eq!(
        registry.demography_intents[operation_id].state(),
        IntentState::Committed
    );
}

#[test]
fn a_birth_with_an_unexecutable_id_refuses_before_any_mutation() {
    let mut registry = households();
    let household = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();
    let frozen = serde_json::to_string(&registry).unwrap();
    for operation_id in ["Birth-1", "birth 1", "birth/1", &"b".repeat(65)] {
        assert_eq!(
            record_birth(
                &mut registry,
                operation_id,
                &household,
                &["resident-handle-1".to_owned()],
                "site-1",
                "home-poi-9",
                3,
                4_000,
            ),
            Err(Refusal::InvalidIntent)
        );
    }
    assert_eq!(serde_json::to_string(&registry).unwrap(), frozen);
    assert_eq!(
        may_issue_native(&registry, "Birth-1"),
        Err(Refusal::UnknownIntent)
    );
}

#[test]
fn a_committed_birth_advances_no_further_sequence() {
    let mut registry = households();
    let household = found_household(
        &mut registry,
        "oak_hollow",
        Origin::Adoption,
        &["resident-handle-1"],
    )
    .unwrap();
    let birth = record_birth(
        &mut registry,
        "birth-seq",
        &household,
        &["resident-handle-1".to_owned()],
        "site-1",
        "home-poi-9",
        3,
        4_000,
    )
    .expect("one household parent records one birth");
    let reserved = registry.settlements["oak_hollow"].resident_sequence;
    assert_eq!(
        commit_birth(&mut registry, "birth-seq", "resident-handle-9"),
        Ok(IntentState::Committed)
    );
    // The opening CAS already reserved the alias: the projection must not
    // advance the sequence a second time.
    assert_eq!(
        registry.settlements["oak_hollow"].resident_sequence,
        reserved
    );
    assert!(registry.settlements["oak_hollow"]
        .residents
        .contains_key(&birth.child_alias));
}
