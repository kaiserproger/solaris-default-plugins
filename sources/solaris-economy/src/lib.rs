//! Durable player balances for the Solaris standard plugin pack.
//!
//! This component preserves the Lua package's commands, replies, `ledger-v1`
//! storage key, and deterministic `v1|<uuid>,<balance>;...|<token>;...` ledger.

use std::collections::{BTreeMap, BTreeSet};

use solaris_plugin_sdk::commands::{StorageBatchCas, StorageMutation};
use solaris_plugin_sdk::events::{
    CommandInvoked, OperationAnswered, OperationOutcome, OperationPayload,
};
use solaris_plugin_sdk::storage::StorageCasMutation;
use solaris_plugin_sdk::{
    export_plugin, message_session, storage_cas, storage_get, types, Command, Config, Event,
    EventContext, Failure, InitContext, Plugin, StorageCasOutcome, StorageGetOutcome,
};
use toml::{Table, Value};

const STORAGE_KEY: &str = "ledger-v1";
const MAXIMUM_BALANCE: u64 = 1_000_000_000;
const MAXIMUM_ACCOUNTS: usize = 48;
const MAXIMUM_TRANSFER_TOKENS: usize = 32;
type DecodedLedger = (BTreeMap<String, u64>, BTreeSet<String>, Vec<String>);

#[derive(Clone)]
struct Settings {
    currency_name: String,
    starting_balance: u64,
    maximum_balance: u64,
    maximum_accounts: usize,
    maximum_transfer_tokens: usize,
}

impl Settings {
    fn parse(config: &Config) -> Result<Self, Failure> {
        let table = config
            .toml()
            .and_then(|value| value.as_table().cloned())
            .ok_or(Failure::Invalid)?;
        let currency_name = required_string(&table, "currency_name")?;
        if currency_name.is_empty() || currency_name.len() > 32 {
            return Err(Failure::Invalid);
        }
        let starting_balance = bounded_u64(&table, "starting_balance", 0, MAXIMUM_BALANCE)?;
        let maximum_balance =
            bounded_u64(&table, "maximum_balance", starting_balance, MAXIMUM_BALANCE)?;
        Ok(Self {
            currency_name: currency_name.to_owned(),
            starting_balance,
            maximum_balance,
            maximum_accounts: bounded_usize(&table, "maximum_accounts", 2, MAXIMUM_ACCOUNTS)?,
            maximum_transfer_tokens: bounded_usize(
                &table,
                "maximum_transfer_tokens",
                1,
                MAXIMUM_TRANSFER_TOKENS,
            )?,
        })
    }

    fn decode(&self, value: Option<&str>) -> Option<DecodedLedger> {
        let Some(value) = value else {
            return Some((BTreeMap::new(), BTreeSet::new(), Vec::new()));
        };
        let value = value.strip_prefix("v1|")?;
        let (account_text, token_text) = value.split_once('|')?;
        if token_text.contains('|') {
            return None;
        }
        let mut accounts = BTreeMap::new();
        if !account_text.is_empty() {
            for row in account_text.split(';').filter(|row| !row.is_empty()) {
                let (uuid_text, amount_text) = row.split_once(',')?;
                let uuid = normalize_uuid(uuid_text)?;
                if amount_text.is_empty() || !amount_text.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return None;
                }
                let amount = amount_text.parse::<u64>().ok()?;
                if amount > self.maximum_balance || accounts.insert(uuid, amount).is_some() {
                    return None;
                }
                if accounts.len() > self.maximum_accounts {
                    return None;
                }
            }
        }
        let mut tokens = BTreeSet::new();
        let mut order = Vec::new();
        if !token_text.is_empty() {
            for token in token_text.split(';').filter(|token| !token.is_empty()) {
                if !tokens.insert(token.to_owned()) {
                    return None;
                }
                order.push(token.to_owned());
                if order.len() > self.maximum_transfer_tokens {
                    return None;
                }
            }
        }
        Some((accounts, tokens, order))
    }

    fn encode(&self, accounts: &BTreeMap<String, u64>, token_order: &[String]) -> String {
        let accounts = accounts
            .iter()
            .map(|(uuid, balance)| format!("{uuid},{balance}"))
            .collect::<Vec<_>>()
            .join(";");
        format!("v1|{accounts}|{}", token_order.join(";"))
    }
}

struct PendingUpdate {
    request: String,
    session: types::SessionId,
    accounts: BTreeMap<String, u64>,
    token_order: Vec<String>,
    operation_id: Option<String>,
    message: String,
}

struct PendingPayStatus {
    request: String,
    operation_id: String,
    session: types::SessionId,
    actor: String,
    target: String,
    amount: u64,
    token_key: String,
}

