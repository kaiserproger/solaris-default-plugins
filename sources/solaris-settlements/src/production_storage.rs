//! Production's bounded metadata, job projections, and immutable native portions.
//! Shared by the component and the shutdown-only inspection/migration operator.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, VecDeque};

pub const ROOT_KEY: &str = "production-root-v2";
pub const LEGACY_KEY: &str = "production-journal-v1";
pub const MAX_JOBS: usize = 64;
pub const VALUE_BYTES: usize = 4096;
const CHUNK_BYTES: usize = 3072;
const MAX_BLOB_BYTES: usize = 80 * 1024;
const MAX_HEADER_BYTES: usize = 32 * 1024;
const PAGE_SIZE: usize = 8;
// Native prepared records contain mutations plus a receipt containing keys, not
// copies of values. Reserving 32 KiB for that envelope is deliberately stricter
// than the real 80 KiB frame; count alone is never the admission check.
const MUTATION_JSON_BYTES: usize = 48 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Root {
    pub schema: u32,
    pub count: usize,
}
impl Root {
    fn empty() -> Self {
        Self {
            schema: 2,
            count: 0,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Blob {
    digest: String,
    bytes: usize,
    fragments: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Portion {
    sequence: u64,
    data: Blob,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Head {
    schema: u32,
    operation_id: String,
    bank: u8,
    header: Blob,
    portions: Vec<Portion>,
}
#[derive(Clone, Debug)]
pub struct Record {
    pub value: Option<String>,
    pub version: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mutation {
    CompareAndSwap {
        key: String,
        expected_version: Option<u64>,
        value: String,
    },
    Delete {
        key: String,
        expected_version: Option<u64>,
    },
}
impl Mutation {
    pub fn key(&self) -> &str {
        match self {
            Self::CompareAndSwap { key, .. } | Self::Delete { key, .. } => key,
        }
    }
}
pub enum Step {
    Read(String),
    Batch {
        operation: String,
        mutations: Vec<Mutation>,
    },
    Complete,
}
fn digest(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}
fn identity(id: &str) -> Result<(), &'static str> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
    {
        return Err("production_operation_id_invalid");
    }
    Ok(())
}
fn prefix(id: &str) -> String {
    format!("production-v2/{}", digest(id))
}
fn head_key(id: &str) -> String {
    format!("{}/job", prefix(id))
}
fn page_key(page: usize) -> String {
    format!("production-page-v2/{page}")
}
fn header_key(id: &str, bank: u8, fragment: usize) -> String {
    format!("{}/h{bank}/{fragment}", prefix(id))
}
fn portion_key(id: &str, portion: &Portion, fragment: usize) -> String {
    // The operation prefix and content digest together exceed the 128-byte key
    // bound. The digest already covers every native operation/component fact.
    let _ = id;
    format!("production-portion-v2/{}/{fragment}", portion.data.digest)
}
fn chunk_count(text: &str) -> usize {
    let mut count = 0;
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + CHUNK_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        count += 1;
        start = end;
    }
    count
}
fn chunks(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + CHUNK_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        result.push(text[start..end].to_owned());
        start = end;
    }
    result
}
fn blob(text: &str) -> Result<Blob, &'static str> {
    if text.is_empty() || text.len() > MAX_BLOB_BYTES {
        return Err("production_receipt_size");
    }
    Ok(Blob {
        digest: digest(text),
        bytes: text.len(),
        fragments: chunk_count(text),
    })
}
fn check_blob(blob: &Blob) -> Result<(), &'static str> {
    if blob.bytes == 0
        || blob.bytes > MAX_BLOB_BYTES
        || blob.fragments == 0
        || blob.fragments > MAX_BLOB_BYTES.div_ceil(CHUNK_BYTES) + 1
        || blob.digest.len() != 64
        || !blob.digest.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("production_manifest_invalid");
    }
    Ok(())
}
fn json<T: Serialize>(value: &T) -> Result<String, &'static str> {
    serde_json::to_string(value).map_err(|_| "production_serialization")
}
fn bounded<T: Serialize>(value: &T) -> Result<String, &'static str> {
    let text = json(value)?;
    if text.len() > VALUE_BYTES {
        return Err("production_value_size");
    }
    Ok(text)
}
fn parse<T: for<'a> Deserialize<'a>>(record: &Record) -> Result<T, &'static str> {
    let value = record.value.as_deref().ok_or("production_record_missing")?;
    if value.len() > VALUE_BYTES {
        return Err("production_value_size");
    }
    serde_json::from_str(value).map_err(|_| "production_record_unreadable")
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub journal: Value,
    root: Root,
    records: BTreeMap<String, Record>,
}
impl Snapshot {
    pub fn empty() -> Self {
        Self {
            journal: serde_json::json!({"schema":2,"jobs":{}}),
            root: Root::empty(),
            records: BTreeMap::new(),
        }
    }
    fn version(&self, key: &str) -> Option<u64> {
        self.records.get(key).and_then(|r| r.version)
    }
}
#[derive(Clone)]
enum Read {
    Root,
    Legacy,
    Page(usize),
    Head(String),
    Fragment(String),
}
pub struct Loader {
    snapshot: Snapshot,
    queue: VecDeque<(String, Read)>,
    wanted: Option<String>,
    heads: BTreeMap<String, Head>,
    ids: Vec<String>,
    migration: bool,
}
impl Loader {
    pub fn all() -> Self {
        Self::new(None, false)
    }
    pub fn job(id: &str) -> Result<Self, &'static str> {
        identity(id)?;
        Ok(Self::new(Some(id.to_owned()), false))
    }
    /// Shutdown operator only: the old source remains authoritative until final deletion.
    pub fn migration() -> Self {
        Self::new(None, true)
    }
    fn new(wanted: Option<String>, migration: bool) -> Self {
        Self {
            snapshot: Snapshot::empty(),
            queue: VecDeque::from([(ROOT_KEY.to_owned(), Read::Root)]),
            wanted,
            heads: BTreeMap::new(),
            ids: Vec::new(),
            migration,
        }
    }
    pub fn next(&self) -> Option<&str> {
        self.queue.front().map(|(key, _)| key.as_str())
    }
    pub fn answer(&mut self, record: Record) -> Result<(), &'static str> {
        let (key, kind) = self.queue.pop_front().ok_or("production_read_unexpected")?;
        if record
            .value
            .as_ref()
            .is_some_and(|value| value.len() > VALUE_BYTES)
            && !matches!(kind, Read::Legacy)
        {
            return Err("production_value_size");
        }
        self.snapshot.records.insert(key, record.clone());
        match kind {
            Read::Root => {
                if !self.migration {
                    self.queue.push_back((LEGACY_KEY.to_owned(), Read::Legacy));
                }
                if record.value.is_some() {
                    let root: Root = parse(&record)?;
                    if root.schema != 2 || root.count > MAX_JOBS {
                        return Err("production_root_invalid");
                    }
                    self.snapshot.root = root;
                    if let Some(id) = &self.wanted {
                        self.queue.push_back((head_key(id), Read::Head(id.clone())));
                    } else {
                        for page in 0..self.snapshot.root.count.div_ceil(PAGE_SIZE) {
                            self.queue.push_back((page_key(page), Read::Page(page)));
                        }
                    }
                }
            }
            Read::Legacy => {
                if record.value.is_some() {
                    return Err("production_migration_required_backup_and_dry_run");
                }
            }
            Read::Page(page) => {
                let ids: Vec<String> = parse(&record)?;
                let expected = (self.snapshot.root.count - page * PAGE_SIZE).min(PAGE_SIZE);
                if ids.len() != expected {
                    return Err("production_index_incomplete");
                }
                for id in ids {
                    identity(&id)?;
                    if self.ids.contains(&id) {
                        return Err("production_index_duplicate");
                    }
                    self.ids.push(id.clone());
                    self.queue.push_back((head_key(&id), Read::Head(id)));
                }
            }
            Read::Head(id) => {
                if record.value.is_none() && self.wanted.is_some() {
                    return Ok(());
                }
                let head: Head = parse(&record)?;
                if head.schema != 2
                    || head.operation_id != id
                    || head.bank > 1
                    || head.portions.len() > 16
                    || head.header.bytes > MAX_HEADER_BYTES
                {
                    return Err("production_manifest_invalid");
                }
                check_blob(&head.header)?;
                for fragment in 0..head.header.fragments {
                    self.queue.push_back((
                        header_key(&id, head.bank, fragment),
                        Read::Fragment(id.clone()),
                    ));
                }
                let mut previous = 0;
                for portion in &head.portions {
                    check_blob(&portion.data)?;
                    if portion.sequence <= previous || portion.sequence > 16 {
                        return Err("production_sequence_invalid");
                    }
                    previous = portion.sequence;
                    for fragment in 0..portion.data.fragments {
                        self.queue.push_back((
                            portion_key(&id, portion, fragment),
                            Read::Fragment(id.clone()),
                        ));
                    }
                }
                self.heads.insert(id, head);
            }
            Read::Fragment(id) => {
                let _ = id;
                if record.value.is_none() {
                    return Err("production_projection_incomplete");
                }
            }
        }
        Ok(())
    }
    fn assemble(
        &self,
        reference: &Blob,
        keys: impl Iterator<Item = String>,
    ) -> Result<String, &'static str> {
        let mut value = String::with_capacity(reference.bytes);
        for key in keys {
            value.push_str(
                self.snapshot
                    .records
                    .get(&key)
                    .and_then(|r| r.value.as_deref())
                    .ok_or("production_projection_incomplete")?,
            );
        }
        if value.len() != reference.bytes || digest(&value) != reference.digest {
            return Err("production_fragment_conflict");
        }
        Ok(value)
    }
    pub fn finish(mut self) -> Result<Snapshot, &'static str> {
        if !self.queue.is_empty() {
            return Err("production_projection_incomplete");
        }
        let mut jobs = Map::new();
        for (id, head) in &self.heads {
            let text = self.assemble(
                &head.header,
                (0..head.header.fragments).map(|n| header_key(id, head.bank, n)),
            )?;
            let mut job: Value =
                serde_json::from_str(&text).map_err(|_| "production_header_unreadable")?;
            if job["intent"]["operation_id"].as_str() != Some(id) {
                return Err("production_operation_conflict");
            }
            let mut portions = Vec::new();
            for portion in &head.portions {
                let text = self.assemble(
                    &portion.data,
                    (0..portion.data.fragments).map(|n| portion_key(id, portion, n)),
                )?;
                let value: Value =
                    serde_json::from_str(&text).map_err(|_| "production_receipt_unreadable")?;
                if value["sequence"].as_u64() != Some(portion.sequence)
                    || value["operation_id"].as_str() != Some(id)
                {
                    return Err("production_portion_conflict");
                }
                portions.push(value);
            }
            if job["outcome"]["kind"] == "native" {
                job["outcome"]["native_portions"] = Value::Array(portions);
            } else if !portions.is_empty() {
                return Err("production_portion_conflict");
            }
            jobs.insert(id.clone(), job);
        }
        self.snapshot.journal = serde_json::json!({"schema":2,"jobs":jobs});
        validate_journal(&self.snapshot.journal, 2)?;
        // The logical projection owns the facts now. Keep only bounded owner
        // metadata/CAS fences, not a second copy of every receipt fragment.
        self.snapshot.records.retain(|key, _| {
            key == ROOT_KEY || key.starts_with("production-page-v2/") || key.ends_with("/job")
        });
        Ok(self.snapshot)
    }
}

