use kairos_types::auth::{
    AuthState, LoginVerifyRequest, RecoveryRequest, RegisterVerifyRequest, RegisterVerifyResponse,
};
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::{
    CoreError, Db, Hub, Ports,
    auth::{
        challenge::{Purpose, clear_challenge_cookie, issue_challenge, take_challenge},
        cookies::set_cookie,
        crypto::{b64url, b64url_decode, random_bytes},
        csrf::check_origin,
        recovery::check_recovery_code,
        session::{
            SESSION_COOKIE, SESSION_TTL_MS, Scope, create_session, insert_session, read_session,
            require_full_session, revoke_all_sessions, revoke_session, session_cookie_for_request,
            upgrade_session,
        },
    },
    db::settings::{read_meta, set_meta},
    ports::Value,
    webauthn::{
        RpContext, StoredCredential, WebauthnError, creation_options, request_options,
        verify_authentication, verify_registration,
    },
};

use super::{Method, Request, Response};

const USER_ID_META_KEY: &str = "webauthn_user_id";

pub(crate) async fn route<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Response, CoreError> {
    check_origin(req, &hub.config.origin)?;
    let mut response = match (req.method, req.path.as_str()) {
        (Method::Get, "/auth/state") => auth_state(hub, req)?,
        (Method::Post, "/auth/recovery") => recover(hub, req)?,
        (Method::Post, "/auth/passkey/register/options") => register_options(hub, req)?,
        (Method::Post, "/auth/passkey/register/verify") => clear_after(register_verify(hub, req)),
        (Method::Post, "/auth/passkey/login/options") => login_options(hub),
        (Method::Post, "/auth/passkey/login/verify") => clear_after(login_verify(hub, req)),
        (Method::Post, "/auth/logout") => {
            if let Some(session) = read_session(hub, req)? {
                revoke_session(&hub.db, &session)?;
            }
            logout_response()
        }
        (Method::Post, "/auth/logout-all") => {
            require_full_session(hub, req)?;
            revoke_all_sessions(&hub.db)?;
            logout_response()
        }
        _ => return Err(CoreError::NotFound),
    };
    if req.method == Method::Post {
        hub.rearm_after_http().await;
    }
    response
        .headers
        .push(("Cache-Control".into(), "no-store".into()));
    Ok(response)
}

fn parse_body<T: DeserializeOwned>(req: &Request) -> Result<T, CoreError> {
    serde_json::from_slice(&req.body).map_err(|_| CoreError::Invalid {
        field: "body",
        message: "invalid request JSON".into(),
    })
}

fn rp_context<P: Ports>(hub: &Hub<P>) -> RpContext<'_> {
    RpContext {
        rp_id: &hub.config.rp_id,
        origin: &hub.config.origin,
    }
}

fn auth_state<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Response, CoreError> {
    let passkeys = hub
        .db
        .exec(
            "SELECT COUNT(*) AS n FROM passkeys WHERE rp_id=?1",
            &[Value::Text(hub.config.rp_id.clone())],
        )?
        .first()
        .map_or(Ok(0), |row| row.i64("n"))?;
    let passkeys = u32::try_from(passkeys)
        .map_err(|_| CoreError::Internal("passkey count exceeds supported range".into()))?;
    let session = read_session(hub, req)?;
    let mut response = Response::json(
        200,
        &AuthState {
            setup_required: passkeys == 0,
            session: session.as_ref().map(|session| session.scope),
            passkeys,
        },
    );
    response.headers.extend(
        session
            .as_ref()
            .and_then(|session| session_cookie_for_request(req, session, hub.now())),
    );
    Ok(response)
}

fn recover<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Response, CoreError> {
    let request: RecoveryRequest = parse_body(req)?;
    check_recovery_code(hub, &request.code)?;
    let (_, token) = create_session(hub, Scope::Recovery)?;
    Ok(session_response(&token))
}

fn get_user_id(db: &impl Db) -> Result<Option<[u8; 16]>, CoreError> {
    read_meta(db, USER_ID_META_KEY)?
        .map(|stored| {
            b64url_decode(&stored)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| CoreError::Internal("invalid stored WebAuthn user ID".into()))
        })
        .transpose()
}

fn registration_user_id<P: Ports>(hub: &Hub<P>) -> Result<[u8; 16], CoreError> {
    hub.db.transaction(|db| {
        if let Some(id) = get_user_id(db)? {
            return Ok(id);
        }
        let id = random_bytes::<16>(&hub.entropy);
        set_meta(db, USER_ID_META_KEY, &b64url(&id))?;
        Ok(id)
    })
}

fn register_options<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Response, CoreError> {
    read_session(hub, req)?.ok_or(CoreError::Unauthorized)?;
    let user_id = registration_user_id(hub)?;
    let exclude = hub
        .db
        .exec(
            "SELECT id FROM passkeys WHERE rp_id=?1 ORDER BY id",
            &[Value::Text(hub.config.rp_id.clone())],
        )?
        .iter()
        .map(|row| row.text("id"))
        .collect::<Result<Vec<_>, _>>()?;
    let (challenge, cookie) = issue_challenge(hub, Purpose::Reg);
    let mut response = Response::json(
        200,
        &creation_options(&rp_context(hub), &user_id, &challenge, &exclude),
    );
    response.headers.push(cookie);
    Ok(response)
}

