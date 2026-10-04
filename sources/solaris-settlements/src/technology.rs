//! Local knowledge is policy; only accepted production receipts establish practice.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Definition {
    pub id: String,
    pub parents: Vec<String>,
    pub practice_recipes: Vec<String>,
    pub unlock_recipes: Vec<String>,
}
impl Definition {
    /// Unlock references remain real native IDs. The player-facing selector
    /// addresses the same graded family before production saves its quality.
    pub(super) fn unlocks(&self, recipe: &str) -> bool {
        self.unlock_recipes.iter().any(|id| {
            id == recipe
                || id.strip_prefix(recipe).is_some_and(|suffix| {
                    matches!(suffix, "_rough" | "_ordinary" | "_good" | "_masterwork")
                })
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    MissingReference(String),
    Cycle(String),
    MissingKnowledge,
    MissingParent(String),
    WrongPractice,
    OperationConflict,
    StationUnavailable,
    MissingCarrier,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Source {
    LocalProject {
        worker: String,
        project: String,
    },
    Teacher {
        worker: String,
        origin_scope: String,
        origin_revision: u64,
    },
    Document {
        carrier: String,
        origin_scope: String,
        origin_revision: u64,
    },
    Contract {
        contract: String,
        teacher: String,
        origin_scope: String,
        origin_revision: u64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Practice {
    pub operation_id: String,
    pub recipe: String,
    pub recipe_revision: Option<String>,
    pub worker: String,
    pub station: String,
    pub station_revision: u64,
    pub portion: Option<String>,
    pub output_item: Option<String>,
    pub output_count: Option<u32>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Record {
    pub source: Source,
    pub accepted_practice: BTreeMap<String, Practice>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Knowledge {
    pub revision: u64,
    pub records: BTreeMap<String, Record>,
    pub transfers: BTreeMap<String, Transfer>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Transfer {
    pub technology: String,
    pub source_scope: String,
    pub source_revision: u64,
    pub destination_scope: String,
    pub source: Source,
}

pub(super) fn validate(
    definitions: &BTreeMap<String, Definition>,
    recipes: &BTreeSet<String>,
) -> Result<(), Refusal> {
    fn visit(
        id: &str,
        defs: &BTreeMap<String, Definition>,
        active: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
    ) -> Result<(), Refusal> {
        if done.contains(id) {
            return Ok(());
        }
        if !active.insert(id.to_owned()) {
            return Err(Refusal::Cycle(id.to_owned()));
        }
        let definition = defs
            .get(id)
            .ok_or_else(|| Refusal::MissingReference(id.to_owned()))?;
        for parent in &definition.parents {
            visit(parent, defs, active, done)?;
        }
        active.remove(id);
        done.insert(id.to_owned());
        Ok(())
    }
    for (id, definition) in definitions {
        if id != &definition.id {
            return Err(Refusal::MissingReference(definition.id.clone()));
        }
        for recipe in definition
            .practice_recipes
            .iter()
            .chain(&definition.unlock_recipes)
        {
            if !recipes.contains(recipe) {
                return Err(Refusal::MissingReference(recipe.clone()));
            }
        }
        visit(id, definitions, &mut BTreeSet::new(), &mut BTreeSet::new())?;
    }
    Ok(())
}
impl Knowledge {
    pub(super) fn learn(
        &mut self,
        id: &str,
        source: Source,
        definitions: &BTreeMap<String, Definition>,
    ) -> Result<(), Refusal> {
        let definition = definitions
            .get(id)
            .ok_or_else(|| Refusal::MissingReference(id.into()))?;
        self.parents(definition)?;
        let valid_source = match &source {
            Source::LocalProject { worker, project } => !worker.is_empty() && !project.is_empty(),
            Source::Teacher {
                worker,
                origin_scope,
                origin_revision,
            } => !worker.is_empty() && !origin_scope.is_empty() && *origin_revision > 0,
            Source::Document {
                carrier,
                origin_scope,
                origin_revision,
            } => !carrier.is_empty() && !origin_scope.is_empty() && *origin_revision > 0,
            Source::Contract {
                contract,
                teacher,
                origin_scope,
                origin_revision,
            } => {
                !contract.is_empty()
                    && !teacher.is_empty()
                    && !origin_scope.is_empty()
                    && *origin_revision > 0
            }
        };
        if !valid_source {
            return Err(Refusal::MissingReference("knowledge source".into()));
        }
        if !self.records.contains_key(id) {
            self.records.insert(
                id.into(),
                Record {
                    source,
                    accepted_practice: BTreeMap::new(),
                },
            );
            self.revision += 1;
        }
        Ok(())
    }
    fn parents(&self, definition: &Definition) -> Result<(), Refusal> {
        for parent in &definition.parents {
            if !self
                .records
                .get(parent)
                .is_some_and(|r| !r.accepted_practice.is_empty())
            {
                return Err(Refusal::MissingParent(parent.clone()));
            }
        }
        Ok(())
    }
    /// Caller must obtain this receipt from the durable production owner, never UI fields.
    pub(super) fn accept_practice(
        &mut self,
        id: &str,
        receipt: Practice,
        definitions: &BTreeMap<String, Definition>,
    ) -> Result<(), Refusal> {
        let definition = definitions
            .get(id)
            .ok_or_else(|| Refusal::MissingReference(id.into()))?;
        self.parents(definition)?;
        if receipt.output_count.is_none_or(|count| count == 0)
            || receipt
                .recipe_revision
                .as_ref()
                .is_none_or(|revision| revision.is_empty())
            || receipt
                .output_item
                .as_ref()
                .is_none_or(|item| item.is_empty())
            || receipt
                .portion
                .as_ref()
                .is_none_or(|portion| portion.is_empty())
            || receipt.worker.is_empty()
            || receipt.operation_id.is_empty()
            || receipt.station.is_empty()
            || !definition.practice_recipes.contains(&receipt.recipe)
        {
            return Err(Refusal::WrongPractice);
        }
        let record = self.records.get_mut(id).ok_or(Refusal::MissingKnowledge)?;
        match record.accepted_practice.get(&receipt.operation_id) {
            Some(existing) if existing == &receipt => return Ok(()),
            Some(_) => return Err(Refusal::OperationConflict),
            None => {}
        }
        record
            .accepted_practice
            .insert(receipt.operation_id.clone(), receipt);
        self.revision += 1;
        Ok(())
    }
    pub(super) fn can_craft(
        &self,
        recipe: &str,
        definitions: &BTreeMap<String, Definition>,
        station_live: bool,
    ) -> Result<(), Refusal> {
        if !station_live {
            return Err(Refusal::StationUnavailable);
        }
        for definition in definitions
            .values()
            .filter(|definition| definition.unlocks(recipe))
        {
            self.parents(definition)?;
            if !self
                .records
                .get(&definition.id)
                .is_some_and(|r| !r.accepted_practice.is_empty())
            {
                return Err(Refusal::MissingKnowledge);
            }
        }
        Ok(())
    }
    pub(super) fn receive(
        &mut self,
        operation: &str,
        transfer: Transfer,
        source_knowledge: &Knowledge,
        carrier_live: bool,
        definitions: &BTreeMap<String, Definition>,
    ) -> Result<(), Refusal> {
        if let Some(existing) = self.transfers.get(operation) {
            return if existing == &transfer {
                Ok(())
            } else {
                Err(Refusal::OperationConflict)
            };
        }
        if !carrier_live {
            return Err(Refusal::MissingCarrier);
        }
        let record = source_knowledge
            .records
            .get(&transfer.technology)
            .filter(|record| !record.accepted_practice.is_empty())
            .ok_or(Refusal::MissingKnowledge)?;
        if operation.is_empty()
            || transfer.source_scope.is_empty()
            || transfer.destination_scope.is_empty()
            || transfer.source_scope == transfer.destination_scope
            || source_knowledge.revision != transfer.source_revision
        {
            return Err(Refusal::MissingKnowledge);
        }
        let (scope, revision, teacher) = match &transfer.source {
            Source::Teacher {
                worker,
                origin_scope,
                origin_revision,
            } => (origin_scope, origin_revision, Some(worker)),
            Source::Document {
                carrier,
                origin_scope,
                origin_revision,
            } if !carrier.is_empty() => (origin_scope, origin_revision, None),
            Source::Contract {
                contract,
                teacher,
                origin_scope,
                origin_revision,
            } if !contract.is_empty() => (origin_scope, origin_revision, Some(teacher)),
            _ => return Err(Refusal::MissingReference("transfer source".into())),
        };
        if scope != &transfer.source_scope
            || *revision != transfer.source_revision
            || teacher.is_some_and(|worker| {
                !record
                    .accepted_practice
                    .values()
                    .any(|practice| &practice.worker == worker)
            })
        {
            return Err(Refusal::MissingReference("transfer source".into()));
        }
        self.learn(&transfer.technology, transfer.source.clone(), definitions)?;
        self.transfers.insert(operation.into(), transfer);
        self.revision += 1;
        Ok(())
    }
}

/// The only initially authored/admitted local-practice gates. Later canonical
/// nodes remain explicit authoring requirements, never empty runnable unlocks.
pub(super) fn initial_definitions() -> BTreeMap<String, Definition> {
    let definitions: Vec<Definition> =
        serde_json::from_str(include_str!("technology_definitions.json"))
            .expect("authored executable technology definitions");
    definitions
        .into_iter()
        .map(|definition| (definition.id.clone(), definition))
        .collect()
}

/// Canonical prerequisites retain the entire approved 64-node graph separately
/// from executable definitions: an absent recipe is not a successful empty gate.
pub(super) fn canonical_prerequisites() -> BTreeMap<String, Vec<String>> {
    let mut graph = BTreeMap::new();
    for chain in ["AG", "MT", "CR", "AR", "ML", "EN", "IN", "GP"] {
        for level in 1..=8 {
            graph.insert(
                format!("{chain}{level}"),
                if level > 1 {
                    vec![format!("{chain}{}", level - 1)]
                } else {
                    vec![]
                },
            );
        }
    }
    for (id, parents) in [
        ("AG4", &["EN2"][..]),
        ("MT5", &["EN3"][..]),
        ("AR2", &["MT2", "CR2"][..]),
        ("AR4", &["MT3", "CR4"][..]),
        ("AR7", &["MT7", "CR7"][..]),
        ("ML5", &["AG7", "CR5"][..]),
        ("EN5", &["EN4", "CR5"][..]),
        ("IN5", &["IN4", "CR4"][..]),
        ("GP1", &["IN5", "MT5"][..]),
        ("GP2", &["GP1", "EN4"][..]),
        ("GP3", &["GP2", "MT6"][..]),
        ("GP4", &["GP3", "AR5"][..]),
        ("GP7", &["GP6", "CR7"][..]),
        ("GP8", &["GP7", "ML7", "EN7"][..]),
    ] {
        let row = graph.get_mut(id).expect("canonical node");
        for parent in parents {
            if !row.iter().any(|existing| existing == parent) {
                row.push((*parent).into());
            }
        }
    }
    graph
}

pub(super) fn missing_authoring() -> Vec<String> {
    let authored = initial_definitions();
    canonical_prerequisites()
        .into_keys()
        .filter(|id| !authored.contains_key(id))
        .collect()
}

pub(super) fn validate_initial_catalog() -> Result<(), Refusal> {
    let canonical: BTreeMap<_, _> = canonical_prerequisites()
        .into_iter()
        .map(|(id, parents)| {
            (
                id.clone(),
                Definition {
                    id,
                    parents,
                    practice_recipes: vec![],
                    unlock_recipes: vec![],
                },
            )
        })
        .collect();
    // These temporary graph-only descriptors are never returned as runtime gates.
    validate(&canonical, &BTreeSet::new())?;
    let definitions = initial_definitions();
    for (id, definition) in &definitions {
        if canonical
            .get(id)
            .is_none_or(|row| row.parents != definition.parents)
        {
            return Err(Refusal::MissingReference(id.clone()));
        }
    }
    let recipes: BTreeSet<String> = super::production::authored_definitions()
        .map(|recipe| recipe["id"].as_str().expect("native recipe id").to_owned())
        .collect();
    validate_executable(&definitions, &recipes)
}

/// Validate both the prerequisite DAG and its practice entry points. A DAG
/// alone can still deadlock if a node practices a recipe locked by itself.
pub(super) fn validate_executable(
    definitions: &BTreeMap<String, Definition>,
    recipes: &BTreeSet<String>,
) -> Result<(), Refusal> {
    validate(definitions, recipes)?;
    let mut reachable = BTreeSet::new();
    loop {
        let before = reachable.len();
        for (id, definition) in definitions {
            if definition.practice_recipes.is_empty() || definition.unlock_recipes.is_empty() {
                return Err(Refusal::MissingReference(id.clone()));
            }
            if definition
                .parents
                .iter()
                .all(|parent| reachable.contains(parent))
                && definition.practice_recipes.iter().any(|practice| {
                    definitions
                        .values()
                        .filter(|gate| gate.unlocks(practice))
                        .all(|gate| reachable.contains(&gate.id))
                })
            {
                reachable.insert(id.clone());
            }
        }
        if reachable.len() == definitions.len() {
            return Ok(());
        }
        if reachable.len() == before {
            return Err(Refusal::MissingReference(
                definitions
                    .keys()
                    .find(|id| !reachable.contains(*id))
                    .unwrap()
                    .clone(),
            ));
        }
    }
}
