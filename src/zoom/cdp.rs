use crate::config::Config;
use crate::zoom::db::ZoomDb;
use crate::zoom::models::{RecordingListResponse, RecordingSummary, ZoomCookie};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message, MaybeTlsStream};
use url::Url;

const ZOOM_BASE: &str = "https://applications.zoom.us";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PendingKey {
    id: i64,
    session: Option<String>,
}

impl PendingKey {
    fn new(id: i64, session: Option<&str>) -> Self {
        Self {
            id,
            session: session.map(|s| s.to_string()),
        }
    }
}

pub struct SniffOptions<'a> {
    pub course_id: u64,
    pub debug_port: u16,
    pub keep_tab: bool,
    pub config: &'a Config,
    pub db: &'a ZoomDb,
}

pub async fn sniff_cdp(opts: SniffOptions<'_>) -> Result<(), Box<dyn std::error::Error>> {
    let target_url = Url::parse(&opts.config.canvas.base_url)?.join(&format!(
        "courses/{}/external_tools/{}",
        opts.course_id, opts.config.zoom.external_tool_id
    ))?;

    let client = reqwest::Client::new();
    let new_endpoint = format!(
        "http://127.0.0.1:{}/json/new?{}",
        opts.debug_port,
        target_url.as_str()
    );

    let response = client.put(&new_endpoint).send().await?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        return Err(format!(
            "no se pudo crear la pestaña CDP (status {}): {}",
            status, text
        )
        .into());
    }
    let tab_resp: Value = serde_json::from_str(&text)
        .map_err(|e| format!("respuesta inesperada del endpoint /json/new: {e}: {text}"))?;
    let target_id = tab_resp
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("missing target id from CDP new tab response")?
        .to_string();
    let ws_url = tab_resp
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .ok_or("missing webSocketDebuggerUrl from CDP response")?
        .to_string();

    println!(
        "Conectado al DevTools de Chromium. Se abrirá una pestaña hacia:\n  {}\nSi Canvas requiere SSO, completa el flujo en esa pestaña.",
        target_url
    );
    println!(
        "Esperando a que la pestaña cargue la tabla de Zoom. Esto puede tardar hasta 2 minutos..."
    );

    let (mut ws, _) = connect_async(&ws_url).await?;

    let mut next_id: i64 = 1;

    send_command(
        &mut ws,
        &mut next_id,
        None,
        "Target.setAutoAttach",
        json!({
            "autoAttach": true,
            "waitForDebuggerOnStart": false,
            "flatten": true
        }),
    )
    .await?;

    send_command(
        &mut ws,
        &mut next_id,
        None,
        "Page.enable",
        Value::Object(Default::default()),
    )
    .await?;
    send_command(
        &mut ws,
        &mut next_id,
        None,
        "Network.enable",
        Value::Object(Default::default()),
    )
    .await?;

    send_command(
        &mut ws,
        &mut next_id,
        None,
        "Page.navigate",
        json!({ "url": target_url.as_str() }),
    )
    .await?;

    let mut pending_bodies: HashMap<PendingKey, PendingResponse> = HashMap::new();
    type RequestKey = (Option<String>, String);
    let mut request_map: HashMap<RequestKey, RequestKind> = HashMap::new();

    let mut scid: Option<String> = None;
    let mut listing: Option<RecordingListResponse> = None;
    let mut meetings: Vec<RecordingSummary> = Vec::new();
    let mut cookies: Vec<ZoomCookie> = Vec::new();

    let deadline = Instant::now() + Duration::from_secs(120);

    while Instant::now() < deadline {
        if let Some(msg) = ws.next().await {
            let msg = msg?;
            let text = match msg {
                Message::Text(t) => t,
                Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                _ => continue,
            };
            let payload: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let session_id = payload
                .get("sessionId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            if let Some(method) = payload.get("method").and_then(|m| m.as_str()) {
                if method == "Target.attachedToTarget" {
                    if let Some(params) = payload.get("params") {
                        if let Some(session) = params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                        {
                            let target_type = params
                                .get("targetInfo")
                                .and_then(|info| info.get("type"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            if matches!(target_type, "iframe" | "page" | "worker") {
                                send_command(
                                    &mut ws,
                                    &mut next_id,
                                    Some(&session),
                                    "Network.enable",
                                    Value::Object(Default::default()),
                                )
                                .await?;
                                send_command(
                                    &mut ws,
                                    &mut next_id,
                                    Some(&session),
                                    "Page.enable",
                                    Value::Object(Default::default()),
                                )
                                .await?;
                            }
                        }
                    }
                    continue;
                }
            }

            if let Some(id) = payload.get("id").and_then(|v| v.as_i64()) {
                let pending_key = PendingKey::new(id, session_id.as_deref());
                if let Some(kind) = pending_bodies.remove(&pending_key) {
                    match kind {
                        PendingResponse::Body(kind) => {
                            if let Some(result) = payload.get("result") {
                                if let Some(body) = result.get("body").and_then(|b| b.as_str()) {
                                    let is_base64 = result
                                        .get("base64Encoded")
                                        .and_then(|b| b.as_bool())
                                        .unwrap_or(false);
                                    let bytes = if is_base64 {
                                        BASE64.decode(body)?
                                    } else {
                                        body.as_bytes().to_vec()
                                    };
                                    match kind {
                                        RequestKind::RecordingList | RequestKind::MeetingList => {
                                            let text_body = String::from_utf8_lossy(&bytes).to_string();
                                            if listing.is_none() {
                                                match serde_json::from_str::<RecordingListResponse>(
                                                    &text_body,
                                                ) {
                                                    Ok(resp) => {
                                                        if let Some(result) = &resp.result {
                                                            if let Some(list) = &result.list {
                                                                meetings = list.clone();
                                                            }
                                                        }
                                                        listing = Some(resp);
                                                        println!(
                                                            "Capturada respuesta de Zoom ({:?}).",
                                                            kind
                                                        );
                                                    }
                                                    Err(err) => {
                                                        println!(
                                                            "No se pudo parsear respuesta de Zoom ({:?}): {}",
                                                            kind, err
                                                        );
                                                    }
                                                }
                                            }
                                            request_cookies_if_needed(
                                                &mut ws,
                                                &mut next_id,
                                                &mut pending_bodies,
                                                session_id.as_deref(),
                                            )
                                            .await?;
                                        }
                                    }
                                }
                            }
                        }
                        PendingResponse::Cookies => {
                            if let Some(result) = payload.get("result") {
                                if let Some(items) =
                                    result.get("cookies").and_then(|c| c.as_array())
                                {
                                    cookies = items
                                        .iter()
                                        .filter_map(|item| parse_cookie(item))
                                        .collect();
                                    println!(
                                        "Se capturaron {} cookies de applications.zoom.us",
                                        cookies.len()
                                    );
                                }
                            }
                        }
                    }
                }
                if scid.is_some() && !cookies.is_empty() {
                    break;
                }
                continue;
            }

            if let Some(method) = payload.get("method").and_then(|m| m.as_str()) {
                match method {
                    "Network.requestWillBeSent" => {
                        if let Some(params) = payload.get("params") {
                            if let Some(request_id) = params
                                .get("requestId")
                                .and_then(|r| r.as_str())
                                .map(|s| s.to_string())
                            {
                                if let Some(request) = params.get("request") {
                                    if let Some(url) = request.get("url").and_then(|u| u.as_str()) {
                                        // println!("[CDP] request {} -> {}", request_id, url);
                                        if let Some(kind) = classify_request(url) {
                                            if kind.includes_scid() {
                                                if let Some(found_scid) = extract_scid(url) {
                                                    scid = Some(found_scid);
                                                }
                                            }
                                            request_map.insert(
                                                (session_id.clone(), request_id.clone()),
                                                kind,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    "Network.loadingFinished" => {
                        if let Some(params) = payload.get("params") {
                            if let Some(request_id) = params
                                .get("requestId")
                                .and_then(|r| r.as_str())
                                .map(|s| s.to_string())
                            {
                                let map_key = (session_id.clone(), request_id.clone());
                                if let Some(kind) = request_map.get(&map_key) {
                                    if matches!(kind, RequestKind::RecordingList | RequestKind::MeetingList) {
                                        let cmd_id = send_command(
                                            &mut ws,
                                            &mut next_id,
                                            session_id.as_deref(),
                                            "Network.getResponseBody",
                                            json!({"requestId": request_id}),
                                        )
                                        .await?;
                                        pending_bodies.insert(
                                            PendingKey::new(cmd_id, session_id.as_deref()),
                                            PendingResponse::Body(*kind),
                                        );
                                    }
                                }
                                request_map.remove(&map_key);
                            }
                        }
                    }
                    _ => {}
                }
            }
        } else {
            break;
        }
    }

    if !opts.keep_tab {
        let close_endpoint = format!(
            "http://127.0.0.1:{}/json/close/{}",
            opts.debug_port, target_id
        );
        let _ = client.get(close_endpoint).send().await;
    }

    let scid = scid.ok_or(
        "no se pudo capturar lti_scid; asegúrate de abrir la pestaña y de que la tabla cargue",
    )?;
    if cookies.is_empty() {
        return Err("no se capturaron cookies para applications.zoom.us".into());
    }

    opts.db.save_scid(opts.course_id, &scid)?;
    opts.db.replace_cookies(&cookies)?;
    if let Some(ref listing) = listing {
        opts.db.save_meetings(opts.course_id, listing)?;
    }

    println!(
        "Listo. Guardado scid={}, {} cookies y {} reuniones.",
        scid,
        cookies.len(),
        meetings.len()
    );
    println!(
        "Ahora puedes ejecutar 'u_crawler zoom list --course-id {}'",
        opts.course_id
    );

    Ok(())
}

fn parse_cookie(value: &Value) -> Option<ZoomCookie> {
    Some(ZoomCookie {
        domain: value.get("domain")?.as_str()?.to_string(),
        name: value.get("name")?.as_str()?.to_string(),
        value: value.get("value")?.as_str()?.to_string(),
        path: value.get("path")?.as_str()?.to_string(),
        expires: value.get("expires").and_then(|v| v.as_i64()),
        secure: value
            .get("secure")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        http_only: value
            .get("httpOnly")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestKind {
    RecordingList,
    MeetingList,
}

impl RequestKind {
    fn includes_scid(self) -> bool {
        matches!(self, RequestKind::RecordingList | RequestKind::MeetingList)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingResponse {
    Body(RequestKind),
    Cookies,
}

async fn send_command(
    ws: &mut tokio_tungstenite::WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: &mut i64,
    session_id: Option<&str>,
    method: &str,
    params: Value,
) -> Result<i64, Box<dyn std::error::Error>> {
    let id = *next_id;
    *next_id += 1;
    let mut message = json!({
        "id": id,
        "method": method,
        "params": params,
    });
    if let Some(session) = session_id {
        message["sessionId"] = Value::String(session.to_string());
    }
    ws.send(Message::Text(message.to_string())).await?;
    Ok(id)
}

fn classify_request(url: &str) -> Option<RequestKind> {
    if url.contains("/api/v1/lti/rich/recording/COURSE") {
        Some(RequestKind::RecordingList)
    } else if url.contains("/api/v1/lti/rich/meeting/") {
        Some(RequestKind::MeetingList)
    } else if url.contains("/locale/timezones") {
        Some(RequestKind::MeetingList)
    } else {
        None
    }
}

fn extract_scid(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    for (k, v) in parsed.query_pairs() {
        if k == "lti_scid" {
            return Some(v.into_owned());
        }
    }
    None
}

async fn request_cookies_if_needed(
    ws: &mut tokio_tungstenite::WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: &mut i64,
    pending: &mut HashMap<PendingKey, PendingResponse>,
    session: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let already_pending = pending
        .iter()
        .any(|(k, v)| k.session.as_deref() == session && matches!(v, PendingResponse::Cookies));
    if already_pending {
        return Ok(());
    }
    let cmd_id = send_command(
        ws,
        next_id,
        session,
        "Network.getCookies",
        json!({"urls": [ZOOM_BASE]}),
    )
    .await?;
    pending.insert(PendingKey::new(cmd_id, session), PendingResponse::Cookies);
    Ok(())
}
