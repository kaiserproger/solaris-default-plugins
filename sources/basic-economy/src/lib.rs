//! Item-currency market component used by the core-client and inventory fixtures.
//!
//! The component retains Basic Economy's package id, `shop:economy:<uuid>` key,
//! v2 ledger representation, menu purchase/refund flow, and the inventory/storage
//! transaction authority owned by Solaris.

use std::collections::{BTreeMap, BTreeSet};

use solaris_plugin_sdk::commands::{InventoryMenuSlot, InventoryResourceDelta};
use solaris_plugin_sdk::events::{
    Event, InventoryClick, InventoryMenuClicked, InventoryStorageOutcome,
    InventoryStorageTransactionAnswered, PlayerZoneTransition, StorageGetAnswered,
};
use solaris_plugin_sdk::storage::{StorageCasMutation, StorageMutation};
use solaris_plugin_sdk::{
    close_inventory_menu, export_plugin, inventory_storage_transaction, message_session,
    open_inventory_menu, storage_get, types, Command, Config, EventContext, Failure, InitContext,
    Plugin, StartupContribution, StorageGetOutcome,
};
use toml::{Table, Value};

const SHOP_ID: &str = "economy";
const MAX_ACTIVE_PLAYERS: usize = 64;
const MAX_PURCHASES_PER_ITEM: u32 = 999_999;
const MAX_LEDGER_ENTRIES: usize = 64;

#[derive(Clone)]
struct Currency {
    resource: String,
    singular: String,
    plural: String,
}

#[derive(Clone)]
struct Zone {
    id: String,
    dimension: String,
    minimum: types::Position,
    maximum: types::Position,
}

#[derive(Clone)]
struct Product {
    id: String,
    resource: String,
    count: u8,
    label: String,
    price: i16,
}

#[derive(Clone)]
struct Settings {
    currency: Currency,
    zone: Zone,
    catalog: Vec<Product>,
}

#[derive(Clone, PartialEq, Eq)]
struct PurchaseRecord {
    resource: String,
    item_count: u8,
    price: i16,
    currency_resource: String,
    purchases: u32,
}

#[derive(Clone)]
struct Ledger {
    session: types::SessionId,
    key: String,
    version: Option<u64>,
    records: BTreeMap<String, PurchaseRecord>,
}

struct PendingRead {
    player: String,
    session: types::SessionId,
    key: String,
}

struct ActiveMenu {
    session: types::SessionId,
    id: String,
}

#[derive(Clone, Copy)]
enum TransactionKind {
    Buy,
    Refund,
}

struct PendingTransaction {
    player: String,
    session: types::SessionId,
    kind: TransactionKind,
    item_label: String,
}

struct DeferredNotice {
    session: types::SessionId,
    message: String,
}

enum RejectedBatch {
    Read {
        request: String,
        player: String,
        session: types::SessionId,
    },
    Open {
        player: String,
        session: types::SessionId,
    },
    Transaction {
        request: String,
        active: ActiveMenu,
        player: String,
    },
}

#[derive(Default)]
struct BasicEconomy {
    settings: Option<Settings>,
    ledgers: BTreeMap<String, Ledger>,
    pending_reads: BTreeMap<String, PendingRead>,
    active_menus: BTreeMap<String, ActiveMenu>,
    pending_transactions: BTreeMap<String, PendingTransaction>,
    deferred_notices: BTreeMap<String, DeferredNotice>,
    sequence: u64,
    rejected_batches: Vec<RejectedBatch>,
}

