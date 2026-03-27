use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

use crate::error::Error;
use crate::shell::{ShellManager, SpawnRequest};

pub fn gethostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into())
}

// --- Wire protocol types ---

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMsg {
    // Auth (first message)
    Auth { auth: String, name: String, role: String },
    AuthOk { status: String },

    // Shytti → Hermytt
    Heartbeat { meta: serde_json::Value },
    SpawnOk { req_id: String, shell_id: String, session_id: String },
    SpawnErr { req_id: String, error: String },
    KillOk { shell_id: String },
    ShellDied { shell_id: String, #[serde(skip_serializing_if = "Option::is_none")] session_id: Option<String> },

    // Hermytt → Shytti
    Spawn {
        req_id: String,
        #[serde(default)]
        shell: Option<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
    Kill { shell_id: String },
    Resize { shell_id: String, cols: u16, rows: u16 },

    // Data plane (Mode 2 — multiplexed over control WS)
    // Shytti → Hermytt: PTY output
    Data { session_id: String, data: String },
    // Hermytt → Shytti: stdin
    Input { session_id: String, data: String },
}

fn send_msg(msg: &ControlMsg) -> Message {
    Message::Text(serde_json::to_string(msg).unwrap().into())
}

fn parse_msg(msg: &Message) -> Option<ControlMsg> {
    match msg {
        Message::Text(t) => serde_json::from_str(t).ok(),
        _ => None,
    }
}

// --- Mode 1: Shytti connects to Hermytt ---

pub async fn connect_to_hermytt(
    hermytt_url: &str,
    auth_key: &str,
    manager: ShellManager,
    max_shells: usize,
    allowed_hosts: Vec<String>,
) {
    let ws_url = hermytt_url
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    let url = format!("{ws_url}/control");
    let key = auth_key.to_string();
    let hostname = gethostname();
    let name = format!("shytti-{hostname}");

    tokio::spawn(async move {
        let mut backoff = 1u64;
        loop {
            tracing::info!("connecting to hermytt control: {url}");
            match tokio_tungstenite::connect_async(&url).await {
                Ok((ws, _)) => {
                    backoff = 1;
                    let (sink, stream) = ws.split();
                    let sink = Arc::new(Mutex::new(sink));

                    // Auth handshake
                    let auth = ControlMsg::Auth {
                        auth: key.clone(),
                        name: name.clone(),
                        role: "shell".into(),
                    };
                    if sink.lock().await.send(send_msg(&auth)).await.is_err() {
                        tracing::error!("control: failed to send auth");
                        continue;
                    }

                    // Run control loop
                    run_control(sink, stream, &manager, &hostname, max_shells, &allowed_hosts).await;
                    tracing::warn!("control: connection lost, reconnecting...");
                }
                Err(e) => {
                    tracing::warn!("control: connect failed: {e}");
                }
            }

            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(30);
        }
    });
}

// --- Shared control loop (works for both modes) ---

pub async fn run_control<S, K>(
    sink: Arc<Mutex<S>>,
    mut stream: K,
    manager: &ShellManager,
    hostname: &str,
    max_shells: usize,
    allowed_hosts: &[String],
) where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin + Send + 'static,
    K: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    // Start heartbeat task
    let hb_sink = sink.clone();
    let hb_manager = manager.clone();
    let hb_hostname = hostname.to_string();
    let heartbeat = tokio::spawn(async move {
        loop {
            let count = hb_manager.list().await.len();
            let msg = ControlMsg::Heartbeat {
                meta: serde_json::json!({
                    "host": hb_hostname,
                    "shells_active": count,
                }),
            };
            let mut sink = hb_sink.lock().await;
            if sink.send(send_msg(&msg)).await.is_err() {
                break;
            }
            // WS-level ping to keep the connection alive through NAT/proxies/macOS
            if sink.send(Message::Ping(vec![].into())).await.is_err() {
                break;
            }
            drop(sink);
            tokio::time::sleep(Duration::from_secs(15)).await;
        }
    });

    // Watch for shell deaths → send shell_died
    let death_sink = sink.clone();
    let mut death_rx = manager.on_death();
    let death_watcher = tokio::spawn(async move {
        while let Ok(death) = death_rx.recv().await {
            let msg = ControlMsg::ShellDied {
                shell_id: death.shell_id,
                session_id: death.session_id,
            };
            if death_sink.lock().await.send(send_msg(&msg)).await.is_err() {
                break;
            }
        }
    });

    // Writers for Mode 2 sessions (session_id → PTY writer)
    let writers: Arc<Mutex<std::collections::HashMap<String, Box<dyn Write + Send>>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));

    // Process incoming messages
    while let Some(Ok(msg)) = stream.next().await {
        // Handle WS-level ping → pong (split streams can't auto-respond)
        if let Message::Ping(data) = &msg {
            let _ = sink.lock().await.send(Message::Pong(data.clone())).await;
            continue;
        }
        let Some(ctrl) = parse_msg(&msg) else {
            if let Message::Text(t) = &msg {
                tracing::warn!("control: unparseable message: {}", t);
            }
            continue;
        };

        match ctrl {
            ControlMsg::AuthOk { .. } => {
                tracing::info!("control: authenticated");
            }
            ControlMsg::Spawn { req_id, shell, cwd, session_id, name } => {
                let result = manager.spawn_with_limits(SpawnRequest {
                    name,
                    shell,
                    cwd,
                    host: None,
                    agent: None,
                    cmd: None,
                }, max_shells, allowed_hosts).await;

                match result {
                    Ok(shell_id) => {
                        let sid = session_id.unwrap_or_else(|| shell_id.clone());
                        manager.set_session_id(&shell_id, &sid).await;

                        match manager.get_reader_writer(&shell_id).await {
                            Err(e) => tracing::error!(%shell_id, "get_reader_writer failed: {e}"),
                            Ok((reader, writer)) => {
                                writers.lock().await.insert(sid.clone(), writer);
                                tracing::info!(%shell_id, session_id = %sid, "data relay started");
                                let data_sink = sink.clone();
                                let data_sid = sid.clone();
                                tokio::spawn(async move {
                                    let mut reader = reader;
                                    loop {
                                        let mut r = reader;
                                        let (r_back, result): (Box<dyn std::io::Read + Send>, std::io::Result<Vec<u8>>) =
                                            tokio::task::spawn_blocking(move || {
                                                let mut buf = [0u8; 4096];
                                                let n = r.read(&mut buf);
                                                (r, n.map(|n| buf[..n].to_vec()))
                                            }).await.unwrap_or_else(|_| panic!("pty read panicked"));
                                        reader = r_back;
                                        match result {
                                            Ok(ref data) if data.is_empty() => {
                                                tracing::info!(session_id = %data_sid, "pty EOF");
                                                break;
                                            }
                                            Ok(data) => {
                                                tracing::info!(session_id = %data_sid, bytes = data.len(), "sending data");
                                                let msg = ControlMsg::Data {
                                                    session_id: data_sid.clone(),
                                                    data: base64_encode(&data),
                                                };
                                                if data_sink.lock().await.send(send_msg(&msg)).await.is_err() {
                                                    tracing::warn!(session_id = %data_sid, "data send failed");
                                                    break;
                                                }
                                            }
                                            Err(e) => {
                                                tracing::error!(session_id = %data_sid, "pty read error: {e}");
                                                break;
                                            }
                                        }
                                    }
                                });
                            }
                        }
                        let resp = ControlMsg::SpawnOk {
                            req_id,
                            shell_id,
                            session_id: sid,
                        };
                        let _ = sink.lock().await.send(send_msg(&resp)).await;
                    }
                    Err(e) => {
                        let resp = ControlMsg::SpawnErr {
                            req_id,
                            error: e.to_string(),
                        };
                        let _ = sink.lock().await.send(send_msg(&resp)).await;
                    }
                }
            }
            ControlMsg::Kill { shell_id } => {
                writers.lock().await.remove(&shell_id);
                match manager.kill(&shell_id).await {
                    Ok(_) => {
                        let resp = ControlMsg::KillOk { shell_id };
                        let _ = sink.lock().await.send(send_msg(&resp)).await;
                    }
                    Err(e) => tracing::warn!("kill failed: {e}"),
                }
            }
            ControlMsg::Resize { shell_id, cols, rows } => {
                if let Err(e) = manager.resize(&shell_id, rows, cols).await {
                    tracing::warn!(%shell_id, "resize failed: {e}");
                }
            }
            ControlMsg::Input { session_id, data } => {
                let decoded = match base64_decode(&data) {
                    Ok(d) => d,
                    Err(_) => { tracing::warn!("input: bad base64"); continue; }
                };
                let mut ws = writers.lock().await;
                if let Some(w) = ws.get_mut(&session_id) {
                    if let Err(e) = w.write_all(&decoded) {
                        tracing::warn!(%session_id, "input write failed: {e}");
                        ws.remove(&session_id);
                    }
                } else {
                    tracing::warn!(%session_id, "input: no writer for session");
                }
            }
            _ => {}
        }
    }

    heartbeat.abort();
    death_watcher.abort();
}

