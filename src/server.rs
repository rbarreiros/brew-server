use crate::{router, state::{AppState, Client}};
use anyhow::Context;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        FromRequestParts, Path, State,
    },
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use futures_util::{SinkExt, StreamExt};
use std::{collections::HashMap, sync::Arc, time::Instant};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

pub async fn run(state: Arc<AppState>) -> anyhow::Result<()> {
    let base = normalized_path(&state.config.websocket_path);
    let session_route = format!("{}/session/{{token}}", base);
    let mut app = Router::new().route(&base, get(brew_discovery));
    let slash = format!("{}/", base);
    if slash != base { app = app.route(&slash, get(brew_discovery)); }
    let app = app
        .route(&session_route, get(brew_session_endpoint))
        .route("/healthz", get(|| async { "ok\n" }))
        .with_state(state.clone());

    if state.config.tls.enabled {
        let tls = &state.config.tls;
        let rustls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            &tls.cert_path,
            &tls.key_path,
        )
        .await
        .with_context(|| {
            format!(
                "loading TLS cert {} and key {}",
                tls.cert_path.display(),
                tls.key_path.display()
            )
        })?;
        info!(listen=%state.config.listen, websocket_path=%base, auth=state.config.auth.enabled, tls=true, "Brew server listening (TLS)");
        axum_server::bind_rustls(state.config.listen, rustls_config)
            .serve(app.into_make_service())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(state.config.listen).await?;
        info!(listen=%state.config.listen, websocket_path=%base, auth=state.config.auth.enabled, tls=false, "Brew server listening");
        axum::serve(listener, app).await?;
    }
    Ok(())
}

fn normalized_path(path: &str) -> String {
    let mut p = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    while p.len() > 1 && p.ends_with('/') { p.pop(); }
    p
}

async fn brew_discovery(State(state): State<Arc<AppState>>, request: Request<axum::body::Body>) -> Response {
    state.purge_ephemeral().await;
    let (mut parts, _body) = request.into_parts();
    let request_uri = parts.uri.path().to_string();
    let is_upgrade = parts.headers.get(header::UPGRADE).and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket")).unwrap_or(false);

    // Direct WS mode remains available only when Digest is disabled.
    if is_upgrade && !state.config.auth.enabled {
        return upgrade_from_parts(state, &mut parts).await;
    }

    if state.config.auth.enabled {
        if !verify_digest(&state, &parts.headers, "GET", &request_uri).await {
            return digest_challenge(&state).await;
        }
        let token = Uuid::new_v4().simple().to_string();
        state.inner.write().await.auth_sessions.insert(token.clone(), Instant::now());
        let path = format!("{}/session/{}", normalized_path(&state.config.websocket_path), token);
        return (StatusCode::OK, [(header::CONTENT_TYPE, "text/plain")], path).into_response();
    }

    (StatusCode::OK, [(header::CONTENT_TYPE, "text/plain")], normalized_path(&state.config.websocket_path)).into_response()
}

async fn brew_session_endpoint(
    State(state): State<Arc<AppState>>,
    Path(token): Path<String>,
    request: Request<axum::body::Body>,
) -> Response {
    state.purge_ephemeral().await;
    if !state.config.auth.enabled { return StatusCode::NOT_FOUND.into_response(); }
    let valid = state.inner.read().await.auth_sessions.contains_key(&token);
    if !valid { return StatusCode::UNAUTHORIZED.into_response(); }

    let (mut parts, _body) = request.into_parts();
    let is_upgrade = parts.headers.get(header::UPGRADE).and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket")).unwrap_or(false);
    if !is_upgrade { return StatusCode::BAD_REQUEST.into_response(); }

    // Session URLs are single-use. The established WebSocket is the authenticated session.
    state.inner.write().await.auth_sessions.remove(&token);
    upgrade_from_parts(state, &mut parts).await
}