impl Settings {
    fn parse(config: &Config) -> Result<Self, Failure> {
        let root = config
            .toml()
            .and_then(|value| value.as_table().cloned())
            .ok_or(Failure::Invalid)?;
        let currency_table = required_table(&root, "currency")?;
        let currency = Currency {
            resource: resource_id(currency_table, "resource")?,
            singular: bounded_string(currency_table, "singular", 1, 128)?,
            plural: bounded_string(currency_table, "plural", 1, 128)?,
        };

        let zone_table = required_table(&root, "zone")?;
        let minimum = parse_position(zone_table, "minimum")?;
        let maximum = parse_position(zone_table, "maximum")?;
        if minimum.x > maximum.x || minimum.y > maximum.y || minimum.z > maximum.z {
            return Err(Failure::Invalid);
        }
        let zone = Zone {
            id: script_id(zone_table, "id")?,
            dimension: resource_id(zone_table, "dimension")?,
            minimum,
            maximum,
        };

        let catalog = root
            .get("catalog")
            .and_then(Value::as_array)
            .ok_or(Failure::Invalid)?;
        if catalog.is_empty() || catalog.len() > 16 {
            return Err(Failure::Invalid);
        }
        let mut product_ids = BTreeSet::new();
        let mut resources = BTreeSet::from([currency.resource.clone()]);
        let mut parsed_catalog = Vec::with_capacity(catalog.len());
        for value in catalog {
            let item = value.as_table().ok_or(Failure::Invalid)?;
            let id = script_id(item, "id")?;
            let resource = resource_id(item, "resource")?;
            let count = bounded_u8(item, "count", 1, 64)?;
            let label = bounded_string(item, "label", 1, 48)?;
            let price = bounded_i16(item, "price", 1, 64)?;
            if !product_ids.insert(id.clone()) || !resources.insert(resource.clone()) {
                return Err(Failure::Invalid);
            }
            parsed_catalog.push(Product {
                id,
                resource,
                count,
                label,
                price,
            });
        }
        Ok(Self {
            currency,
            zone,
            catalog: parsed_catalog,
        })
    }
}

impl BasicEconomy {
    fn settings(&self) -> &Settings {
        self.settings
            .as_ref()
            .expect("configure and init establish settings")
    }

    fn next_request(&mut self, kind: &str) -> String {
        self.sequence += 1;
        format!("{kind}-{}", self.sequence)
    }

    fn player_has_pending_transaction(&self, player: &str) -> bool {
        self.pending_transactions
            .values()
            .any(|pending| pending.player == player)
    }

    fn start_catalog_read(&mut self, player: &str, session: types::SessionId) -> Vec<Command> {
        if self.active_menus.contains_key(player)
            || self
                .pending_reads
                .values()
                .any(|pending| pending.player == player)
            || self.player_has_pending_transaction(player)
        {
            return Vec::new();
        }
        if !self.ledgers.contains_key(player)
            && self.ledgers.len() + self.pending_reads.len() >= MAX_ACTIVE_PLAYERS
        {
            return vec![message_session(
                session,
                "Economy unavailable: active-player limit reached.",
            )];
        }
        let key = format!("shop:{SHOP_ID}:{player}");
        let request = self.next_request("read");
        self.pending_reads.insert(
            request.clone(),
            PendingRead {
                player: player.to_owned(),
                session,
                key: key.clone(),
            },
        );
        self.rejected_batches.push(RejectedBatch::Read {
            request: request.clone(),
            player: player.to_owned(),
            session,
        });
        vec![storage_get(&request, &key)]
    }

