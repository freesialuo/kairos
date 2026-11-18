use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EventPayload {
    pub title: String,
    pub start: String,
    pub end: Option<String>,
    pub all_day: Option<bool>,
    pub timezone: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub rrule: Option<String>,
    pub exdates: Option<Vec<String>>,
    pub reminders: Option<Vec<u32>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PutEventResponse {
    pub id: i64,
    pub created: bool,
    pub reminders_scheduled: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct OccurrenceDto {
    pub occ_key: String,
    pub origin: String,
    pub external_id: Option<String>,
    pub source: Option<String>,
    pub title: String,
    pub location: Option<String>,
    pub all_day: bool,
    pub start_utc: i64,
    pub end_utc: i64,
    pub start_local: String,
    pub end_local: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ListEventsResponse {
    pub occurrences: Vec<OccurrenceDto>,
    pub truncated: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TokenInfo {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}
