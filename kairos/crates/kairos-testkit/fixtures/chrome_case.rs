#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test as test;

use kairos_core::{
    auth::crypto::b64url_decode,
    webauthn::{RpContext, StoredCredential, verify_authentication, verify_registration},
};
use kairos_types::auth::{LoginVerifyRequest, RegistrationResponseJson};
use serde_json::Value;

fn challenge(client_data_json: &str) -> [u8; 32] {
    let client = b64url_decode(client_data_json).expect("browser client data base64url");
    let json: Value = serde_json::from_slice(&client).expect("browser client data JSON");
    b64url_decode(json["challenge"].as_str().expect("browser challenge"))
        .expect("browser challenge base64url")
        .try_into()
        .expect("32-byte browser challenge")
}

#[test]
fn chrome_fixture_verifies() {
    let rp = RpContext {
        rp_id: "localhost",
        origin: "http://localhost:8799",
    };
    let fixture: Value = serde_json::from_str(include_str!("chrome_registration.json"))
        .expect("Chrome registration fixture");
    let user_id: [u8; 16] =
        b64url_decode(fixture["user_id"].as_str().expect("registration user ID"))
            .expect("browser user ID base64url")
            .try_into()
            .expect("16-byte user ID");
    let registration: RegistrationResponseJson =
        serde_json::from_value(fixture["response"].clone())
            .expect("WebAuthn registration response");
    let registration_challenge = challenge(&registration.response.client_data_json);
    let new = verify_registration(&rp, &registration_challenge, &registration)
        .expect("Chrome should register its virtual authenticator");
    let stored = StoredCredential {
        id: new.id,
        public_key: new.public_key,
        alg: new.alg,
        counter: new.counter,
    };
    let login: LoginVerifyRequest =
        serde_json::from_str(include_str!("chrome_login.json")).expect("Chrome login fixture");
    let login_challenge = challenge(&login.response.response.client_data_json);
    assert_ne!(registration_challenge, login_challenge);
    verify_authentication(&rp, &login_challenge, &user_id, &stored, &login.response)
        .expect("Chrome should sign in with the registered credential");
}
