use kairos_core::{
    CoreError,
    auth::{
        challenge::{Purpose, issue_challenge, take_challenge},
        csrf::check_origin,
        recovery::check_recovery_code,
        session::{SESSION_TTL_MS, Scope, create_session, read_session},
    },
    http::{Method, Request},
};
use kairos_testkit::TestHub;

fn request(method: Method, cookie: Option<&str>) -> Request {
    Request {
        method,
        path: "/auth/test".into(),
        query: vec![],
        headers: cookie
            .into_iter()
            .map(|value| ("Cookie".into(), value.into()))
            .collect(),
        body: vec![],
    }
}

#[test]
fn session_slides_once_per_day() {
    let hub = TestHub::new();
    let (session, token) = create_session(&hub, Scope::Full).unwrap();
    let req = request(Method::Get, Some(&format!("__Host-sid={token}")));
    assert_eq!(read_session(&hub, &req).unwrap(), Some(session.clone()));
    hub.advance_min(1_439);
    assert_eq!(read_session(&hub, &req).unwrap(), Some(session));
    hub.advance_min(1);
    let extended = read_session(&hub, &req).unwrap().unwrap();
    assert_eq!(extended.expires_at, hub.now() + SESSION_TTL_MS);
    hub.advance_min(1);
    assert_eq!(read_session(&hub, &req).unwrap(), Some(extended));
    let rows = hub.sql("SELECT id_hash FROM sessions");
    let hash = rows.first().unwrap().text("id_hash").unwrap();
    assert_eq!(hash.len(), 64);
    assert_ne!(hash, token);
}

#[test]
fn expired_session_is_none() {
    let hub = TestHub::new();
    let (_, token) = create_session(&hub, Scope::Recovery).unwrap();
    let req = request(Method::Get, Some(&format!("__Host-sid={token}")));
    hub.advance_min(30 * 24 * 60);
    assert_eq!(read_session(&hub, &req).unwrap(), None);
}

#[test]
fn origin_required_for_post_not_get() {
    let origin = "https://kairos.test";
    assert_eq!(check_origin(&request(Method::Get, None), origin), Ok(()));
    let mut post = request(Method::Post, None);
    assert_eq!(check_origin(&post, origin), Err(CoreError::Forbidden));
    post.headers.push(("oRiGiN".into(), origin.into()));
    assert_eq!(check_origin(&post, origin), Ok(()));
    post.headers.push(("Origin".into(), origin.into()));
    assert_eq!(check_origin(&post, origin), Err(CoreError::Forbidden));
}

#[test]
fn recovery_locks_after_five_failures_for_one_hour() {
    let hub = TestHub::new();
    for _ in 0..5 {
        assert_eq!(
            check_recovery_code(&hub, "wrong"),
            Err(CoreError::Unauthorized)
        );
    }
    assert_eq!(hub.sql("SELECT at FROM auth_failures").len(), 5);
    assert_eq!(
        check_recovery_code(&hub, "correct horse"),
        Err(CoreError::TooManyRequests)
    );
    hub.advance_min(61);
    assert_eq!(check_recovery_code(&hub, "correct horse"), Ok(()));
}

fn challenge_request(hub: &TestHub, purpose: Purpose) -> ([u8; 32], Request) {
    let (challenge, (_, set_cookie)) = issue_challenge(hub, purpose);
    let cookie = set_cookie.split(';').next().unwrap();
    (challenge, request(Method::Post, Some(cookie)))
}

#[test]
fn challenge_wrong_purpose_rejected() {
    let hub = TestHub::new();
    let (_, req) = challenge_request(&hub, Purpose::Reg);
    assert_eq!(
        take_challenge(&hub, &req, Purpose::Auth),
        Err(CoreError::Unauthorized)
    );
}

#[test]
fn challenge_expires_after_two_minutes() {
    let hub = TestHub::new();
    let (_, req) = challenge_request(&hub, Purpose::Reg);
    hub.advance_min(2);
    assert_eq!(
        take_challenge(&hub, &req, Purpose::Reg),
        Err(CoreError::Unauthorized)
    );
}

#[test]
fn challenge_is_single_use() {
    let hub = TestHub::new();
    let (challenge, req) = challenge_request(&hub, Purpose::Reg);
    let (actual, (_, clear_cookie)) = take_challenge(&hub, &req, Purpose::Reg).unwrap();
    assert_eq!(challenge, actual);
    assert!(clear_cookie.starts_with("__Host-chal=; "));
    assert!(clear_cookie.ends_with("Max-Age=0"));
    assert_eq!(
        take_challenge(&hub, &req, Purpose::Reg),
        Err(CoreError::Unauthorized)
    );
}
