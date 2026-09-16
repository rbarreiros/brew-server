use crate::{
    control::{self, ControlCommand, SendError},
    state::AppState,
    telemetry::TelemetryBts,
};
use anyhow::Context;
use axum::{
    extract::{Path, State, ws::{Message, WebSocket, WebSocketUpgrade}},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use axum::extract::Request;
use base64::Engine;
use std::{collections::HashMap, sync::Arc};

/// Runs the monitoring dashboard on its own listener (separate from the Brew
/// API). Gated behind optional HTTP Basic auth and optional TLS via the
/// `[dashboard]` config section. Returns immediately if disabled.
pub async fn run(state: Arc<AppState>) -> anyhow::Result<()> {
    let cfg = &state.config.dashboard;
    if !cfg.enabled {
        return Ok(());
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/calls", get(calls_page))
        .route("/sds", get(sds_page))
        .route("/telemetry-sds", get(telemetry_sds_page))
        .route("/registrations", get(registrations_page))
        .route("/map", get(map_page))
        .route("/api/status", get(snapshot))
        .route("/api/live", get(live))
        .route("/api/telemetry", get(telemetry_snapshot))
        .route("/api/registrations", get(registration_log))
        .route("/api/positions", get(positions_snapshot))
        .route("/api/control", get(control_list))
        .route("/api/control/{id}", axum::routing::post(control_command))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_basic))
        .with_state(state.clone());

    if cfg.tls.enabled {
        let rustls_config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            &cfg.tls.cert_path,
            &cfg.tls.key_path,
        )
        .await
        .with_context(|| {
            format!(
                "loading dashboard TLS cert {} and key {}",
                cfg.tls.cert_path.display(),
                cfg.tls.key_path.display()
            )
        })?;
        tracing::info!(listen=%cfg.listen, auth=!cfg.users.is_empty(), tls=true, "dashboard listening (TLS)");
        axum_server::bind_rustls(cfg.listen, rustls_config)
            .serve(app.into_make_service())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(cfg.listen).await?;
        tracing::info!(listen=%cfg.listen, auth=!cfg.users.is_empty(), tls=false, "dashboard listening");
        axum::serve(listener, app).await?;
    }
    Ok(())
}

/// HTTP Basic auth guard for every dashboard route (including the `/api/live`
/// WebSocket upgrade, which browsers authenticate with a normal Authorization
/// header on the handshake). No configured users means auth is disabled.
async fn require_basic(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let users = &state.config.dashboard.users;
    if users.is_empty() || basic_ok(users, request.headers()) {
        return next.run(request).await;
    }
    basic_challenge(&state.config.dashboard.realm)
}

fn basic_ok(users: &HashMap<String, String>, headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else { return false };
    let Some(b64) = value.strip_prefix("Basic ") else { return false };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(b64) else { return false };
    let Ok(text) = String::from_utf8(decoded) else { return false };
    let Some((user, pass)) = text.split_once(':') else { return false };
    users.get(user).map(|p| p == pass).unwrap_or(false)
}

fn basic_challenge(realm: &str) -> Response {
    let mut response = StatusCode::UNAUTHORIZED.into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_str(&format!("Basic realm=\"{realm}\"")).unwrap(),
    );
    response
}

/// The main dashboard HTML with the shared stylesheet substituted in, built
/// once on first access.
static INDEX_HTML: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| HTML.replace("__STYLE__", STYLE));

pub async fn index() -> Html<&'static str> { Html(INDEX_HTML.as_str()) }