// --- Mode 2: Pairing ---

/// Token payload encoded as base64 JSON
#[derive(Debug, Serialize, Deserialize)]
pub struct PairToken {
    pub ip: String,
    pub port: u16,
    pub key: String,
    pub expires: u64,
}

impl PairToken {
    pub fn generate(listen_addr: &str) -> (Self, String) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let (ip, port) = parse_listen_addr(listen_addr);
        let key = gen_key();
        let expires = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() + 300; // 5 minutes

        let token = PairToken { ip, port, key, expires };
        let json = serde_json::to_string(&token).unwrap();
        let encoded = base64_encode(json.as_bytes());
        (token, encoded)
    }

    pub fn decode(encoded: &str) -> Result<Self, Error> {
        let bytes = base64_decode(encoded)
            .map_err(|_| Error::Bridge("invalid token encoding".into()))?;
        let token: PairToken = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Bridge(format!("invalid token: {e}")))?;

        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        if now > token.expires {
            return Err(Error::Bridge("token expired".into()));
        }
        Ok(token)
    }
}

/// Stored after successful pairing
#[derive(Debug, Serialize, Deserialize)]
pub struct PairState {
    pub pair_key: String,
    pub long_lived_key: Option<String>,
    pub used: bool,
}

fn parse_listen_addr(addr: &str) -> (String, u16) {
    if let Some((ip, port)) = addr.rsplit_once(':') {
        let ip = if ip == "0.0.0.0" {
            // Try to get a real IP
            local_ip().unwrap_or_else(|| "0.0.0.0".into())
        } else {
            ip.to_string()
        };
        (ip, port.parse().unwrap_or(7778))
    } else {
        (addr.to_string(), 7778)
    }
}

