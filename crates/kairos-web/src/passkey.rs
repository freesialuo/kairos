use crate::{
    api::{ApiError, call},
    b64,
};
use js_sys::{Array, ArrayBuffer, Reflect, Uint8Array};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AuthenticatorAssertionResponse, AuthenticatorAttestationResponse, CredentialCreationOptions,
    CredentialRequestOptions, PublicKeyCredential,
};

fn js_error(e: JsValue) -> ApiError {
    let msg = Reflect::get(&e, &"message".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| "The passkey request could not be completed.".into());
    ApiError::Other(msg)
}
fn bytes(value: &ArrayBuffer) -> String {
    b64::encode(&Uint8Array::new(value).to_vec())
}
fn decode_field(obj: &JsValue, field: &str) -> Result<(), ApiError> {
    let value = Reflect::get(obj, &field.into())
        .map_err(js_error)?
        .as_string()
        .ok_or_else(|| ApiError::Other("Missing credential field.".into()))?;
    let bytes = b64::decode(&value).map_err(ApiError::Other)?;
    Reflect::set(obj, &field.into(), &Uint8Array::from(bytes.as_slice())).map_err(js_error)?;
    Ok(())
}

pub async fn ceremony(register: bool, name: Option<String>) -> Result<(), ApiError> {
    let flow = if register { "register" } else { "login" };
    let options = call(
        "POST",
        &format!("/auth/passkey/{flow}/options"),
        Some(json!({})),
    )
    .await?;
    let opts = js_sys::JSON::parse(&options.to_string()).map_err(js_error)?;
    decode_field(&opts, "challenge")?;
    let win = web_sys::window().ok_or_else(|| ApiError::Other("Browser unavailable.".into()))?;
    let credentials = win.navigator().credentials();
    let promise = if register {
        let user = Reflect::get(&opts, &"user".into()).map_err(js_error)?;
        decode_field(&user, "id")?;
        let excluded = Reflect::get(&opts, &"excludeCredentials".into()).map_err(js_error)?;
        if Array::is_array(&excluded) {
            for entry in Array::from(&excluded) {
                decode_field(&entry, "id")?;
            }
        }
        let creation = CredentialCreationOptions::new();
        creation.set_public_key(opts.unchecked_ref());
        credentials.create_with_options(&creation)
    } else {
        let req = CredentialRequestOptions::new();
        req.set_public_key(opts.unchecked_ref());
        credentials.get_with_options(&req)
    }
    .map_err(js_error)?;
    let key = JsFuture::from(promise)
        .await
        .map_err(js_error)?
        .dyn_into::<PublicKeyCredential>()
        .map_err(js_error)?;
    let response = if register {
        let r = key
            .response()
            .dyn_into::<AuthenticatorAttestationResponse>()
            .map_err(|_| ApiError::Other("Invalid registration response.".into()))?;
        json!({"clientDataJSON":bytes(&r.client_data_json()),"attestationObject":bytes(&r.attestation_object()),"transports":r.get_transports().iter().filter_map(|v|v.as_string()).collect::<Vec<_>>()})
    } else {
        let r = key
            .response()
            .dyn_into::<AuthenticatorAssertionResponse>()
            .map_err(|_| ApiError::Other("Invalid login response.".into()))?;
        json!({"clientDataJSON":bytes(&r.client_data_json()),"authenticatorData":bytes(&r.authenticator_data()),"signature":bytes(&r.signature()),"userHandle":r.user_handle().as_ref().map(bytes)})
    };
    let mut payload = json!({"response":{"id":key.id(),"rawId":bytes(&key.raw_id()),"type":"public-key","response":response}});
    if register && let Some(fields) = payload.as_object_mut() {
        fields.insert("name".into(), name.map_or(Value::Null, Value::String));
    }
    call(
        "POST",
        &format!("/auth/passkey/{flow}/verify"),
        Some(payload),
    )
    .await?;
    Ok(())
}

pub fn browser_zone() -> String {
    let intl = js_sys::Intl::DateTimeFormat::new(&Array::new(), &js_sys::Object::new());
    Reflect::get(&intl.resolved_options(), &"timeZone".into())
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_else(|| "UTC".into())
}