/// Builds a standalone, auto-refreshing log page for one table. `endpoint` is
/// the JSON API the page polls; `extract_js` is a JS expression that, given the
/// parsed response bound to `d`, yields the array of items to paginate;
/// `row_js` renders one item to a `<tr>`; `columns` are the table headers.
fn log_page(title: &str, endpoint: &str, extract_js: &str, row_js: &str, columns: &[&str], per_page: usize, empty_msg: &str) -> String {
    let headers: String = columns.iter().map(|c| format!("<th>{c}</th>")).collect();
    let colspan = columns.len();
    format!(r#"<!doctype html><html><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>{title} - TETRA Network</title>{style}</head><body><header><h1>{title}</h1><div><span class=live></span><span id=status>Live</span></div></header><main class=wrap>
<p><a class=backlink href="/">&larr; Back to dashboard</a></p>
<section class=panel><table><thead><tr>{headers}</tr></thead><tbody id=log></tbody></table><div class=pager id=log-pager></div></section>
</main><script>
const $=id=>document.getElementById(id);const dt=x=>new Date(x).toLocaleTimeString();const dur=(a,b)=>Math.max(0,Math.floor(((b||Date.now())-a)/1000))+'s';const esc=s=>String(s??'').replace(/[&<>]/g,c=>({{'&':'&amp;','<':'&lt;','>':'&gt;'}}[c]));
const pages={{}};
function renderPaged(bodyId,items,rowFn,perPage,colspan,emptyMsg){{
  const body=$(bodyId),pager=$(bodyId+'-pager');
  const total=items.length,pageCount=Math.max(1,Math.ceil(total/perPage));
  if(pages[bodyId]==null)pages[bodyId]=0;
  if(pages[bodyId]>pageCount-1)pages[bodyId]=pageCount-1;
  if(pages[bodyId]<0)pages[bodyId]=0;
  const page=pages[bodyId],start=page*perPage;
  const slice=items.slice(start,start+perPage);
  body.innerHTML=slice.map(rowFn).join('')||`<tr><td colspan=${{colspan}} class=muted>${{emptyMsg}}</td></tr>`;
  if(pager){{
    if(total<=perPage){{pager.innerHTML='';}}
    else{{
      const from=start+1,to=start+slice.length;
      pager.innerHTML=`<button data-pg="${{bodyId}}" data-dir="-1"${{page<=0?' disabled':''}}>&larr; Prev</button>`+
        `<span class=pginfo>${{from}}\u2013${{to}} of ${{total}} (page ${{page+1}}/${{pageCount}})</span>`+
        `<button data-pg="${{bodyId}}" data-dir="1"${{page>=pageCount-1?' disabled':''}}>Next &rarr;</button>`;
    }}
  }}
}}
let last=[];
document.addEventListener('click',e=>{{const b=e.target.closest('button[data-pg]');if(!b)return;pages['log']=(pages['log']||0)+parseInt(b.getAttribute('data-dir'),10);draw();}});
function draw(){{renderPaged('log',last,x=>{row_js},{per_page},{colspan},'{empty_msg}');}}
async function load(){{try{{const d=await(await fetch('{endpoint}')).json();last={extract_js};draw();$('status').textContent='Live';}}catch(e){{$('status').textContent='Disconnected';}}}}
load();setInterval(load,2000);
</script></body></html>"#,
        title = title, style = STYLE, headers = headers, colspan = colspan,
        row_js = row_js, per_page = per_page, empty_msg = empty_msg,
        endpoint = endpoint, extract_js = extract_js,
    )
}

static CALLS_HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| log_page(
    "Recent calls", "/api/status", "d.recent_calls",
    "`<tr><td>${esc(x.kind)}</td><td>${x.source}</td><td>${x.destination}</td><td>${dt(x.started_at_ms)}</td><td>${dur(x.started_at_ms,x.ended_at_ms)}</td><td>${x.voice_frames}</td></tr>`",
    &["Type", "From", "To", "Start", "Duration", "Frames"], 10, "No completed calls",
));

static SDS_HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| log_page(
    "Recent SDS", "/api/status", "d.recent_sds",
    "`<tr><td>${dt(x.at_ms)}</td><td>${x.source}</td><td>${x.destination}</td><td>${x.reports}</td><td class=muted>${String(x.uuid).slice(0,8)}</td></tr>`",
    &["Time", "From", "To", "Reports", "UUID"], 10, "No SDS yet",
));

static TELEMETRY_SDS_HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| log_page(
    "Telemetry SDS Log", "/api/telemetry",
    "d.flatMap(s=>(s.recent_sds_out||[]).map(x=>({...x,bts:s.id}))).sort((a,b)=>b.at_ms-a.at_ms).slice(0,50)",
    "`<tr><td>${dt(x.at_ms)}</td><td>${esc(x.bts)}</td><td>${esc(x.direction)}</td><td>${x.source_issi}</td><td>${x.dest_issi}${x.is_group?' (grp)':''}</td><td>${x.protocol_id===10?'<span class=\"badge badge-pos\">\\uD83D\\uDCCD Position</span>':`<span class=\"badge badge-sds\">SDS<\\/span> <span class=muted>pid ${x.protocol_id}<\\/span>`}</td><td>${x.protocol_id===10&&!(x.text||'').trim()?'<span class=pos-undec>binary LIP (undecoded)<\\/span>':esc(x.text)}</td></tr>`",
    &["Time", "BTS", "Dir", "From", "To", "Type", "Text"], 5, "No telemetry SDS yet",
));

static REGISTRATIONS_HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| log_page(
    "Mobile Station Registrations", "/api/registrations",
    "d",
    "`<tr><td>${dt(x.at_ms)}</td><td>${esc(x.bts)}</td><td>${x.issi}</td><td>${x.kind==='register'?'<span class=\"badge badge-reg-in\">Registered</span>':x.kind==='deregister'?'<span class=\"badge badge-reg-out\">Deregistered</span>':'<span class=\"badge badge-reg-timeout\">Timed out</span>'}</td></tr>`",
    &["Time", "BTS", "ISSI", "Event"], 15, "No registration events yet",
));

