//! Durable production policy. Native work is the only item authority.
//! The journal records native deltas verbatim; it is not a second inventory.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(super) const MAX_JOBS: usize = 64;
pub(super) const TIMER: &str = "settlement-production";

/// Small executable authored catalog; quality changes instance components, not
/// item identities. Recipe variants are fixed startup-native execution data.
const RECIPE_IDS: &[&str] = &[
    "solaris-settlements:oak_planks",
    "solaris-settlements:stick",
    "solaris-settlements:wooden_pickaxe",
    "solaris-settlements:wooden_hoe",
    "solaris-settlements:wooden_axe",
    "solaris-settlements:stone_pickaxe",
    "solaris-settlements:bread",
    "solaris-settlements:sugar",
    "solaris-settlements:cake",
    "solaris-settlements:saddle",
    "solaris-settlements:hay_block",
    "solaris-settlements:open_hay",
    "solaris-settlements:wooden_pickaxe_rough",
    "solaris-settlements:wooden_pickaxe_ordinary",
    "solaris-settlements:wooden_pickaxe_good",
    "solaris-settlements:wooden_pickaxe_masterwork",
    "solaris-settlements:wooden_hoe_rough",
    "solaris-settlements:wooden_hoe_ordinary",
    "solaris-settlements:wooden_hoe_good",
    "solaris-settlements:wooden_hoe_masterwork",
    "solaris-settlements:wooden_axe_rough",
    "solaris-settlements:wooden_axe_ordinary",
    "solaris-settlements:wooden_axe_good",
    "solaris-settlements:wooden_axe_masterwork",
    "solaris-settlements:stone_pickaxe_rough",
    "solaris-settlements:stone_pickaxe_ordinary",
    "solaris-settlements:stone_pickaxe_good",
    "solaris-settlements:stone_pickaxe_masterwork",
    "solaris-settlements:repair_wooden_pickaxe_rough",
    "solaris-settlements:repair_wooden_pickaxe_ordinary",
    "solaris-settlements:repair_wooden_pickaxe_good",
    "solaris-settlements:repair_wooden_pickaxe_masterwork",
    "solaris-settlements:salvage_wooden_pickaxe_rough",
    "solaris-settlements:salvage_wooden_pickaxe_ordinary",
    "solaris-settlements:salvage_wooden_pickaxe_good",
    "solaris-settlements:salvage_wooden_pickaxe_masterwork",
    "solaris-settlements:arming_sword",
    "solaris-settlements:arming_sword_rough",
    "solaris-settlements:arming_sword_ordinary",
    "solaris-settlements:arming_sword_good",
    "solaris-settlements:arming_sword_masterwork",
    "solaris-settlements:longsword",
    "solaris-settlements:longsword_rough",
    "solaris-settlements:longsword_ordinary",
    "solaris-settlements:longsword_good",
    "solaris-settlements:longsword_masterwork",
    "solaris-settlements:spear",
    "solaris-settlements:spear_rough",
    "solaris-settlements:spear_ordinary",
    "solaris-settlements:spear_good",
    "solaris-settlements:spear_masterwork",
    "solaris-settlements:war_bow",
    "solaris-settlements:war_bow_rough",
    "solaris-settlements:war_bow_ordinary",
    "solaris-settlements:war_bow_good",
    "solaris-settlements:war_bow_masterwork",
    "solaris-settlements:mail_hauberk",
    "solaris-settlements:mail_hauberk_rough",
    "solaris-settlements:mail_hauberk_ordinary",
    "solaris-settlements:mail_hauberk_good",
    "solaris-settlements:mail_hauberk_masterwork",
    "solaris-settlements:pavise",
    "solaris-settlements:pavise_rough",
    "solaris-settlements:pavise_ordinary",
    "solaris-settlements:pavise_good",
    "solaris-settlements:pavise_masterwork",
];
pub(super) fn recipe(name: &str) -> Option<&'static str> {
    RECIPE_IDS
        .iter()
        .copied()
        .find(|id| *id == name || id.strip_prefix("solaris-settlements:") == Some(name))
}
/// Caller options share admission's catalog; manufacturing quality is rolled
/// once from the saved operation seed, never selected by a form or command.
pub(super) fn selectable_recipes() -> impl Iterator<Item = &'static str> {
    RECIPE_IDS.iter().copied().filter(|id| {
        ![
            "wooden_pickaxe_",
            "wooden_hoe_",
            "wooden_axe_",
            "stone_pickaxe_",
            "arming_sword_",
            "longsword_",
            "spear_",
            "war_bow_",
            "mail_hauberk_",
            "pavise_",
        ]
        .iter()
        .any(|prefix| {
            id.strip_prefix("solaris-settlements:")
                .is_some_and(|path| path.starts_with(prefix))
        })
    })
}
#[derive(Deserialize, Serialize)]
struct Ruleset {
    decision_status: String,
    revision: String,
    quality: Vec<Quality>,
}
#[derive(Deserialize, Serialize)]
struct Quality {
    tier: String,
    weight_percent: u64,
    max_damage_percent: u64,
}
fn ruleset() -> Ruleset {
    let rules: Ruleset = serde_json::from_str(include_str!("production_ruleset.json"))
        .expect("authored production ruleset");
    assert_eq!(
        rules
            .quality
            .iter()
            .map(|tier| tier.weight_percent)
            .sum::<u64>(),
        100
    );
    rules
}
fn quality_data(tier: &str) -> Vec<u8> {
    let mut bytes = vec![10, 0, 0, 8, 0, 7, b'q', b'u', b'a', b'l', b'i', b't', b'y'];
    bytes.extend_from_slice(&(tier.len() as u16).to_be_bytes());
    bytes.extend_from_slice(tier.as_bytes());
    bytes.push(0);
    bytes
}
fn authored_bases() -> Vec<serde_json::Value> {
    let mut bases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("production_recipes.json"))
            .expect("authored production recipes");
    bases.extend(super::military_catalog::authored_recipes());
    bases
}