    fn open_catalog(&mut self, player: &str) -> Vec<Command> {
        let ledger = self
            .ledgers
            .get(player)
            .expect("only loaded ledger opens a menu");
        let menu_id = ledger.version.map_or_else(
            || "economy-new".to_owned(),
            |version| format!("economy-v{version}"),
        );
        let session = ledger.session;
        let records = ledger.records.clone();
        let currency = self.settings().currency.clone();
        let slots = self
            .settings()
            .catalog
            .iter()
            .enumerate()
            .map(|(index, product)| {
                let owned = records
                    .get(&product.id)
                    .map_or(0, |record| record.purchases);
                let changed_terms = records.get(&product.id).is_some_and(|record| {
                    !record_matches_product(record, product, &currency.resource)
                });
                let terms = if changed_terms {
                    " | changed terms: refund only"
                } else {
                    ""
                };
                InventoryMenuSlot {
                    slot: u8::try_from(index).expect("catalog holds at most 16 items"),
                    resource: product.resource.clone(),
                    count: product.count,
                    label: Some(format!(
                        "{} | buy {} {}{} | refund | owned {}",
                        product.label,
                        product.price,
                        currency_name(&currency, product.price),
                        terms,
                        owned
                    )),
                }
            })
            .collect();
        self.active_menus.insert(
            player.to_owned(),
            ActiveMenu {
                session,
                id: menu_id.clone(),
            },
        );
        self.rejected_batches.push(RejectedBatch::Open {
            player: player.to_owned(),
            session,
        });
        vec![open_inventory_menu(
            session,
            &menu_id,
            &format!("Market - {}", currency.plural),
            slots,
        )]
    }

    fn storage_read(&mut self, answered: &StorageGetAnswered) -> Vec<Command> {
        let Some(pending) = self.pending_reads.remove(&answered.request) else {
            return Vec::new();
        };
        let StorageGetOutcome::Read(record) = &answered.outcome else {
            return vec![message_session(
                pending.session,
                "Economy unavailable: storage read failed.",
            )];
        };
        let Some(records) = decode_records(record.value.as_deref()) else {
            return vec![message_session(
                pending.session,
                "Economy unavailable: invalid ledger record.",
            )];
        };
        self.ledgers.insert(
            pending.player.clone(),
            Ledger {
                session: pending.session,
                key: pending.key,
                version: record.version,
                records,
            },
        );
        self.open_catalog(&pending.player)
    }

    fn click(&mut self, clicked: &InventoryMenuClicked) -> Vec<Command> {
        if !matches!(
            clicked.click,
            InventoryClick::Primary | InventoryClick::Secondary
        ) {
            return Vec::new();
        }
        let Some(active) = self.active_menus.get(&clicked.player) else {
            return Vec::new();
        };
        if active.id != clicked.menu || active.session != clicked.session {
            return Vec::new();
        }
        if self.player_has_pending_transaction(&clicked.player) {
            return Vec::new();
        }
        let Some(product) = self
            .settings()
            .catalog
            .get(usize::from(clicked.slot))
            .cloned()
        else {
            return Vec::new();
        };
        let Some(ledger) = self.ledgers.get(&clicked.player).cloned() else {
            return Vec::new();
        };
        let kind = if matches!(clicked.click, InventoryClick::Primary) {
            TransactionKind::Buy
        } else {
            TransactionKind::Refund
        };
        let existing = ledger.records.get(&product.id).cloned();
        match kind {
            TransactionKind::Refund if existing.is_none() => {
                return vec![message_session(
                    clicked.session,
                    "Nothing from this shop is eligible for refund.",
                )]
            }
            TransactionKind::Buy
                if existing.as_ref().is_some_and(|record| {
                    !record_matches_product(record, &product, &self.settings().currency.resource)
                }) =>
            {
                return vec![message_session(
                    clicked.session,
                    "Product terms changed; refund old purchases first.",
                )]
            }
            TransactionKind::Buy
                if existing
                    .as_ref()
                    .is_some_and(|record| record.purchases >= MAX_PURCHASES_PER_ITEM) =>
            {
                return vec![message_session(
                    clicked.session,
                    "Purchase limit reached for this product.",
                )]
            }
            _ => {}
        }

        let mut records = ledger.records.clone();
        let currency = self.settings().currency.clone();
        let inventory = match kind {
            TransactionKind::Buy => {
                let record = records.entry(product.id.clone()).or_insert(PurchaseRecord {
                    resource: product.resource.clone(),
                    item_count: product.count,
                    price: product.price,
                    currency_resource: currency.resource.clone(),
                    purchases: 0,
                });
                record.purchases += 1;
                vec![
                    InventoryResourceDelta {
                        resource: currency.resource.clone(),
                        delta: -product.price,
                    },
                    InventoryResourceDelta {
                        resource: product.resource.clone(),
                        delta: i16::from(product.count),
                    },
                ]
            }
            TransactionKind::Refund => {
                let record = existing.expect("refund requires a ledger record");
                let next = records
                    .get_mut(&product.id)
                    .expect("record was cloned into next ledger");
                next.purchases -= 1;
                if next.purchases == 0 {
                    records.remove(&product.id);
                }
                vec![
                    InventoryResourceDelta {
                        resource: record.currency_resource,
                        delta: record.price,
                    },
                    InventoryResourceDelta {
                        resource: record.resource,
                        delta: -i16::from(record.item_count),
                    },
                ]
            }
        };
        let request = self.next_request(match kind {
            TransactionKind::Buy => "buy",
            TransactionKind::Refund => "refund",
        });
        let active = self
            .active_menus
            .remove(&clicked.player)
            .expect("validated active menu");
        self.pending_transactions.insert(
            request.clone(),
            PendingTransaction {
                player: clicked.player.clone(),
                session: clicked.session,
                kind,
                item_label: product.label,
            },
        );
        self.rejected_batches.push(RejectedBatch::Transaction {
            request: request.clone(),
            active,
            player: clicked.player.clone(),
        });
        vec![
            close_inventory_menu(clicked.session, &clicked.menu),
            inventory_storage_transaction(
                &request,
                clicked.session,
                inventory,
                vec![StorageMutation::Cas(StorageCasMutation {
                    key: ledger.key,
                    expected_version: ledger.version,
                    value: encode_records(&records),
                })],
            ),
        ]
    }

