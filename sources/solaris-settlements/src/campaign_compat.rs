//! CP-067 campaign/adapter compatibility (RULESET DECISION only).
//!
//! Building, mining and the allowed vanilla mechanics never bypass the
//! campaign rules and never break without a reason. Every real action source
//! is covered: the check runs at the actual damage/transfer path, and a
//! refusal there is the only gate — there is no global disabling of
//! Minecraft, and no per-plugin invisible exception.
//!
//! Rule: an effect against a known war target always defers to
//! `war_policy::authorize` (compat never substitutes its own permission).
//! Private/Neutral/Unknown protection refuses every source, including
//! explosion splash, fire, fluid and vanilla causal mutations — splash never
//! routes around protection. Everything else is ordinary survival and is
//! allowed. The absence of a store record never opens a protected scope:
//! protection refusal is unconditional and takes no store input.

use super::war_policy::{Protection, Refusal, Source};

/// Every real action source. If a new source ever appears, it must be added
/// here and covered by [`no_global_disable`]: an uncovered source is a
/// bypass by omission.
pub(super) fn covered_sources() -> [Source; 8] {
    [
        Source::Player,
        Source::Npc,
        Source::Projectile,
        Source::NativeWork,
        Source::Explosion,
        Source::Fire,
        Source::Fluid,
        Source::Mutation,
    ]
}

/// Sources whose area effects must never route around protection. Listed
/// explicitly so a splash-bypass regression has a named test, not a comment.
pub(super) const SPLASH_SOURCES: [Source; 3] = [Source::Explosion, Source::Fire, Source::Fluid];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CompatDecision {
    /// Ordinary survival: the campaign claims no gate here.
    Allow,
    /// A known campaign target: the caller must run `war_policy::authorize`
    /// (and, for breach/salvage, the siege admission) at the real path.
    DeferToWar,
}

/// One adapter check at the actual damage/transfer path. `known_target` is
/// whether the target is a known lawful war target; protection is the
/// target's current protection. No store, budget, or plugin identity enters:
/// the absence of a record never opens a protected scope.
pub(super) fn admit_compat(
    source: Source,
    protection: Protection,
    known_target: bool,
) -> Result<CompatDecision, Refusal> {
    // The source deliberately cannot select a weaker rule, including vanilla
    // fire/fluid spread and machine automation. It only names the coverage.
    let _ = source;
    if known_target {
        return Ok(CompatDecision::DeferToWar);
    }
    match protection {
        Protection::Unprotected | Protection::OfflineOwner => Ok(CompatDecision::Allow),
        Protection::Private | Protection::Neutral | Protection::Unknown => Err(Refusal::Protected),
    }
}

/// The campaign never globally disables Minecraft: every covered source
/// allows ordinary unprotected play outside known targets.
pub(super) fn no_global_disable(source: Source) -> bool {
    matches!(
        admit_compat(source, Protection::Unprotected, false),
        Ok(CompatDecision::Allow)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_real_source_is_covered_and_leaves_survival_alone() {
        let sources = covered_sources();
        assert_eq!(sources.len(), 8, "one entry per real action source");
        for source in sources {
            assert!(
                no_global_disable(source),
                "ordinary survival stays allowed: {source:?}"
            );
            assert_eq!(
                admit_compat(source, Protection::OfflineOwner, false),
                Ok(CompatDecision::Allow),
                "offline handling belongs to the war layer, not to a compat deny: {source:?}"
            );
            assert_eq!(
                admit_compat(source, Protection::Unprotected, true),
                Ok(CompatDecision::DeferToWar),
                "known targets always defer to the war predicate: {source:?}"
            );
        }
    }

    #[test]
    fn splash_fire_fluid_and_machines_never_bypass_protection() {
        for source in covered_sources() {
            for protection in [
                Protection::Private,
                Protection::Neutral,
                Protection::Unknown,
            ] {
                assert_eq!(
                    admit_compat(source, protection.clone(), false),
                    Err(Refusal::Protected),
                    "no bypass through {source:?} against {protection:?}"
                );
                // A known target behind protection still defers first: the war
                // predicate runs before any splash, and its refusal wins.
                assert_eq!(
                    admit_compat(source, protection.clone(), true),
                    Ok(CompatDecision::DeferToWar),
                    "deferral precedes splash handling: {source:?}"
                );
            }
        }
        assert_eq!(SPLASH_SOURCES.len(), 3);
        for source in SPLASH_SOURCES {
            assert_eq!(
                admit_compat(source, Protection::Private, false),
                Err(Refusal::Protected),
                "splash never routes around protection: {source:?}"
            );
        }
    }

    #[test]
    fn stranger_and_automation_paths_hit_the_same_gate() {
        // A stranger's hand, an NPC proxy, a projectile, native work and a
        // vanilla mutation all land on the identical predicate: unknown actors
        // acquire no rights there (the war layer refuses UnknownActor), and
        // compat itself never invents a per-actor exception.
        for source in [
            Source::Player,
            Source::Npc,
            Source::Projectile,
            Source::NativeWork,
            Source::Mutation,
        ] {
            assert_eq!(
                admit_compat(source, Protection::Unknown, false),
                Err(Refusal::Protected),
                "{source:?} gains nothing from being unlisted"
            );
        }
    }
}
