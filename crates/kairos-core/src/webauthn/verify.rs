use ciborium::value::Value;
use kairos_types::auth::{AuthenticationResponseJson, RegistrationResponseJson};
use serde::Deserialize;

use crate::auth::crypto::{b64url, b64url_decode, ct_eq, sha256};

use super::{
    MAX_CREDENTIAL_ID_BYTES, NewCredential, RpContext, StoredCredential, WebauthnError as Error,
    authdata::{parse_auth_data, read_cbor},
    cose::Key,
};

// Bound the work of decoding browser-controlled ceremony fields.
const MAX_FIELD_LENGTH: usize = 128 * 1024;

pub fn verify_registration(
    rp: &RpContext<'_>,
    challenge: &[u8; 32],
    resp: &RegistrationResponseJson,
) -> Result<NewCredential, Error> {
    let id = verify_id(&resp.r#type, &resp.id, &resp.raw_id)?;
    verify_client_data(
        rp,
        challenge,
        &resp.response.client_data_json,
        "webauthn.create",
    )?;
    let object = decode(&resp.response.attestation_object, Error::Cbor)?;
    let auth_bytes = attestation_auth_data(&object)?;
    let auth_data = parse_auth_data(&auth_bytes, rp.rp_id, true)?;
    let attested = auth_data.attested.ok_or(Error::NoAttestedData)?;
    if !ct_eq(attested.id, &id) {
        return Err(Error::CredIdMismatch);
    }
    let key = Key::from_cbor(attested.cose_value)?;
    Ok(NewCredential {
        id: resp.id.clone(),
        public_key: attested.cose_bytes.to_vec(),
        alg: key.alg(),
        counter: auth_data.counter,
        transports: resp.response.transports.clone().unwrap_or_default(),
    })
}

pub fn verify_authentication(
    rp: &RpContext<'_>,
    challenge: &[u8; 32],
    user_id: &[u8; 16],
    cred: &StoredCredential,
    resp: &AuthenticationResponseJson,
) -> Result<u32, Error> {
    verify_id(&resp.r#type, &resp.id, &resp.raw_id)?;
    if resp.id != cred.id {
        return Err(Error::UnknownCredential);
    }
    let client = verify_client_data(
        rp,
        challenge,
        &resp.response.client_data_json,
        "webauthn.get",
    )?;
    let mut auth_bytes = decode(&resp.response.authenticator_data, Error::AuthenticatorData)?;
    let counter = parse_auth_data(&auth_bytes, rp.rp_id, false)?.counter;
    if let Some(handle) = &resp.response.user_handle {
        let value = decode(handle, Error::UserHandle)?;
        if !ct_eq(&value, user_id) {
            return Err(Error::UserHandle);
        }
    }
    let mut cose = cred.public_key.as_slice();
    let value = read_cbor(&mut cose).map_err(|_| Error::BadKey)?;
    if !cose.is_empty() {
        return Err(Error::BadKey);
    }
    let key = Key::from_cbor(value)?;
    if key.alg() != cred.alg {
        return Err(Error::BadKey);
    }
    let signature = decode(&resp.response.signature, Error::Signature)?;
    auth_bytes.extend_from_slice(&sha256(&client));
    key.verify(&auth_bytes, &signature)?;
    if counter <= cred.counter && (counter != 0 || cred.counter != 0) {
        return Err(Error::CounterRegression);
    }
    Ok(counter)
}

fn verify_id(kind: &str, id: &str, raw: &str) -> Result<Vec<u8>, Error> {
    if kind != "public-key" {
        return Err(Error::BadType);
    }
    if raw.len() > MAX_CREDENTIAL_ID_BYTES * 4 / 3 {
        return Err(Error::IdTooLong);
    }
    let raw = b64url_decode(raw).map_err(|_| Error::IdMismatch)?;
    if raw.is_empty() || id != b64url(&raw) {
        return Err(Error::IdMismatch);
    }
    Ok(raw)
}

#[derive(Deserialize)]
struct ClientData {
    #[serde(rename = "type")]
    kind: String,
    challenge: String,
    origin: String,
    #[serde(rename = "crossOrigin", default)]
    cross_origin: bool,
}

fn verify_client_data(
    rp: &RpContext<'_>,
    challenge: &[u8; 32],
    encoded: &str,
    kind: &str,
) -> Result<Vec<u8>, Error> {
    let bytes = decode(encoded, Error::ClientData)?;
    let json = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    if json.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err(Error::ClientData);
    }
    let data: ClientData = serde_json::from_slice(json).map_err(|_| Error::ClientData)?;
    if data.kind != kind {
        return Err(Error::ClientDataType);
    }
    let given = b64url_decode(&data.challenge).map_err(|_| Error::Challenge)?;
    if !ct_eq(&given, challenge) {
        return Err(Error::Challenge);
    }
    if data.origin != rp.origin {
        return Err(Error::Origin);
    }
    if data.cross_origin {
        return Err(Error::CrossOrigin);
    }
    Ok(bytes)
}

fn attestation_auth_data(mut bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let Value::Map(fields) = read_cbor(&mut bytes)? else {
        return Err(Error::Cbor);
    };
    if !bytes.is_empty() {
        return Err(Error::TrailingBytes);
    }
    let mut fmt = false;
    let mut statement = false;
    let mut data = None;
    for (key, value) in fields {
        match key.as_text() {
            Some("fmt") if !fmt && matches!(value, Value::Text(_)) => fmt = true,
            Some("attStmt") if !statement && matches!(value, Value::Map(_)) => statement = true,
            Some("authData") if data.is_none() => {
                data = Some(value.into_bytes().map_err(|_| Error::Cbor)?);
            }
            Some("fmt" | "attStmt" | "authData") => return Err(Error::Cbor),
            _ => {}
        }
    }
    if !fmt || !statement {
        return Err(Error::Cbor);
    }
    data.ok_or(Error::Cbor)
}

fn decode(value: &str, error: Error) -> Result<Vec<u8>, Error> {
    if value.len() > MAX_FIELD_LENGTH {
        return Err(error);
    }
    b64url_decode(value).map_err(|_| error)
}
