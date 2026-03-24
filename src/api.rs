use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State, WebSocketUpgrade},
    extract::ws::WebSocket,
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{delete, get, post},
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::control::{self, PairState, gen_long_lived_key};
use crate::error::Error;
use crate::shell::{ShellInfo, ShellManager, SpawnRequest};

const DEFAULT_MAX_SHELLS: usize = 64;

pub struct AppState {
    pub manager: ShellManager,
    api_key: String,
    max_shells: usize,
    allowed_hosts: Vec<String>,
    pub pair_state: Mutex<Option<PairState>>,
    pub long_lived_key: Mutex<Option<String>>,
    pub key_path: Mutex<Option<std::path::PathBuf>>,
}

pub fn router(cfg: &Config, manager: ShellManager) -> Router {
    let (app, _state) = router_with_state(cfg, manager);
    app
}

pub fn router_with_state(cfg: &Config, manager: ShellManager) -> (Router, Arc<AppState>) {
    let allowed_hosts: Vec<String> = cfg.shells.iter()
        .filter_map(|s| s.host.clone())
        .collect();

    let state = Arc::new(AppState {
        manager,
        api_key: cfg.daemon.hermytt_key.clone(),
        max_shells: cfg.daemon.max_shells.unwrap_or(DEFAULT_MAX_SHELLS),
        allowed_hosts,
        pair_state: Mutex::new(None),
        long_lived_key: Mutex::new(None),
        key_path: Mutex::new(None),
    });

    let app = Router::new()
        .route("/shells", post(spawn_shell))
        .route("/shells", get(list_shells))
        .route("/shells/{id}", delete(kill_shell))
        .route("/shells/{id}/resize", post(resize_shell))
        .route("/pair", get(ws_pair))
        .route("/control", get(ws_control))
        .layer(middleware::from_fn_with_state(state.clone(), auth_middleware))
        .layer(axum::extract::DefaultBodyLimit::max(65536))
        .with_state(state.clone());

    (app, state)
}

