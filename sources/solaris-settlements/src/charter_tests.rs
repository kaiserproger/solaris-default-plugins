//! CP-061 charter-holder record tests (structural; no runtime claim).
use super::charter::*;

#[test]
fn vacant_until_admitted_then_holder_reads_back() {
    let mut charter = Charter::new("charter-a").unwrap();
    assert_eq!(charter.current_holder(), None);
    assert_eq!(Charter::new(""), Err(CharterRefusal::InvalidRecord));
    charter
        .admit(HolderRecord {
            charter: "charter-a".into(),
            holder: "holder-a".into(),
            event: "founding-1".into(),
            revision: 1,
        })
        .unwrap();
    assert_eq!(charter.current_holder(), Some("holder-a"));
    // Replay of the admitted record is idempotent.
    charter
        .admit(HolderRecord {
            charter: "charter-a".into(),
            holder: "holder-a".into(),
            event: "founding-1".into(),
            revision: 1,
        })
        .unwrap();
    assert_eq!(charter.revision, 1);
}

#[test]
fn unknown_dead_or_revoked_heir_vacates_without_inventing_one() {
    let mut charter = Charter::new("charter-a").unwrap();
    charter
        .admit(HolderRecord {
            charter: "charter-a".into(),
            holder: "holder-a".into(),
            event: "founding-1".into(),
            revision: 1,
        })
        .unwrap();
    charter.vacate("heir-lost-1", 2).unwrap();
    assert_eq!(charter.current_holder(), None);
    // Repeat vacate replay is idempotent.
    charter.vacate("heir-lost-1", 2).unwrap();
    assert_eq!(charter.revision, 2);
    // A newer committed holder closes the vacancy; older revisions fail.
    assert_eq!(
        charter.admit(HolderRecord {
            charter: "charter-a".into(),
            holder: "holder-a".into(),
            event: "founding-1".into(),
            revision: 1,
        }),
        Err(CharterRefusal::StaleRevision)
    );
    charter
        .admit(HolderRecord {
            charter: "charter-a".into(),
            holder: "holder-b".into(),
            event: "election-2".into(),
            revision: 3,
        })
        .unwrap();
    assert_eq!(charter.current_holder(), Some("holder-b"));
}

#[test]
fn malformed_or_conflicting_records_fail() {
    let mut charter = Charter::new("charter-a").unwrap();
    for record in [
        HolderRecord {
            charter: "charter-b".into(),
            holder: "holder-a".into(),
            event: "founding-1".into(),
            revision: 1,
        },
        HolderRecord {
            charter: "charter-a".into(),
            holder: String::new(),
            event: "founding-1".into(),
            revision: 1,
        },
        HolderRecord {
            charter: "charter-a".into(),
            holder: "holder-a".into(),
            event: String::new(),
            revision: 1,
        },
        HolderRecord {
            charter: "charter-a".into(),
            holder: "holder-a".into(),
            event: "founding-1".into(),
            revision: 0,
        },
    ] {
        assert_eq!(charter.admit(record), Err(CharterRefusal::InvalidRecord));
    }
    charter
        .admit(HolderRecord {
            charter: "charter-a".into(),
            holder: "holder-a".into(),
            event: "founding-1".into(),
            revision: 1,
        })
        .unwrap();
    // Same revision with different content is a conflict, not an update.
    assert_eq!(
        charter.admit(HolderRecord {
            charter: "charter-a".into(),
            holder: "holder-b".into(),
            event: "election-1".into(),
            revision: 1,
        }),
        Err(CharterRefusal::InvalidRecord)
    );
    assert_eq!(charter.vacate("", 2), Err(CharterRefusal::InvalidRecord));
    assert_eq!(
        charter.vacate("heir-lost-1", 0),
        Err(CharterRefusal::InvalidRecord)
    );
}