fn local_ip() -> Option<String> {
    std::process::Command::new("hostname")
        .arg("-I")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().next().map(|s| s.to_string()))
        .or_else(|| {
            // macOS fallback
            std::process::Command::new("ipconfig")
                .arg("getifaddr")
                .arg("en0")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        use std::io::Read;
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn gen_key() -> String {
    random_hex(16)
}

pub fn gen_long_lived_key() -> String {
    format!("sk-{}", random_hex(32))
}

// --- Key persistence ---

/// Path to store the long-lived key (next to config)
pub fn key_path(listen_addr: &str) -> std::path::PathBuf {
    let _ = listen_addr; // could use for naming, but keep simple
    std::path::PathBuf::from("/opt/shytti/.shytti-key")
}

pub fn save_key(path: &std::path::Path, key: &str) {
    if let Err(e) = std::fs::write(path, key) {
        tracing::error!("failed to save pairing key: {e}");
    } else {
        // Restrict permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        tracing::info!(path = %path.display(), "saved pairing key");
    }
}

pub fn load_key(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

// Minimal base64 — no dep needed
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[(n >> 18 & 63) as usize] as char);
        result.push(CHARS[(n >> 12 & 63) as usize] as char);
        if chunk.len() > 1 { result.push(CHARS[(n >> 6 & 63) as usize] as char); } else { result.push('='); }
        if chunk.len() > 2 { result.push(CHARS[(n & 63) as usize] as char); } else { result.push('='); }
    }
    result
}