/// Standalone map page. Plots the latest decoded MS positions on an
/// OpenStreetMap base layer using Leaflet (loaded from unpkg CDN). Positions
/// come only from *textual* beacons; the page explains that binary LIP is not
/// yet decoded so an empty map is not mistaken for a bug.
static MAP_HTML: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| format!(r#"<!doctype html><html><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>MS Map - TETRA Network</title>
<link rel="stylesheet" href="https://unpkg.com/leaflet@1.9.4/dist/leaflet.css"/>
{style}
<style>#map{{height:70vh;border:1px solid #203047;border-radius:12px}}.map-note{{font-size:12px;color:#8fa2b8;margin-top:10px}}.leaflet-popup-content{{color:#0d1826}}</style>
</head><body><header><h1>MS MAP</h1><div><span class=live></span><span id=status>Live</span></div></header><main class=wrap>
<p><a class=backlink href="/">&larr; Back to dashboard</a></p>
<section class=panel><h2>Mobile station positions</h2><div id=map></div>
<div class=map-note id=note>Loading positions&hellip;</div>
<div class=map-note>Positions come from decoded LIP (binary short &amp; long reports) and textual beacons over the Brew channel. Radios that beacon but can't be plotted (no GPS fix, or coordinates not relayed to this server) are listed below.</div>
</section>
<section class=panel><h2>Beaconing but not plottable</h2><div id=undecoded class=reg-list><span class=muted>None</span></div>
<div class=map-note>These subscribers sent a position beacon that carried no usable coordinates &mdash; typically no GPS fix yet, or a locally-delivered beacon whose bytes never reach this server.</div>
</section>
</main>
<script src="https://unpkg.com/leaflet@1.9.4/dist/leaflet.js"></script>
<script>
const $=id=>document.getElementById(id);
const esc=s=>String(s??'').replace(/[&<>]/g,c=>({{'&':'&amp;','<':'&lt;','>':'&gt;'}}[c]));
const map=L.map('map').setView([44.43,26.10],5);
L.tileLayer('https://{{s}}.tile.openstreetmap.org/{{z}}/{{x}}/{{y}}.png',{{maxZoom:19,attribution:'&copy; OpenStreetMap'}}).addTo(map);
let markers={{}};let fitted=false;
async function load(){{
  try{{
    const fixes=await(await fetch('/api/positions')).json();
    $('status').textContent='Live';
    const seen=new Set();
    fixes.forEach(f=>{{
      seen.add(f.issi);
      const when=new Date(f.at_ms).toLocaleString();
      const html=`<b>ISSI ${{f.issi}}</b><br>${{f.lat.toFixed(5)}}, ${{f.lon.toFixed(5)}}<br>Station: ${{f.bts}}<br>${{when}}<br><span style="color:#555">${{(f.source_text||'').replace(/[<>&]/g,'')}}</span>`;
      if(markers[f.issi]){{markers[f.issi].setLatLng([f.lat,f.lon]).setPopupContent(html);}}
      else{{markers[f.issi]=L.marker([f.lat,f.lon]).addTo(map).bindPopup(html);}}
    }});
    Object.keys(markers).forEach(k=>{{if(!seen.has(Number(k))){{map.removeLayer(markers[k]);delete markers[k];}}}});
    $('note').textContent=fixes.length?`${{fixes.length}} station(s) positioned.`:'No decodable position beacons received yet.';
    if(!fitted&&fixes.length){{fitted=true;map.fitBounds(fixes.map(f=>[f.lat,f.lon]),{{padding:[40,40],maxZoom:13}});}}
  }}catch(e){{$('status').textContent='Disconnected';}}
  // Undecodable beacons (beaconing but not plottable), from telemetry.
  try{{
    const stations=await(await fetch('/api/telemetry')).json();
    const byIssi={{}};
    stations.forEach(s=>(s.undecoded_beacons_out||[]).forEach(u=>{{
      const prev=byIssi[u.issi];
      if(!prev||u.at_ms>prev.at_ms)byIssi[u.issi]=u;
    }}));
    const plotted=new Set(Object.keys(markers).map(Number));
    const list=Object.values(byIssi).filter(u=>!plotted.has(u.issi)).sort((a,b)=>b.at_ms-a.at_ms);
    $('undecoded').innerHTML=list.length?list.map(u=>`<span class=reg-issi title="${{esc(u.reason)}} \u2014 ${{u.count}} beacon(s)">${{u.issi}} <span class=muted>${{new Date(u.at_ms).toLocaleTimeString()}}</span></span>`).join(''):'<span class=muted>None</span>';
  }}catch(e){{}}
}}
load();setInterval(load,3000);
</script></body></html>"#, style = STYLE));

pub async fn calls_page() -> Html<&'static str> { Html(CALLS_HTML.as_str()) }
pub async fn sds_page() -> Html<&'static str> { Html(SDS_HTML.as_str()) }
pub async fn telemetry_sds_page() -> Html<&'static str> { Html(TELEMETRY_SDS_HTML.as_str()) }
pub async fn registrations_page() -> Html<&'static str> { Html(REGISTRATIONS_HTML.as_str()) }
pub async fn snapshot(State(state): State<Arc<AppState>>) -> Json<crate::monitor::Snapshot> { let i=state.inner.read().await; let counts=(i.bluestation_count(),i.ms_registration_count(),i.group_clients.len()); drop(i); Json(state.monitor.snapshot(counts.0,counts.1,counts.2).await) }
pub async fn live(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> impl IntoResponse { ws.on_upgrade(move |s| live_socket(state,s)) }
async fn live_socket(state: Arc<AppState>, mut socket: WebSocket) { let mut rx=state.monitor.subscribe(); while let Ok(ev)=rx.recv().await { if socket.send(Message::Text(serde_json::to_string(&ev).unwrap().into())).await.is_err(){break;} } }

pub async fn telemetry_snapshot(State(state): State<Arc<AppState>>) -> Json<Vec<TelemetryBts>> {
    Json(state.telemetry.read().await.snapshot())
}

pub async fn registration_log(State(state): State<Arc<AppState>>) -> Json<Vec<crate::telemetry::RegLogRow>> {
    Json(state.telemetry.read().await.registration_log())
}

pub async fn positions_snapshot(State(state): State<Arc<AppState>>) -> Json<Vec<crate::telemetry::PositionFix>> {
    Json(state.telemetry.read().await.positions())
}

pub async fn map_page() -> Html<&'static str> { Html(MAP_HTML.as_str()) }

pub async fn control_list(State(state): State<Arc<AppState>>) -> Json<Vec<String>> {
    Json(state.control.read().await.connected_ids())
}

pub async fn control_command(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(command): Json<ControlCommand>,
) -> Response {
    match control::send_command(&state, &id, command).await {
        Ok(response) => Json(serde_json::json!({"sent": true, "response": response})).into_response(),
        Err(SendError::NotConnected) => (StatusCode::NOT_FOUND, "control BTS not connected").into_response(),
        Err(SendError::Timeout) => (StatusCode::GATEWAY_TIMEOUT, "no response from BTS").into_response(),
    }
}

/// Shared CSS for the dashboard and its sub-pages, so the standalone log pages
/// match the main dashboard exactly.
const STYLE: &str = r#"<style>
:root{font-family:Inter,system-ui,sans-serif;color:#e7edf5;background:#09111c}*{box-sizing:border-box}body{margin:0}header{padding:22px 28px;border-bottom:1px solid #203047;display:flex;justify-content:space-between;align-items:center}h1{font-size:20px;margin:0}.muted{color:#8fa2b8}.wrap{padding:24px;max-width:1500px;margin:auto}.cards{display:grid;grid-template-columns:repeat(6,1fr);gap:12px}.card,.panel{background:#101b2a;border:1px solid #203047;border-radius:12px}.card{padding:16px}.n{font-size:28px;font-weight:700;margin-top:6px}.panel{margin-top:16px;padding:18px}h2{font-size:14px;text-transform:uppercase;letter-spacing:.08em;color:#8fa2b8;margin:0 0 14px}table{width:100%;border-collapse:collapse}th,td{text-align:left;padding:10px;border-bottom:1px solid #1c2a3c;font-size:13px}th{color:#8fa2b8}.pill{padding:3px 8px;border-radius:99px;background:#203047}.live{display:inline-block;width:8px;height:8px;border-radius:50%;background:#52d273;margin-right:7px}@media(max-width:900px){.cards{grid-template-columns:repeat(2,1fr)}.wrap{padding:12px}}
.health-ok{background:#173822;color:#52d273}.health-degraded{background:#3a2f12;color:#e8b93d}.health-critical{background:#3a1414;color:#f2545b}.health-unknown{background:#203047;color:#8fa2b8}
.bts-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(300px,1fr));gap:12px}.bts-card{background:#0d1826;border:1px solid #203047;border-radius:10px;padding:14px}.bts-card h3{margin:0;font-size:15px}.bts-meta{font-size:12px;margin-top:4px}.bts-card table{margin-top:10px}.bts-card th,.bts-card td{padding:6px;font-size:12px}
.banner{display:none;background:#3a1414;border:1px solid #f2545b;color:#ffb4b8;padding:12px 18px;border-radius:10px;margin-bottom:16px;font-weight:600}
.ctl-row{display:flex;gap:6px;align-items:center;margin-top:8px;flex-wrap:wrap}.ctl-row input{background:#0d1826;border:1px solid #203047;color:#e7edf5;border-radius:6px;padding:5px 8px;font-size:12px;width:auto}.ctl-row label{font-size:12px;display:flex;align-items:center;gap:4px}.ctl-row button{background:#203047;color:#e7edf5;border:1px solid #2c405c;border-radius:6px;padding:5px 10px;font-size:12px;cursor:pointer}.ctl-row button:hover{background:#2c405c}.ctl-result{font-size:12px;margin-top:8px;word-break:break-all}
.pager{display:flex;align-items:center;gap:10px;margin-top:12px;font-size:12px;color:#8fa2b8}.pager button{background:#203047;color:#e7edf5;border:1px solid #2c405c;border-radius:6px;padding:4px 10px;font-size:12px;cursor:pointer}.pager button:hover:not(:disabled){background:#2c405c}.pager button:disabled{opacity:.4;cursor:default}.pager .pginfo{min-width:120px}
.ts-wrap{margin-top:10px}.ts-carrier{display:flex;align-items:center;gap:6px;margin-top:5px}.ts-carrier .lbl{font-size:11px;color:#8fa2b8;min-width:64px}.ts-slots{display:flex;gap:4px}.ts-slot{width:34px;height:22px;border-radius:4px;border:1px solid #203047;display:flex;align-items:center;justify-content:center;font-size:10px;font-weight:600}.ts-free{background:#0d1826;color:#3f5a78}.ts-busy{background:#173822;color:#52d273;border-color:#245c37}.ts-legend{display:flex;gap:12px;font-size:11px;color:#8fa2b8;margin-top:6px}.ts-legend span{display:inline-flex;align-items:center;gap:4px}.ts-dot{width:10px;height:10px;border-radius:2px;display:inline-block}
.reg-list{display:flex;flex-wrap:wrap;gap:5px;margin-top:8px}.reg-issi{background:#0d1826;border:1px solid #203047;border-radius:5px;padding:3px 7px;font-size:12px;font-family:ui-monospace,monospace;color:#cfe0f2}.reg-count{font-size:12px;color:#8fa2b8}
.navlinks{display:flex;gap:12px;flex-wrap:wrap}.navlink{display:block;background:#0d1826;border:1px solid #203047;border-radius:10px;padding:14px 18px;color:#cfe0f2;text-decoration:none;font-size:14px;font-weight:600;transition:background .1s}.navlink:hover{background:#16273c;border-color:#2c405c}.navlink .sub{display:block;font-size:12px;font-weight:400;color:#8fa2b8;margin-top:4px}
.backlink{color:#8fa2b8;text-decoration:none;font-size:13px}.backlink:hover{color:#cfe0f2}
h2 .backlink{text-transform:none;letter-spacing:normal;margin-left:8px}
.badge{display:inline-block;padding:2px 7px;border-radius:99px;font-size:11px;font-weight:600}.badge-pos{background:#123047;color:#5cc0f2;border:1px solid #1d4a66}.badge-sds{background:#203047;color:#8fa2b8}.pos-undec{color:#8fa2b8;font-style:italic}
.badge-reg-in{background:#173822;color:#52d273;border:1px solid #245c37}.badge-reg-out{background:#203047;color:#8fa2b8;border:1px solid #2c405c}.badge-reg-timeout{background:#3a2f12;color:#e8b93d;border:1px solid #5c4a1d}
</style>"#;

const HTML: &str = r#"<!doctype html><html><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>TETRA Network</title>__STYLE__</head><body><header><h1>TETRA NETWORK MONITOR</h1><div><span class=live></span><span id=status>Live</span></div></header><main class=wrap>
<div class=banner id=emergency-banner></div>
<section class=cards><div class=card><div class=muted>BlueStations</div><div class=n id=bs>-</div></div><div class=card><div class=muted>Subscribers</div><div class=n id=subs>-</div></div><div class=card><div class=muted>Groups</div><div class=n id=groups>-</div></div><div class=card><div class=muted>Active calls</div><div class=n id=active>-</div></div><div class=card><div class=muted>Total calls</div><div class=n id=calls>-</div></div><div class=card><div class=muted>SDS</div><div class=n id=sds>-</div></div></section><section class=panel><h2>Live calls</h2><table><thead><tr><th>Type</th><th>From</th><th>To</th><th>Priority</th><th>Duration</th><th>Voice frames</th><th>MS RSSI</th><th>UUID</th></tr></thead><tbody id=livecalls></tbody></table></section><section class=panel><h2>Logs</h2><div class=navlinks><a class=navlink href="/calls">Recent calls<span class=sub>Completed call history</span></a><a class=navlink href="/sds">Recent SDS<span class=sub>Short data messages</span></a><a class=navlink href="/telemetry-sds">Telemetry SDS Log<span class=sub>Per-FlowStation SDS stream</span></a><a class=navlink href="/registrations">MS Registrations<span class=sub>Register/deregister/timeout events</span></a><a class=navlink href="/map">MS Map<span class=sub>Plot positioned mobiles</span></a></div></section>
<section class=panel><h2>FlowStation Telemetry</h2><div class=bts-grid id=telemetry-stations></div></section>
<section class=panel><h2>Registered Subscribers <a class=backlink href="/registrations">(view registration log &rarr;)</a></h2><div class=bts-grid id=registrations></div></section>
<section class=panel><h2>FlowStation Control</h2><div class=bts-grid id=control-stations></div></section>
</main><script>
let snap=null;let tsnap=null;const $=id=>document.getElementById(id);const dt=x=>new Date(x).toLocaleTimeString();const dur=(a,b)=>Math.max(0,Math.floor(((b||Date.now())-a)/1000))+'s';const esc=s=>String(s??'').replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]));
function render(s){snap=s;$('bs').textContent=s.connected_bluestations;$('subs').textContent=s.subscribers;$('groups').textContent=s.groups;$('active').textContent=s.active_calls.length;$('calls').textContent=s.total_calls;$('sds').textContent=s.total_sds;
// Build an ISSI -> RSSI (dBFS) lookup from the latest telemetry snapshot so each
// live call can show its mobile station's received signal strength. RSSI is not
// SNR; it is the real per-MS metric FlowStation reports.
const rssiByIssi={};(tsnap||[]).forEach(st=>(st.ms_rssi_out||[]).forEach(([issi,dbfs])=>{rssiByIssi[issi]=dbfs;}));
const rssiCell=issi=>rssiByIssi[issi]!=null?`${rssiByIssi[issi].toFixed(1)} dBFS`:'<span class=muted>&ndash;</span>';
$('livecalls').innerHTML=s.active_calls.map(c=>`<tr><td><span class=pill>${c.kind}</span></td><td>${c.source}</td><td>${c.destination}</td><td>${c.priority}</td><td>${dur(c.started_at_ms)}</td><td>${c.voice_frames}</td><td>${rssiCell(c.source)}</td><td class=muted>${c.uuid.slice(0,8)}</td></tr>`).join('')||'<tr><td colspan=8 class=muted>No active calls</td></tr>';}
async function refresh(){try{render(await(await fetch('/api/status')).json())}catch(e){$('status').textContent='Disconnected'}}
function healthPill(level){const cls=level==='ok'?'health-ok':level==='degraded'?'health-degraded':level==='critical'?'health-critical':'health-unknown';return `<span class="pill ${cls}">${level||'unknown'}</span>`;}
// Build a small timeslot occupancy grid from the station's active calls. TETRA
// carriers are TDMA with 4 timeslots each (TS1-TS4). A slot is "busy" when an
// active call occupies that carrier/timeslot; every other slot on a carrier
// that is in use is shown as "available". Only carriers that appear in at least
// one active call are drawn (we cannot know the full carrier plan otherwise).
const TS_PER_CARRIER=4;
function tsGrid(calls){
  if(!calls.length)return '';
  // Map carrier_num -> {ts -> speaker issi/dest} for busy slots.
  const carriers=new Map();
  calls.forEach(c=>{
    if(!carriers.has(c.carrier_num))carriers.set(c.carrier_num,new Map());
    carriers.get(c.carrier_num).set(c.ts,c);
  });
  const rows=[...carriers.keys()].sort((a,b)=>a-b).map(cn=>{
    const busy=carriers.get(cn);
    const slots=[];
    for(let ts=1;ts<=TS_PER_CARRIER;ts++){
      const c=busy.get(ts);
      if(c){
        const who=c.is_group?('grp '+c.gssi_or_called):('to '+c.gssi_or_called);
        slots.push(`<div class="ts-slot ts-busy" title="Carrier ${cn} TS${ts}: ${esc(String(c.source_issi))} \u2192 ${esc(who)}">TS${ts}</div>`);
      }else{
        slots.push(`<div class="ts-slot ts-free" title="Carrier ${cn} TS${ts}: available">TS${ts}</div>`);
      }
    }
    return `<div class=ts-carrier><span class=lbl>Carrier ${cn}</span><div class=ts-slots>${slots.join('')}</div></div>`;
  }).join('');
  return `<div class=ts-wrap>${rows}<div class=ts-legend><span><span class="ts-dot" style="background:#173822;border:1px solid #245c37"></span>busy</span><span><span class="ts-dot" style="background:#0d1826;border:1px solid #203047"></span>available</span></div></div>`;
}
function renderTelemetry(stations){
  tsnap=stations;
  const emergencies=stations.flatMap(s=>(s.emergencies||[]).map(issi=>({bts:s.id,issi})));
  const banner=$('emergency-banner');
  if(emergencies.length){banner.style.display='block';banner.textContent='EMERGENCY ACTIVE: '+emergencies.map(e=>`ISSI ${e.issi} on ${e.bts}`).join(', ');}else{banner.style.display='none';}
  $('telemetry-stations').innerHTML=stations.length?stations.map(s=>{
    const calls=Object.values(s.active_calls||{});
    const backhaul=s.backhaul_connected===true?'up':s.backhaul_connected===false?'down':'unknown';
    const q=s.last_tx_quality,sdr=s.last_sdr_health;
    const ipLabel=s.ip?` <span class=muted style="font-weight:400">- ${esc(s.ip)}</span>`:'';
    const evm=(s.evm_pct!=null)?`EVM ${s.evm_pct.toFixed(2)}%`:(q?`EVM ${q.evm_pct.toFixed(2)}%`:'');
    const rssi=(s.rssi_dbfs!=null)?`RSSI ${s.rssi_dbfs.toFixed(1)} dBFS`:'';
    const sig=[evm,rssi].filter(Boolean).join(' &middot; ');
    return `<div class=bts-card>
      <div style="display:flex;justify-content:space-between;align-items:center"><h3>${esc(s.id)}${ipLabel}</h3>${healthPill(s.health&&s.health.overall)}</div>
      <div class="bts-meta muted">Backhaul ${backhaul} &middot; ${s.registration_count} registered &middot; ${calls.length} active call(s)</div>
      ${sig?`<div class="bts-meta muted" title="EVM is transmit error-vector magnitude (SNR proxy); RSSI is received signal strength — neither is a true SNR">${sig}${q?` &middot; PAPR ${q.papr_db.toFixed(1)}dB`:''}${sdr&&sdr.temperature_c!=null?` &middot; SDR ${sdr.temperature_c.toFixed(1)}&deg;C`:''}</div>`:''}
      ${calls.length?`<table><thead><tr><th>Type</th><th>From</th><th>To</th><th>Carrier/TS</th><th>Pri</th></tr></thead><tbody>${calls.map(c=>`<tr><td>${c.is_group?'Group':'Private'}</td><td>${c.source_issi}</td><td>${c.gssi_or_called}</td><td>${c.carrier_num}/${c.ts}</td><td>${c.priority}</td></tr>`).join('')}</tbody></table>`:''}
      ${tsGrid(calls)}
    </div>`;
  }).join(''):'<div class=muted>No FlowStation telemetry connections</div>';
  // Registered subscribers, grouped by FlowStation. Shows which ISSIs are
  // currently registered on each connected station.
  $('registrations').innerHTML=stations.length?stations.map(s=>{
    const issis=s.registrations_list||[];
    const chips=issis.length?`<div class=reg-list>${issis.map(i=>`<span class=reg-issi>${esc(String(i))}</span>`).join('')}</div>`:'<div class="bts-meta muted" style="margin-top:8px">No subscribers registered</div>';
    return `<div class=bts-card><div style="display:flex;justify-content:space-between;align-items:center"><h3>${esc(s.id)}</h3><span class=reg-count>${issis.length} registered</span></div>${chips}</div>`;
  }).join(''):'<div class=muted>No FlowStation telemetry connections</div>';
}
async function refreshTelemetry(){try{renderTelemetry(await(await fetch('/api/telemetry')).json())}catch(e){}}
function ctlSafeId(id){return 'ctl_'+id.replace(/[^a-zA-Z0-9_-]/g,'_');}
function jsq(s){return String(s).replace(/\\/g,'\\\\').replace(/'/g,"\\'");}
function ctlCardHtml(id){
  const s=ctlSafeId(id);
  return `<div class=ctl-row><input id="${s}_kick_issi" placeholder="ISSI" size=8><button onclick="ctlKick('${jsq(id)}','${s}')">Kick MS</button></div>
      <div class=ctl-row><input id="${s}_clr_issi" placeholder="ISSI (0=all)" size=8><button onclick="ctlClearEmergency('${jsq(id)}','${s}')">Clear emergency</button></div>
      <div class=ctl-row><input id="${s}_dgna_issi" placeholder="ISSI" size=6><input id="${s}_dgna_gssi" placeholder="GSSI" size=6><input id="${s}_dgna_mode" placeholder="mode" size=3 value=0><label><input type=checkbox id="${s}_dgna_attach" checked>attach</label><button onclick="ctlDgna('${jsq(id)}','${s}')">DGNA</button></div>
      <div class=ctl-row><input id="${s}_sds_text" placeholder="live SDS text"><input id="${s}_sds_issi" placeholder="src ISSI" size=8><input id="${s}_sds_repeat" placeholder="repeat" size=4 value=0><button onclick="ctlAddLiveSds('${jsq(id)}','${s}')">Add live SDS</button><button onclick="ctlClearLiveSds('${jsq(id)}','${s}')">Clear all</button></div>
      <div class=ctl-row><input id="${s}_raw_src" placeholder="src ISSI" size=8><input id="${s}_raw_dest" placeholder="dest ISSI/GSSI" size=8><label><input type=checkbox id="${s}_raw_grp">group</label><input id="${s}_raw_len" placeholder="len bits" size=6><input id="${s}_raw_hex" placeholder="payload hex"><button onclick="ctlSendSds('${jsq(id)}','${s}')">Send raw SDS</button></div>
      <div class=ctl-row><button onclick="ctlRestart('${jsq(id)}')">Restart service</button><button onclick="ctlShutdown('${jsq(id)}')">Shutdown service</button></div>
      <div class="ctl-result muted" id="${s}_result"></div>`;
}
// Incrementally reconcile the control cards against the connected station list.
// We never rebuild an existing card, so user input and focus in its fields are
// preserved across the periodic refresh (this was the bug: a full innerHTML
// rewrite every few seconds wiped whatever the operator was typing).
function renderControl(ids){
  const host=$('control-stations');
  const wanted=new Set(ids);
  if(!ids.length){host.innerHTML='<div class="muted ctl-empty">No FlowStation control connections</div>';return;}
  // Clear the "no connections" placeholder before adding the first real card.
  const ph=host.querySelector('.ctl-empty');
  if(ph)ph.remove();
  // Remove cards for stations that are no longer connected.
  Array.from(host.querySelectorAll('.bts-card[data-ctl-id]')).forEach(card=>{
    if(!wanted.has(card.getAttribute('data-ctl-id')))card.remove();
  });
  // Add cards for newly connected stations, preserving existing ones untouched.
  const existing=new Set(Array.from(host.querySelectorAll('.bts-card[data-ctl-id]')).map(c=>c.getAttribute('data-ctl-id')));
  ids.forEach(id=>{
    if(existing.has(id))return;
    const card=document.createElement('div');
    card.className='bts-card';
    card.setAttribute('data-ctl-id',id);
    card.innerHTML=`<h3>${esc(id)}</h3>`+ctlCardHtml(id);
    host.appendChild(card);
  });
}
async function refreshControl(){try{renderControl(await(await fetch('/api/control')).json())}catch(e){}}
async function ctlSend(id,body,resultId){
  try{
    const r=await fetch('/api/control/'+encodeURIComponent(id),{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)});
    const text=await r.text();
    if(resultId) $(resultId).textContent=(r.ok?'OK: ':'Error: ')+text;
  }catch(e){ if(resultId) $(resultId).textContent='Error: '+e; }
}
function hexToBytes(hex){hex=(hex||'').replace(/\s+/g,'');const out=[];for(let i=0;i<hex.length-1;i+=2)out.push(parseInt(hex.substr(i,2),16)||0);return out;}
function ctlKick(id,s){ctlSend(id,{action:'KickMs',issi:Number($(s+'_kick_issi').value||0)},s+'_result');}
function ctlClearEmergency(id,s){ctlSend(id,{action:'ClearEmergency',issi:Number($(s+'_clr_issi').value||0)},s+'_result');}
function ctlDgna(id,s){ctlSend(id,{action:'Dgna',issi:Number($(s+'_dgna_issi').value||0),gssi:Number($(s+'_dgna_gssi').value||0),mnemonic:null,attachment_mode:Number($(s+'_dgna_mode').value||0),attach:$(s+'_dgna_attach').checked},s+'_result');}
function ctlAddLiveSds(id,s){ctlSend(id,{action:'AddLiveSds',text:$(s+'_sds_text').value,protocol_id:10,source_issi:Number($(s+'_sds_issi').value||0),repeat_count:Number($(s+'_sds_repeat').value||0)},s+'_result');}
function ctlClearLiveSds(id,s){ctlSend(id,{action:'ClearLiveSds'},s+'_result');}
function ctlSendSds(id,s){const payload=hexToBytes($(s+'_raw_hex').value);ctlSend(id,{action:'SendSds',source_ssi:Number($(s+'_raw_src').value||0),dest_ssi:Number($(s+'_raw_dest').value||0),dest_is_group:$(s+'_raw_grp').checked,len_bits:Number($(s+'_raw_len').value||payload.length*8),payload},s+'_result');}
function ctlRestart(id){if(confirm('Restart FlowStation service on '+id+'? This disconnects it.'))ctlSend(id,{action:'RestartService'},null);}
function ctlShutdown(id){if(confirm('Shutdown FlowStation service on '+id+'? This stops the BTS process.'))ctlSend(id,{action:'ShutdownService'},null);}
refresh();refreshTelemetry();refreshControl();setInterval(refresh,2000);setInterval(refreshTelemetry,2000);setInterval(refreshControl,3000);
// Keep a live WebSocket for push updates, but never reload the page on drop:
// a reload would wipe anything the operator is typing in the control panel.
// Instead we reconnect in the background and fall back to the polling above.
function connectLive(){
  let ws=new WebSocket((location.protocol==='https:'?'wss://':'ws://')+location.host+'/api/live');
  ws.onopen=()=>{$('status').textContent='Live';};
  ws.onmessage=()=>{refresh();refreshTelemetry();refreshControl();};
  ws.onclose=()=>{$('status').textContent='Reconnecting';setTimeout(connectLive,3000);};
  ws.onerror=()=>{try{ws.close();}catch(e){}};
}
connectLive();
</script></body></html>"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_has_style_and_navlinks_no_removed_tables() {
        let h = INDEX_HTML.as_str();
        assert!(!h.contains("__STYLE__"), "style placeholder must be substituted");
        assert!(h.contains("<style>"), "style present");
        // nav links to the three sub-pages
        assert!(h.contains("href=\"/calls\""));
        assert!(h.contains("href=\"/sds\""));
        assert!(h.contains("href=\"/telemetry-sds\""));
        assert!(h.contains("href=\"/registrations\""));
        // the three moved tables must be gone from the main page
        assert!(!h.contains("id=sdstable"), "recent SDS table removed from index");
        assert!(!h.contains("id=history"), "recent calls table removed from index");
        assert!(!h.contains("id=telemetry-sds"), "telemetry SDS table removed from index");
    }

    #[test]
    fn subpages_build_and_contain_expected_bits() {
        for (name, html, endpoint) in [
            ("calls", CALLS_HTML.as_str(), "/api/status"),
            ("sds", SDS_HTML.as_str(), "/api/status"),
            ("telemetry", TELEMETRY_SDS_HTML.as_str(), "/api/telemetry"),
            ("registrations", REGISTRATIONS_HTML.as_str(), "/api/registrations"),
        ] {
            assert!(!html.contains("__STYLE__"), "{name}: style substituted");
            assert!(html.contains("id=log"), "{name}: log table body present");
            assert!(html.contains("id=log-pager"), "{name}: pager present");
            assert!(html.contains(endpoint), "{name}: polls {endpoint}");
            assert!(html.contains("Back to dashboard"), "{name}: back link present");
            // write out the embedded script for external JS syntax checking
            let script = html.split("<script>").nth(1).unwrap().split("</script>").next().unwrap();
            std::fs::write(format!("/tmp/subpage_{name}.js"), script).unwrap();
        }
    }

    #[test]
    fn map_page_builds() {
        let h = MAP_HTML.as_str();
        assert!(!h.contains("__STYLE__"), "style substituted");
        assert!(h.contains("/api/positions"), "map polls positions api");
        assert!(h.contains("leaflet"), "leaflet loaded");
        assert!(h.contains("Back to dashboard"), "back link present");
        // OSM tile template must survive the format! escaping as literal braces
        assert!(h.contains("{s}.tile.openstreetmap.org/{z}/{x}/{y}"), "tile template intact");
        let script = h.rsplit("<script>").next().unwrap().split("</script>").next().unwrap();
        std::fs::write("/tmp/map_page.js", script).unwrap();
    }

    #[test]
    fn index_links_to_map() {
        assert!(INDEX_HTML.as_str().contains("href=\"/map\""), "dashboard links to /map");
    }
}