    fn transaction_answered(
        &mut self,
        answered: &InventoryStorageTransactionAnswered,
    ) -> Vec<Command> {
        let Some(pending) = self.pending_transactions.remove(&answered.request) else {
            return Vec::new();
        };
        self.ledgers.remove(&pending.player);
        let message = match (&pending.kind, &answered.outcome) {
            (TransactionKind::Buy, InventoryStorageOutcome::Committed) => {
                format!("Purchased {}.", pending.item_label)
            }
            (TransactionKind::Refund, InventoryStorageOutcome::Committed) => {
                format!("Refunded {}.", pending.item_label)
            }
            (_, InventoryStorageOutcome::Refused) => {
                "Transaction rejected: inventory or storage precondition changed.".to_owned()
            }
        };
        let mut commands = vec![message_session(pending.session, message)];
        commands.extend(self.start_catalog_read(&pending.player, pending.session));
        commands
    }

    fn clear_player_session(
        &mut self,
        player: &str,
        session: types::SessionId,
        close_menu: bool,
    ) -> Vec<Command> {
        if self
            .ledgers
            .get(player)
            .is_some_and(|ledger| ledger.session == session)
        {
            self.ledgers.remove(player);
        }
        self.pending_reads
            .retain(|_, pending| pending.player != player || pending.session != session);
        self.pending_transactions
            .retain(|_, pending| pending.player != player || pending.session != session);
        if self
            .deferred_notices
            .get(player)
            .is_some_and(|notice| notice.session == session)
        {
            self.deferred_notices.remove(player);
        }
        self.rejected_batches.retain(|batch| match batch {
            RejectedBatch::Read {
                player: batch_player,
                session: batch_session,
                ..
            }
            | RejectedBatch::Open {
                player: batch_player,
                session: batch_session,
            } => batch_player != player || *batch_session != session,
            RejectedBatch::Transaction {
                player: batch_player,
                active,
                ..
            } => batch_player != player || active.session != session,
        });
        let Some(menu) = self.active_menus.get(player) else {
            return Vec::new();
        };
        if menu.session != session {
            return Vec::new();
        }
        let menu = self
            .active_menus
            .remove(player)
            .expect("matching menu remains present");
        if close_menu {
            vec![close_inventory_menu(menu.session, &menu.id)]
        } else {
            Vec::new()
        }
    }

