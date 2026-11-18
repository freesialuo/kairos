//! Headless browser for the local passkey journey.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use chromiumoxide::{
    Browser as Chrome, BrowserConfig, Page,
    cdp::browser_protocol::{log, network, web_authn},
    page::ScreenshotParams,
};
use futures::StreamExt as _;
use serde_json::Value;
use tokio::{
    task::JoinHandle,
    time::{sleep, timeout},
};

pub const ORIGIN: &str = "http://localhost:8799";
const WAIT: Duration = Duration::from_secs(20);

pub struct Browser {
    chrome: Chrome,
    page: Page,
    handler: JoinHandle<()>,
    log_handler: JoinHandle<()>,
    csp_errors: Arc<Mutex<Vec<String>>>,
}

impl Browser {
    pub async fn launch() -> Self {
        let mut builder = BrowserConfig::builder()
            .new_headless_mode()
            .window_size(1280, 900);
        if let Some(executable) = std::env::var_os("KAIROS_CHROME_PATH") {
            builder = builder.chrome_executable(executable);
        }
        let config = builder.build().expect("installed Chrome or Chromium");
        let (chrome, mut events) = Chrome::launch(config).await.expect("launch Chrome");
        let handler = tokio::spawn(async move {
            while let Some(event) = events.next().await {
                if event.is_err() {
                    break;
                }
            }
        });
        let page = chrome
            .new_page("about:blank")
            .await
            .expect("new browser tab");
        page.execute(web_authn::EnableParams::default())
            .await
            .expect("enable virtual WebAuthn");
        let options = web_authn::VirtualAuthenticatorOptions::builder()
            .protocol(web_authn::AuthenticatorProtocol::Ctap2)
            .transport(web_authn::AuthenticatorTransport::Internal)
            .has_resident_key(true)
            .has_user_verification(true)
            .is_user_verified(true)
            .automatic_presence_simulation(true)
            .build()
            .expect("CTAP2 authenticator options");
        page.execute(web_authn::AddVirtualAuthenticatorParams::new(options))
            .await
            .expect("create virtual authenticator");
        page.execute(
            network::EnableParams::builder()
                .max_total_buffer_size(4 * 1024 * 1024)
                .max_resource_buffer_size(128 * 1024)
                .max_post_data_size(128 * 1024)
                .build(),
        )
        .await
        .expect("capture browser requests");
        let mut log_entries = page
            .event_listener::<log::EventEntryAdded>()
            .await
            .expect("subscribe to Chrome logs");
        page.execute(log::EnableParams::default())
            .await
            .expect("enable Chrome logs");
        let csp_errors = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::clone(&csp_errors);
        let log_handler = tokio::spawn(async move {
            while let Some(log) = log_entries.next().await {
                if log.entry.text.contains("Content-Security-Policy")
                    || log.entry.text.contains("Content Security Policy")
                {
                    errors.lock().expect("CSP log").push(log.entry.text.clone());
                }
            }
        });
        Self {
            chrome,
            page,
            handler,
            log_handler,
            csp_errors,
        }
    }

    pub async fn open(&self, path: &str) {
        self.page
            .goto(format!("{ORIGIN}{path}"))
            .await
            .expect("navigate browser");
    }

    pub async fn evaluate(&self, script: &str) -> Value {
        self.page
            .evaluate(script)
            .await
            .expect("evaluate browser expression")
            .into_value()
            .expect("browser result should be JSON")
    }

