use std::{
    fs::OpenOptions,
    io::{self, Write},
    path::Path,
};

use e2e::{MockServer, WranglerDev, browser::Browser};
use serde_json::{Value, json};

const TITLE: &str = "Dentist <b>x</b>";
const SIGN_IN_BUTTON: &str = ".entry form button";
const BARK_FORM: &str = "form:has(input[name='device_key'])";

#[tokio::test]
async fn full_journey() {
    let worker = WranglerDev::start().await;
    let mock = MockServer::start().await;
    let browser = Browser::launch().await;

    browser.open("/").await;
    browser.wait_for_page("/setup", "input[name='code']").await;
    browser
        .set_input("input[name='code']", "correct horse")
        .await;
    browser.click(SIGN_IN_BUTTON).await;
    browser.wait_for_page("/setup", "input[name='name']").await;
    let (register_options, registration) = browser
        .click_and_capture_passkey(SIGN_IN_BUTTON, "register")
        .await;
    browser.wait_for_page("/calendar", ".calendar-grid").await;

    browser.click("nav a[href='/settings']").await;
    browser
        .wait_for_page("/settings", "input[name='server']")
        .await;
    browser
        .set_input("input[name='server']", &mock.url("/bark"))
        .await;
    browser.set_input("input[name='device_key']", "k").await;
    if browser
        .evaluate("document.querySelector('input[name=enabled]').checked")
        .await
        == false
    {
        browser
            .click(&format!("{BARK_FORM} input[name='enabled']"))
            .await;
    }
    browser.click(&format!("{BARK_FORM} button.primary")).await;
    browser.wait_for("document.querySelector('[role=alert]').textContent.includes('Notification settings saved.')").await;
    browser
        .click(&format!("{BARK_FORM} button[type='button']"))
        .await;
    browser.wait_for("document.querySelector('[role=alert]').textContent.includes('Test notification sent.')").await;
    assert_eq!(mock.pushes().len(), 1);
    assert_eq!(mock.pushes()[0]["device_key"], "k");
    browser.screenshot("settings").await;

    browser.click("nav a[href='/events']").await;
    browser
        .wait_for_page("/events", ".page-head button.primary")
        .await;
    browser.click(".page-head button.primary").await;
    browser
        .wait_for("!!document.querySelector('input[name=title]')")
        .await;
    browser.set_input("input[name='title']", TITLE).await;
    // Date and time are entered in the same browser zone used to initialize settings.
    let start = browser
        .evaluate(
            r#"(() => {
        const d = new Date(Date.now() + 2 * 60 * 60 * 1000);
        const two = n => String(n).padStart(2, "0");
        return d.getFullYear() + "-" + two(d.getMonth()+1) + "-" + two(d.getDate()) +
            "T" + two(d.getHours()) + ":" + two(d.getMinutes()) + ":" + two(d.getSeconds());
    })()"#,
        )
        .await;
    browser
        .set_input("input[name='start']", start.as_str().unwrap())
        .await;
    browser
        .set_input("select[name='reminders']", "custom")
        .await;
    browser
        .wait_for("!!document.querySelector('input[name=leads]').getClientRects().length")
        .await;
    browser.set_input("input[name='leads']", "30").await;
    browser.screenshot("events").await;
    browser
        .click("form:has(input[name='title']) button[type='submit']")
        .await;
    browser.wait_for(&format!(
        "Array.from(document.querySelectorAll('.row-list li')).some(e => e.textContent.includes({}))",
        serde_json::to_string(TITLE).unwrap(),
    )).await;

    browser.click("nav a[href='/calendar']").await;
    browser.wait_for_page("/calendar", ".calendar-grid").await;
    browser.wait_for(&format!(
        "Array.from(document.querySelectorAll('.event-chip')).some(e => e.textContent.includes({}) && !e.querySelector('b'))",
        serde_json::to_string(TITLE).unwrap(),
    )).await;

    browser.screenshot("calendar").await;
    hook(&worker, "clock", json!({"offset_ms": 90 * 60_000})).await;
    let alarm = hook(&worker, "alarm", json!({})).await;
    assert_eq!(alarm["step_errors"], json!([]));
    assert_eq!(alarm["top_level_error"], Value::Null);
    let pushes = mock.pushes();
    assert_eq!(pushes.len(), 2);
    assert_eq!(pushes[1]["title"], format!("⏰ {TITLE}"));
    assert_eq!(pushes[1]["device_key"], "k");

    browser.click("nav a[href='/log']").await;
    browser.wait_for_page("/log", ".page-head h1").await;
    browser
        .wait_for(
            r#"Array.from(document.querySelectorAll('.panel'))
        .filter(e => e.querySelector('h2')?.textContent === 'Recent deliveries')
        .some(e => Array.from(e.querySelectorAll('.row')).some(row =>
            row.querySelector('strong')?.textContent.startsWith('manual:') &&
            row.querySelector('p')?.textContent.startsWith('Sent')
        ))"#,
        )
        .await;

    browser.screenshot("log").await;
    browser.click("button.logout").await;
    browser.wait_for_page("/login", SIGN_IN_BUTTON).await;
    let (login_options, login) = browser
        .click_and_capture_passkey(SIGN_IN_BUTTON, "login")
        .await;
    browser.wait_for_page("/calendar", ".calendar-grid").await;
    browser.assert_no_csp_errors();
    browser.close().await;

    assert_eq!(register_options["rp"]["id"], "localhost");
    assert_eq!(login_options["rpId"], "localhost");
    assert!(register_options["user"]["id"].is_string());
    assert!(registration["response"]["response"]["attestationObject"].is_string());
    assert!(login["response"]["response"]["signature"].is_string());
    save_fixture(
        "chrome_registration.json",
        json!({
            "user_id": register_options["user"]["id"],
            "response": registration["response"],
        }),
    );
    save_fixture("chrome_login.json", login);
}

async fn hook(worker: &WranglerDev, name: &str, body: Value) -> Value {
    worker
        .request(reqwest::Method::POST, &format!("/__test/{name}"))
        .json(&body)
        .send()
        .await
        .expect("call local Worker test hook")
        .error_for_status()
        .expect("successful test hook")
        .json()
        .await
        .expect("test hook JSON")
}

fn save_fixture(name: &str, value: Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../kairos-testkit/fixtures")
        .join(name);
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return,
        Err(error) => panic!("could not save {name}: {error}"),
    };
    serde_json::to_writer_pretty(&mut file, &value).expect("write browser fixture");
    file.write_all(b"\n").expect("finish browser fixture");
}