    fn begin_output_batch(&mut self) {
        self.rejected_batches.clear();
    }

    fn reject_last_batch(&mut self) {
        for batch in std::mem::take(&mut self.rejected_batches).into_iter().rev() {
            match batch {
                RejectedBatch::Read { request, .. } => {
                    if let Some(pending) = self.pending_reads.remove(&request) {
                        self.deferred_notices.insert(
                            pending.player,
                            DeferredNotice {
                                session: pending.session,
                                message: "Economy request rejected by server admission.".to_owned(),
                            },
                        );
                    }
                }
                RejectedBatch::Open { player, session } => {
                    if self
                        .active_menus
                        .get(&player)
                        .is_some_and(|menu| menu.session == session)
                    {
                        self.active_menus.remove(&player);
                    }
                    self.deferred_notices.insert(
                        player,
                        DeferredNotice {
                            session,
                            message: "Economy menu rejected by server admission.".to_owned(),
                        },
                    );
                }
                RejectedBatch::Transaction {
                    request,
                    active,
                    player,
                } => {
                    self.pending_transactions.remove(&request);
                    let session = active.session;
                    self.active_menus.insert(player.clone(), active);
                    self.deferred_notices.insert(
                        player,
                        DeferredNotice {
                            session,
                            message: "Economy transaction rejected by server admission.".to_owned(),
                        },
                    );
                }
            }
        }
    }

    fn zone_entered(&mut self, transition: &PlayerZoneTransition) -> Vec<Command> {
        if transition.zone != self.settings().zone.id {
            return Vec::new();
        }
        self.start_catalog_read(&transition.player, transition.session)
    }

    fn zone_exited(&mut self, transition: &PlayerZoneTransition) -> Vec<Command> {
        if transition.zone != self.settings().zone.id {
            return Vec::new();
        }
        self.clear_player_session(&transition.player, transition.session, true)
    }

    fn player_left(&mut self, session: types::SessionId) {
        let mut players = BTreeSet::new();
        players.extend(
            self.ledgers
                .iter()
                .filter(|(_, ledger)| ledger.session == session)
                .map(|(player, _)| player.clone()),
        );
        players.extend(
            self.pending_reads
                .values()
                .filter(|pending| pending.session == session)
                .map(|pending| pending.player.clone()),
        );
        players.extend(
            self.active_menus
                .iter()
                .filter(|(_, menu)| menu.session == session)
                .map(|(player, _)| player.clone()),
        );
        players.extend(
            self.pending_transactions
                .values()
                .filter(|pending| pending.session == session)
                .map(|pending| pending.player.clone()),
        );
        players.extend(
            self.deferred_notices
                .iter()
                .filter(|(_, notice)| notice.session == session)
                .map(|(player, _)| player.clone()),
        );
        players.extend(
            self.rejected_batches
                .iter()
                .filter_map(|batch| match batch {
                    RejectedBatch::Read {
                        player,
                        session: batch_session,
                        ..
                    }
                    | RejectedBatch::Open {
                        player,
                        session: batch_session,
                    } if *batch_session == session => Some(player.clone()),
                    RejectedBatch::Transaction { player, active, .. }
                        if active.session == session =>
                    {
                        Some(player.clone())
                    }
                    _ => None,
                }),
        );
        for player in players {
            self.clear_player_session(&player, session, false);
        }
    }
}

impl Plugin for BasicEconomy {
    fn configure(&mut self, config: &Config) -> Result<Option<StartupContribution>, Failure> {
        Settings::parse(config)?;
        Ok(None)
    }

