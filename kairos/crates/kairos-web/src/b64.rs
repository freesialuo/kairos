use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
pub fn decode(s: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| "Invalid passkey data.".into())
}
pub fn encode(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn base64url_roundtrip() {
        let encoded = encode(&[255, 0]);
        assert_eq!(encoded, "_wA");
        assert_eq!(decode(&encoded).ok().as_deref(), Some([255, 0].as_slice()));
    }
}
