use kairos_core::{
    auth::crypto::b64url,
    webauthn::{
        RpContext, StoredCredential, WebauthnError, creation_options, request_options,
        verify_authentication, verify_registration,
    },
};
use kairos_testkit::authenticator::{Alg, SoftAuthenticator};
use kairos_types::auth::{AuthenticationResponseJson, RegistrationResponseJson};

mod chrome_fixture {
    include!("../fixtures/chrome_case.rs");
}

const RP: RpContext<'static> = RpContext {
    rp_id: "kairos.test",
    origin: "https://kairos.test",
};
const USER: [u8; 16] = [7; 16];
const CHALLENGE: [u8; 32] = [39; 32];

fn register(authenticator: &mut SoftAuthenticator) -> RegistrationResponseJson {
    authenticator.register(&creation_options(&RP, &USER, &CHALLENGE, &[]), RP.origin)
}

fn registered(authenticator: &mut SoftAuthenticator) -> StoredCredential {
    let response = register(authenticator);
    let key = verify_registration(&RP, &CHALLENGE, &response).unwrap();
    assert_eq!(key.id, response.id);
    assert_eq!(key.counter, 0);
    assert_eq!(key.transports, ["internal"]);
    StoredCredential {
        id: key.id,
        public_key: key.public_key,
        alg: key.alg,
        counter: key.counter,
    }
}

fn login(authenticator: &mut SoftAuthenticator) -> AuthenticationResponseJson {
    authenticator.login(&request_options(&RP, &CHALLENGE), RP.origin)
}

fn round_trip(alg: Alg, id: i64) {
    let mut authenticator = SoftAuthenticator::new(alg, 11);
    let mut cred = registered(&mut authenticator);
    assert_eq!(cred.alg, id);
    let response = login(&mut authenticator);
    cred.counter = verify_authentication(&RP, &CHALLENGE, &USER, &cred, &response).unwrap();
    assert_eq!(cred.counter, 1);
    let response = login(&mut authenticator);
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &response),
        Ok(2)
    );
}

#[test]
fn registers_and_logs_in_es256() {
    round_trip(Alg::Es256, -7);
}

#[test]
fn registers_and_logs_in_eddsa() {
    round_trip(Alg::EdDsa, -8);
}

#[test]
fn registers_and_logs_in_rs256() {
    round_trip(Alg::Rs256, -257);
}

#[test]
fn options_match_browser_json() {
    let reg = creation_options(&RP, &USER, &CHALLENGE, &["existing".into()]);
    let json = serde_json::to_value(reg).unwrap();
    assert_eq!(json["rp"]["id"], RP.rp_id);
    assert_eq!(json["user"]["id"], b64url(&USER));
    assert_eq!(json["user"]["displayName"], "Kairos owner");
    assert_eq!(json["authenticatorSelection"]["residentKey"], "required");
    assert_eq!(
        json["authenticatorSelection"]["userVerification"],
        "preferred"
    );
    assert_eq!(json["attestation"], "none");
    assert_eq!(json["timeout"], 120_000);
    assert_eq!(
        json["pubKeyCredParams"],
        serde_json::json!([
            {"type":"public-key","alg":-7}, {"type":"public-key","alg":-8}, {"type":"public-key","alg":-257}
        ])
    );
    assert_eq!(
        json["excludeCredentials"],
        serde_json::json!([{"type":"public-key","id":"existing"}])
    );
    let json = serde_json::to_value(request_options(&RP, &CHALLENGE)).unwrap();
    assert_eq!(json["rpId"], RP.rp_id);
    assert_eq!(json["allowCredentials"], serde_json::json!([]));
    assert_eq!(json["userVerification"], "preferred");
}

#[test]
fn response_fields_match_browser_json() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let reg = register(&mut a);
    let json = serde_json::to_value(&reg).unwrap();
    assert_eq!(json["rawId"], reg.raw_id);
    assert_eq!(json["type"], "public-key");
    assert_eq!(
        json["response"]["clientDataJSON"],
        reg.response.client_data_json
    );
    assert_eq!(
        json["response"]["attestationObject"],
        reg.response.attestation_object
    );
    assert_eq!(
        serde_json::from_value::<RegistrationResponseJson>(json).unwrap(),
        reg
    );
    let response = login(&mut a);
    let json = serde_json::to_value(&response).unwrap();
    assert_eq!(
        json["response"]["clientDataJSON"],
        response.response.client_data_json
    );
    assert_eq!(
        json["response"]["authenticatorData"],
        response.response.authenticator_data
    );
    assert_eq!(json["response"]["userHandle"], b64url(&USER));
    assert_eq!(
        serde_json::from_value::<AuthenticationResponseJson>(json).unwrap(),
        response
    );
}