    fn init(&mut self, config: &Config, _context: &InitContext) -> Result<Vec<Command>, Failure> {
        let settings = Settings::parse(config)?;
        let zone = settings.zone.clone();
        self.settings = Some(settings);
        self.ledgers.clear();
        self.pending_reads.clear();
        self.active_menus.clear();
        self.pending_transactions.clear();
        self.deferred_notices.clear();
        self.sequence = 0;
        self.rejected_batches.clear();
        Ok(vec![Command::UpsertZone(
            solaris_plugin_sdk::commands::UpsertZone {
                zone: zone.id,
                dimension: zone.dimension,
                minimum: zone.minimum,
                maximum: zone.maximum,
            },
        )])
    }

    fn on_events(
        &mut self,
        _context: &EventContext,
        events: &[Event],
    ) -> Result<Vec<Command>, Failure> {
        if !matches!(events, [Event::CommandBatchRejected]) {
            self.begin_output_batch();
        }
        let mut commands = Vec::new();
        for event in events {
            match event {
                Event::CommandInvoked(invoked) if invoked.name == "economy" => {
                    let words = invoked.raw_arguments.split_whitespace().collect::<Vec<_>>();
                    if words.is_empty() || words == ["shop"] {
                        if let Some(notice) = self.deferred_notices.remove(&invoked.player) {
                            if notice.session == invoked.session {
                                commands.push(message_session(invoked.session, notice.message));
                            }
                        }
                        commands.extend(self.start_catalog_read(&invoked.player, invoked.session));
                    } else {
                        commands.push(message_session(invoked.session, "Usage: /economy [shop]"));
                    }
                }
                Event::StorageGetAnswered(answered) => commands.extend(self.storage_read(answered)),
                Event::InventoryMenuClicked(clicked) => commands.extend(self.click(clicked)),
                Event::InventoryStorageTransactionAnswered(answered) => {
                    commands.extend(self.transaction_answered(answered));
                }
                Event::PlayerZoneEntered(transition) => {
                    commands.extend(self.zone_entered(transition))
                }
                Event::PlayerZoneExited(transition) => {
                    commands.extend(self.zone_exited(transition))
                }
                Event::PlayerLeft(left) => self.player_left(left.session),
                Event::CommandBatchRejected => self.reject_last_batch(),
                _ => {}
            }
        }
        Ok(commands)
    }
}

fn required_table<'a>(table: &'a Table, key: &str) -> Result<&'a Table, Failure> {
    table
        .get(key)
        .and_then(Value::as_table)
        .ok_or(Failure::Invalid)
}

fn bounded_string(
    table: &Table,
    key: &str,
    minimum: usize,
    maximum: usize,
) -> Result<String, Failure> {
    let value = table
        .get(key)
        .and_then(Value::as_str)
        .ok_or(Failure::Invalid)?;
    if !(minimum..=maximum).contains(&value.len()) {
        return Err(Failure::Invalid);
    }
    Ok(value.to_owned())
}

fn resource_id(table: &Table, key: &str) -> Result<String, Failure> {
    let value = bounded_string(table, key, 3, 128)?;
    let Some((namespace, path)) = value.split_once(':') else {
        return Err(Failure::Invalid);
    };
    if namespace.is_empty()
        || path.is_empty()
        || !namespace.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
        })
        || !path.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'.' | b'/' | b'-')
        })
    {
        return Err(Failure::Invalid);
    }
    Ok(value)
}

fn script_id(table: &Table, key: &str) -> Result<String, Failure> {
    let value = bounded_string(table, key, 1, 64)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
    }) {
        return Err(Failure::Invalid);
    }
    Ok(value)
}

fn bounded_u8(table: &Table, key: &str, minimum: u8, maximum: u8) -> Result<u8, Failure> {
    let value = table
        .get(key)
        .and_then(Value::as_integer)
        .ok_or(Failure::Invalid)?;
    u8::try_from(value)
        .ok()
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or(Failure::Invalid)
}

fn bounded_i16(table: &Table, key: &str, minimum: i16, maximum: i16) -> Result<i16, Failure> {
    let value = table
        .get(key)
        .and_then(Value::as_integer)
        .ok_or(Failure::Invalid)?;
    i16::try_from(value)
        .ok()
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or(Failure::Invalid)
}

