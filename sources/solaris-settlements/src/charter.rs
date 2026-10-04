//! CP-061 charter-holder records (structural only).
//!
//! Tracks the current charter-authorized holder that war authority reads
//! through `PoliticalFacts::holder`. Unknown, dead, or revoked heirs leave the
//! charter vacant: no heir is invented, respawned, or inferred, and a vacancy
//! closes only by admitting a newer committed holder record. No succession
//! policy (seniority/partition/elective), numeric bounds, or payment rules are
//! admitted here, and no storage codec is claimed: records are admitted in
//! memory and a restart replays the same committed record sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HolderRecord {
    pub charter: String,
    pub holder: String,
    pub event: String,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Charter {
    pub id: String,
    pub holder: Option<HolderRecord>,
    pub revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CharterRefusal {
    InvalidRecord,
    StaleRevision,
}
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':' | b'.'))
}
impl Charter {
    pub fn new(charter_id: &str) -> Result<Self, CharterRefusal> {
        if !id(charter_id) {
            return Err(CharterRefusal::InvalidRecord);
        }
        Ok(Self {
            id: charter_id.into(),
            holder: None,
            revision: 0,
        })
    }
    /// Current charter-authorized holder; `None` while vacant.
    pub fn current_holder(&self) -> Option<&str> {
        self.holder.as_ref().map(|record| record.holder.as_str())
    }
    /// Admit a committed holder record. Newer revisions replace the holder
    /// (including closing a vacancy); replaying the admitted record is
    /// idempotent; older revisions and conflicting same-revision records fail.
    pub fn admit(&mut self, record: HolderRecord) -> Result<(), CharterRefusal> {
        if record.charter != self.id
            || !id(&record.holder)
            || !id(&record.event)
            || record.revision == 0
        {
            return Err(CharterRefusal::InvalidRecord);
        }
        if record.revision < self.revision {
            return Err(CharterRefusal::StaleRevision);
        }
        if record.revision == self.revision {
            let same = self.holder.as_ref().is_some_and(|current| {
                current.holder == record.holder && current.event == record.event
            });
            if self.revision == 0 || !same {
                return Err(CharterRefusal::InvalidRecord);
            }
            return Ok(());
        }
        self.revision = record.revision;
        self.holder = Some(record);
        Ok(())
    }
    /// Record that the holder is unknown, dead, or revoked. The charter stays
    /// vacant until a newer holder record is admitted; replays are idempotent.
    pub fn vacate(&mut self, event: &str, revision: u64) -> Result<(), CharterRefusal> {
        if !id(event) || revision == 0 {
            return Err(CharterRefusal::InvalidRecord);
        }
        if revision < self.revision {
            return Err(CharterRefusal::StaleRevision);
        }
        if revision == self.revision {
            if self.holder.is_some() {
                return Err(CharterRefusal::InvalidRecord);
            }
            return Ok(());
        }
        self.revision = revision;
        self.holder = None;
        Ok(())
    }
}
