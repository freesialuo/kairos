use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RegistrationResponseJson {
    pub id: String,
    #[serde(rename = "rawId")]
    pub raw_id: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub response: AttestationResponseJson,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AttestationResponseJson {
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    #[serde(rename = "attestationObject")]
    pub attestation_object: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transports: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AuthenticationResponseJson {
    pub id: String,
    #[serde(rename = "rawId")]
    pub raw_id: String,
    #[serde(rename = "type")]
    pub r#type: String,
    pub response: AssertionResponseJson,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AssertionResponseJson {
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    #[serde(rename = "authenticatorData")]
    pub authenticator_data: String,
    pub signature: String,
    #[serde(
        rename = "userHandle",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub user_handle: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationOptionsJson {
    pub rp: RpEntityJson,
    pub user: UserEntityJson,
    pub challenge: String,
    pub pub_key_cred_params: Vec<PublicKeyCredParamJson>,
    pub authenticator_selection: AuthenticatorSelectionJson,
    pub attestation: String,
    pub exclude_credentials: Vec<CredentialDescriptorJson>,
    pub timeout: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestOptionsJson {
    pub challenge: String,
    pub rp_id: String,
    pub user_verification: String,
    pub allow_credentials: Vec<CredentialDescriptorJson>,
    pub timeout: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RpEntityJson {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserEntityJson {
    pub id: String,
    pub name: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PublicKeyCredParamJson {
    #[serde(rename = "type")]
    pub r#type: String,
    pub alg: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticatorSelectionJson {
    pub resident_key: String,
    pub user_verification: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct CredentialDescriptorJson {
    #[serde(rename = "type")]
    pub r#type: String,
    pub id: String,
}