/// Validates original identities/references before either loading or migration.
pub fn validate_journal(journal: &Value, schema: u64) -> Result<(), &'static str> {
    let jobs = journal["jobs"]
        .as_object()
        .ok_or("production_store_unreadable")?;
    if journal["schema"].as_u64() != Some(schema) || jobs.len() > MAX_JOBS {
        return Err("production_store_unsupported");
    }
    for (id, job) in jobs {
        identity(id)?;
        let intent = &job["intent"];
        let count = intent["count"]
            .as_u64()
            .filter(|n| (1..=16).contains(n))
            .ok_or("production_store_invalid")?;
        if intent["operation_id"].as_str() != Some(id)
            || intent["due_tick"].as_u64().is_none()
            || intent["due_tick"].as_u64() < intent["accepted_tick"].as_u64()
            || intent["accepted_tick"].as_u64().is_none()
        {
            return Err("production_store_invalid");
        }
        let outcome = &job["outcome"];
        let pause = &job["pause"];
        if !pause.is_null()
            && (!outcome.is_null()
                || pause["reason"].as_str().is_none_or(str::is_empty)
                || pause["retry_tick"]
                    .as_u64()
                    .is_none_or(|tick| tick < intent["due_tick"].as_u64().unwrap()))
        {
            return Err("production_store_invalid");
        }
        if outcome.is_null() {
            continue;
        }
        if outcome["kind"] == "refused" {
            if !outcome["reason"].is_string() {
                return Err("production_store_invalid");
            }
            continue;
        }
        if outcome["kind"] != "native"
            || outcome["operation_id"] != intent["operation_id"]
            || outcome["worker"] != intent["worker"]
            || outcome["settlement"] != intent["settlement"]
            || outcome["recipe_id"] != intent["recipe_id"]
            || outcome["station"] != intent["station"]
            || outcome["work_units_planned"].as_u64() != Some(count)
            || outcome["work_units_done"]
                .as_u64()
                .is_none_or(|done| done > count)
        {
            return Err("production_receipt_mismatch");
        }
        let portions = outcome["native_portions"]
            .as_array()
            .ok_or("production_receipt_mismatch")?;
        if portions.len() > count as usize {
            return Err("production_receipt_mismatch");
        }
        let mut sequences = std::collections::BTreeSet::new();
        let mut ids = std::collections::BTreeSet::new();
        for portion in portions {
            if portion["operation_id"] != intent["operation_id"]
                || portion["handle"] != intent["worker"]
                || portion["recipe"] != intent["recipe_id"]
                || portion["sequence"]
                    .as_u64()
                    .is_none_or(|n| n == 0 || n > count || !sequences.insert(n))
                || !portion["portion_id"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty() && ids.insert(s))
                || !portion["recipe_revision"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
                || !portion["inputs"].as_array().is_some_and(|v| !v.is_empty())
                || !portion["outputs"].as_array().is_some_and(|v| !v.is_empty())
            {
                return Err("production_receipt_mismatch");
            }
        }
        if sequences
            .last()
            .is_some_and(|last| (1..=*last).any(|n| !sequences.contains(&n)))
        {
            return Err("production_sequence_incomplete");
        }
        if outcome["acknowledged_sequence"]
            .as_u64()
            .is_none_or(|ack| ack > count || (1..=ack).any(|n| !sequences.contains(&n)))
        {
            return Err("production_ack_reference_missing");
        }
    }
    Ok(())
}