#[test]
fn rejects_wrong_origin() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let cred = registered(&mut a);
    a.tamper().origin = Some("https://other.test".into());
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &register(&mut a)),
        Err(WebauthnError::Origin)
    );
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Err(WebauthnError::Origin)
    );
}

#[test]
fn rejects_wrong_challenge() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let cred = registered(&mut a);
    assert_eq!(
        verify_registration(&RP, &[0; 32], &register(&mut a)),
        Err(WebauthnError::Challenge)
    );
    assert_eq!(
        verify_authentication(&RP, &[0; 32], &USER, &cred, &login(&mut a)),
        Err(WebauthnError::Challenge)
    );
}

#[test]
fn rejects_create_type_on_login() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let cred = registered(&mut a);
    a.tamper().client_data_type = Some("webauthn.create".into());
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Err(WebauthnError::ClientDataType)
    );
}

#[test]
fn rejects_cross_origin_true() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let cred = registered(&mut a);
    a.tamper().cross_origin = Some(true);
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &register(&mut a)),
        Err(WebauthnError::CrossOrigin)
    );
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Err(WebauthnError::CrossOrigin)
    );
}

#[test]
fn rejects_wrong_rp_id_hash() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let cred = registered(&mut a);
    a.tamper().rp_id_hash = Some([0; 32]);
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &register(&mut a)),
        Err(WebauthnError::RpIdHash)
    );
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Err(WebauthnError::RpIdHash)
    );
}

#[test]
fn rejects_user_presence_clear() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let cred = registered(&mut a);
    a.tamper().flags = Some(0x40);
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &register(&mut a)),
        Err(WebauthnError::UserPresence)
    );
    a.tamper().flags = Some(0);
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Err(WebauthnError::UserPresence)
    );
}

#[test]
fn rejects_trailing_bytes() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let cred = registered(&mut a);
    a.tamper().trailing_bytes = vec![0];
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &register(&mut a)),
        Err(WebauthnError::TrailingBytes)
    );
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Err(WebauthnError::TrailingBytes)
    );
}

#[test]
fn accepts_extensions_map() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    a.tamper().extensions = Some(ciborium::value::Value::Map(vec![(
        ciborium::value::Value::Text("example".into()),
        ciborium::value::Value::Bool(true),
    )]));
    let cred = registered(&mut a);
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Ok(1)
    );
}

#[test]
fn accepts_uv_clear() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    a.tamper().flags = Some(0x41);
    let cred = registered(&mut a);
    a.tamper().flags = Some(0x01);
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Ok(1)
    );
}

fn cose(fields: &[(i64, ciborium::value::Value)]) -> ciborium::value::Value {
    ciborium::value::Value::Map(
        fields
            .iter()
            .map(|(k, v)| ((*k).into(), v.clone()))
            .collect(),
    )
}

#[test]
fn rejects_unsupported_alg() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    a.tamper().cose_key = Some(cose(&[
        (1, 2.into()),
        (3, (-35).into()),
        (-1, 2.into()), // P-384 + ES384
    ]));
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &register(&mut a)),
        Err(WebauthnError::UnsupportedAlgorithm)
    );
}

#[test]
fn rejects_rsa_1024() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    a.tamper().cose_key = Some(cose(&[
        (1, 3.into()),
        (3, (-257).into()),
        (-1, ciborium::value::Value::Bytes(vec![255; 128])),
        (-2, ciborium::value::Value::Bytes(vec![1, 0, 1])),
    ]));
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &register(&mut a)),
        Err(WebauthnError::WeakRsaKey)
    );
}

#[test]
fn rejects_id_mismatch() {
    let mut response = register(&mut SoftAuthenticator::new(Alg::Es256, 1));
    response.id = b64url(b"different ID");
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &response),
        Err(WebauthnError::IdMismatch)
    );
}

