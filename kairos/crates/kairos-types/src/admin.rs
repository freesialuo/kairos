use crate::repeat::RepeatPreset;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReminderChoice {
    #[default]
    Default,
    Custom(Vec<u32>),
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualEventInput {
    pub title: String,
    pub start: String,
    pub end: Option<String>,
    #[serde(default)]
    pub all_day: bool,
    pub timezone: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub repeat: Option<RepeatPreset>,
    #[serde(default)]
    pub reminders: ReminderChoice,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInput {
    pub name: Option<String>,
    pub url: Option<String>,
    pub color: Option<String>,
    pub enabled: Option<bool>,
    pub notify: Option<bool>,
    pub interval_min: Option<u32>,
    pub honor_valarm: Option<bool>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_lead_min: Option<Option<u32>>,
}
fn nullable<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Option<Option<u32>>, D::Error> {
    Option::<u32>::deserialize(decoder).map(Some)
}
