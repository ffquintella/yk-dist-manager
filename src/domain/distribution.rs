//! The distribution event: *who* handed *which* key to *whom*, *when*, and
//! *what* was applied to it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryMethod {
    /// Handed over in person, receipt signed on the spot.
    InPerson,
    /// Sent by internal mail / courier.
    Courier,
    /// Sent by post to a remote holder.
    Post,
}

impl DeliveryMethod {
    pub const ALL: [DeliveryMethod; 3] = [
        DeliveryMethod::InPerson,
        DeliveryMethod::Courier,
        DeliveryMethod::Post,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            DeliveryMethod::InPerson => "In person",
            DeliveryMethod::Courier => "Internal courier",
            DeliveryMethod::Post => "Post",
        }
    }
}

/// One handover. Immutable except for the return fields — a correction is a new
/// record plus an audit entry, never an in-place rewrite of history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributionRecord {
    pub id: Uuid,
    pub key_id: Uuid,
    /// Denormalised so a distribution report stays readable if a key record is
    /// later archived.
    pub key_serial: u32,
    pub holder_id: Uuid,
    pub holder_display: String,
    pub distributed_at: DateTime<Utc>,
    /// The operator who performed the handover ("pessoa que fez a distribuição").
    pub distributed_by: String,
    pub method: DeliveryMethod,
    /// Reference to the signed responsibility term / receipt.
    pub receipt_ref: String,
    /// The bootstrap run applied to this key before handover, if any.
    pub bootstrap_run_id: Option<Uuid>,
    pub returned_at: Option<DateTime<Utc>>,
    pub returned_to: Option<String>,
    pub notes: String,
}

impl DistributionRecord {
    /// True while the key is still with the holder.
    pub fn is_open(&self) -> bool {
        self.returned_at.is_none()
    }

    /// Days the key has been (or was) with the holder.
    pub fn days_held(&self, now: DateTime<Utc>) -> i64 {
        let end = self.returned_at.unwrap_or(now);
        (end - self.distributed_at).num_days()
    }
}
