//! Paid shipment policy. Native inventories remain the only commodity/money authority.
use super::caravan::{Container, Contract as Caravan, Stack, StorageGuard};
use super::hire::Fence;
use serde::{Deserialize, Serialize};

pub(super) const VALUE_BYTES: usize = 4096;
pub(super) fn key(id: &str) -> String {
    format!("paid-shipment-v1/{id}")
}

/// Content-addressed immutable records carry the complete component/native receipt
/// image, never a truncated projection. Each publication is one bounded mutation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Blob {
    pub digest: String,
    pub bytes: usize,
    pub fragments: usize,
}
impl Blob {
    pub fn encode<T: Serialize>(value: &T) -> Result<(Self, Vec<String>), &'static str> {
        let text = serde_json::to_string(value).map_err(|_| "paid serialization refused")?;
        let mut chunks = Vec::new();
        let mut start = 0;
        while start < text.len() {
            let mut end = (start + VALUE_BYTES).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            chunks.push(text[start..end].to_owned());
            start = end;
        }
        Ok((
            Self {
                digest: blake3::hash(text.as_bytes()).to_hex().to_string(),
                bytes: text.len(),
                fragments: chunks.len(),
            },
            chunks,
        ))
    }
    pub fn key(&self, fragment: usize) -> String {
        format!("paid-blob-v1/{}/{fragment}", self.digest)
    }
    pub fn decode<T: for<'a> Deserialize<'a>>(&self, chunks: &[String]) -> Result<T, &'static str> {
        if chunks.len() != self.fragments || chunks.iter().any(|c| c.len() > VALUE_BYTES) {
            return Err("paid blob incomplete");
        }
        let text = chunks.concat();
        if text.len() != self.bytes
            || blake3::hash(text.as_bytes()).to_hex().as_str() != self.digest
        {
            return Err("paid blob digest mismatch");
        }
        serde_json::from_str(&text).map_err(|_| "paid blob unreadable")
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Terms {
    pub shipment: String,
    pub buyer: String,
    pub buyer_scope: String,
    pub authorized_registry_revision: u64,
    pub seller: String,
    pub source: Container,
    pub destination: Container,
    pub escrow: Container,
    pub carrier: String,
    pub recipient: String,
    pub recipient_scope: String,
    pub recipient_entity_uuid: String,
    pub quantity: u32,
    pub commodity: Stack,
    pub price: u32,
    pub emerald: Stack,
    pub escrow_slot: u8,
    pub escrow_fence: Fence,
    pub reserve_operation: String,
    pub payout_operation: String,
    pub storage_guards: Vec<StorageGuard>,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct Reservation {
    pub reference: String,
    pub revision: u64,
    pub plan_hash: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Payout {
    pub operation: String,
    pub source_slot: u8,
    pub destination_slot: u8,
    pub source_fence: Fence,
    pub destination_fence: Fence,
    pub recipient_uuid: String,
    pub route_ref: String,
    pub executor_epoch: u64,
    pub reservation: Reservation,
    pub storage_guards: Vec<StorageGuard>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum State {
    ReserveIntent,
    Escrowed,
    DeliveryRecorded,
    PayoutIntent,
    Paid,
    Lost,
    Closed,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Mandate {
    pub shipment: String,
    pub buyer: String,
    pub buyer_scope: String,
    pub registry_revision: u64,
    pub seller: String,
    pub source: Container,
    pub destination: Container,
    pub carrier: String,
    pub recipient: String,
    pub recipient_scope: String,
    pub recipient_entity_uuid: String,
    pub quantity: u32,
    pub commodity_digest: String,
    pub contract: Blob,
    pub state: State,
    pub reservation: Option<Reservation>,
    pub gross: Option<Blob>,
    pub payout: Option<Blob>,
    pub payment_receipt: Option<Blob>,
    pub loss: Option<Blob>,
    pub blocked: Option<String>,
    #[serde(default)]
    pub payout_attempt: u64,
}
impl Mandate {
    /// This is only an effect-free pre-load gate. The native physical decision
    /// must independently fence the exact mandate/registry versions at commit.
    pub fn escrow_ready_for(&self, caravan: &Caravan) -> bool {
        self.shipment == caravan.party.id
            && self.seller == caravan.owner()
            && !self.buyer_scope.is_empty()
            && !self.recipient.is_empty()
            && !self.recipient_scope.is_empty()
            && !self.recipient_entity_uuid.is_empty()
            && self.source == caravan.source
            && self.destination == caravan.destination
            && self.carrier == caravan.carrier
            && self.quantity == caravan.quantity
            && caravan.manifest.as_ref().is_none_or(|m| {
                Blob::encode(m).is_ok_and(|(b, _)| b.digest == self.commodity_digest)
            })
            && self.state == State::Escrowed
            && self.reservation.is_some()
            && self.gross.is_none()
            && self.loss.is_none()
            && self.blocked.is_none()
    }
}
impl Terms {
    pub fn matches_caravan(&self, c: &Caravan) -> bool {
        self.shipment == c.party.id
            && self.seller == c.owner()
            && self.source == c.source
            && self.destination == c.destination
            && self.escrow == c.destination
            && self.carrier == c.carrier
            && self.quantity == c.quantity
            && c.manifest.as_ref().is_some_and(|m| m == &self.commodity)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.shipment.is_empty()
            || self.buyer.is_empty()
            || self.seller.is_empty()
            || self.carrier.is_empty()
            || self.recipient.is_empty()
            || self.recipient_scope.is_empty()
            || self.recipient_entity_uuid.is_empty()
            || self.price == 0
            || self.price > 4096
            || self.quantity == 0
            || self.quantity > 4096
            || self.escrow != self.destination
            || self.escrow.scope != self.buyer_scope
            || self.emerald.resource != "minecraft:emerald"
            || self.reserve_operation == self.payout_operation
            || self.reserve_operation.is_empty()
            || self.payout_operation.is_empty()
            || self.storage_guards.is_empty()
            || self
                .storage_guards
                .iter()
                .any(|guard| guard.key.is_empty() || guard.version == 0)
        {
            return Err("exact paid shipment terms invalid");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn immutable_component_bundle_preserves_utf8_and_large_custom_data() {
        let value =
            serde_json::json!({"name":"貨物é", "custom_data":vec![255u8; 8192],"uid":"original"});
        let (blob, chunks) = Blob::encode(&value).unwrap();
        assert!(chunks.iter().all(|c| c.len() <= VALUE_BYTES));
        assert_eq!(blob.decode::<serde_json::Value>(&chunks).unwrap(), value);
        let mut damaged = chunks.clone();
        damaged[0].replace_range(..1, " ");
        assert!(blob.decode::<serde_json::Value>(&damaged).is_err());
        assert!(blob
            .decode::<serde_json::Value>(&chunks[..chunks.len() - 1])
            .is_err());
    }
    fn terms() -> Terms {
        let endpoint = Container {
            scope: "buyer".into(),
            building: "warehouse".into(),
            live_ref: "actual_certificate".into(),
            dimension: "minecraft:overworld".into(),
            at: super::super::caravan::Cell { x: 4, y: 64, z: 8 },
        };
        let stack = Stack {
            resource: "minecraft:emerald".into(),
            damage: None,
            enchantments: vec![],
            name: Some("owned coin".into()),
            model: None,
            max_damage: None,
            custom_data: Some(vec![1, 2, 3]),
            stew_effects: vec![],
        };
        Terms {
            shipment: "shipment".into(),
            buyer: "actual_buyer".into(),
            buyer_scope: "buyer".into(),
            authorized_registry_revision: 9,
            seller: "actual_seller".into(),
            source: endpoint.clone(),
            destination: endpoint.clone(),
            escrow: endpoint,
            carrier: "native_carrier".into(),
            recipient: "actual_other_native_recipient".into(),
            recipient_scope: "actual_recipient_home".into(),
            recipient_entity_uuid: "00000000-0000-0000-0000-000000000081".into(),
            quantity: 1,
            commodity: stack.clone(),
            price: 1,
            emerald: stack,
            escrow_slot: 0,
            escrow_fence: Fence {
                revision: 7,
                snapshot_hash: "actual_image".into(),
            },
            reserve_operation: "original_escrow".into(),
            payout_operation: "original_payout".into(),
            storage_guards: vec![StorageGuard {
                key: "actual/scope/head".into(),
                version: 9,
            }],
        }
    }
    #[test]
    fn mandate_requires_exact_recipient_scope_price_and_distinct_original_operations() {
        let valid = terms();
        assert!(valid.validate().is_ok());
        let mut wrong = valid.clone();
        wrong.buyer_scope = "other_scope".into();
        assert!(wrong.validate().is_err());
        let mut wrong = valid.clone();
        wrong.price = 0;
        assert!(wrong.validate().is_err());
        let mut wrong = valid.clone();
        wrong.price = 4097;
        assert!(wrong.validate().is_err());
        let mut wrong = valid;
        wrong.payout_operation = wrong.reserve_operation.clone();
        assert!(wrong.validate().is_err());
    }
}
