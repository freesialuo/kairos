//! Deterministic WebAuthn authenticator for tests. Never use these keys outside tests.

use chacha20::{
    ChaCha20Rng,
    rand_core::{Rng, SeedableRng},
};
use ciborium::value::Value;
use kairos_core::auth::crypto::{b64url, sha256};
use kairos_types::auth::{
    AssertionResponseJson, AttestationResponseJson, AuthenticationResponseJson,
    CreationOptionsJson, RegistrationResponseJson, RequestOptionsJson,
};
use p256::{
    ecdsa::{self, signature::Signer},
    elliptic_curve::Generate,
};
use rsa::traits::PublicKeyParts;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alg {
    Es256,
    EdDsa,
    Rs256,
}

#[derive(Debug, Default)]
pub struct Tamper {
    pub flags: Option<u8>,
    pub rp_id_hash: Option<[u8; 32]>,
    pub origin: Option<String>,
    pub client_data_type: Option<String>,
    pub cross_origin: Option<bool>,
    pub trailing_bytes: Vec<u8>,
    pub extensions: Option<Value>,
    pub counter: Option<u32>,
    pub bad_signature: bool,
    pub high_s: bool,
    pub cose_key: Option<Value>,
    pub attested_id: Option<Vec<u8>>,
    pub omit_user_handle: bool,
}

enum PrivateKey {
    Ec(ecdsa::SigningKey),
    Ed(ed25519_dalek::SigningKey),
    Rsa(Box<rsa::PrivateKey>),
}

pub struct SoftAuthenticator {
    key: PrivateKey,
    credential_id: [u8; 32],
    user_handle: Option<String>,
    counter: u32,
    tamper: Tamper,
}

impl SoftAuthenticator {
    pub fn new(alg: Alg, seed: u64) -> Self {
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let mut id = [0; 32];
        rng.fill_bytes(&mut id);
        let key = match alg {
            Alg::Es256 => PrivateKey::Ec(ecdsa::SigningKey::generate_from_rng(&mut rng)),
            Alg::EdDsa => {
                let mut secret = [0; 32];
                rng.fill_bytes(&mut secret);
                PrivateKey::Ed(ed25519_dalek::SigningKey::from_bytes(&secret))
            }
            Alg::Rs256 => PrivateKey::Rsa(Box::new(
                rsa::PrivateKey::new(&mut rng, 2048).expect("test RSA key"),
            )),
        };
        Self {
            key,
            credential_id: id,
            user_handle: None,
            counter: 0,
            tamper: Tamper::default(),
        }
    }

    pub fn register(
        &mut self,
        opts: &CreationOptionsJson,
        origin: &str,
    ) -> RegistrationResponseJson {
        self.counter = 0;
        self.user_handle = Some(opts.user.id.clone());
        let mut data = self.auth_data_header(&opts.rp.id, true);
        let id = self
            .tamper
            .attested_id
            .as_deref()
            .unwrap_or(&self.credential_id);
        data.extend_from_slice(&[0; 16]);
        data.extend_from_slice(
            &u16::try_from(id.len())
                .expect("test attested ID length")
                .to_be_bytes(),
        );
        data.extend_from_slice(id);
        let cose = self
            .tamper
            .cose_key
            .clone()
            .unwrap_or_else(|| self.public_key());
        data.extend_from_slice(&cbor(cose));
        self.auth_data_suffix(&mut data);

        let attestation = Value::Map(vec![
            (Value::Text("fmt".into()), Value::Text("none".into())),
            (Value::Text("attStmt".into()), Value::Map(Vec::new())),
            (Value::Text("authData".into()), Value::Bytes(data)),
        ]);
        let id = b64url(&self.credential_id);
        RegistrationResponseJson {
            id: id.clone(),
            raw_id: id,
            r#type: "public-key".into(),
            response: AttestationResponseJson {
                client_data_json: b64url(&self.client_data(
                    "webauthn.create",
                    &opts.challenge,
                    origin,
                )),
                attestation_object: b64url(&cbor(attestation)),
                transports: Some(vec!["internal".into()]),
            },
        }
    }

