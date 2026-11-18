#[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub ok: bool,
}

pub mod auth;
