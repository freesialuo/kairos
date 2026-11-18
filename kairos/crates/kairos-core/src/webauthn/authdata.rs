use ciborium::{de::from_reader_with_recursion_limit, value::Value};

use crate::auth::crypto::{ct_eq, sha256};

use super::{MAX_CREDENTIAL_ID_BYTES, WebauthnError as Error};

const UP: u8 = 0x01;
const AT: u8 = 0x40;
const ED: u8 = 0x80;
const CBOR_DEPTH: usize = 32;

pub(super) struct Attested<'a> {
    pub id: &'a [u8],
    pub cose_bytes: &'a [u8],
    pub cose_value: Value,
}

pub(super) struct AuthenticatorData<'a> {
    pub counter: u32,
    pub attested: Option<Attested<'a>>,
}

pub(super) fn read_cbor(bytes: &mut &[u8]) -> Result<Value, Error> {
    from_reader_with_recursion_limit(bytes, CBOR_DEPTH).map_err(|_| Error::Cbor)
}

pub(super) fn parse_auth_data<'a>(
    data: &'a [u8],
    rp_id: &str,
    registration: bool,
) -> Result<AuthenticatorData<'a>, Error> {
    let (rp_hash, header) = data.split_at_checked(32).ok_or(Error::AuthenticatorData)?;
    let ([flags, a, b, c, d], mut rest) =
        header.split_at_checked(5).ok_or(Error::AuthenticatorData)?
    else {
        return Err(Error::AuthenticatorData);
    };
    if !ct_eq(rp_hash, &sha256(rp_id.as_bytes())) {
        return Err(Error::RpIdHash);
    }
    if flags & UP == 0 {
        return Err(Error::UserPresence);
    }

    let attested = match (registration, flags & AT != 0) {
        (true, true) => Some(parse_attested(&mut rest)?),
        (true, false) => return Err(Error::NoAttestedData),
        (false, true) => return Err(Error::UnexpectedAttestedData),
        (false, false) => None,
    };
    if flags & ED != 0 && !matches!(read_cbor(&mut rest)?, Value::Map(_)) {
        return Err(Error::Cbor);
    }
    if !rest.is_empty() {
        return Err(Error::TrailingBytes);
    }
    Ok(AuthenticatorData {
        counter: u32::from_be_bytes([*a, *b, *c, *d]),
        attested,
    })
}

fn parse_attested<'a>(rest: &mut &'a [u8]) -> Result<Attested<'a>, Error> {
    // The first 16 bytes are the AAGUID, which carries no trust in this verifier.
    let [high, low, data @ ..] = rest.get(16..).ok_or(Error::NoAttestedData)? else {
        return Err(Error::NoAttestedData);
    };
    let len = usize::from(u16::from_be_bytes([*high, *low]));
    if len > MAX_CREDENTIAL_ID_BYTES {
        return Err(Error::IdTooLong);
    }
    let (id, key_and_extensions) = data.split_at_checked(len).ok_or(Error::NoAttestedData)?;
    *rest = key_and_extensions;
    let cose_value = read_cbor(rest)?;
    let consumed = key_and_extensions.len() - rest.len();
    let cose_bytes = key_and_extensions.get(..consumed).ok_or(Error::Cbor)?;
    Ok(Attested {
        id,
        cose_bytes,
        cose_value,
    })
}