#[derive(Default)]
struct Economy {
    settings: Option<Settings>,
    accounts: BTreeMap<String, u64>,
    tokens: BTreeSet<String>,
    token_order: Vec<String>,
    storage_version: Option<u64>,
    loaded: bool,
    pending: Option<PendingUpdate>,
    pending_pay_status: Option<PendingPayStatus>,
    sequence: u64,
}

impl Economy {
    fn settings(&self) -> &Settings {
        self.settings
            .as_ref()
            .expect("init establishes validated settings before events")
    }

    fn next_request(&mut self) -> String {
        self.sequence += 1;
        format!("ledger-reload-{}", self.sequence)
    }

    fn balance(&self, uuid: &str) -> u64 {
        self.accounts
            .get(uuid)
            .copied()
            .unwrap_or(self.settings().starting_balance)
    }

    fn save(
        &mut self,
        session: types::SessionId,
        accounts: BTreeMap<String, u64>,
        token_order: Vec<String>,
        message: String,
        operation_id: Option<String>,
    ) -> Vec<Command> {
        if self.pending.is_some() || self.pending_pay_status.is_some() {
            return vec![message_session(
                session,
                "Another economy update is committing; retry.",
            )];
        }
        let revision = self
            .storage_version
            .map_or_else(|| "new".to_owned(), |version| version.to_string());
        let request = format!("ledger-v{revision}");
        let value = self.settings().encode(&accounts, &token_order);
        let command = if let Some(operation_id) = &operation_id {
            Command::StorageBatchCas(StorageBatchCas {
                request: request.clone(),
                operation_id: operation_id.clone(),
                mutations: vec![StorageMutation::Cas(StorageCasMutation {
                    key: STORAGE_KEY.to_owned(),
                    expected_version: self.storage_version,
                    value,
                })],
            })
        } else {
            storage_cas(&request, STORAGE_KEY, self.storage_version, value)
        };
        self.pending = Some(PendingUpdate {
            request,
            operation_id,
            session,
            accounts,
            token_order,
            message,
        });
        vec![command]
    }

    fn command(&mut self, invoked: &CommandInvoked) -> Vec<Command> {
        if !self.loaded {
            return vec![message_session(
                invoked.session,
                "Economy is still loading.",
            )];
        }
        let Some(actor) = normalize_uuid(&invoked.player) else {
            return Vec::new();
        };
        let words = invoked.raw_arguments.split_whitespace().collect::<Vec<_>>();
        match invoked.name.as_str() {
            "money" => self.money(invoked.session, &actor, &words),
            "pay" => self.pay(invoked, &actor, &words),
            "econadmin" => self.admin(invoked, &words),
            _ => Vec::new(),
        }
    }

    fn money(&self, session: types::SessionId, actor: &str, words: &[&str]) -> Vec<Command> {
        if !words.is_empty() {
            return vec![message_session(session, "Usage: /money")];
        }
        vec![message_session(
            session,
            format!(
                "Balance: {} {}.",
                self.balance(actor),
                self.settings().currency_name
            ),
        )]
    }

    fn pay(&mut self, invoked: &CommandInvoked, actor: &str, words: &[&str]) -> Vec<Command> {
        if words.len() != 3 {
            return vec![message_session(
                invoked.session,
                "Usage: /pay <player-uuid> <amount> <token>",
            )];
        }
        let target = normalize_uuid(words[0]);
        let amount = parse_unsigned_integer(words[1]);
        let token = words[2].to_ascii_lowercase();
        let Some(target) = target else {
            return vec![message_session(
                invoked.session,
                "Invalid recipient, amount, or token.",
            )];
        };
        let Some(amount) = amount else {
            return vec![message_session(
                invoked.session,
                "Invalid recipient, amount, or token.",
            )];
        };
        if target == actor
            || !(1..=self.settings().maximum_balance).contains(&amount)
            || !valid_token(&token)
        {
            return vec![message_session(
                invoked.session,
                "Invalid recipient, amount, or token.",
            )];
        }
        let token_key = format!("{actor}:{token}");
        if self.tokens.contains(&token_key) {
            return vec![message_session(
                invoked.session,
                "That transfer token was already committed.",
            )];
        }
        if self.pending.is_some() || self.pending_pay_status.is_some() {
            return vec![message_session(
                invoked.session,
                "Another economy update is committing; retry.",
            )];
        }
        // The bounded ledger evicts old tokens. Its durable operation receipt
        // does not: probe before calculating another payment under the same id.
        self.sequence += 1;
        let request = format!("pay-status-{}", self.sequence);
        let operation_id = format!("pay-{actor}-{token}");
        self.pending_pay_status = Some(PendingPayStatus {
            request: request.clone(),
            operation_id: operation_id.clone(),
            session: invoked.session,
            actor: actor.to_owned(),
            target,
            amount,
            token_key,
        });
        vec![Command::OperationStatus(
            solaris_plugin_sdk::commands::OperationStatus {
                request,
                operation_id,
            },
        )]
    }