pub struct Writer {
    snapshot: Snapshot,
    id: String,
    head: Head,
    fragments: VecDeque<(String, String, bool)>,
    awaiting: Option<(String, String, bool)>,
    guards: Vec<Mutation>,
    final_mutations: Option<Vec<Mutation>>,
    published: bool,
}
impl Writer {
    pub fn new(snapshot: Snapshot, id: &str, job: &Value) -> Result<Self, &'static str> {
        identity(id)?;
        validate_journal(
            &serde_json::json!({"schema":2,"jobs":BTreeMap::from([(id, job)])}),
            2,
        )?;
        if job["outcome"]["kind"] == "native" {
            let done = job["outcome"]["work_units_done"]
                .as_u64()
                .ok_or("production_receipt_mismatch")?;
            let portions = job["outcome"]["native_portions"]
                .as_array()
                .ok_or("production_receipt_mismatch")?;
            if (1..=done).any(|sequence| {
                !portions
                    .iter()
                    .any(|portion| portion["sequence"].as_u64() == Some(sequence))
            }) {
                return Err("production_full_native_receipt_missing");
            }
        }
        let old = snapshot
            .records
            .get(&head_key(id))
            .filter(|r| r.value.is_some())
            .map(parse::<Head>)
            .transpose()?;
        if old.is_none() && snapshot.root.count >= MAX_JOBS {
            return Err("production_capacity");
        }
        let mut header = job.clone();
        let mut portions = Vec::new();
        let mut fragments = VecDeque::new();
        if header["outcome"]["kind"] == "native" {
            let actual = header["outcome"]["native_portions"]
                .as_array_mut()
                .ok_or("production_receipt_mismatch")?;
            actual.sort_by_key(|v| v["sequence"].as_u64());
            for value in actual.iter() {
                let text = json(value)?;
                let portion = Portion {
                    sequence: value["sequence"]
                        .as_u64()
                        .ok_or("production_sequence_invalid")?,
                    data: blob(&text)?,
                };
                if let Some(previous) = old
                    .as_ref()
                    .and_then(|h| h.portions.iter().find(|p| p.sequence == portion.sequence))
                {
                    if *previous != portion {
                        return Err("production_portion_conflict");
                    }
                    // Loader verified the full immutable object. A new owner
                    // projection does not rewrite or re-read old native facts.
                    portions.push(portion);
                    continue;
                }
                for (n, value) in chunks(&text).into_iter().enumerate() {
                    fragments.push_back((portion_key(id, &portion, n), value, true));
                }
                portions.push(portion);
            }
            actual.clear();
        }
        if old
            .as_ref()
            .is_some_and(|h| h.portions.iter().any(|p| !portions.contains(p)))
        {
            return Err("production_portion_conflict");
        }
        let text = json(&header)?;
        if text.len() > MAX_HEADER_BYTES {
            return Err("production_header_size");
        }
        let bank = old.as_ref().map_or(0, |h| 1 - h.bank);
        let head = Head {
            schema: 2,
            operation_id: id.to_owned(),
            bank,
            header: blob(&text)?,
            portions,
        };
        bounded(&head)?;
        for (n, value) in chunks(&text).into_iter().enumerate() {
            fragments.push_back((header_key(id, bank, n), value, false));
        }
        Ok(Self {
            snapshot,
            id: id.to_owned(),
            head,
            fragments,
            awaiting: None,
            guards: Vec::new(),
            final_mutations: None,
            published: false,
        })
    }
    pub fn next(&mut self) -> Result<Step, &'static str> {
        if self.published {
            return Ok(Step::Complete);
        }
        if let Some((key, _, _)) = &self.awaiting {
            return Ok(Step::Read(key.clone()));
        }
        if let Some(item) = self.fragments.pop_front() {
            let key = item.0.clone();
            self.awaiting = Some(item);
            return Ok(Step::Read(key));
        }
        if self.final_mutations.is_none() {
            let mut mutations = self.guards.clone();
            mutations.push(Mutation::CompareAndSwap {
                key: head_key(&self.id),
                expected_version: self.snapshot.version(&head_key(&self.id)),
                value: bounded(&self.head)?,
            });
            if self.snapshot.version(&head_key(&self.id)).is_none() {
                if self.snapshot.root.count >= MAX_JOBS {
                    return Err("production_capacity");
                }
                let page = self.snapshot.root.count / PAGE_SIZE;
                let key = page_key(page);
                let Some(record) = self.snapshot.records.get(&key) else {
                    return Ok(Step::Read(key));
                };
                let mut ids: Vec<String> = if record.value.is_some() {
                    parse(record)?
                } else {
                    Vec::new()
                };
                if ids.len() != self.snapshot.root.count % PAGE_SIZE || ids.contains(&self.id) {
                    return Err("production_index_conflict");
                }
                ids.push(self.id.clone());
                mutations.push(Mutation::CompareAndSwap {
                    key,
                    expected_version: record.version,
                    value: bounded(&ids)?,
                });
                mutations.push(Mutation::CompareAndSwap {
                    key: ROOT_KEY.to_owned(),
                    expected_version: self.snapshot.version(ROOT_KEY),
                    value: bounded(&Root {
                        schema: 2,
                        count: self.snapshot.root.count + 1,
                    })?,
                });
            }
            check_batch(&mutations)?;
            self.final_mutations = Some(mutations);
        }
        let mutations = self.final_mutations.clone().expect("prepared publication");
        Ok(Step::Batch {
            operation: batch_id(&mutations)?,
            mutations,
        })
    }
    pub fn answer_read(&mut self, record: Record) -> Result<Step, &'static str> {
        if let Some((key, value, immutable)) = self.awaiting.take() {
            if immutable && record.value.is_some() {
                if record.value.as_deref() != Some(&value) {
                    return Err("production_fragment_conflict");
                }
                return self.next();
            }
            let mutation = Mutation::CompareAndSwap {
                key: key.clone(),
                expected_version: record.version,
                value,
            };
            let mut mutations = vec![mutation.clone()];
            if !immutable {
                // Fence EVERY mutable staging write against the published owner.
                // A late losing writer cannot overwrite the now-active bank.
                let head = head_key(&self.id);
                let (guard_key, guard_value) = if let Some(value) = self
                    .snapshot
                    .records
                    .get(&head)
                    .and_then(|r| r.value.clone())
                {
                    (head, value)
                } else {
                    (ROOT_KEY.to_owned(), bounded(&self.snapshot.root)?)
                };
                mutations.push(Mutation::CompareAndSwap {
                    expected_version: self.snapshot.version(&guard_key),
                    key: guard_key,
                    value: guard_value,
                });
            }
            check_batch(&mutations)?;
            self.awaiting = Some((
                key,
                match &mutation {
                    Mutation::CompareAndSwap { value, .. } => value.clone(),
                    _ => unreachable!(),
                },
                immutable,
            ));
            return Ok(Step::Batch {
                operation: batch_id(&mutations)?,
                mutations,
            });
        }
        let key = page_key(self.snapshot.root.count / PAGE_SIZE);
        self.snapshot.records.insert(key, record);
        self.next()
    }
    pub fn committed(&mut self, revision: u64) -> Result<Step, &'static str> {
        if let Some((key, value, immutable)) = self.awaiting.take() {
            if !immutable {
                let head = head_key(&self.id);
                let guard = if self.snapshot.version(&head).is_some() {
                    head
                } else {
                    ROOT_KEY.to_owned()
                };
                let record = self.snapshot.records.entry(guard).or_insert(Record {
                    value: None,
                    version: None,
                });
                if record.value.is_none() {
                    record.value = Some(bounded(&self.snapshot.root)?);
                }
                record.version = Some(revision);
                self.guards.push(Mutation::CompareAndSwap {
                    key,
                    expected_version: Some(revision),
                    value,
                });
            }
        } else {
            self.published = true;
        }
        self.next()
    }
}
pub fn check_batch(mutations: &[Mutation]) -> Result<(), &'static str> {
    if mutations.is_empty() || mutations.len() > 16 || json(&mutations)?.len() > MUTATION_JSON_BYTES
    {
        return Err("production_prepared_frame_size");
    }
    for mutation in mutations {
        if mutation.key().len() > 128 {
            return Err("production_key_size");
        }
        if let Mutation::CompareAndSwap { value, .. } = mutation {
            if value.len() > VALUE_BYTES {
                return Err("production_value_size");
            }
        }
    }
    Ok(())
}
fn batch_id(mutations: &[Mutation]) -> Result<String, &'static str> {
    Ok(format!(
        "prod-store-v2-{}",
        &digest(&json(&mutations)?)[..48]
    ))
}

/// Only call after the component/world has shut down. This never mutates storage.
pub fn inspect(mut get: impl FnMut(&str) -> Option<(String, u64)>) -> Result<Value, &'static str> {
    let mut loader = Loader::all();
    while let Some(key) = loader.next() {
        let record = get(key);
        loader.answer(Record {
            value: record.as_ref().map(|r| r.0.clone()),
            version: record.map(|r| r.1),
        })?;
    }
    Ok(loader.finish()?.journal)
}