async fn upgrade_from_parts(state: Arc<AppState>, parts: &mut axum::http::request::Parts) -> Response {
    match WebSocketUpgrade::from_request_parts(parts, &state).await {
        Ok(ws) => {
            let requested = parts.headers.get(header::SEC_WEBSOCKET_PROTOCOL).and_then(|v| v.to_str().ok()).unwrap_or_default();
            debug!(requested_subprotocol=requested, "WebSocket upgrade request");
            let protocol = state.config.websocket_subprotocol.clone();
            ws.protocols([protocol]).on_upgrade(move |socket| client_session(state, socket)).into_response()
        }
        Err(rejection) => rejection.into_response(),
    }
}

async fn digest_challenge(state: &Arc<AppState>) -> Response {
    let nonce = Uuid::new_v4().simple().to_string();
    state.inner.write().await.digest_nonces.insert(nonce.clone(), Instant::now());
    let opaque = md5_hex(&format!("{}:brew", state.config.auth.realm));
    let challenge = format!("Digest realm=\"{}\", nonce=\"{}\", qop=\"auth\", opaque=\"{}\"",
        state.config.auth.realm, nonce, opaque);
    let mut response = StatusCode::UNAUTHORIZED.into_response();
    response.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_str(&challenge).unwrap());
    response
}

fn md5_hex(input: &str) -> String { format!("{:x}", md5::compute(input.as_bytes())) }

fn parse_digest(header_value: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let value = header_value.trim().strip_prefix("Digest ").unwrap_or(header_value.trim());
    for part in value.split(',') {
        if let Some((k, v)) = part.trim().split_once('=') {
            out.insert(k.trim().to_ascii_lowercase(), v.trim().trim_matches('"').to_string());
        }
    }
    out
}

async fn verify_digest(state: &Arc<AppState>, headers: &HeaderMap, method: &str, expected_uri: &str) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else { return false; };
    if !value.starts_with("Digest ") { return false; }
    let p = parse_digest(value);
    let Some(username) = p.get("username") else { return false; };
    let Some(password) = state.config.auth.users.get(username) else { return false; };
    let Some(nonce) = p.get("nonce") else { return false; };
    if !state.inner.read().await.digest_nonces.contains_key(nonce) { return false; }
    let realm = p.get("realm").map(String::as_str).unwrap_or("");
    if realm != state.config.auth.realm { return false; }
    let uri = p.get("uri").map(String::as_str).unwrap_or("");
    if uri != expected_uri { return false; }
    let Some(received) = p.get("response") else { return false; };

    let ha1 = md5_hex(&format!("{}:{}:{}", username, realm, password));
    let ha2 = md5_hex(&format!("{}:{}", method, uri));
    let expected = if p.get("qop").map(|s| s.contains("auth")).unwrap_or(false) {
        let nc = p.get("nc").map(String::as_str).unwrap_or("");
        let cnonce = p.get("cnonce").map(String::as_str).unwrap_or("");
        md5_hex(&format!("{}:{}:{}:{}:auth:{}", ha1, nonce, nc, cnonce, ha2))
    } else {
        md5_hex(&format!("{}:{}:{}", ha1, nonce, ha2))
    };
    let ok = expected.eq_ignore_ascii_case(received);
    if ok { state.inner.write().await.digest_nonces.remove(nonce); }
    ok
}

async fn client_session(state: Arc<AppState>, socket: WebSocket) {
    let id = Uuid::new_v4();
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    state.inner.write().await.clients.insert(id, Client { tx });
    info!(%id, "BlueStation connected");

    let writer = tokio::spawn(async move {
        while let Some(packet) = rx.recv().await {
            if ws_tx.send(Message::Binary(packet.into())).await.is_err() { break; }
        }
    });

    while let Some(item) = ws_rx.next().await {
        match item {
            Ok(Message::Binary(data)) => router::handle_packet(state.clone(), id, data.to_vec()).await,
            Ok(Message::Ping(_)) => debug!(%id, "ping received"),
            Ok(Message::Pong(_)) => debug!(%id, "pong received"),
            Ok(Message::Close(_)) => break,
            Ok(Message::Text(_)) => warn!(%id, "text WebSocket message ignored"),
            Err(e) => { warn!(%id, error=%e, "WebSocket receive error"); break; }
        }
    }

    writer.abort();
    state.cleanup_client(id).await;
    info!(%id, "BlueStation disconnected");
}