    pub async fn wait_for(&self, condition: &str) {
        let ready = timeout(WAIT, async {
            loop {
                // Full page navigation can replace the execution context between polls.
                if let Ok(result) = self.page.evaluate(condition).await
                    && result.into_value::<bool>().unwrap_or(false)
                {
                    break;
                }
                sleep(Duration::from_millis(40)).await;
            }
        })
        .await;
        if ready.is_err() {
            let details = self
                .page
                .evaluate(
                    r#"({
                path: location.pathname,
                text: document.body?.innerText.slice(0, 1_200)
            })"#,
                )
                .await
                .ok()
                .and_then(|result| result.into_value::<Value>().ok());
            self.screenshot("failed").await;
            panic!("browser did not satisfy {condition}; page: {details:?}");
        }
    }

    pub async fn wait_for_page(&self, path: &str, selector: &str) {
        self.wait_for(&format!(
            "location.pathname === {} && !!document.querySelector({})",
            serde_json::to_string(path).unwrap(),
            serde_json::to_string(selector).unwrap(),
        ))
        .await;
    }

    pub async fn click(&self, selector: &str) {
        self.page
            .find_element(selector)
            .await
            .unwrap_or_else(|err| panic!("find {selector}: {err}"))
            .click()
            .await
            .unwrap_or_else(|err| panic!("click {selector}: {err}"));
    }

    pub async fn set_input(&self, selector: &str, value: &str) {
        let js = format!(
            r#"(() => {{
                const input = document.querySelector({});
                if (!input) return false;
                input.value = {};
                input.dispatchEvent(new Event("input", {{bubbles:true}}));
                input.dispatchEvent(new Event("change", {{bubbles:true}}));
                return !!input.value;
            }})()"#,
            serde_json::to_string(selector).unwrap(),
            serde_json::to_string(value).unwrap(),
        );
        assert_eq!(self.evaluate(&js).await, true, "set input {selector}");
    }

    pub async fn click_and_capture_passkey(
        &self,
        selector: &str,
        ceremony: &str,
    ) -> (Value, Value) {
        let path = format!("{ORIGIN}/auth/passkey/{ceremony}");
        let options_url = format!("{path}/options");
        let verify_url = format!("{path}/verify");
        let mut responses = self
            .page
            .event_listener::<network::EventResponseReceived>()
            .await
            .expect("observe WebAuthn options");
        let mut loaded = self
            .page
            .event_listener::<network::EventLoadingFinished>()
            .await
            .expect("observe completed options response");
        let mut requests = self
            .page
            .event_listener::<network::EventRequestWillBeSent>()
            .await
            .expect("observe WebAuthn verification");
        let capture_options = async {
            while let Some(event) = responses.next().await {
                if event.response.url != options_url {
                    continue;
                }
                assert_eq!(event.response.status, 200);
                while let Some(done) = loaded.next().await {
                    if done.request_id == event.request_id {
                        let response = self
                            .page
                            .execute(network::GetResponseBodyParams::new(
                                event.request_id.clone(),
                            ))
                            .await
                            .expect("read WebAuthn options");
                        assert!(!response.result.base64_encoded);
                        return serde_json::from_str(&response.result.body)
                            .expect("WebAuthn options JSON");
                    }
                }
            }
            panic!("no WebAuthn options response");
        };
        let capture_request = async {
            while let Some(event) = requests.next().await {
                if event.request.url == verify_url && event.request.method == "POST" {
                    let body = self
                        .page
                        .execute(network::GetRequestPostDataParams::new(
                            event.request_id.clone(),
                        ))
                        .await
                        .expect("read WebAuthn verification payload");
                    return serde_json::from_str(&body.result.post_data)
                        .expect("WebAuthn verification JSON");
                }
            }
            panic!("no WebAuthn verification request");
        };
        let ((), options, body) = timeout(WAIT, async {
            tokio::join!(self.click(selector), capture_options, capture_request)
        })
        .await
        .unwrap_or_else(|_| panic!("timed out during passkey {ceremony}"));
        (options, body)
    }

    pub fn assert_no_csp_errors(&self) {
        let errors = self.csp_errors.lock().expect("CSP log");
        assert!(errors.is_empty(), "browser CSP failures: {errors:?}");
    }

    pub async fn screenshot(&self, page: &str) {
        let Some(directory) = std::env::var_os("KAIROS_E2E_SCREENSHOT_DIR") else {
            return;
        };
        let path = PathBuf::from(directory).join(format!("kairos-visual-{page}.png"));
        self.page
            .save_screenshot(ScreenshotParams::builder().full_page(true).build(), path)
            .await
            .expect("save local browser screenshot");
    }

    pub async fn close(mut self) {
        let _ = self.chrome.close().await;
        self.chrome.wait().await.expect("reap Chrome process");
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.log_handler.abort();
        self.handler.abort();
    }
}