pub async fn serve(cfg: Config, manager: ShellManager) -> Result<(), Error> {
    let app = router(&cfg, manager);
    let listener = tokio::net::TcpListener::bind(&cfg.daemon.listen).await?;
    tracing::info!(addr = %cfg.daemon.listen, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    let path = request.uri().path();

    // /pair and /control handle their own auth via WS first-message
    if path == "/pair" || path == "/control" {
        return Ok(next.run(request).await);
    }

    if state.api_key.is_empty() {
        return Ok(next.run(request).await);
    }

    let provided = headers
        .get("x-shytti-key")
        .and_then(|v| v.to_str().ok());

    match provided {
        Some(key) if key == state.api_key => Ok(next.run(request).await),
        _ => {
            tracing::warn!("rejected unauthenticated request");
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

// --- REST handlers ---

async fn spawn_shell(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SpawnRequest>,
) -> Result<Json<ShellInfo>, Error> {
    let id = state.manager.spawn_with_limits(
        req,
        state.max_shells,
        &state.allowed_hosts,
    ).await?;

    let shells = state.manager.list().await;
    let info = shells.into_iter().find(|s| s.id == id)
        .ok_or_else(|| Error::NotFound(id))?;
    Ok(Json(info))
}

async fn list_shells(State(state): State<Arc<AppState>>) -> Json<Vec<ShellInfo>> {
    Json(state.manager.list().await)
}

async fn kill_shell(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ShellInfo>, Error> {
    Ok(Json(state.manager.kill(&id).await?))
}

#[derive(Deserialize)]
struct ResizeRequest {
    rows: u16,
    cols: u16,
}

async fn resize_shell(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<ResizeRequest>,
) -> Result<(), Error> {
    state.manager.resize(&id, req.rows, req.cols).await
}

// --- Mode 2: /pair WS endpoint ---

async fn ws_pair(
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_pair(socket, state))
}

async fn handle_pair(socket: WebSocket, state: Arc<AppState>) {
    let (mut sink, mut stream) = socket.split();

    // Expect first message: {"pair_key": "one-time-key"}
    let pair_key = match stream.next().await {
        Some(Ok(axum::extract::ws::Message::Text(text))) => {
            let v: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => {
                    let _ = sink.send(axum::extract::ws::Message::Text(
                        r#"{"error":"invalid json"}"#.into()
                    )).await;
                    return;
                }
            };
            match v.get("pair_key").and_then(|k| k.as_str()) {
                Some(k) => k.to_string(),
                None => {
                    let _ = sink.send(axum::extract::ws::Message::Text(
                        r#"{"error":"missing pair_key"}"#.into()
                    )).await;
                    return;
                }
            }
        }
        _ => return,
    };

    // Validate against active pair state
    {
        let mut ps = state.pair_state.lock().await;
        match ps.as_ref() {
            Some(p) if !p.used && p.pair_key == pair_key => {
                // Valid! Mark as used
                ps.as_mut().unwrap().used = true;
            }
            _ => {
                let _ = sink.send(axum::extract::ws::Message::Text(
                    r#"{"error":"invalid or expired pair key"}"#.into()
                )).await;
                return;
            }
        }
    }

    // Generate long-lived key, persist it, and send it back
    let llk = gen_long_lived_key();
    *state.long_lived_key.lock().await = Some(llk.clone());
    if let Some(path) = state.key_path.lock().await.as_ref() {
        control::save_key(path, &llk);
    }

    let resp = serde_json::json!({
        "status": "paired",
        "long_lived_key": llk,
    });
    if sink.send(axum::extract::ws::Message::Text(resp.to_string().into())).await.is_err() {
        return;
    }

    tracing::info!("paired with hermytt (Mode 2)");

    // Upgrade this connection to control channel
    let sink = Arc::new(tokio::sync::Mutex::new(WsSinkAdapter(sink)));
    let stream = WsStreamAdapter(stream);
    let hostname = crate::control::gethostname();
    control::run_control(sink, stream, &state.manager, &hostname).await;
}

// --- Mode 2: /control WS endpoint (reconnect with long-lived key) ---

async fn ws_control(
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_control(socket, state))
}

async fn handle_control(socket: WebSocket, state: Arc<AppState>) {
    let (mut sink, mut stream) = socket.split();

    // Expect first message: {"auth": "long-lived-key"}
    let auth_key = match stream.next().await {
        Some(Ok(axum::extract::ws::Message::Text(text))) => {
            let v: serde_json::Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => return,
            };
            match v.get("auth").and_then(|k| k.as_str()) {
                Some(k) => k.to_string(),
                None => return,
            }
        }
        _ => return,
    };

    // Validate against stored long-lived key
    {
        let llk = state.long_lived_key.lock().await;
        match llk.as_ref() {
            Some(k) if *k == auth_key => {}
            _ => {
                let _ = sink.send(axum::extract::ws::Message::Text(
                    r#"{"error":"unauthorized"}"#.into()
                )).await;
                return;
            }
        }
    }

    let _ = sink.send(axum::extract::ws::Message::Text(
        r#"{"status":"ok"}"#.into()
    )).await;

    tracing::info!("hermytt reconnected (Mode 2)");

    let sink = Arc::new(tokio::sync::Mutex::new(WsSinkAdapter(sink)));
    let stream = WsStreamAdapter(stream);
    let hostname = crate::control::gethostname();
    control::run_control(sink, stream, &state.manager, &hostname).await;
}

// --- Adapters: axum WS ↔ tungstenite Message ---

use tokio_tungstenite::tungstenite::Message as TungMessage;

struct WsSinkAdapter(futures_util::stream::SplitSink<WebSocket, axum::extract::ws::Message>);

impl futures_util::Sink<TungMessage> for WsSinkAdapter {
    type Error = tokio_tungstenite::tungstenite::Error;

    fn poll_ready(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut self.0).poll_ready(cx)
            .map_err(|_| tokio_tungstenite::tungstenite::Error::ConnectionClosed)
    }

    fn start_send(mut self: std::pin::Pin<&mut Self>, item: TungMessage) -> Result<(), Self::Error> {
        let axum_msg = match item {
            TungMessage::Text(t) => axum::extract::ws::Message::Text(t.to_string().into()),
            TungMessage::Binary(b) => axum::extract::ws::Message::Binary(b.to_vec().into()),
            TungMessage::Close(_) => axum::extract::ws::Message::Close(None),
            _ => return Ok(()),
        };
        std::pin::Pin::new(&mut self.0).start_send(axum_msg)
            .map_err(|_| tokio_tungstenite::tungstenite::Error::ConnectionClosed)
    }

    fn poll_flush(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut self.0).poll_flush(cx)
            .map_err(|_| tokio_tungstenite::tungstenite::Error::ConnectionClosed)
    }

    fn poll_close(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        std::pin::Pin::new(&mut self.0).poll_close(cx)
            .map_err(|_| tokio_tungstenite::tungstenite::Error::ConnectionClosed)
    }
}

struct WsStreamAdapter(futures_util::stream::SplitStream<WebSocket>);

impl futures_util::Stream for WsStreamAdapter {
    type Item = Result<TungMessage, tokio_tungstenite::tungstenite::Error>;

    fn poll_next(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        match std::pin::Pin::new(&mut self.0).poll_next(cx) {
            std::task::Poll::Ready(Some(Ok(msg))) => {
                let tung_msg = match msg {
                    axum::extract::ws::Message::Text(t) => TungMessage::Text(t.to_string().into()),
                    axum::extract::ws::Message::Binary(b) => TungMessage::Binary(b.to_vec().into()),
                    axum::extract::ws::Message::Close(_) => TungMessage::Close(None),
                    // Ping/Pong — skip, re-poll
                    _ => {
                        cx.waker().wake_by_ref();
                        return std::task::Poll::Pending;
                    }
                };
                std::task::Poll::Ready(Some(Ok(tung_msg)))
            }
            std::task::Poll::Ready(Some(Err(_))) => std::task::Poll::Ready(None),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}
