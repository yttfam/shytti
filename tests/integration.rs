use shytti::api;
use shytti::bridge::HermyttBridge;
use shytti::config::Config;
use shytti::shell::ShellManager;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_config() -> Config {
    // Empty hermytt_key means auth is skipped in tests
    Config::new(
        shytti::config::DaemonConfig {
            listen: "127.0.0.1:0".into(),
            hermytt_url: "http://127.0.0.1:1".into(),
            hermytt_key: String::new(),
            max_shells: Some(64),
        },
        shytti::config::DefaultsConfig::default(),
        vec![],
    )
}

async fn start_server() -> (String, ShellManager) {
    let manager = ShellManager::new();
    let bridge = HermyttBridge::new("http://127.0.0.1:1", "test");
    let cfg = test_config();

    let app = api::router(&cfg, manager.clone(), bridge);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (base, manager)
}

/// Parse "http://host:port" into "host:port"
fn host_from_base(base: &str) -> &str {
    base.strip_prefix("http://").unwrap()
}

async fn http_request(base: &str, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let addr = host_from_base(base);
    let mut stream = TcpStream::connect(addr).await.unwrap();

    let req = match body {
        Some(b) => format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
            b.len()
        ),
        None => format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        ),
    };

    stream.write_all(req.as_bytes()).await.unwrap();

    let mut resp = String::new();
    stream.read_to_string(&mut resp).await.unwrap();

    let (head, body) = resp.split_once("\r\n\r\n").unwrap_or((&resp, ""));
    let status_line = head.lines().next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    (status, body.to_string())
}

async fn http_get(base: &str, path: &str) -> (u16, String) {
    http_request(base, "GET", path, None).await
}

async fn http_post(base: &str, path: &str, body: &str) -> (u16, String) {
    http_request(base, "POST", path, Some(body)).await
}

async fn http_delete(base: &str, path: &str) -> (u16, String) {
    http_request(base, "DELETE", path, None).await
}

fn spawn_cmd_body(name: Option<&str>, cmd: &str) -> String {
    match name {
        Some(n) => format!(r#"{{"name":"{n}","cmd":"{cmd}"}}"#),
        None => format!(r#"{{"cmd":"{cmd}"}}"#),
    }
}

// --- Tests ---

#[tokio::test]
async fn list_empty() {
    let (base, _mgr) = start_server().await;
    let (status, body) = http_get(&base, "/shells").await;
    assert_eq!(status, 200);
    assert_eq!(body, "[]");
}

#[tokio::test]
async fn spawn_returns_running() {
    let (base, _mgr) = start_server().await;
    let payload = spawn_cmd_body(None, "echo hello");
    let (status, body) = http_post(&base, "/shells", &payload).await;
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["status"], "running");
    assert!(!v["id"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn spawn_then_list() {
    let (base, _mgr) = start_server().await;
    let payload = spawn_cmd_body(Some("test-shell"), "sleep 60");
    let (status, _) = http_post(&base, "/shells", &payload).await;
    assert_eq!(status, 200);

    let (status, body) = http_get(&base, "/shells").await;
    assert_eq!(status, 200);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "test-shell");
}

#[tokio::test]
async fn spawn_then_kill() {
    let (base, _mgr) = start_server().await;
    let payload = spawn_cmd_body(None, "sleep 60");
    let (_, spawn_body) = http_post(&base, "/shells", &payload).await;
    let v: serde_json::Value = serde_json::from_str(&spawn_body).unwrap();
    let id = v["id"].as_str().unwrap();

    let (status, kill_body) = http_delete(&base, &format!("/shells/{id}")).await;
    assert_eq!(status, 200);
    let kv: serde_json::Value = serde_json::from_str(&kill_body).unwrap();
    assert_eq!(kv["status"], "dead");
}

#[tokio::test]
async fn kill_nonexistent_404() {
    let (base, _mgr) = start_server().await;
    let (status, _) = http_delete(&base, "/shells/does-not-exist").await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn resize_shell() {
    let (base, _mgr) = start_server().await;
    let payload = spawn_cmd_body(None, "sleep 60");
    let (_, spawn_body) = http_post(&base, "/shells", &payload).await;
    let v: serde_json::Value = serde_json::from_str(&spawn_body).unwrap();
    let id = v["id"].as_str().unwrap();

    let (status, _) = http_post(
        &base,
        &format!("/shells/{id}/resize"),
        r#"{"rows":40,"cols":120}"#,
    ).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn resize_nonexistent_404() {
    let (base, _mgr) = start_server().await;
    let (status, _) = http_post(
        &base,
        "/shells/nope/resize",
        r#"{"rows":24,"cols":80}"#,
    ).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn spawn_with_name() {
    let (base, _mgr) = start_server().await;
    let payload = spawn_cmd_body(Some("my-shell"), "sleep 10");
    let (status, body) = http_post(&base, "/shells", &payload).await;
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["name"], "my-shell");
}

#[tokio::test]
async fn multiple_spawns_listed() {
    let (base, _mgr) = start_server().await;
    for i in 0..3 {
        let payload = spawn_cmd_body(Some(&format!("s{i}")), "sleep 60");
        let (status, _) = http_post(&base, "/shells", &payload).await;
        assert_eq!(status, 200);
    }

    let (status, body) = http_get(&base, "/shells").await;
    assert_eq!(status, 200);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(arr.len(), 3);
}

#[tokio::test]
async fn spawn_kill_then_list_empty() {
    let (base, _mgr) = start_server().await;
    let payload = spawn_cmd_body(None, "sleep 60");
    let (_, spawn_body) = http_post(&base, "/shells", &payload).await;
    let v: serde_json::Value = serde_json::from_str(&spawn_body).unwrap();
    let id = v["id"].as_str().unwrap();

    http_delete(&base, &format!("/shells/{id}")).await;

    let (status, body) = http_get(&base, "/shells").await;
    assert_eq!(status, 200);
    assert_eq!(body, "[]");
}

#[tokio::test]
async fn spawn_default_local_shell() {
    let (base, _mgr) = start_server().await;
    let (status, body) = http_post(&base, "/shells", "{}").await;
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["shell_type"], "local");
    assert_eq!(v["status"], "running");
}

#[tokio::test]
async fn bad_json_returns_4xx() {
    let (base, _mgr) = start_server().await;
    let (status, _) = http_post(&base, "/shells", "not json at all").await;
    assert!(status >= 400 && status < 500);
}