    fn issue_pay(&mut self, pay: PendingPayStatus) -> Vec<Command> {
        let actor_balance = self.balance(&pay.actor);
        let target_balance = self.balance(&pay.target);
        if actor_balance < pay.amount
            || target_balance + pay.amount > self.settings().maximum_balance
        {
            return vec![message_session(
                pay.session,
                "Transfer rejected by balance limits.",
            )];
        }
        let mut accounts = self.accounts.clone();
        if !accounts.contains_key(&pay.actor) && accounts.len() >= self.settings().maximum_accounts
        {
            return vec![message_session(pay.session, "Account limit reached.")];
        }
        accounts.insert(pay.actor, actor_balance - pay.amount);
        if !accounts.contains_key(&pay.target) && accounts.len() >= self.settings().maximum_accounts
        {
            return vec![message_session(pay.session, "Account limit reached.")];
        }
        accounts.insert(pay.target, target_balance + pay.amount);
        let mut token_order = self.token_order.clone();
        token_order.push(pay.token_key);
        while token_order.len() > self.settings().maximum_transfer_tokens {
            token_order.remove(0);
        }
        self.save(
            pay.session,
            accounts,
            token_order,
            format!("Paid {} {}.", pay.amount, self.settings().currency_name),
            Some(pay.operation_id),
        )
    }

    fn operation_answer(&mut self, answered: &OperationAnswered) -> Vec<Command> {
        if let Some(status) = self.pending_pay_status.take() {
            if status.request == answered.request
                && answered.operation_id.as_deref() == Some(status.operation_id.as_str())
            {
                return match &answered.outcome {
                    OperationOutcome::Refused(refused)
                        if refused.reason
                            == solaris_plugin_sdk::operation_types::OperationFailure::NotFound =>
                    {
                        self.issue_pay(status)
                    }
                    OperationOutcome::Committed(_) => {
                        self.loaded = false;
                        let reload = self.next_request();
                        vec![
                            message_session(
                                status.session,
                                "That transfer token was already committed.",
                            ),
                            storage_get(&reload, STORAGE_KEY),
                        ]
                    }
                    _ => vec![message_session(
                        status.session,
                        "Payment status unavailable; retry with the same token.",
                    )],
                };
            }
            self.pending_pay_status = Some(status);
        }
        let Some(pending) = self.pending.take() else {
            return Vec::new();
        };
        if pending.request != answered.request
            || pending.operation_id.as_deref() != answered.operation_id.as_deref()
            || pending.operation_id.is_none()
        {
            self.pending = Some(pending);
            return Vec::new();
        }
        let OperationOutcome::Committed(committed) = &answered.outcome else {
            self.loaded = false;
            let reload = self.next_request();
            return vec![
                message_session(
                    pending.session,
                    "Payment not confirmed; retry with the same token after reload.",
                ),
                storage_get(&reload, STORAGE_KEY),
            ];
        };
        if !matches!(&committed.payload, OperationPayload::StorageBatch(changes)
            if matches!(changes.as_slice(), [change] if change.key == STORAGE_KEY && !change.deleted))
        {
            self.loaded = false;
            return vec![message_session(
                pending.session,
                "Payment result was invalid; inspect the ledger before retrying.",
            )];
        }
        self.apply_update(pending, committed.revision)
    }

    fn apply_update(&mut self, pending: PendingUpdate, revision: u64) -> Vec<Command> {
        self.accounts = pending.accounts;
        self.token_order = pending.token_order;
        self.tokens = self.token_order.iter().cloned().collect();
        self.storage_version = Some(revision);
        vec![message_session(pending.session, pending.message)]
    }

    fn admin(&mut self, invoked: &CommandInvoked, words: &[&str]) -> Vec<Command> {
        if !invoked.operator {
            return vec![message_session(
                invoked.session,
                "Only an operator can administer balances.",
            )];
        }
        if words.len() != 3 || !matches!(words[0], "set" | "add") {
            return vec![message_session(
                invoked.session,
                "Usage: /econadmin <set|add> <player-uuid> <amount>",
            )];
        }
        let target = normalize_uuid(words[1]);
        let amount = parse_signed_integer(words[2]);
        let (Some(target), Some(amount)) = (target, amount) else {
            return vec![message_session(
                invoked.session,
                "Invalid UUID or integer amount.",
            )];
        };
        let next_amount = if words[0] == "set" {
            amount
        } else {
            match i64::try_from(self.balance(&target))
                .ok()
                .and_then(|balance| balance.checked_add(amount))
            {
                Some(value) => value,
                None => {
                    return vec![message_session(
                        invoked.session,
                        "Balance is outside configured limits.",
                    )]
                }
            }
        };
        if next_amount < 0
            || u64::try_from(next_amount)
                .ok()
                .is_none_or(|value| value > self.settings().maximum_balance)
        {
            return vec![message_session(
                invoked.session,
                "Balance is outside configured limits.",
            )];
        }
        let next_amount = u64::try_from(next_amount).expect("configured range is non-negative");
        let mut accounts = self.accounts.clone();
        if !accounts.contains_key(&target) && accounts.len() >= self.settings().maximum_accounts {
            return vec![message_session(invoked.session, "Account limit reached.")];
        }
        accounts.insert(target, next_amount);
        self.save(
            invoked.session,
            accounts,
            self.token_order.clone(),
            format!("Balance set to {next_amount}."),
            None,
        )
    }

