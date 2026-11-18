//! WebAuthn verification for a single relying party with no attestation trust.

mod authdata;
mod cose;
mod options;
mod verify;

pub use options::{creation_options, request_options};
pub use verify::{verify_authentication, verify_registration};

const ES256: i64 = -7;
const EDDSA: i64 = -8;
const RS256: i64 = -257;
const MAX_CREDENTIAL_ID_BYTES: usize = 1_023;

pub struct RpContext<'a> {
    pub rp_id: &'a str,
    pub origin: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCredential {
    pub id: String,
    pub public_key: Vec<u8>,
    pub alg: i64,
    pub counter: u32,
    pub transports: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCredential {
    pub id: String,
    pub public_key: Vec<u8>,
    pub alg: i64,
    pub counter: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebauthnError {
    BadType,
    IdMismatch,
    IdTooLong,
    ClientData,
    ClientDataType,
    Challenge,
    Origin,
    CrossOrigin,
    Cbor,
    AuthenticatorData,
    RpIdHash,
    UserPresence,
    NoAttestedData,
    UnexpectedAttestedData,
    TrailingBytes,
    CredIdMismatch,
    UnsupportedAlgorithm,
    WeakRsaKey,
    BadKey,
    UserHandle,
    Signature,
    CounterRegression,
    UnknownCredential,
}

impl WebauthnError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadType => "bad_type",
            Self::IdMismatch => "id_mismatch",
            Self::IdTooLong => "id_too_long",
            Self::ClientData => "client_data",
            Self::ClientDataType => "client_data_type",
            Self::Challenge => "challenge",
            Self::Origin => "origin",
            Self::CrossOrigin => "cross_origin",
            Self::Cbor => "cbor",
            Self::AuthenticatorData => "authenticator_data",
            Self::RpIdHash => "rp_id_hash",
            Self::UserPresence => "user_presence",
            Self::NoAttestedData => "no_attested_data",
            Self::UnexpectedAttestedData => "unexpected_attested_data",
            Self::TrailingBytes => "trailing_bytes",
            Self::CredIdMismatch => "cred_id_mismatch",
            Self::UnsupportedAlgorithm => "unsupported_algorithm",
            Self::WeakRsaKey => "weak_rsa_key",
            Self::BadKey => "bad_key",
            Self::UserHandle => "user_handle",
            Self::Signature => "signature",
            Self::CounterRegression => "counter_regression",
            Self::UnknownCredential => "unknown_credential",
        }
    }
}

impl std::fmt::Display for WebauthnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CounterRegression => f.write_str("possible cloned authenticator"),
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for WebauthnError {}

#[cfg(test)]
mod chrome_fixture {
    use crate as kairos_core;

    include!("../../../kairos-testkit/fixtures/chrome_case.rs");
}