fn base64_decode(data: &str) -> Result<Vec<u8>, ()> {
    const TABLE: [u8; 128] = {
        let mut t = [255u8; 128];
        let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0;
        while i < 64 {
            t[chars[i] as usize] = i as u8;
            i += 1;
        }
        t
    };

    let bytes: Vec<u8> = data.bytes().filter(|&b| b != b'=').collect();
    let mut result = Vec::new();
    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 { break; }
        let b = |i: usize| -> u32 {
            chunk.get(i).copied()
                .and_then(|c| TABLE.get(c as usize).copied())
                .filter(|&v| v != 255)
                .unwrap_or(0) as u32
        };
        let n = (b(0) << 18) | (b(1) << 12) | (b(2) << 6) | b(3);
        result.push((n >> 16) as u8);
        if chunk.len() > 2 { result.push((n >> 8) as u8); }
        if chunk.len() > 3 { result.push(n as u8); }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        let data = b"hello world";
        let encoded = base64_encode(data);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn base64_roundtrip_json() {
        let json = r#"{"ip":"10.10.0.4","port":7778,"key":"abc123","expires":9999999999}"#;
        let encoded = base64_encode(json.as_bytes());
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, json.as_bytes());
    }

    #[test]
    fn pair_token_generate_and_decode() {
        let (token, encoded) = PairToken::generate("0.0.0.0:7778");
        let decoded = PairToken::decode(&encoded).unwrap();
        assert_eq!(decoded.port, 7778);
        assert_eq!(decoded.key, token.key);
    }

    #[test]
    fn pair_token_expired() {
        let json = r#"{"ip":"10.10.0.4","port":7778,"key":"abc","expires":1}"#;
        let encoded = base64_encode(json.as_bytes());
        assert!(PairToken::decode(&encoded).is_err());
    }

    #[test]
    fn control_msg_spawn_serialize() {
        let msg = ControlMsg::Spawn {
            req_id: "r1".into(),
            shell: Some("/bin/bash".into()),
            cwd: None,
            session_id: None,
            name: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"spawn\""));
        assert!(json.contains("\"req_id\":\"r1\""));
    }

    #[test]
    fn control_msg_heartbeat_serialize() {
        let msg = ControlMsg::Heartbeat {
            meta: serde_json::json!({"host": "iggy", "shells_active": 3}),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"heartbeat\""));
    }

    #[test]
    fn control_msg_spawn_deserialize() {
        let json = r#"{"type":"spawn","req_id":"x","shell":"/bin/bash"}"#;
        let msg: ControlMsg = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, ControlMsg::Spawn { req_id, .. } if req_id == "x"));
    }

    #[test]
    fn control_msg_kill_roundtrip() {
        let msg = ControlMsg::Kill { shell_id: "abc".into() };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ControlMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ControlMsg::Kill { shell_id } if shell_id == "abc"));
    }

    #[test]
    fn gen_key_is_unique() {
        let a = gen_key();
        let b = gen_key();
        assert_ne!(a, b);
    }

    #[test]
    fn parse_listen_addr_with_port() {
        let (ip, port) = parse_listen_addr("10.10.0.4:7778");
        assert_eq!(ip, "10.10.0.4");
        assert_eq!(port, 7778);
    }

    #[test]
    fn parse_listen_addr_no_port() {
        let (ip, port) = parse_listen_addr("10.10.0.4");
        assert_eq!(ip, "10.10.0.4");
        assert_eq!(port, 7778);
    }

    #[test]
    fn control_msg_data_roundtrip() {
        let msg = ControlMsg::Data {
            session_id: "sess-42".into(),
            data: base64_encode(b"hello from pty"),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"data\""));
        let parsed: ControlMsg = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMsg::Data { session_id, data } => {
                assert_eq!(session_id, "sess-42");
                assert_eq!(base64_decode(&data).unwrap(), b"hello from pty");
            }
            _ => panic!("expected Data variant"),
        }
    }

    #[test]
    fn control_msg_input_roundtrip() {
        let msg = ControlMsg::Input {
            session_id: "sess-7".into(),
            data: base64_encode(b"ls -la\n"),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"input\""));
        let parsed: ControlMsg = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMsg::Input { session_id, data } => {
                assert_eq!(session_id, "sess-7");
                assert_eq!(base64_decode(&data).unwrap(), b"ls -la\n");
            }
            _ => panic!("expected Input variant"),
        }
    }

    #[test]
    fn control_msg_shell_died_with_session_id() {
        let msg = ControlMsg::ShellDied {
            shell_id: "sh-1".into(),
            session_id: Some("sess-1".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"shell_died\""));
        assert!(json.contains("\"session_id\":\"sess-1\""));
        let parsed: ControlMsg = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMsg::ShellDied { shell_id, session_id } => {
                assert_eq!(shell_id, "sh-1");
                assert_eq!(session_id, Some("sess-1".into()));
            }
            _ => panic!("expected ShellDied variant"),
        }
    }

    #[test]
    fn control_msg_shell_died_without_session_id() {
        let msg = ControlMsg::ShellDied {
            shell_id: "sh-2".into(),
            session_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("session_id"));
    }

    #[test]
    fn control_msg_resize_roundtrip() {
        let msg = ControlMsg::Resize {
            shell_id: "sh-3".into(),
            cols: 120,
            rows: 40,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ControlMsg = serde_json::from_str(&json).unwrap();
        match parsed {
            ControlMsg::Resize { shell_id, cols, rows } => {
                assert_eq!(shell_id, "sh-3");
                assert_eq!(cols, 120);
                assert_eq!(rows, 40);
            }
            _ => panic!("expected Resize variant"),
        }
    }

    #[test]
    fn base64_roundtrip_binary_non_utf8() {
        let data: Vec<u8> = (0..=255).collect();
        let encoded = base64_encode(&data);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn base64_roundtrip_empty() {
        let encoded = base64_encode(b"");
        let decoded = base64_decode(&encoded).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn pair_token_generate_produces_decodable_base64() {
        let (_token, encoded) = PairToken::generate("127.0.0.1:7778");
        // Must be valid base64 that decodes to valid JSON
        let bytes = base64_decode(&encoded).unwrap();
        let decoded: PairToken = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.port, 7778);
        assert!(!decoded.key.is_empty());
        assert!(decoded.expires > 0);
    }

    #[test]
    fn gethostname_returns_non_empty() {
        let hostname = gethostname();
        assert!(!hostname.is_empty());
        assert_ne!(hostname, "unknown");
    }
}
