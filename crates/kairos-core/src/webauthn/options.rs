use kairos_types::auth::{
    AuthenticatorSelectionJson, CreationOptionsJson, CredentialDescriptorJson,
    PublicKeyCredParamJson, RequestOptionsJson, RpEntityJson, UserEntityJson,
};

use crate::auth::crypto::b64url;

use super::{EDDSA, ES256, RS256, RpContext};

const TIMEOUT_MS: u32 = 120_000;
const RP_NAME: &str = "Kairos";
const USER_VERIFICATION: &str = "preferred";

pub fn creation_options(
    rp: &RpContext<'_>,
    user_id: &[u8; 16],
    challenge: &[u8; 32],
    exclude: &[String],
) -> CreationOptionsJson {
    CreationOptionsJson {
        rp: RpEntityJson {
            id: rp.rp_id.into(),
            name: RP_NAME.into(),
        },
        user: UserEntityJson {
            id: b64url(user_id),
            name: "owner".into(),
            display_name: "Kairos owner".into(),
        },
        challenge: b64url(challenge),
        pub_key_cred_params: [ES256, EDDSA, RS256]
            .into_iter()
            .map(|alg| PublicKeyCredParamJson {
                r#type: "public-key".into(),
                alg,
            })
            .collect(),
        authenticator_selection: AuthenticatorSelectionJson {
            resident_key: "required".into(),
            user_verification: USER_VERIFICATION.into(),
        },
        attestation: "none".into(),
        exclude_credentials: exclude
            .iter()
            .map(|id| CredentialDescriptorJson {
                r#type: "public-key".into(),
                id: id.clone(),
            })
            .collect(),
        timeout: TIMEOUT_MS,
    }
}

pub fn request_options(rp: &RpContext<'_>, challenge: &[u8; 32]) -> RequestOptionsJson {
    RequestOptionsJson {
        challenge: b64url(challenge),
        rp_id: rp.rp_id.into(),
        user_verification: USER_VERIFICATION.into(),
        allow_credentials: vec![],
        timeout: TIMEOUT_MS,
    }
}
