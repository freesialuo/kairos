use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{CoreError, ports::Entropy};

pub fn random_bytes<const N: usize>(e: &impl Entropy) -> [u8; N] {
    let mut bytes = [0; N];
    e.fill(&mut bytes);
    bytes
}

pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn b64url_decode(s: &str) -> Result<Vec<u8>, CoreError> {
    URL_SAFE_NO_PAD.decode(s).map_err(|_| CoreError::Invalid {
        field: "base64url",
        message: "invalid base64url".into(),
    })
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    sha256(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    bool::from(a.ct_eq(b))
}

pub fn sign(secret: &[u8], payload: &[u8]) -> String {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else {
        return String::new();
    };
    mac.update(payload);
    format!(
        "{}.{}",
        b64url(payload),
        b64url(&mac.finalize().into_bytes())
    )
}

pub fn verify(secret: &[u8], token: &str) -> Option<Vec<u8>> {
    let (payload, tag) = token.split_once('.')?;
    let payload = b64url_decode(payload).ok()?;
    let tag = b64url_decode(tag).ok()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).ok()?;
    mac.update(&payload);
    mac.verify_slice(&tag).ok()?;
    Some(payload)
}

#[cfg(test)]
mod tests {
    #[cfg(target_arch = "wasm32")]
    use wasm_bindgen_test::wasm_bindgen_test as test;

    use super::*;

    #[test]
    fn sign_verify_round_trip_and_tamper() {
        let tag = sign(b"secret", b"hello");
        assert_eq!(verify(b"secret", &tag), Some(b"hello".to_vec()));
        let altered = tag.replace("aGVsbG8.", "aGVsbGA.");
        assert_ne!(tag, altered);
        assert_eq!(verify(b"secret", &altered), None);
        assert_eq!(verify(b"other", &tag), None);
    }

    #[test]
    fn ct_eq_lengths() {
        assert!(ct_eq(b"same", b"same"));
        assert!(!ct_eq(b"same", b"sam"));
        assert!(!ct_eq(b"same", b"samE"));
    }
}
