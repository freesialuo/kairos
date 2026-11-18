pub struct Config {
    pub rp_id: String,
    pub origin: String,
    pub recovery_digest: [u8; 32],
    pub session_secret: Vec<u8>,
    pub allow_http_urls: bool,
}