    fn storage_get(&mut self, outcome: &StorageGetOutcome) {
        let StorageGetOutcome::Read(record) = outcome else {
            self.loaded = false;
            return;
        };
        let Some((accounts, tokens, token_order)) = self.settings().decode(record.value.as_deref())
        else {
            self.loaded = false;
            return;
        };
        self.accounts = accounts;
        self.tokens = tokens;
        self.token_order = token_order;
        self.storage_version = record.version;
        self.loaded = true;
    }

    fn storage_cas(&mut self, request: &str, outcome: &StorageCasOutcome) -> Vec<Command> {
        let Some(pending) = self.pending.take() else {
            return Vec::new();
        };
        if pending.request != request {
            self.pending = Some(pending);
            return Vec::new();
        }
        if pending.operation_id.is_some() {
            self.pending = Some(pending);
            return Vec::new();
        }
        let StorageCasOutcome::Committed(version) = outcome else {
            self.loaded = false;
            let reload = self.next_request();
            return vec![
                message_session(
                    pending.session,
                    "Economy ledger changed; retry after reload.",
                ),
                storage_get(&reload, STORAGE_KEY),
            ];
        };
        self.apply_update(pending, *version)
    }
}

impl Plugin for Economy {
    fn configure(
        &mut self,
        config: &Config,
    ) -> Result<Option<solaris_plugin_sdk::StartupContribution>, Failure> {
        Settings::parse(config)?;
        Ok(None)
    }

    fn init(&mut self, config: &Config, _context: &InitContext) -> Result<Vec<Command>, Failure> {
        self.settings = Some(Settings::parse(config)?);
        self.accounts.clear();
        self.tokens.clear();
        self.token_order.clear();
        self.storage_version = None;
        self.loaded = false;
        self.pending = None;
        self.pending_pay_status = None;
        self.sequence = 0;
        Ok(vec![storage_get("ledger-load", STORAGE_KEY)])
    }

    fn on_events(
        &mut self,
        _context: &EventContext,
        events: &[Event],
    ) -> Result<Vec<Command>, Failure> {
        let mut commands = Vec::new();
        for event in events {
            match event {
                Event::CommandInvoked(invoked)
                    if matches!(invoked.name.as_str(), "money" | "pay" | "econadmin") =>
                {
                    commands.extend(self.command(invoked));
                }
                Event::StorageGetAnswered(answered) => self.storage_get(&answered.outcome),
                Event::StorageCasAnswered(answered) => {
                    commands.extend(self.storage_cas(&answered.request, &answered.outcome));
                }
                Event::OperationAnswered(answered) => {
                    commands.extend(self.operation_answer(answered));
                }
                Event::CommandBatchRejected => {
                    self.pending = None;
                    self.pending_pay_status = None;
                }
                _ => {}
            }
        }
        Ok(commands)
    }
}

fn required_string<'a>(table: &'a Table, key: &str) -> Result<&'a str, Failure> {
    table
        .get(key)
        .and_then(Value::as_str)
        .ok_or(Failure::Invalid)
}

fn bounded_u64(table: &Table, key: &str, minimum: u64, maximum: u64) -> Result<u64, Failure> {
    table
        .get(key)
        .and_then(Value::as_integer)
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or(Failure::Invalid)
}

fn bounded_usize(
    table: &Table,
    key: &str,
    minimum: usize,
    maximum: usize,
) -> Result<usize, Failure> {
    table
        .get(key)
        .and_then(Value::as_integer)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or(Failure::Invalid)
}

fn normalize_uuid(value: &str) -> Option<String> {
    let normalized = value.replace('-', "").to_ascii_lowercase();
    (normalized.len() == 32 && normalized.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(normalized)
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn parse_unsigned_integer(value: &str) -> Option<u64> {
    let value = value.parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= u64::MAX as f64)
        .then_some(value as u64)
}

fn parse_signed_integer(value: &str) -> Option<i64> {
    let value = value.parse::<f64>().ok()?;
    (value.is_finite()
        && value.fract() == 0.0
        && value >= i64::MIN as f64
        && value <= i64::MAX as f64)
        .then_some(value as i64)
}

export_plugin!(Economy);
