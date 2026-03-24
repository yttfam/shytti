//! Security tests — adversarial inputs, auth enforcement, resource limits.

use shytti::api;
use shytti::config::{Config, DaemonConfig, DefaultsConfig, ShellConfig};
use shytti::shell::ShellManager;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn config_with_auth(key: &str, max_shells: usize) -> Config {
    Config::new(
        DaemonConfig {
            listen: "127.0.0.1:0".into(),
            hermytt_url: "http://127.0.0.1:1".into(),
            advertise: None,
            hermytt_key: key.into(),
            max_shells: Some(max_shells),
        },
        DefaultsConfig::default(),
        vec![
            ShellConfig {
                name: "allowed-host".into(),
                host: Some("cali@10.10.0.7".into()),
                shell: None, cwd: None, key: None, agent: None,
                project: None, cmd: None, autostart: false,
            },
        ],
    )
}

fn config_no_auth() -> Config {
    Config::new(
        DaemonConfig {
            listen: "127.0.0.1:0".into(),
            hermytt_url: "http://127.0.0.1:1".into(),
            advertise: None,
            hermytt_key: String::new(),
            max_shells: Some(3),
        },
        DefaultsConfig::default(),
        vec![],
    )
}

async fn start_with_config(cfg: Config) -> String {
    let manager = ShellManager::new();

    let app = api::router(&cfg, manager);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    addr
}

async fn http_req(addr: &str, method: &str, path: &str, body: Option<&str>, headers: &[(&str, &str)]) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let body_bytes = body.unwrap_or("");

    let mut extra_headers = String::new();
    for (k, v) in headers {
        extra_headers.push_str(&format!("{k}: {v}\r\n"));
    }

    let req = if body.is_some() {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{extra_headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_bytes}",
            body_bytes.len()
        )
    } else {
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n{extra_headers}Connection: close\r\n\r\n")
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

// --- Auth tests ---

#[tokio::test]
async fn auth_rejects_no_key() {
    let addr = start_with_config(config_with_auth("secret123", 64)).await;
    let (status, _) = http_req(&addr, "GET", "/shells", None, &[]).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn auth_rejects_wrong_key() {
    let addr = start_with_config(config_with_auth("secret123", 64)).await;
    let (status, _) = http_req(&addr, "GET", "/shells", None, &[("x-shytti-key", "wrong")]).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn auth_accepts_correct_key() {
    let addr = start_with_config(config_with_auth("secret123", 64)).await;
    let (status, _) = http_req(&addr, "GET", "/shells", None, &[("x-shytti-key", "secret123")]).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn auth_required_for_spawn() {
    let addr = start_with_config(config_with_auth("secret123", 64)).await;
    let (status, _) = http_req(&addr, "POST", "/shells", Some(r#"{"cmd":"echo hi"}"#), &[]).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn auth_required_for_kill() {
    let addr = start_with_config(config_with_auth("secret123", 64)).await;
    let (status, _) = http_req(&addr, "DELETE", "/shells/foo", None, &[]).await;
    assert_eq!(status, 401);
}

// --- Shell allowlist tests ---

#[tokio::test]
async fn rejects_arbitrary_binary_as_shell() {
    let addr = start_with_config(config_no_auth()).await;
    let (status, body) = http_req(
        &addr, "POST", "/shells",
        Some(r#"{"shell":"/usr/bin/python3"}"#), &[],
    ).await;
    assert!(status >= 400, "should reject arbitrary binary, got {status}: {body}");
}

#[tokio::test]
async fn rejects_rm_as_shell() {
    let addr = start_with_config(config_no_auth()).await;
    let (status, _) = http_req(
        &addr, "POST", "/shells",
        Some(r#"{"shell":"/bin/rm"}"#), &[],
    ).await;
    assert!(status >= 400);
}

#[tokio::test]
async fn accepts_valid_shell() {
    let addr = start_with_config(config_no_auth()).await;
    let (status, body) = http_req(
        &addr, "POST", "/shells",
        Some(r#"{"shell":"/bin/zsh"}"#), &[],
    ).await;
    assert_eq!(status, 200, "should accept /bin/zsh: {body}");
}

// --- Host allowlist tests ---

#[tokio::test]
async fn rejects_unknown_ssh_host() {
    let addr = start_with_config(config_with_auth("s", 64)).await;
    let (status, body) = http_req(
        &addr, "POST", "/shells",
        Some(r#"{"host":"attacker@evil.com"}"#),
        &[("x-shytti-key", "s")],
    ).await;
    assert!(status >= 400, "should reject unknown host, got {status}: {body}");
}

// --- Resource limits ---

#[tokio::test]
async fn shell_count_limit_enforced() {
    let addr = start_with_config(config_no_auth()).await; // max_shells = 3

    // Spawn 3 shells — should all succeed
    for i in 0..3 {
        let (status, _) = http_req(
            &addr, "POST", "/shells",
            Some(&format!(r#"{{"name":"s{i}","cmd":"sleep 60"}}"#)), &[],
        ).await;
        assert_eq!(status, 200, "shell {i} should spawn");
    }

    // 4th should fail
    let (status, body) = http_req(
        &addr, "POST", "/shells",
        Some(r#"{"cmd":"sleep 60"}"#), &[],
    ).await;
    assert!(status >= 400, "4th shell should be rejected: {body}");
    assert!(body.contains("limit"), "error should mention limit: {body}");
}

// --- Name length ---

#[tokio::test]
async fn rejects_oversized_name() {
    let addr = start_with_config(config_no_auth()).await;
    let long_name = "x".repeat(200);
    let body = format!(r#"{{"name":"{long_name}","cmd":"echo hi"}}"#);
    let (status, resp) = http_req(&addr, "POST", "/shells", Some(&body), &[]).await;
    assert!(status >= 400, "should reject 200-char name: {resp}");
}

// --- Path traversal ---

#[tokio::test]
async fn rejects_proc_cwd() {
    let addr = start_with_config(config_no_auth()).await;
    let (status, _) = http_req(
        &addr, "POST", "/shells",
        Some(r#"{"cwd":"/proc","cmd":"echo hi"}"#), &[],
    ).await;
    assert!(status >= 400);
}

#[tokio::test]
async fn rejects_root_cwd() {
    let addr = start_with_config(config_no_auth()).await;
    let (status, _) = http_req(
        &addr, "POST", "/shells",
        Some(r#"{"cwd":"/root","cmd":"echo hi"}"#), &[],
    ).await;
    assert!(status >= 400);
}

#[tokio::test]
async fn rejects_relative_cwd() {
    let addr = start_with_config(config_no_auth()).await;
    let (status, _) = http_req(
        &addr, "POST", "/shells",
        Some(r#"{"cwd":"../../etc","cmd":"echo hi"}"#), &[],
    ).await;
    assert!(status >= 400);
}

// --- Malformed input ---

#[tokio::test]
async fn handles_empty_body() {
    let addr = start_with_config(config_no_auth()).await;
    let (status, _) = http_req(&addr, "POST", "/shells", Some(""), &[]).await;
    assert!(status >= 400);
}

#[tokio::test]
async fn handles_binary_garbage() {
    let addr = start_with_config(config_no_auth()).await;
    let (status, _) = http_req(&addr, "POST", "/shells", Some("\x00\x01\x02\x7f"), &[]).await;
    assert!(status >= 400);
}
