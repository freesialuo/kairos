use serde::{Deserialize, Serialize};

mod webauthn;
pub use webauthn::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionScope {
    Full,
    Recovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AuthState {
    pub setup_required: bool,
    pub session: Option<SessionScope>,
    pub passkeys: u32,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRequest {
    pub code: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterVerifyRequest {
    pub response: RegistrationResponseJson,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LoginVerifyRequest {
    pub response: AuthenticationResponseJson,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RegisterVerifyResponse {
    pub id: String,
}