fn parse_position(table: &Table, key: &str) -> Result<types::Position, Failure> {
    let position = required_table(table, key)?;
    let coordinate = |axis| {
        let value = position.get(axis).and_then(Value::as_float).or_else(|| {
            position
                .get(axis)
                .and_then(Value::as_integer)
                .map(|value| value as f64)
        });
        value
            .filter(|value| value.is_finite() && value.abs() <= 30_000_000.0)
            .ok_or(Failure::Invalid)
    };
    Ok(types::Position {
        x: coordinate("x")?,
        y: coordinate("y")?,
        z: coordinate("z")?,
    })
}

fn currency_name(currency: &Currency, amount: i16) -> &str {
    if amount == 1 {
        &currency.singular
    } else {
        &currency.plural
    }
}

fn record_matches_product(
    record: &PurchaseRecord,
    product: &Product,
    currency_resource: &str,
) -> bool {
    record.resource == product.resource
        && record.item_count == product.count
        && record.price == product.price
        && record.currency_resource == currency_resource
}

fn encode_records(records: &BTreeMap<String, PurchaseRecord>) -> String {
    let entries = records
        .iter()
        .map(|(id, record)| {
            format!(
                "{id},{},{},{},{},{}",
                record.resource,
                record.item_count,
                record.price,
                record.currency_resource,
                record.purchases
            )
        })
        .collect::<Vec<_>>();
    format!("v2|{}", entries.join(";"))
}

fn decode_records(value: Option<&str>) -> Option<BTreeMap<String, PurchaseRecord>> {
    let Some(value) = value else {
        return Some(BTreeMap::new());
    };
    let payload = value.strip_prefix("v2|")?;
    if payload.is_empty() {
        return Some(BTreeMap::new());
    }
    let mut records = BTreeMap::new();
    for encoded in payload.split(';') {
        let fields = encoded.split(',').collect::<Vec<_>>();
        let [id, resource, item_count, price, currency_resource, purchases] = fields.as_slice()
        else {
            return None;
        };
        if !valid_script_id(id)
            || !valid_resource_id(resource)
            || !valid_resource_id(currency_resource)
        {
            return None;
        }
        let item_count = item_count
            .parse::<u8>()
            .ok()
            .filter(|value| (1..=64).contains(value))?;
        let price = price
            .parse::<i16>()
            .ok()
            .filter(|value| (1..=64).contains(value))?;
        let purchases = purchases
            .parse::<u32>()
            .ok()
            .filter(|value| (1..=MAX_PURCHASES_PER_ITEM).contains(value))?;
        if records.len() >= MAX_LEDGER_ENTRIES || records.contains_key(*id) {
            return None;
        }
        records.insert(
            (*id).to_owned(),
            PurchaseRecord {
                resource: (*resource).to_owned(),
                item_count,
                price,
                currency_resource: (*currency_resource).to_owned(),
                purchases,
            },
        );
    }
    Some(records)
}

fn valid_script_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

fn valid_resource_id(value: &str) -> bool {
    let Some((namespace, path)) = value.split_once(':') else {
        return false;
    };
    !namespace.is_empty()
        && !path.is_empty()
        && namespace.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
        })
        && path.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'.' | b'/' | b'-')
        })
}

export_plugin!(BasicEconomy);

#[cfg(test)]
mod tests {
    use super::{
        decode_records, encode_records, BasicEconomy, DeferredNotice, Ledger, PendingRead,
        PendingTransaction, PurchaseRecord, RejectedBatch, TransactionKind,
    };
    use std::collections::BTreeMap;