    pub fn login(&mut self, opts: &RequestOptionsJson, origin: &str) -> AuthenticationResponseJson {
        self.counter = self.counter.saturating_add(1);
        let client = self.client_data("webauthn.get", &opts.challenge, origin);
        let mut auth = self.auth_data_header(&opts.rp_id, false);
        self.auth_data_suffix(&mut auth);
        let mut signed = auth.clone();
        signed.extend_from_slice(&sha256(&client));
        let mut signature = self.sign(&signed);
        if self.tamper.bad_signature
            && let Some(last) = signature.last_mut()
        {
            *last ^= 1;
        }
        let id = b64url(&self.credential_id);
        AuthenticationResponseJson {
            id: id.clone(),
            raw_id: id,
            r#type: "public-key".into(),
            response: AssertionResponseJson {
                client_data_json: b64url(&client),
                authenticator_data: b64url(&auth),
                signature: b64url(&signature),
                user_handle: self
                    .user_handle
                    .clone()
                    .filter(|_| !self.tamper.omit_user_handle),
            },
        }
    }

    pub fn tamper(&mut self) -> &mut Tamper {
        &mut self.tamper
    }

    fn auth_data_header(&self, rp: &str, registration: bool) -> Vec<u8> {
        let mut flags = if registration { 0x45 } else { 0x05 };
        if self.tamper.extensions.is_some() {
            flags |= 0x80;
        }
        let mut data = self
            .tamper
            .rp_id_hash
            .unwrap_or_else(|| sha256(rp.as_bytes()))
            .to_vec();
        data.push(self.tamper.flags.unwrap_or(flags));
        data.extend_from_slice(&self.tamper.counter.unwrap_or(self.counter).to_be_bytes());
        data
    }

    fn auth_data_suffix(&self, data: &mut Vec<u8>) {
        if let Some(extensions) = &self.tamper.extensions {
            data.extend_from_slice(&cbor(extensions.clone()));
        }
        data.extend_from_slice(&self.tamper.trailing_bytes);
    }

    fn client_data(&self, kind: &str, challenge: &str, origin: &str) -> Vec<u8> {
        let mut json = serde_json::json!({
            "type": self.tamper.client_data_type.as_deref().unwrap_or(kind),
            "challenge": challenge,
            "origin": self.tamper.origin.as_deref().unwrap_or(origin),
        });
        if let Some(cross_origin) = self.tamper.cross_origin {
            json["crossOrigin"] = cross_origin.into();
        }
        json.to_string().into_bytes()
    }

    fn public_key(&self) -> Value {
        let pairs = match &self.key {
            PrivateKey::Ec(key) => {
                let point = key.verifying_key().to_sec1_point(false);
                vec![
                    (1, 2.into()),
                    (3, (-7).into()),
                    (-1, 1.into()),
                    (
                        -2,
                        Value::Bytes(point.x().expect("P-256 x coordinate").to_vec()),
                    ),
                    (
                        -3,
                        Value::Bytes(point.y().expect("P-256 y coordinate").to_vec()),
                    ),
                ]
            }
            PrivateKey::Ed(key) => vec![
                (1, 1.into()),
                (3, (-8).into()),
                (-1, 6.into()),
                (-2, Value::Bytes(key.verifying_key().to_bytes().to_vec())),
            ],
            PrivateKey::Rsa(key) => vec![
                (1, 3.into()),
                (3, (-257).into()),
                (
                    -1,
                    Value::Bytes(key.n().as_ref().to_be_bytes_trimmed_vartime().into_vec()),
                ),
                (
                    -2,
                    Value::Bytes(key.e().to_be_bytes_trimmed_vartime().into_vec()),
                ),
            ],
        };
        Value::Map(
            pairs
                .into_iter()
                .map(|(label, value): (i64, _)| (label.into(), value))
                .collect(),
        )
    }

    fn sign(&self, data: &[u8]) -> Vec<u8> {
        match &self.key {
            PrivateKey::Ec(key) => {
                let signature: ecdsa::Signature = key.sign(data);
                let signature = if self.tamper.high_s {
                    let (r, s) = signature.normalize_s().split_scalars();
                    let r: p256::FieldBytes = r.into();
                    let s: p256::FieldBytes = (-s).into();
                    ecdsa::Signature::from_scalars(r, s).expect("test high-S signature")
                } else {
                    signature
                };
                signature.to_der().as_bytes().to_vec()
            }
            PrivateKey::Ed(key) => key.sign(data).to_bytes().to_vec(),
            PrivateKey::Rsa(key) => key
                .sign(rsa::Pkcs1v15Sign::new::<rsa::sha2::Sha256>(), &sha256(data))
                .expect("test RS256 signature"),
        }
    }
}

fn cbor(value: Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    ciborium::into_writer(&value, &mut bytes).expect("test authenticator CBOR");
    bytes
}
