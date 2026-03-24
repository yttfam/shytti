//! End-to-end tests: start the full daemon, interact via raw HTTP.

use shytti::api;
use shytti::config::{Config, DaemonConfig, DefaultsConfig};
use shytti::shell::ShellManager;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_config() -> Config {
    Config::new(
        DaemonConfig {
            listen: "127.0.0.1:0".into(),
            hermytt_url: "http://127.0.0.1:1".into(),
            advertise: None,
            hermytt_key: String::new(),
            max_shells: Some(64),
        },
        DefaultsConfig::default(),
        vec![],
    )
}

async fn start_daemon() -> String {
    let manager = ShellManager::new();
    let cfg = test_config();

    let app = api::router(&cfg, manager);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    addr
}

async fn http(addr: &str, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let body_bytes = body.unwrap_or("");
    let req = if body.is_some() {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_bytes}",
            body_bytes.len()
        )
    } else {
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
    };

    stream.write_all(req.as_bytes()).await.unwrap();

    let mut resp = String::new();
    stream.read_to_string(&mut resp).await.unwrap();

    let (head, body) = resp.split_once("\r\n\r\n").unwrap_or((&resp, ""));
    let status: u16 = head
        .lines().next().unwrap_or("")
        .split_whitespace().nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    (status, body.to_string())
}

// --- E2E: Full lifecycle ---

#[tokio::test]
async fn e2e_full_lifecycle() {
    let addr = start_daemon().await;

    // 1. List — empty
    let (status, body) = http(&addr, "GET", "/shells", None).await;
    assert_eq!(status, 200);
    assert_eq!(body, "[]");

    // 2. Spawn a shell
    let (status, body) = http(&addr, "POST", "/shells", Some(r#"{"name":"e2e-test","cmd":"sleep 60"}"#)).await;
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["name"], "e2e-test");
    assert_eq!(v["shell_type"], "command");
    assert_eq!(v["status"], "running");
    let id = v["id"].as_str().unwrap().to_string();

    // 3. List — one shell
    let (status, body) = http(&addr, "GET", "/shells", None).await;
    assert_eq!(status, 200);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"].as_str().unwrap(), &id);

    // 4. Resize
    let (status, _) = http(&addr, "POST", &format!("/shells/{id}/resize"), Some(r#"{"rows":50,"cols":200}"#)).await;
    assert_eq!(status, 200);

    // 5. Kill
    let (status, body) = http(&addr, "DELETE", &format!("/shells/{id}"), None).await;
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["status"], "dead");

    // 6. List — empty again
    let (status, body) = http(&addr, "GET", "/shells", None).await;
    assert_eq!(status, 200);
    assert_eq!(body, "[]");

    // 7. Kill again — 404
    let (status, _) = http(&addr, "DELETE", &format!("/shells/{id}"), None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn e2e_spawn_local_shell() {
    let addr = start_daemon().await;

    let (status, body) = http(&addr, "POST", "/shells", Some("{}")).await;
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["shell_type"], "local");

    let id = v["id"].as_str().unwrap();
    http(&addr, "DELETE", &format!("/shells/{id}"), None).await;
}

#[tokio::test]
async fn e2e_multiple_shells_independent() {
    let addr = start_daemon().await;

    let mut ids = vec![];
    for i in 0..5 {
        let (status, body) = http(
            &addr, "POST", "/shells",
            Some(&format!(r#"{{"name":"shell-{i}","cmd":"sleep 60"}}"#)),
        ).await;
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        ids.push(v["id"].as_str().unwrap().to_string());
    }

    let (_, body) = http(&addr, "GET", "/shells", None).await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(arr.len(), 5);

    // Kill #2 and #4
    http(&addr, "DELETE", &format!("/shells/{}", ids[1]), None).await;
    http(&addr, "DELETE", &format!("/shells/{}", ids[3]), None).await;

    let (_, body) = http(&addr, "GET", "/shells", None).await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(arr.len(), 3);

    let remaining: Vec<&str> = arr.iter().map(|v| v["id"].as_str().unwrap()).collect();
    assert!(remaining.contains(&ids[0].as_str()));
    assert!(remaining.contains(&ids[2].as_str()));
    assert!(remaining.contains(&ids[4].as_str()));
}
