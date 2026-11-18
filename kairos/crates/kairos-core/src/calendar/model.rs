use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Ics,
    Api,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRow {
    pub origin: Origin,
    pub source_id: Option<i64>,
    pub series_key: String,
    pub uid: Option<String>,
    pub recurrence_id: Option<String>,
    pub external_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub all_day: bool,
    pub start_local: String,
    /// Exact start for the initial slot when local time cannot recover the API instant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_start_utc: Option<i64>,
    pub tzid: Option<String>,
    pub duration_sec: i64,
    pub rrule: Option<String>,
    pub exdates: Vec<String>,
    pub lead_minutes: Option<Vec<u32>>,
    pub valarm_minutes: Vec<u32>,
}

impl EventRow {
    pub fn content_hash(&self) -> String {
        // Only JSON primitives, arrays and string keys occur in EventRow.
        let mut object = serde_json::json!(self);
        object.sort_all_objects();
        let digest = Sha256::digest(object.to_string().as_bytes());
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpandStatus {
    Complete,
    CatchingUp,
    TooDense,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub slot: String,
    pub index: u32,
    pub hash: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    pub id: i64,
    pub row: EventRow,
    pub hash: String,
    pub updated_at: i64,
    pub expand_anchor: Option<Anchor>,
    pub expand_status: ExpandStatus,
    pub expand_warning: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub event_id: i64,
    pub series_key: String,
    pub occ_key: String,
    pub slot: String,
    pub start_utc: i64,
    pub end_utc: i64,
    pub all_day: bool,
}