#[test]
fn rejects_tampered_signature_before_reporting_counter_error() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let mut cred = registered(&mut a);
    cred.counter = 30;
    a.tamper().bad_signature = true;
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Err(WebauthnError::Signature)
    );
}

#[test]
fn rejects_counter_regression() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let mut cred = registered(&mut a);
    cred.counter = 1;
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Err(WebauthnError::CounterRegression)
    );
}

#[test]
fn accepts_zero_counters() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let cred = registered(&mut a);
    a.tamper().counter = Some(0);
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Ok(0)
    );
}

#[test]
fn accepts_high_s_es256() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let cred = registered(&mut a);
    a.tamper().high_s = true;
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Ok(1)
    );
}

#[test]
fn rejects_wrong_user_handle() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let cred = registered(&mut a);
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &[0; 16], &cred, &login(&mut a)),
        Err(WebauthnError::UserHandle)
    );
}

#[test]
fn rejects_oversized_id_and_wrong_outer_type() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let mut response = register(&mut a);
    response.id = b64url(&[1; 1024]);
    response.raw_id = response.id.clone();
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &response),
        Err(WebauthnError::IdTooLong)
    );
    response.r#type = "password".into();
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &response),
        Err(WebauthnError::BadType)
    );
}

#[test]
fn accepts_the_full_1023_byte_credential_id() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let id = vec![7; 1_023];
    a.tamper().attested_id = Some(id.clone());
    let mut response = register(&mut a);
    response.id = b64url(&id);
    response.raw_id = response.id.clone();
    assert_eq!(response.raw_id.len(), 1_364);
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &response).unwrap().id,
        response.id
    );
}

#[test]
fn rejects_unknown_credential() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let mut cred = registered(&mut a);
    cred.id = b64url(b"different ID");
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Err(WebauthnError::UnknownCredential)
    );
}

#[test]
fn rejects_missing_attested_data_and_inner_id_mismatch() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    a.tamper().flags = Some(0x01);
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &register(&mut a)),
        Err(WebauthnError::NoAttestedData)
    );
    a.tamper().flags = None;
    a.tamper().attested_id = Some(vec![1; 32]);
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &register(&mut a)),
        Err(WebauthnError::CredIdMismatch)
    );
}

#[test]
fn rejects_malformed_cbor_and_invalid_key() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let mut response = register(&mut a);
    response.response.attestation_object = b64url(&[0xff]);
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &response),
        Err(WebauthnError::Cbor)
    );
    a.tamper().cose_key = Some(cose(&[
        (1, 2.into()),
        (3, (-7).into()),
        (-1, 1.into()),
        (-2, ciborium::value::Value::Bytes(vec![0; 32])),
        (-3, ciborium::value::Value::Bytes(vec![0; 32])),
    ]));
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &register(&mut a)),
        Err(WebauthnError::BadKey)
    );
}

#[test]
fn rejects_collected_client_data_array() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let mut response = register(&mut a);
    let array = serde_json::json!(["webauthn.create", b64url(&CHALLENGE), RP.origin, false]);
    response.response.client_data_json = b64url(array.to_string().as_bytes());
    assert_eq!(
        verify_registration(&RP, &CHALLENGE, &response),
        Err(WebauthnError::ClientData)
    );
}

#[test]
fn rejects_malformed_authenticator_data() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let cred = registered(&mut a);
    let mut response = login(&mut a);
    response.response.authenticator_data = b64url(&[0]);
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &response),
        Err(WebauthnError::AuthenticatorData)
    );
    a.tamper().extensions = Some(ciborium::value::Value::Array(vec![]));
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Err(WebauthnError::Cbor)
    );
    a.tamper().flags = Some(0x41);
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Err(WebauthnError::UnexpectedAttestedData)
    );
}

#[test]
fn accepts_an_omitted_user_handle() {
    let mut a = SoftAuthenticator::new(Alg::Es256, 1);
    let cred = registered(&mut a);
    a.tamper().omit_user_handle = true;
    assert_eq!(
        verify_authentication(&RP, &CHALLENGE, &USER, &cred, &login(&mut a)),
        Ok(1)
    );
}
