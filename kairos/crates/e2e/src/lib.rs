//! Local Workers runtime and chunked HTTP fixtures for adapter tests.
use axum::{
    Router,
    body::{Body, Bytes},
    extract::Path,
    http::StatusCode,
    routing::{get, post},
};
use reqwest::Client;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    path::{Path as FsPath, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, task::JoinHandle};

pub struct MockServer {
    origin: String,
    server: JoinHandle<()>,
}

impl MockServer {
    pub async fn start() -> Self {
        let app = Router::new()
            .route("/big/{mb}", get(chunked_body))
            .route(
                "/bark/push",
                post(|| async { axum::Json(json!({"code": 200})) }),
            )
            .route(
                "/feeds/{name}",
                get(|| async { "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n" }),
            );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock HTTP server");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { origin, server }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn chunked_body(Path(mb): Path<u8>) -> Body {
    let chunk = Bytes::from(vec![0; 64 * 1024]);
    let stream = futures::stream::iter(
        (0..usize::from(mb) * 16).map(move |_| Ok::<_, Infallible>(chunk.clone())),
    );
    Body::from_stream(stream)
}

pub struct WranglerDev {
    process: Option<Child>,
    root: PathBuf,
    client: Client,
}

const ORIGIN: &str = "http://127.0.0.1:8799";

impl WranglerDev {
    pub async fn start() -> Self {
        let root = FsPath::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let mut worker = Self {
            process: None,
            root,
            client,
        };
        worker.launch().await;
        worker
    }

    async fn launch(&mut self) {
        let listener = std::net::TcpListener::bind("127.0.0.1:8799")
            .expect("port 8799 is in use; refusing to use another Worker");
        drop(listener);
        let mut cmd = Command::new("npx");
        cmd.current_dir(&self.root)
            .args([
                "--yes",
                "wrangler@4.164.0",
                "dev",
                "--env",
                "e2e",
                "--local",
                "--ip",
                "127.0.0.1",
                "--port",
                "8799",
                "--persist-to",
                "target/e2e-state",
                "--var",
                "RECOVERY_CODE:correct horse",
                "--var",
                "SESSION_SECRET:0000000000000000000000000000000000000000000000000000000000000001",
            ])
            .stdin(Stdio::null());
        // npm, Wrangler and workerd must stop together even if an assertion panics.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        self.process = Some(cmd.spawn().expect("start local Wrangler"));
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            if let Some(status) = self.process.as_mut().unwrap().try_wait().unwrap() {
                panic!("Wrangler exited during startup: {status}");
            }
            if let Ok(response) = self.client.get(format!("{ORIGIN}/api/health")).send().await
                && response.status() == StatusCode::OK
                && response.json::<Value>().await.ok() == Some(json!({"ok": true}))
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Wrangler did not serve /api/health within five minutes"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.process.take() {
            #[cfg(unix)]
            {
                let _ = Command::new("kill")
                    .args(["-TERM", "--", &format!("-{}", child.id())])
                    .status();
            }
            #[cfg(windows)]
            {
                let _ = Command::new("taskkill")
                    .args(["/T", "/F", "/PID", &child.id().to_string()])
                    .status();
            }
            let _ = child.wait();
        }
    }

    pub async fn restart(&mut self) {
        self.stop();
        self.launch().await;
    }

    pub async fn get(&self, path: &str) -> Value {
        self.client
            .get(format!("{ORIGIN}{path}"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    pub async fn post(&self, probe: &str, body: Value) -> Value {
        self.post_response(probe, body)
            .await
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    pub async fn post_response(&self, probe: &str, body: Value) -> reqwest::Response {
        self.client
            .post(format!("{ORIGIN}/__test/adapter/{probe}"))
            .json(&body)
            .send()
            .await
            .unwrap()
    }
}

impl Drop for WranglerDev {
    fn drop(&mut self) {
        self.stop();
    }
}