    #[test]
    fn v2_ledger_round_trips_in_product_order() {
        let mut records = BTreeMap::new();
        records.insert(
            "apples".to_owned(),
            PurchaseRecord {
                resource: "minecraft:apple".to_owned(),
                item_count: 2,
                price: 3,
                currency_resource: "minecraft:emerald".to_owned(),
                purchases: 1,
            },
        );
        let encoded = encode_records(&records);
        assert_eq!(encoded, "v2|apples,minecraft:apple,2,3,minecraft:emerald,1");
        assert_eq!(
            decode_records(Some(&encoded))
                .unwrap()
                .get("apples")
                .unwrap()
                .purchases,
            1
        );
        assert!(decode_records(None).unwrap().is_empty());
    }

    #[test]
    fn player_left_discards_only_departed_session_correlations() {
        let mut economy = BasicEconomy::default();
        economy.pending_reads.insert(
            "old-read".to_owned(),
            PendingRead {
                player: "pending-player".to_owned(),
                session: 7,
                key: "shop:economy:pending-player".to_owned(),
            },
        );
        economy.pending_reads.insert(
            "new-read".to_owned(),
            PendingRead {
                player: "pending-player".to_owned(),
                session: 8,
                key: "shop:economy:pending-player".to_owned(),
            },
        );
        economy.pending_transactions.insert(
            "old-transaction".to_owned(),
            PendingTransaction {
                player: "transaction-player".to_owned(),
                session: 7,
                kind: TransactionKind::Buy,
                item_label: "Apples".to_owned(),
            },
        );
        economy.ledgers.insert(
            "ledger-player".to_owned(),
            Ledger {
                session: 7,
                key: "shop:economy:ledger-player".to_owned(),
                version: None,
                records: BTreeMap::new(),
            },
        );
        economy.active_menus.insert(
            "menu-player".to_owned(),
            super::ActiveMenu {
                session: 7,
                id: "economy-new".to_owned(),
            },
        );
        economy.deferred_notices.insert(
            "notice-player".to_owned(),
            DeferredNotice {
                session: 7,
                message: "old notice".to_owned(),
            },
        );
        economy.rejected_batches = vec![
            RejectedBatch::Read {
                request: "old-read".to_owned(),
                player: "pending-player".to_owned(),
                session: 7,
            },
            RejectedBatch::Read {
                request: "new-read".to_owned(),
                player: "pending-player".to_owned(),
                session: 8,
            },
        ];

        economy.player_left(7);

        assert!(!economy.pending_reads.contains_key("old-read"));
        assert!(economy.pending_reads.contains_key("new-read"));
        assert!(economy.pending_transactions.is_empty());
        assert!(economy.ledgers.is_empty());
        assert!(economy.active_menus.is_empty());
        assert!(economy.deferred_notices.is_empty());
        assert!(matches!(
            economy.rejected_batches.as_slice(),
            [RejectedBatch::Read {
                request,
                session: 8,
                ..
            }] if request == "new-read"
        ));
    }

    #[test]
    fn rejected_batch_discards_every_pending_correlation() {
        let mut economy = BasicEconomy::default();
        for (request, player, session) in [("read-a", "player-a", 7), ("read-b", "player-b", 8)] {
            economy.pending_reads.insert(
                request.to_owned(),
                PendingRead {
                    player: player.to_owned(),
                    session,
                    key: format!("shop:economy:{player}"),
                },
            );
            economy.rejected_batches.push(RejectedBatch::Read {
                request: request.to_owned(),
                player: player.to_owned(),
                session,
            });
        }

        economy.reject_last_batch();

        assert!(economy.pending_reads.is_empty());
        assert!(economy.rejected_batches.is_empty());
        assert_eq!(economy.deferred_notices.get("player-a").unwrap().session, 7);
        assert_eq!(economy.deferred_notices.get("player-b").unwrap().session, 8);
    }

    #[test]
    fn next_output_batch_discards_accepted_batch_rollback_state() {
        let mut economy = BasicEconomy::default();
        economy.rejected_batches.push(RejectedBatch::Read {
            request: "accepted-read".to_owned(),
            player: "player".to_owned(),
            session: 7,
        });

        economy.begin_output_batch();

        assert!(economy.rejected_batches.is_empty());
    }
}
