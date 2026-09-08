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
        .route("/api/status", get(snapshot))
        .route("/api/live", get(live))
        .route("/api/telemetry", get(telemetry_snapshot))
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

pub async fn index() -> Html<&'static str> { Html(HTML) }
pub async fn snapshot(State(state): State<Arc<AppState>>) -> Json<crate::monitor::Snapshot> { let i=state.inner.read().await; let counts=(i.clients.len(),i.subscribers.len(),i.group_clients.len()); drop(i); Json(state.monitor.snapshot(counts.0,counts.1,counts.2).await) }
pub async fn live(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> impl IntoResponse { ws.on_upgrade(move |s| live_socket(state,s)) }
async fn live_socket(state: Arc<AppState>, mut socket: WebSocket) { let mut rx=state.monitor.subscribe(); while let Ok(ev)=rx.recv().await { if socket.send(Message::Text(serde_json::to_string(&ev).unwrap().into())).await.is_err(){break;} } }

pub async fn telemetry_snapshot(State(state): State<Arc<AppState>>) -> Json<Vec<TelemetryBts>> {
    Json(state.telemetry.read().await.snapshot())
}

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

const HTML: &str = r#"<!doctype html><html><head><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'><title>TETRA Network</title><style>
:root{font-family:Inter,system-ui,sans-serif;color:#e7edf5;background:#09111c}*{box-sizing:border-box}body{margin:0}header{padding:22px 28px;border-bottom:1px solid #203047;display:flex;justify-content:space-between;align-items:center}h1{font-size:20px;margin:0}.muted{color:#8fa2b8}.wrap{padding:24px;max-width:1500px;margin:auto}.cards{display:grid;grid-template-columns:repeat(6,1fr);gap:12px}.card,.panel{background:#101b2a;border:1px solid #203047;border-radius:12px}.card{padding:16px}.n{font-size:28px;font-weight:700;margin-top:6px}.panel{margin-top:16px;padding:18px}h2{font-size:14px;text-transform:uppercase;letter-spacing:.08em;color:#8fa2b8;margin:0 0 14px}table{width:100%;border-collapse:collapse}th,td{text-align:left;padding:10px;border-bottom:1px solid #1c2a3c;font-size:13px}th{color:#8fa2b8}.pill{padding:3px 8px;border-radius:99px;background:#203047}.live{display:inline-block;width:8px;height:8px;border-radius:50%;background:#52d273;margin-right:7px}@media(max-width:900px){.cards{grid-template-columns:repeat(2,1fr)}.wrap{padding:12px}}
.health-ok{background:#173822;color:#52d273}.health-degraded{background:#3a2f12;color:#e8b93d}.health-critical{background:#3a1414;color:#f2545b}.health-unknown{background:#203047;color:#8fa2b8}
.bts-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(300px,1fr));gap:12px}.bts-card{background:#0d1826;border:1px solid #203047;border-radius:10px;padding:14px}.bts-card h3{margin:0;font-size:15px}.bts-meta{font-size:12px;margin-top:4px}.bts-card table{margin-top:10px}.bts-card th,.bts-card td{padding:6px;font-size:12px}
.banner{display:none;background:#3a1414;border:1px solid #f2545b;color:#ffb4b8;padding:12px 18px;border-radius:10px;margin-bottom:16px;font-weight:600}
.ctl-row{display:flex;gap:6px;align-items:center;margin-top:8px;flex-wrap:wrap}.ctl-row input{background:#0d1826;border:1px solid #203047;color:#e7edf5;border-radius:6px;padding:5px 8px;font-size:12px;width:auto}.ctl-row label{font-size:12px;display:flex;align-items:center;gap:4px}.ctl-row button{background:#203047;color:#e7edf5;border:1px solid #2c405c;border-radius:6px;padding:5px 10px;font-size:12px;cursor:pointer}.ctl-row button:hover{background:#2c405c}.ctl-result{font-size:12px;margin-top:8px;word-break:break-all}
</style></head><body><header><h1>TETRA NETWORK MONITOR</h1><div><span class=live></span><span id=status>Live</span></div></header><main class=wrap>
<div class=banner id=emergency-banner></div>
<section class=cards><div class=card><div class=muted>BlueStations</div><div class=n id=bs>-</div></div><div class=card><div class=muted>Subscribers</div><div class=n id=subs>-</div></div><div class=card><div class=muted>Groups</div><div class=n id=groups>-</div></div><div class=card><div class=muted>Active calls</div><div class=n id=active>-</div></div><div class=card><div class=muted>Total calls</div><div class=n id=calls>-</div></div><div class=card><div class=muted>SDS</div><div class=n id=sds>-</div></div></section><section class=panel><h2>Live calls</h2><table><thead><tr><th>Type</th><th>From</th><th>To</th><th>Priority</th><th>Duration</th><th>Voice frames</th><th>UUID</th></tr></thead><tbody id=livecalls></tbody></table></section><section class=panel><h2>Recent SDS</h2><table><thead><tr><th>Time</th><th>From</th><th>To</th><th>Reports</th><th>UUID</th></tr></thead><tbody id=sdstable></tbody></table></section><section class=panel><h2>Recent calls</h2><table><thead><tr><th>Type</th><th>From</th><th>To</th><th>Start</th><th>Duration</th><th>Frames</th></tr></thead><tbody id=history></tbody></table></section>
<section class=panel><h2>FlowStation Telemetry</h2><div class=bts-grid id=telemetry-stations></div></section>
<section class=panel><h2>Telemetry SDS Log</h2><table><thead><tr><th>Time</th><th>BTS</th><th>Dir</th><th>From</th><th>To</th><th>Text</th></tr></thead><tbody id=telemetry-sds></tbody></table></section>
<section class=panel><h2>FlowStation Control</h2><div class=bts-grid id=control-stations></div></section>
</main><script>
let snap=null;const $=id=>document.getElementById(id);const dt=x=>new Date(x).toLocaleTimeString();const dur=(a,b)=>Math.max(0,Math.floor(((b||Date.now())-a)/1000))+'s';const esc=s=>String(s??'').replace(/[&<>]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c]));
function render(s){snap=s;$('bs').textContent=s.connected_bluestations;$('subs').textContent=s.subscribers;$('groups').textContent=s.groups;$('active').textContent=s.active_calls.length;$('calls').textContent=s.total_calls;$('sds').textContent=s.total_sds;$('livecalls').innerHTML=s.active_calls.map(c=>`<tr><td><span class=pill>${c.kind}</span></td><td>${c.source}</td><td>${c.destination}</td><td>${c.priority}</td><td>${dur(c.started_at_ms)}</td><td>${c.voice_frames}</td><td class=muted>${c.uuid.slice(0,8)}</td></tr>`).join('')||'<tr><td colspan=7 class=muted>No active calls</td></tr>';$('sdstable').innerHTML=s.recent_sds.map(x=>`<tr><td>${dt(x.at_ms)}</td><td>${x.source}</td><td>${x.destination}</td><td>${x.reports}</td><td class=muted>${x.uuid.slice(0,8)}</td></tr>`).join('')||'<tr><td colspan=5 class=muted>No SDS yet</td></tr>';$('history').innerHTML=s.recent_calls.map(c=>`<tr><td>${c.kind}</td><td>${c.source}</td><td>${c.destination}</td><td>${dt(c.started_at_ms)}</td><td>${dur(c.started_at_ms,c.ended_at_ms)}</td><td>${c.voice_frames}</td></tr>`).join('')||'<tr><td colspan=6 class=muted>No completed calls</td></tr>';}
async function refresh(){try{render(await(await fetch('/api/status')).json())}catch(e){$('status').textContent='Disconnected'}}
function healthPill(level){const cls=level==='ok'?'health-ok':level==='degraded'?'health-degraded':level==='critical'?'health-critical':'health-unknown';return `<span class="pill ${cls}">${level||'unknown'}</span>`;}
function renderTelemetry(stations){
  const emergencies=stations.flatMap(s=>(s.emergencies||[]).map(issi=>({bts:s.id,issi})));
  const banner=$('emergency-banner');
  if(emergencies.length){banner.style.display='block';banner.textContent='EMERGENCY ACTIVE: '+emergencies.map(e=>`ISSI ${e.issi} on ${e.bts}`).join(', ');}else{banner.style.display='none';}
  $('telemetry-stations').innerHTML=stations.length?stations.map(s=>{
    const calls=Object.values(s.active_calls||{});
    const backhaul=s.backhaul_connected===true?'up':s.backhaul_connected===false?'down':'unknown';
    const q=s.last_tx_quality,sdr=s.last_sdr_health;
    return `<div class=bts-card>
      <div style="display:flex;justify-content:space-between;align-items:center"><h3>${esc(s.id)}</h3>${healthPill(s.health&&s.health.overall)}</div>
      <div class="bts-meta muted">Backhaul ${backhaul} &middot; ${s.registration_count} registered &middot; ${calls.length} active call(s)</div>
      ${q?`<div class="bts-meta muted">EVM ${q.evm_pct.toFixed(2)}% &middot; PAPR ${q.papr_db.toFixed(1)}dB${sdr&&sdr.temperature_c!=null?` &middot; SDR ${sdr.temperature_c.toFixed(1)}&deg;C`:''}</div>`:''}
      ${calls.length?`<table><thead><tr><th>Type</th><th>From</th><th>To</th><th>Carrier/TS</th><th>Pri</th></tr></thead><tbody>${calls.map(c=>`<tr><td>${c.is_group?'Group':'Private'}</td><td>${c.source_issi}</td><td>${c.gssi_or_called}</td><td>${c.carrier_num}/${c.ts}</td><td>${c.priority}</td></tr>`).join('')}</tbody></table>`:''}
    </div>`;
  }).join(''):'<div class=muted>No FlowStation telemetry connections</div>';
  const allSds=stations.flatMap(s=>(s.recent_sds_out||[]).map(x=>({...x,bts:s.id}))).sort((a,b)=>b.at_ms-a.at_ms).slice(0,50);
  $('telemetry-sds').innerHTML=allSds.length?allSds.map(x=>`<tr><td>${dt(x.at_ms)}</td><td>${esc(x.bts)}</td><td>${esc(x.direction)}</td><td>${x.source_issi}</td><td>${x.dest_issi}${x.is_group?' (grp)':''}</td><td>${esc(x.text)}</td></tr>`).join(''):'<tr><td colspan=6 class=muted>No telemetry SDS yet</td></tr>';
}
async function refreshTelemetry(){try{renderTelemetry(await(await fetch('/api/telemetry')).json())}catch(e){}}
function ctlSafeId(id){return 'ctl_'+id.replace(/[^a-zA-Z0-9_-]/g,'_');}
function jsq(s){return String(s).replace(/\\/g,'\\\\').replace(/'/g,"\\'");}
function renderControl(ids){
  $('control-stations').innerHTML=ids.length?ids.map(id=>{
    const s=ctlSafeId(id);
    return `<div class=bts-card><h3>${esc(id)}</h3>
      <div class=ctl-row><input id="${s}_kick_issi" placeholder="ISSI" size=8><button onclick="ctlKick('${jsq(id)}','${s}')">Kick MS</button></div>
      <div class=ctl-row><input id="${s}_clr_issi" placeholder="ISSI (0=all)" size=8><button onclick="ctlClearEmergency('${jsq(id)}','${s}')">Clear emergency</button></div>
      <div class=ctl-row><input id="${s}_dgna_issi" placeholder="ISSI" size=6><input id="${s}_dgna_gssi" placeholder="GSSI" size=6><input id="${s}_dgna_mode" placeholder="mode" size=3 value=0><label><input type=checkbox id="${s}_dgna_attach" checked>attach</label><button onclick="ctlDgna('${jsq(id)}','${s}')">DGNA</button></div>
      <div class=ctl-row><input id="${s}_sds_text" placeholder="live SDS text"><input id="${s}_sds_issi" placeholder="src ISSI" size=8><input id="${s}_sds_repeat" placeholder="repeat" size=4 value=0><button onclick="ctlAddLiveSds('${jsq(id)}','${s}')">Add live SDS</button><button onclick="ctlClearLiveSds('${jsq(id)}','${s}')">Clear all</button></div>
      <div class=ctl-row><input id="${s}_raw_src" placeholder="src ISSI" size=8><input id="${s}_raw_dest" placeholder="dest ISSI/GSSI" size=8><label><input type=checkbox id="${s}_raw_grp">group</label><input id="${s}_raw_len" placeholder="len bits" size=6><input id="${s}_raw_hex" placeholder="payload hex"><button onclick="ctlSendSds('${jsq(id)}','${s}')">Send raw SDS</button></div>
      <div class=ctl-row><button onclick="ctlRestart('${jsq(id)}')">Restart service</button><button onclick="ctlShutdown('${jsq(id)}')">Shutdown service</button></div>
      <div class="ctl-result muted" id="${s}_result"></div>
    </div>`;
  }).join(''):'<div class=muted>No FlowStation control connections</div>';
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
let ws=new WebSocket((location.protocol==='https:'?'wss://':'ws://')+location.host+'/api/live');ws.onmessage=()=>{refresh();refreshTelemetry();refreshControl();};ws.onclose=()=>{$('status').textContent='Reconnecting';setTimeout(()=>location.reload(),3000)};
</script></body></html>"#;
