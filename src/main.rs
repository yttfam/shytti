mod cli;

use cli::Command;
use shytti::{api, config, control, shell};

#[tokio::main]
async fn main() {
    let cmd = cli::parse();

    match cmd {
        Command::Daemon { config } => {
            tracing_subscriber::fmt().init();

            let cfg = match config::Config::load(config) {
                Ok(c) => c,
                Err(e) => { eprintln!("config error: {e}"); std::process::exit(1); }
            };

            tracing::info!("shytti starting");

            let manager = shell::ShellManager::new();

            for shell_cfg in &cfg.shells {
                if shell_cfg.autostart {
                    match manager.spawn(shell_cfg.into()).await {
                        Ok(id) => tracing::info!(name = %shell_cfg.name, %id, "auto-spawned"),
                        Err(e) => tracing::error!(name = %shell_cfg.name, "spawn failed: {e}"),
                    }
                }
            }

            // Mode 1: connect control WS to Hermytt
            let has_mode1 = !cfg.daemon.hermytt_url.contains("localhost") || !cfg.daemon.hermytt_key.is_empty();
            if has_mode1 {
                control::connect_to_hermytt(
                    &cfg.daemon.hermytt_url,
                    &cfg.daemon.hermytt_key,
                    manager.clone(),
                ).await;
            }

            let (app, state) = api::router_with_state(&cfg, manager);
            let key_path = control::key_path(&cfg.daemon.listen);
            *state.key_path.lock().await = Some(key_path.clone());

            if let Some(key) = control::load_key(&key_path) {
                tracing::info!("loaded pairing key, accepting Mode 2 reconnects");
                *state.long_lived_key.lock().await = Some(key);
            } else if !has_mode1 {
                let (token, encoded) = control::PairToken::generate(&cfg.daemon.listen);
                tracing::info!("no hermytt config and no pairing key — entering pair mode");
                eprintln!();
                eprintln!("PAIRING TOKEN (expires in 5 minutes):");
                eprintln!("{encoded}");
                eprintln!();
                *state.pair_state.lock().await = Some(control::PairState {
                    pair_key: token.key,
                    long_lived_key: None,
                    used: false,
                });
            }

            let listener = tokio::net::TcpListener::bind(&cfg.daemon.listen).await.unwrap();
            tracing::info!(addr = %cfg.daemon.listen, "listening");
            axum::serve(listener, app).await.unwrap();
        }
        Command::Pair { config } => {
            tracing_subscriber::fmt().init();

            let cfg = match config::Config::load(config) {
                Ok(c) => c,
                Err(e) => { eprintln!("config error: {e}"); std::process::exit(1); }
            };

            let (token, encoded) = control::PairToken::generate(&cfg.daemon.listen);

            eprintln!("Pairing token (expires in 5 minutes):");
            eprintln!();
            println!("{encoded}");
            eprintln!();
            eprintln!("Paste this token in the Hermytt admin UI to pair.");
            eprintln!("Listening on {}:{} ...", token.ip, token.port);

            let manager = shell::ShellManager::new();
            let pair_state = control::PairState {
                pair_key: token.key.clone(),
                long_lived_key: None,
                used: false,
            };

            let (app, state) = api::router_with_state(&cfg, manager);
            *state.pair_state.lock().await = Some(pair_state);
            let key_path = control::key_path(&cfg.daemon.listen);
            *state.key_path.lock().await = Some(key_path);

            let listener = tokio::net::TcpListener::bind(&cfg.daemon.listen).await.unwrap();
            tracing::info!(addr = %cfg.daemon.listen, "listening (pair mode)");
            axum::serve(listener, app).await.unwrap();
        }
        Command::Spawn { name, shell, cwd, host, agent, cmd } => {
            let body = serde_json::json!({
                "name": name, "shell": shell, "cwd": cwd,
                "host": host, "agent": agent, "cmd": cmd,
            });
            print_response(http_req("POST", "/shells", Some(&body.to_string())));
        }
        Command::List => {
            print_response(http_req("GET", "/shells", None));
        }
        Command::Kill { id } => {
            print_response(http_req("DELETE", &format!("/shells/{id}"), None));
        }
    }
}

fn http_req(method: &str, path: &str, body: Option<&str>) -> Result<String, String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let mut conn = TcpStream::connect("127.0.0.1:7778")
        .map_err(|e| format!("connect failed (is daemon running?): {e}"))?;

    let body_bytes = body.unwrap_or("");
    let req = if body.is_some() {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_bytes}",
            body_bytes.len()
        )
    } else {
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
    };

    conn.write_all(req.as_bytes()).map_err(|e| e.to_string())?;

    let mut resp = String::new();
    conn.read_to_string(&mut resp).map_err(|e| e.to_string())?;

    match resp.split_once("\r\n\r\n") {
        Some((_, body)) => Ok(body.to_string()),
        None => Ok(resp),
    }
}

fn print_response(res: Result<String, String>) {
    match res {
        Ok(body) => println!("{body}"),
        Err(e) => { eprintln!("error: {e}"); std::process::exit(1); }
    }
}
