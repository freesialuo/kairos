use ciborium::value::Value;
use coset::{AsCborValue, CoseError, CoseKey, KeyType, Label, iana};
use p256::{
    ecdsa::{self, signature::Verifier},
    elliptic_curve::bigint::BoxedUint,
};
use rsa::{Pkcs1v15Sign, PublicKey as RsaPublicKey, traits::PublicKeyParts};

use crate::auth::crypto::sha256;

use super::{EDDSA, ES256, RS256, WebauthnError as Error};

pub(super) enum Key {
    P256(ecdsa::VerifyingKey),
    Ed25519(ed25519_dalek::VerifyingKey),
    Rsa(RsaPublicKey),
}

impl Key {
    pub fn from_cbor(value: Value) -> Result<Self, Error> {
        let key = CoseKey::from_cbor_value(value).map_err(|err| match err {
            CoseError::UnregisteredIanaValue | CoseError::UnregisteredIanaNonPrivateValue => {
                Error::UnsupportedAlgorithm
            }
            _ => Error::BadKey,
        })?;
        // A WebAuthn credential public key has alg and the required public-key parameters.
        if !key.key_id.is_empty() || !key.key_ops.is_empty() || !key.base_iv.is_empty() {
            return Err(Error::BadKey);
        }
        use coset::RegisteredLabelWithPrivate::Assigned;
        match (&key.alg, &key.kty) {
            (Some(Assigned(iana::Algorithm::ES256)), KeyType::Assigned(iana::KeyType::EC2))
                if curve_is(&key, iana::EllipticCurve::P_256) =>
            {
                if key.params.len() != 3 {
                    return Err(Error::BadKey);
                }
                p256_key(&key).map(Self::P256)
            }
            (Some(Assigned(iana::Algorithm::EdDSA)), KeyType::Assigned(iana::KeyType::OKP))
                if curve_is(&key, iana::EllipticCurve::Ed25519) =>
            {
                if key.params.len() != 2 {
                    return Err(Error::BadKey);
                }
                ed25519_key(&key).map(Self::Ed25519)
            }
            (Some(Assigned(iana::Algorithm::RS256)), KeyType::Assigned(iana::KeyType::RSA)) => {
                if key.params.len() != 2 {
                    return Err(Error::BadKey);
                }
                rsa_key(&key).map(Self::Rsa)
            }
            _ => Err(Error::UnsupportedAlgorithm),
        }
    }

    pub fn alg(&self) -> i64 {
        match self {
            Self::P256(_) => ES256,
            Self::Ed25519(_) => EDDSA,
            Self::Rsa(_) => RS256,
        }
    }

    pub fn verify(&self, bytes: &[u8], signature: &[u8]) -> Result<(), Error> {
        match self {
            Self::P256(key) => {
                let signature =
                    ecdsa::Signature::from_der(signature).map_err(|_| Error::Signature)?;
                key.verify(bytes, &signature.normalize_s())
                    .map_err(|_| Error::Signature)
            }
            Self::Ed25519(key) => {
                let signature = ed25519_dalek::Signature::from_slice(signature)
                    .map_err(|_| Error::Signature)?;
                key.verify_strict(bytes, &signature)
                    .map_err(|_| Error::Signature)
            }
            Self::Rsa(key) => {
                if signature.len() != key.size() {
                    return Err(Error::Signature);
                }
                key.verify(
                    Pkcs1v15Sign::new::<rsa::sha2::Sha256>(),
                    &sha256(bytes),
                    signature,
                )
                .map_err(|_| Error::Signature)
            }
        }
    }
}

fn parameter(key: &CoseKey, label: i64) -> Result<&Value, Error> {
    key.params
        .iter()
        .find_map(|(name, value)| (*name == Label::Int(label)).then_some(value))
        .ok_or(Error::BadKey)
}

fn curve_is(key: &CoseKey, curve: iana::EllipticCurve) -> bool {
    parameter(key, -1)
        .ok()
        .and_then(Value::as_integer)
        .is_some_and(|value| value == (curve as i64).into())
}

fn key_bytes(key: &CoseKey, label: i64) -> Result<&[u8], Error> {
    parameter(key, label)?
        .as_bytes()
        .map(Vec::as_slice)
        .ok_or(Error::BadKey)
}

fn p256_key(key: &CoseKey) -> Result<ecdsa::VerifyingKey, Error> {
    let x = key_bytes(key, -2)?;
    if x.len() != 32 {
        return Err(Error::BadKey);
    }
    let mut point = Vec::with_capacity(65);
    match parameter(key, -3)? {
        Value::Bytes(y) if y.len() == 32 => {
            point.push(4);
            point.extend_from_slice(x);
            point.extend_from_slice(y);
        }
        Value::Bool(odd) => {
            point.push(2 + u8::from(*odd));
            point.extend_from_slice(x);
        }
        _ => return Err(Error::BadKey),
    }
    ecdsa::VerifyingKey::from_sec1_bytes(&point).map_err(|_| Error::BadKey)
}

fn ed25519_key(key: &CoseKey) -> Result<ed25519_dalek::VerifyingKey, Error> {
    let x = key_bytes(key, -2)?.try_into().map_err(|_| Error::BadKey)?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(x).map_err(|_| Error::BadKey)?;
    if key.is_weak() {
        return Err(Error::BadKey);
    }
    Ok(key)
}

fn rsa_key(key: &CoseKey) -> Result<RsaPublicKey, Error> {
    let modulus = key_bytes(key, -1)?;
    let exponent = key_bytes(key, -2)?;
    if modulus.is_empty()
        || modulus.len() > RsaPublicKey::MAX_SIZE / 8
        || exponent.len() > 8
        || exponent.is_empty()
    {
        return Err(Error::BadKey);
    }
    let modulus = BoxedUint::from_be_slice_vartime(modulus);
    let exponent = BoxedUint::from_be_slice_vartime(exponent);
    RsaPublicKey::new_with_size_limits(modulus, exponent, 2048..=RsaPublicKey::MAX_SIZE).map_err(
        |err| match err {
            rsa::Error::ModulusTooSmall => Error::WeakRsaKey,
            _ => Error::BadKey,
        },
    )
}