fn expand_recipe(base: serde_json::Value, rules: &Ruleset) -> Vec<serde_json::Value> {
    let graded = base["outputs"]
        .as_array()
        .expect("recipe outputs")
        .iter()
        .any(|output| output["components"]["max_damage"].is_number())
        || base["inputs"]
            .as_array()
            .expect("recipe inputs")
            .iter()
            .any(|input| input["components"]["max_damage"].is_number());
    if !graded {
        return vec![base];
    }
    rules
        .quality
        .iter()
        .map(|tier| {
            let mut variant = base.clone();
            variant["id"] = serde_json::Value::String(format!(
                "{}_{}",
                base["id"].as_str().expect("recipe id"),
                tier.tier,
            ));
            variant["revision"] = serde_json::Value::String(format!(
                "{}-ruleset-{}",
                base["revision"].as_str().expect("recipe revision"),
                rules.revision,
            ));
            for field in ["inputs", "outputs"] {
                for stack in variant[field].as_array_mut().expect("recipe stacks") {
                    if let Some(cap) = stack["components"]["max_damage"].as_u64() {
                        stack["components"]["max_damage"] =
                            serde_json::json!(cap * tier.max_damage_percent / 100);
                        stack["components"]["custom_data"] =
                            serde_json::json!(quality_data(&tier.tier));
                    }
                }
            }
            variant
        })
        .collect()
}

pub(super) fn authored_definitions() -> impl Iterator<Item = serde_json::Value> {
    let rules = ruleset();
    authored_bases()
        .into_iter()
        .flat_map(move |base| expand_recipe(base, &rules))
}

pub(super) fn authored_recipes() -> Vec<String> {
    authored_definitions()
        .map(|definition| definition.to_string())
        .collect()
}