fn register_verify<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Response, CoreError> {
    let session = read_session(hub, req)?.ok_or(CoreError::Unauthorized)?;
    let (challenge, _) = take_challenge(hub, req, Purpose::Reg)?;
    let body: RegisterVerifyRequest = parse_body(req)?;
    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    if name.is_some_and(|name| name.chars().count() > 60) {
        return Err(CoreError::Invalid {
            field: "name",
            message: "passkey name exceeds 60 characters".into(),
        });
    }
    let credential = match verify_registration(&rp_context(hub), &challenge, &body.response) {
        Ok(credential) => credential,
        Err(error) => return Ok(verification_error(error.code())),
    };
    let transports = serde_json::to_string(&credential.transports)
        .map_err(|_| CoreError::Internal("could not encode passkey transports".into()))?;
    let now = hub.now();
    hub.db.transaction(|db| {
        let name = if let Some(name) = name {
            name.to_owned()
        } else {
            let count = db
                .exec("SELECT COUNT(*) AS n FROM passkeys", &[])?
                .first()
                .map_or(Ok(0), |row| row.i64("n"))?;
            format!("Passkey {}", count.saturating_add(1))
        };
        let inserted = db.exec(
            "INSERT INTO passkeys(id, public_key, alg, counter, transports, rp_id, name, created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(id) DO NOTHING RETURNING id",
            &[
                Value::Text(credential.id.clone()), Value::Blob(credential.public_key),
                Value::Integer(credential.alg), Value::Integer(i64::from(credential.counter)),
                Value::Text(transports), Value::Text(hub.config.rp_id.clone()),
                Value::Text(name), Value::Integer(now),
            ],
        )?;
        if inserted.is_empty() {
            return Ok(Response::json(409, &json!({"error": "credential_exists"})));
        }
        upgrade_session(db, &session)?;
        Ok(Response::json(
            200,
            &RegisterVerifyResponse { id: credential.id },
        ))
    })
}

fn login_options<P: Ports>(hub: &Hub<P>) -> Response {
    let (challenge, cookie) = issue_challenge(hub, Purpose::Auth);
    let mut response = Response::json(200, &request_options(&rp_context(hub), &challenge));
    response.headers.push(cookie);
    response
}

fn login_verify<P: Ports>(hub: &Hub<P>, req: &Request) -> Result<Response, CoreError> {
    let (challenge, _) = take_challenge(hub, req, Purpose::Auth)?;
    let body: LoginVerifyRequest = parse_body(req)?;
    let rows = hub.db.exec(
        "SELECT id, public_key, alg, counter FROM passkeys WHERE id=?1 AND rp_id=?2",
        &[
            Value::Text(body.response.id.clone()),
            Value::Text(hub.config.rp_id.clone()),
        ],
    )?;
    let Some(row) = rows.first() else {
        return Ok(verification_error("unknown_credential"));
    };
    let credential = StoredCredential {
        id: row.text("id")?,
        public_key: row.blob("public_key")?,
        alg: row.i64("alg")?,
        counter: u32::try_from(row.i64("counter")?)
            .map_err(|_| CoreError::Internal("invalid stored passkey counter".into()))?,
    };
    let user_id = get_user_id(&hub.db)?
        .ok_or_else(|| CoreError::Internal("missing WebAuthn user ID".into()))?;
    let counter = match verify_authentication(
        &rp_context(hub),
        &challenge,
        &user_id,
        &credential,
        &body.response,
    ) {
        Ok(counter) => counter,
        Err(error) => {
            if error == WebauthnError::CounterRegression {
                let _ = set_meta(&hub.db, "last_auth_error", &error.to_string());
            }
            return Ok(verification_error(error.code()));
        }
    };
    let now = hub.now();
    hub.db.transaction(|db| {
        let updated = db.exec(
            "UPDATE passkeys SET counter=?1,last_used_at=?2 WHERE id=?3 AND rp_id=?4 AND counter=?5 RETURNING id",
            &[
                Value::Integer(i64::from(counter)), Value::Integer(now), Value::Text(credential.id),
                Value::Text(hub.config.rp_id.clone()), Value::Integer(i64::from(credential.counter)),
            ],
        )?;
        if updated.is_empty() {
            return Err(CoreError::Unauthorized);
        }
        let (_, token) = insert_session(db, &hub.entropy, now, Scope::Full)?;
        Ok(session_response(&token))
    })
}

fn verification_error(code: &str) -> Response {
    Response::json(400, &json!({"error": code}))
}

fn clear_after(result: Result<Response, CoreError>) -> Response {
    let mut response = result.unwrap_or_else(|error| Response::from_error(&error));
    response.headers.push(clear_challenge_cookie());
    response
}

fn session_response(token: &str) -> Response {
    let mut response = Response::json(200, &json!({"ok": true}));
    response.headers.push(set_cookie(
        SESSION_COOKIE,
        token,
        Some(SESSION_TTL_MS / 1000),
    ));
    response
}

fn logout_response() -> Response {
    let mut response = Response::empty(204);
    response
        .headers
        .push(set_cookie(SESSION_COOKIE, "", Some(0)));
    response
}
