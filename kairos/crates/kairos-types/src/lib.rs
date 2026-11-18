#[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct HealthResponse {
    pub ok: bool,
}

pub mod admin;
pub mod api_v1;
pub mod auth;
pub mod limits;
pub mod repeat;
