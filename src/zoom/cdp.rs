use crate::config::Config;
use crate::zoom::db::ZoomDb;
use crate::zoom::models::{RecordingListResponse, RecordingSummary, ReplayHeader, ZoomCookie};
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
        json!({"includeExtraInfo": true}),
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
    let mut captured_headers: HashMap<RequestKind, HashMap<String, String>> = HashMap::new();
    let mut asset_headers: HashMap<RequestKey, HashMap<String, String>> = HashMap::new();
    let mut asset_urls: HashMap<RequestKey, String> = HashMap::new();
    let mut replay_assets: HashMap<String, ReplayHeader> = HashMap::new();
    let mut automation_triggered = false;
    let mut automation_deadline: Option<Instant> = None;

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
                                    json!({"includeExtraInfo": true}),
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

                                let auto_attach = params
                                    .get("targetInfo")
                                    .and_then(|info| info.get("attached"))
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                if !auto_attach {
                                    send_command(
                                        &mut ws,
                                        &mut next_id,
                                        Some(&session),
                                        "Runtime.runIfWaitingForDebugger",
                                        Value::Object(Default::default()),
                                    )
                                    .await?;
                                }
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
                                            let text_body =
                                                String::from_utf8_lossy(&bytes).to_string();
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
                                        RequestKind::RecordingFile => {}
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
                if scid.is_some() && !cookies.is_empty() && have_required_headers(&captured_headers)
                {
                    if !automation_triggered {
                        if let Err(err) = trigger_download_automation(&mut ws, &mut next_id).await {
                            println!("No se pudo iniciar la automatización de descargas: {}", err);
                        } else {
                            println!(
                                "Automatización de descarga iniciada; espera mientras se clonan los assets..."
                            );
                            automation_triggered = true;
                            automation_deadline = Some(Instant::now() + Duration::from_secs(60));
                        }
                    } else if automation_deadline
                        .map(|deadline| Instant::now() > deadline)
                        .unwrap_or(true)
                    {
                        break;
                    }
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
                                        let map_key = (session_id.clone(), request_id.clone());
                                        if let Some(kind) = classify_request(url) {
                                            if let Some(headers) =
                                                request.get("headers").and_then(|h| h.as_object())
                                            {
                                                merge_headers(&mut captured_headers, kind, headers);
                                            }
                                            if kind.includes_scid() {
                                                if let Some(found_scid) = extract_scid(url) {
                                                    scid = Some(found_scid);
                                                    print!(
                                                        "Se capturó lti_scid: {}. ",
                                                        scid.as_deref().unwrap()
                                                    );
                                                }
                                            }
                                            request_map.insert(map_key.clone(), kind);
                                        } else if is_replay_asset(url) {
                                            let mut headers_map = HashMap::new();
                                            if let Some(headers) =
                                                request.get("headers").and_then(|h| h.as_object())
                                            {
                                                merge_headers_into(&mut headers_map, headers);
                                            }
                                            asset_headers.insert(map_key.clone(), headers_map);
                                            asset_urls.insert(map_key, url.to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    "Network.requestWillBeSentExtraInfo" => {
                        if let Some(params) = payload.get("params") {
                            if let Some(request_id) = params
                                .get("requestId")
                                .and_then(|r| r.as_str())
                                .map(|s| s.to_string())
                            {
                                let map_key = (session_id.clone(), request_id.clone());
                                if let Some(kind) = request_map.get(&map_key).copied() {
                                    if let Some(headers) =
                                        params.get("headers").and_then(|h| h.as_object())
                                    {
                                        merge_headers(&mut captured_headers, kind, headers);
                                    }
                                    if let Some(text) =
                                        params.get("headersText").and_then(|t| t.as_str())
                                    {
                                        merge_raw_headers(&mut captured_headers, kind, text);
                                    }
                                } else if let Some(headers) =
                                    params.get("headers").and_then(|h| h.as_object())
                                {
                                    if let Some(entry) = asset_headers.get_mut(&map_key) {
                                        merge_headers_into(entry, headers);
                                    }
                                    if let Some(text) =
                                        params.get("headersText").and_then(|t| t.as_str())
                                    {
                                        if let Some(entry) = asset_headers.get_mut(&map_key) {
                                            merge_raw_headers_into(entry, text);
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
                                    if matches!(
                                        kind,
                                        RequestKind::RecordingList | RequestKind::MeetingList
                                    ) {
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
                                if asset_headers.contains_key(&map_key) {
                                    if let Some(headers_map) = asset_headers.remove(&map_key) {
                                        if let Some(url) = asset_urls.remove(&map_key) {
                                            let referer = headers_map
                                                .get("referer")
                                                .cloned()
                                                .unwrap_or_default();
                                            if !referer.is_empty()
                                                && !replay_assets.contains_key(&referer)
                                            {
                                                println!(
                                                    "Capturada URL de descarga para {}",
                                                    referer
                                                );
                                                replay_assets.insert(
                                                    referer.clone(),
                                                    ReplayHeader {
                                                        download_url: url,
                                                        headers: headers_map,
                                                    },
                                                );
                                            }
                                        }
                                    }
                                }
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
    if !have_required_headers(&captured_headers) {
        return Err("no se capturaron encabezados de la petición de recordings; espera a que la tabla cargue completamente".into());
    }

    opts.db.save_scid(opts.course_id, &scid)?;
    opts.db.replace_cookies(&cookies)?;
    for (kind, headers_map) in &captured_headers {
        let mut flattened: Vec<(String, String)> = headers_map
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        flattened.sort_by(|a, b| a.0.cmp(&b.0));
        opts.db
            .save_request_headers(opts.course_id, kind.request_path(), &flattened)?;
    }
    if !replay_assets.is_empty() {
        opts.db
            .save_replay_headers(opts.course_id, &replay_assets)?;
    }
    if let Some(ref listing) = listing {
        opts.db.save_meetings(opts.course_id, listing)?;
    }

    println!(
        "Listo. Guardado scid={}, {} cookies, {} encabezados API, {} assets y {} reuniones.",
        scid,
        cookies.len(),
        captured_headers.len(),
        replay_assets.len(),
        meetings.len()
    );
    if replay_assets.is_empty() {
        println!(
            "(No se capturaron descargas MP4; presiona el botón 'Descargar' en la reproducción durante el sniff para clonar esas cabeceras.)"
        );
    }
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

fn merge_headers(
    store: &mut HashMap<RequestKind, HashMap<String, String>>,
    kind: RequestKind,
    headers: &serde_json::Map<String, Value>,
) {
    let entry = store.entry(kind).or_insert_with(HashMap::new);
    merge_headers_into(entry, headers);
}

fn merge_raw_headers(
    store: &mut HashMap<RequestKind, HashMap<String, String>>,
    kind: RequestKind,
    headers_text: &str,
) {
    let entry = store.entry(kind).or_insert_with(HashMap::new);
    merge_raw_headers_into(entry, headers_text);
}

fn merge_headers_into(
    target: &mut HashMap<String, String>,
    headers: &serde_json::Map<String, Value>,
) {
    for (name, value) in headers.iter() {
        let key = name.to_ascii_lowercase();
        if should_skip_header(&key) {
            continue;
        }
        let text = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            other => other.to_string(),
        };
        target.insert(key, text);
    }
}

fn merge_raw_headers_into(target: &mut HashMap<String, String>, headers_text: &str) {
    for line in headers_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.contains(':') {
            continue;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            let key = name.trim().to_ascii_lowercase();
            if should_skip_header(&key) {
                continue;
            }
            target.insert(key, value.trim().to_string());
        }
    }
}

fn have_required_headers(captured_headers: &HashMap<RequestKind, HashMap<String, String>>) -> bool {
    captured_headers.contains_key(&RequestKind::RecordingList)
}

fn should_skip_header(name: &str) -> bool {
    matches!(
        name,
        _ if name.starts_with(':')
            || name == "content-length"
            || name == "accept-encoding"
            || name == "transfer-encoding"
        || name == "connection"
        || name == "upgrade"
    )
}

fn is_replay_asset(url: &str) -> bool {
    if let Ok(parsed) = Url::parse(url) {
        let host_ok = parsed
            .host_str()
            .map(|host| host.ends_with("zoom.us"))
            .unwrap_or(false);
        let path = parsed.path().to_ascii_lowercase();
        host_ok && path.ends_with(".mp4")
    } else {
        false
    }
}

async fn trigger_download_automation(
    ws: &mut tokio_tungstenite::WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: &mut i64,
) -> Result<(), Box<dyn std::error::Error>> {
    let script = r#"
        (async () => {
            const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
            const clickTab = () => {
                const tabs = Array.from(document.querySelectorAll('div[role="tab"]'));
                const tab = tabs.find(el => (el.textContent || '').trim().toLowerCase() === 'cloud recordings');
                if (tab && !tab.classList.contains('ant-tabs-tab-active')) {
                    tab.click();
                }
            };
            clickTab();
            await sleep(1200);
            const anchors = Array.from(document.querySelectorAll('.ant-table-body tbody tr td:first-child a'))
                .map(a => a.href)
                .filter(Boolean);
            let opened = 0;
            for (const href of anchors) {
                try {
                    const child = window.open(href, '_blank', 'noopener=yes');
                    if (!child) {
                        continue;
                    }
                    opened++;
                    await new Promise(resolve => {
                        let attempts = 0;
                        const tick = () => {
                            attempts++;
                            try {
                                if (!child || child.closed) {
                                    resolve();
                                    return;
                                }
                                const doc = child.document;
                                if (!doc) {
                                    if (attempts < 80) {
                                        setTimeout(tick, 300);
                                    } else {
                                        try { child.close(); } catch (err) {}
                                        resolve();
                                    }
                                    return;
                                }
                                const candidates = Array.from(doc.querySelectorAll('a,button'));
                                const target = candidates.find(el => {
                                    const text = (el.textContent || '').toLowerCase();
                                    const qa = (el.getAttribute('data-qa') || '').toLowerCase();
                                    return text.includes('download') || text.includes('descargar') || qa.includes('download');
                                });
                                if (target) {
                                    target.click();
                                    setTimeout(() => {
                                        try { child.close(); } catch (err) {}
                                        resolve();
                                    }, 700);
                                    return;
                                }
                            } catch (err) {}
                            if (attempts < 80) {
                                setTimeout(tick, 300);
                            } else {
                                try { child.close(); } catch (err) {}
                                resolve();
                            }
                        };
                        tick();
                    });
                    await sleep(1500);
                } catch (err) {
                    console.warn('automation error', err);
                }
            }
            return opened;
        })();
    "#;

    runtime_evaluate_fire(ws, next_id, None, script).await
}

async fn runtime_evaluate_fire(
    ws: &mut tokio_tungstenite::WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: &mut i64,
    session: Option<&str>,
    expression: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let params = json!({
        "expression": expression,
        "awaitPromise": false,
        "userGesture": true,
    });
    let _ = send_command(ws, next_id, session, "Runtime.evaluate", params).await?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RequestKind {
    RecordingList,
    MeetingList,
    RecordingFile,
}

impl RequestKind {
    fn includes_scid(self) -> bool {
        matches!(
            self,
            RequestKind::RecordingList | RequestKind::MeetingList | RequestKind::RecordingFile
        )
    }

    fn request_path(self) -> &'static str {
        match self {
            RequestKind::RecordingList => "/api/v1/lti/rich/recording/COURSE",
            RequestKind::MeetingList => "/api/v1/lti/rich/meeting",
            RequestKind::RecordingFile => "/api/v1/lti/rich/recording/file",
        }
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
    } else if url.contains("/api/v1/lti/rich/recording/file") {
        Some(RequestKind::RecordingFile)
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