/// The same native definitions used at startup, restricted to caller options.
/// A manufacturing plan shows all admission possibilities; it never offers a
/// rolled variant as a caller-selected quality or claims an effect occurred.
pub(super) fn recipe_plan(name: &str) -> Option<serde_json::Value> {
    let canonical = recipe(name)?;
    if !selectable_recipes().any(|allowed| allowed == canonical) {
        return None;
    }
    let base = authored_bases().into_iter().find(|base| {
        let id = base["id"].as_str().expect("authored recipe id");
        canonical == id
            || canonical
                .strip_prefix(id)
                .is_some_and(|suffix| suffix.starts_with('_'))
    })?;
    let rules = ruleset();
    let choices = expand_recipe(base, &rules);
    let rolled = choices.len() > 1
        && !choices
            .iter()
            .any(|choice| choice["id"].as_str() == Some(canonical));
    let variants: Vec<_> = choices
        .into_iter()
        .filter(|choice| rolled || choice["id"].as_str() == Some(canonical))
        .collect();
    Some(serde_json::json!({
        "recipe": canonical,
        "variants": variants,
        "quality_admission": rolled.then_some(rules),
    }))
}
pub(super) fn admitted_recipe(
    base: &str,
    operation: &str,
) -> (String, Option<u64>, Option<String>) {
    if !matches!(
        base,
        "solaris-settlements:wooden_pickaxe"
            | "solaris-settlements:wooden_hoe"
            | "solaris-settlements:wooden_axe"
            | "solaris-settlements:stone_pickaxe"
            | "solaris-settlements:arming_sword"
            | "solaris-settlements:longsword"
            | "solaris-settlements:spear"
            | "solaris-settlements:war_bow"
            | "solaris-settlements:mail_hauberk"
            | "solaris-settlements:pavise"
    ) {
        return (base.to_owned(), None, None);
    }
    // Stable seed is saved before native admission; retry/restart cannot reroll.
    let seed = operation.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    let mut roll = seed % 100;
    for tier in ruleset().quality {
        if roll < tier.weight_percent {
            return (format!("{base}_{}", tier.tier), Some(seed), Some(tier.tier));
        }
        roll -= tier.weight_percent;
    }
    unreachable!("validated authored quality weights")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Station {
    pub dimension: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub building: String,
    pub origin_id: String,
    pub worksite_id: Option<String>,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Intent {
    pub operation_id: String,
    pub settlement: String,
    /// A policy authorization identity, never a native execution session.
    pub authorized_by: String,
    pub worker: String,
    pub recipe_id: String,
    #[serde(default)]
    pub quality_seed: Option<u64>,
    #[serde(default)]
    pub quality: Option<String>,
    pub count: u32,
    pub expected_revision: u64,
    pub station: Station,
    pub accepted_tick: u64,
    pub due_tick: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ItemChange {
    pub item_id: String,
    pub delta: i64,
}

/// Native receipt projection, including immutable canonical gross slot facts.
/// Legacy net-only projections never authorize technological practice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Receipt {
    pub operation_id: String,
    pub settlement: String,
    pub worker: String,
    pub recipe_id: String,
    pub station: Station,
    pub native_revision: u64,
    pub work_units_done: u64,
    pub work_units_planned: u64,
    pub state: String,
    pub reason: Option<String>,
    pub changes: Vec<ItemChange>,
    #[serde(default)]
    pub native_portions: Vec<serde_json::Value>,
    #[serde(default)]
    pub acknowledged_sequence: u64,
    #[serde(default)]
    pub due_tick: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Outcome {
    Native(Receipt),
    Refused { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Job {
    pub intent: Intent,
    pub outcome: Option<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<Pause>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Pause {
    pub reason: String,
    pub retry_tick: u64,
}
impl Job {
    fn deadline(&self) -> Option<u64> {
        if let Some(pause) = &self.pause {
            return Some(pause.retry_tick);
        }
        match &self.outcome {
            None => Some(self.intent.due_tick),
            Some(Outcome::Native(receipt))
                if receipt
                    .native_portions
                    .iter()
                    .filter_map(|portion| {
                        portion.get("sequence").and_then(serde_json::Value::as_u64)
                    })
                    .any(|sequence| sequence > receipt.acknowledged_sequence) =>
            {
                Some(0)
            }
            Some(Outcome::Native(receipt))
                if matches!(receipt.state.as_str(), "Accepted" | "Running") =>
            {
                receipt.due_tick
            }
            _ => None,
        }
    }
    fn unfinished(&self) -> bool {
        self.outcome.is_none()
            || matches!(&self.outcome,Some(Outcome::Native(receipt)) if matches!(receipt.state.as_str(),"Accepted"|"Running"|"Paused"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Journal {
    pub schema: u32,
    pub jobs: BTreeMap<String, Job>,
}
impl Default for Journal {
    fn default() -> Self {
        Self {
            schema: 2,
            jobs: BTreeMap::new(),
        }
    }
}
impl Journal {
    pub(super) fn load(value: Option<&str>) -> Result<Self, &'static str> {
        let journal = match value {
            None => Self::default(),
            Some(value) => {
                serde_json::from_str(value).map_err(|_| "production_store_unreadable")?
            }
        };
        if journal.schema != 2 || journal.jobs.len() > MAX_JOBS {
            return Err("production_store_unsupported");
        }
        let value = serde_json::to_value(&journal).map_err(|_| "production_store_unreadable")?;
        super::production_storage::validate_journal(&value, 2)?;
        Ok(journal)
    }

    pub(super) fn can_insert(&self, intent: &Intent) -> Result<bool, &'static str> {
        if let Some(job) = self.jobs.get(&intent.operation_id) {
            // Delivery time is not part of identity; replay cannot move the
            // original simulation deadline or silently change its executor.
            let mut replay = intent.clone();
            replay.accepted_tick = job.intent.accepted_tick;
            replay.due_tick = job.intent.due_tick;
            return if replay == job.intent {
                Ok(false)
            } else {
                Err("operation_conflict")
            };
        }
        if self.jobs.len() >= MAX_JOBS {
            return Err("production_capacity");
        }
        if self
            .jobs
            .values()
            .any(|job| job.unfinished() && job.intent.worker == intent.worker)
        {
            return Err("worker_busy");
        }
        Ok(true)
    }

    pub(super) fn insert(&mut self, intent: Intent) -> Result<bool, &'static str> {
        if !self.can_insert(&intent)? {
            return Ok(false);
        }
        self.jobs.insert(
            intent.operation_id.clone(),
            Job {
                intent,
                outcome: None,
                pause: None,
            },
        );
        Ok(true)
    }

    pub(super) fn due(&self, tick: u64) -> Option<&Job> {
        self.jobs
            .values()
            .filter(|job| job.deadline().is_some_and(|due| due <= tick))
            .min_by_key(|job| (job.deadline(), &job.intent.operation_id))
    }

    pub(super) fn next_deadline(&self) -> Option<u64> {
        self.jobs.values().filter_map(Job::deadline).min()
    }

    /// Only used before native admission. Never invent a native receipt for a
    /// missing station; retain the original operation, seed and inventory fence.
    pub(super) fn pause(
        &mut self,
        operation: &str,
        reason: String,
        retry_tick: u64,
    ) -> Result<(), &'static str> {
        let job = self
            .jobs
            .get_mut(operation)
            .ok_or("production_job_missing")?;
        if job.outcome.is_some() {
            return Err("production_already_admitted");
        }
        job.pause = Some(Pause { reason, retry_tick });
        Ok(())
    }

    pub(super) fn accepted_practice(&self, settlement: &str, operation: &str) -> Option<&Receipt> {
        let Outcome::Native(receipt) = self.jobs.get(operation)?.outcome.as_ref()? else {
            return None;
        };
        (receipt.settlement == settlement
            && receipt.work_units_done > 0
            && receipt.changes.iter().any(|change| change.delta > 0))
        .then_some(receipt)
    }

    /// Technology can consume only a genuinely committed native portion.
    /// Saved gross output facts, rather than net balance or author estimates,
    /// identify the exact practice and canonical recipe revision.
    pub(super) fn exact_practice(
        &self,
        scope: &str,
        operation: &str,
    ) -> Option<super::technology::Practice> {
        let job = self.jobs.get(operation)?;
        let Outcome::Native(receipt) = job.outcome.as_ref()? else {
            return None;
        };
        if job.intent.settlement != scope || receipt.operation_id != operation {
            return None;
        }
        let definition = authored_definitions()
            .find(|value| value["id"].as_str() == Some(job.intent.recipe_id.as_str()))?;
        let outputs = definition["outputs"].as_array()?;
        for portion in &receipt.native_portions {
            if portion.get("operation_id")?.as_str()? != operation
                || portion.get("handle")?.as_str()? != job.intent.worker
                || portion.get("recipe")?.as_str()? != job.intent.recipe_id
            {
                continue;
            }
            let revision = portion.get("recipe_revision")?.as_str()?;
            let portion_id = portion.get("portion_id")?.as_str()?;
            if revision.is_empty()
                || portion_id.is_empty()
                || portion.get("sequence")?.as_u64()? == 0
            {
                continue;
            }
            let output = portion
                .get("outputs")?
                .as_array()?
                .iter()
                .filter_map(|stack| stack.get("item"))
                .find(|item| {
                    outputs.iter().any(|output| {
                        item.get("resource_id").and_then(serde_json::Value::as_str)
                            == output["item"].as_str()
                    })
                })?;
            let count = u32::try_from(output.get("count")?.as_u64()?).ok()?;
            if count == 0 {
                continue;
            }
            return Some(super::technology::Practice {
                operation_id: operation.to_owned(),
                recipe: job.intent.recipe_id.clone(),
                recipe_revision: Some(revision.to_owned()),
                worker: job.intent.worker.clone(),
                station: job.intent.station.origin_id.clone(),
                station_revision: job.intent.station.revision,
                portion: Some(portion_id.to_owned()),
                output_item: Some(output.get("resource_id")?.as_str()?.to_owned()),
                output_count: Some(count),
            });
        }
        None
    }

    pub(super) fn finish(&mut self, operation: &str, outcome: Outcome) -> Result<(), &'static str> {
        let job = self
            .jobs
            .get_mut(operation)
            .ok_or("production_job_missing")?;
        if let Some(existing) = &job.outcome {
            if *existing == outcome {
                return Ok(());
            }
            let (Outcome::Native(previous), Outcome::Native(next)) = (existing, &outcome) else {
                return Err("receipt_conflict");
            };
            if next.native_revision <= previous.native_revision
                || next.work_units_done < previous.work_units_done
                || previous
                    .native_portions
                    .iter()
                    .any(|saved| !next.native_portions.contains(saved))
            {
                return Err("receipt_conflict");
            }
        }
        if let Outcome::Native(receipt) = &outcome {
            if receipt.operation_id != job.intent.operation_id
                || receipt.worker != job.intent.worker
                || receipt.settlement != job.intent.settlement
                || receipt.recipe_id != job.intent.recipe_id
                || receipt.station != job.intent.station
                || receipt.work_units_planned != u64::from(job.intent.count)
                || receipt.work_units_done > receipt.work_units_planned
                || receipt.native_portions.len() > job.intent.count as usize
                || receipt.native_portions.iter().any(|portion| {
                    portion
                        .get("operation_id")
                        .and_then(serde_json::Value::as_str)
                        != Some(job.intent.operation_id.as_str())
                        || portion.get("handle").and_then(serde_json::Value::as_str)
                            != Some(job.intent.worker.as_str())
                        || portion.get("recipe").and_then(serde_json::Value::as_str)
                            != Some(job.intent.recipe_id.as_str())
                        || !portion
                            .get("sequence")
                            .and_then(serde_json::Value::as_u64)
                            .is_some_and(|value| value > 0)
                        || !portion
                            .get("recipe_revision")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|value| !value.is_empty())
                        || !portion
                            .get("portion_id")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|value| !value.is_empty())
                        || !portion
                            .get("inputs")
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(|value| !value.is_empty())
                        || !portion
                            .get("outputs")
                            .and_then(serde_json::Value::as_array)
                            .is_some_and(|value| !value.is_empty())
                })
            {
                return Err("production_receipt_mismatch");
            }
        }
        job.outcome = Some(outcome);
        job.pause = None;
        Ok(())
    }
}
