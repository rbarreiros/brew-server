use crate::{
    protocol::{
        self, BrewMessage, CallPayload, SubscriberMessage, CALL_ALERT, CALL_CONNECT_CONFIRM,
        CALL_CONNECT_REQUEST, CALL_GROUP_IDLE, CALL_GROUP_TX, CALL_RELEASE, CALL_SETUP_ACCEPT,
        CALL_SETUP_REJECT, CALL_SETUP_REQUEST, CALL_SHORT_TRANSFER, CALL_SIMPLEX_GRANTED,
        CALL_SIMPLEX_IDLE, FRAME_SDS_REPORT, FRAME_SDS_TRANSFER, FRAME_TRAFFIC_CHANNEL,
        SUB_AFFILIATE, SUB_DEAFFILIATE, SUB_DEREGISTER, SUB_REGISTER, SUB_REREGISTER,
    },
    state::{ActiveCall, AppState, CallKind, ClientId, SdsRoute, Subscriber},
};
use std::{collections::HashSet, sync::Arc, time::Instant};
use tracing::{debug, info, warn};

pub async fn handle_packet(state: Arc<AppState>, source: ClientId, raw: Vec<u8>) {
    state.purge_ephemeral().await;
    let version = state.client_version(source).await;
    let (parsed, detected) = match protocol::parse_with_version(&raw, version) {
        Ok(v) => v,
        Err(e) => {
            warn!(%source, error = %e, bytes = raw.len(), "dropping malformed Brew packet");
            return;
        }
    };
    // Lazily resolve the connection version from message content, mirroring the
    // client side. Log the promotion once so operators can see when a peer is
    // confirmed to speak v1 despite sending no X-Brew-Version handshake header.
    if detected.as_u8() > version.as_u8() && state.promote_client_version(source, detected).await {
        info!(%source, from = version.as_u8(), to = detected.as_u8(), "Brew connection version promoted from message content");
    }

    match parsed {
        BrewMessage::Subscriber(msg) => handle_subscriber(&state, source, msg).await,
        BrewMessage::CallControl(cc) if cc.call_state == CALL_GROUP_TX => {
            handle_group_tx(&state, source, cc.identifier, cc.payload, raw).await;
        }
        BrewMessage::CallControl(cc) if cc.call_state == CALL_SHORT_TRANSFER => {
            handle_sds_header(&state, source, cc.identifier, cc.payload, raw).await;
        }
        BrewMessage::Frame(frame) if frame.frame_type == FRAME_SDS_TRANSFER => {
            handle_sds_transfer(&state, source, frame.identifier, raw).await;
        }
        BrewMessage::Frame(frame) if frame.frame_type == FRAME_SDS_REPORT => {
            handle_sds_report(&state, source, frame.identifier, raw).await;
        }
        BrewMessage::CallControl(cc) if cc.call_state == CALL_SETUP_REQUEST => {
            handle_private_setup(&state, source, cc.identifier, cc.payload, raw).await;
        }
        BrewMessage::CallControl(cc)
            if matches!(cc.call_state, CALL_SETUP_ACCEPT | CALL_SETUP_REJECT | CALL_ALERT |
                CALL_CONNECT_REQUEST | CALL_CONNECT_CONFIRM | CALL_SIMPLEX_GRANTED | CALL_SIMPLEX_IDLE) =>
        {
            route_private_control(&state, source, cc.identifier, raw).await;
        }
        BrewMessage::CallControl(cc)
            if cc.call_state == CALL_GROUP_IDLE || cc.call_state == CALL_RELEASE =>
        {
            end_call(&state, source, cc.identifier, raw).await;
        }
        BrewMessage::Frame(frame) if frame.frame_type == FRAME_TRAFFIC_CHANNEL => {
            route_call_frame(&state, source, frame.identifier, raw).await;
        }
        BrewMessage::Service(svc) => {
            debug!(%source, service_type = svc.service_type, json = %svc.json_data, "service message ignored");
        }
        BrewMessage::Error(err) => {
            warn!(%source, error_type = err.error_type, bytes = err.data.len(), "client sent Brew error");
        }
        other => debug!(%source, ?other, "Brew message not handled"),
    }
}

async fn handle_group_tx(state: &Arc<AppState>, source: ClientId, id: uuid::Uuid, payload: CallPayload, raw: Vec<u8>) {
    let CallPayload::GroupTransmission(gt) = payload else { return };
    let mut inner = state.inner.write().await;

    if !state.config.allow_multiple_calls_per_group {
        if let Some(existing_id) = inner.group_floor.get(&gt.destination).copied() {
            if existing_id != id {
                if let Some(existing) = inner.calls.get(&existing_id).cloned() {
                    let wins = if state.config.higher_priority_number_wins {
                        gt.priority > existing.priority
                    } else {
                        gt.priority < existing.priority
                    };
                    if !wins {
                        warn!(%source, gssi=gt.destination, priority=gt.priority, active_priority=existing.priority,
                            rejected_uuid=%id, "group floor occupied by equal/higher priority call");
                        return;
                    }

                    let release = protocol::build_call_cause(CALL_GROUP_IDLE, &existing_id, state.config.preempt_cause);
                    let mut notify = existing.peers.clone();
                    notify.insert(existing.owner);
                    let txs = notify.iter().filter_map(|cid| inner.clients.get(cid).map(|c| c.tx.clone())).collect::<Vec<_>>();
                    for tx in txs { let _ = tx.send(release.clone()); }
                    inner.calls.remove(&existing_id);
                    info!(old_uuid=%existing_id, new_uuid=%id, gssi=gt.destination,
                        old_priority=existing.priority, new_priority=gt.priority, "pre-empted group call");
                }
            }
        }
    }

    let mut targets = if state.config.route_without_affiliations {
        inner.clients.keys().copied().collect::<HashSet<_>>()
    } else {
        inner.group_clients.get(&gt.destination).cloned().unwrap_or_default()
    };

    // BlueStation can be connected and forwarding calls before an AFFILIATE event
    // has reached Brew (for example during startup/resync or while debugging MM
    // group-affiliation propagation). In that case a strict affiliation-only core
    // silently produces target_count=0. For small/private networks we support an
    // explicit fallback to every other connected BS. Once affiliations exist, the
    // normal selective routing above is used.
    if targets.is_empty()
        && !state.config.route_without_affiliations
        && state.config.fallback_broadcast_when_no_affiliations
    {
        targets = inner.clients.keys().copied().collect::<HashSet<_>>();
        warn!(
            %source,
            gssi = gt.destination,
            connected_clients = inner.clients.len(),
            "no Brew affiliations recorded for GSSI; falling back to all connected BlueStations"
        );
    }
    targets.remove(&source);
    inner.group_floor.insert(gt.destination, id);
    inner.calls.insert(id, ActiveCall {
        kind: CallKind::Group,
        owner: source,
        source_issi: gt.source,
        destination: gt.destination,
        priority: gt.priority,
        peers: targets.clone(),
    });
    let txs = targets.iter().filter_map(|cid| inner.clients.get(cid).map(|c| c.tx.clone())).collect::<Vec<_>>();
    drop(inner);
    for tx in txs { let _ = tx.send(raw.clone()); }
    state.monitor.call_started(id, "group", gt.source, gt.destination, gt.priority).await;
    info!(%source, uuid=%id, src_issi=gt.source, gssi=gt.destination, priority=gt.priority,
        target_count=targets.len(), "routed GROUP_TX");
}

async fn handle_sds_header(state: &Arc<AppState>, source: ClientId, id: uuid::Uuid, payload: CallPayload, raw: Vec<u8>) {
    let CallPayload::ShortTransfer { source: source_issi, destination } = payload else { return };
    let mut inner = state.inner.write().await;
    let mut targets = HashSet::new();
    if let Some(sub) = inner.subscribers.get(&destination) { targets.insert(sub.client_id); }
    if let Some(group_targets) = inner.group_clients.get(&destination) { targets.extend(group_targets.iter().copied()); }
    targets.remove(&source);
    if targets.is_empty() {
        warn!(%source, uuid=%id, source_issi, destination, "SDS has no registered destination");
        return;
    }
    inner.sds_routes.insert(id, SdsRoute { source_client: source, targets: targets.clone(), source_issi, destination, created_at: Instant::now() });
    let txs = targets.iter().filter_map(|cid| inner.clients.get(cid).map(|c| c.tx.clone())).collect::<Vec<_>>();
    drop(inner);
    for tx in txs { let _ = tx.send(raw.clone()); }
    state.monitor.sds(id, source_issi, destination).await;
    info!(%source, uuid=%id, source_issi, destination, target_count=targets.len(), "routed SDS header");
}

async fn handle_sds_transfer(state: &Arc<AppState>, source: ClientId, id: uuid::Uuid, raw: Vec<u8>) {
    let inner = state.inner.read().await;
    let Some(route) = inner.sds_routes.get(&id) else { warn!(uuid=%id, "SDS_TRANSFER without SHORT_TRANSFER"); return; };
    if route.source_client != source { warn!(%source, uuid=%id, "SDS_TRANSFER from non-originating client"); return; }
    let txs = route.targets.iter().filter_map(|cid| inner.clients.get(cid).map(|c| c.tx.clone())).collect::<Vec<_>>();
    drop(inner);
    for tx in txs { let _ = tx.send(raw.clone()); }
}

async fn handle_sds_report(state: &Arc<AppState>, source: ClientId, id: uuid::Uuid, raw: Vec<u8>) {
    let mut inner = state.inner.write().await;
    let Some(route) = inner.sds_routes.get(&id).cloned() else { return };
    if !route.targets.contains(&source) { warn!(%source, uuid=%id, "SDS_REPORT from unexpected client"); return; }
    let tx = inner.clients.get(&route.source_client).map(|c| c.tx.clone());
    // For unicast the transaction is complete. For multicast keep it until TTL so multiple reports can return.
    if route.targets.len() == 1 { inner.sds_routes.remove(&id); }
    drop(inner);
    if let Some(tx) = tx { let _ = tx.send(raw); }
    state.monitor.sds_report(id).await;
    info!(%source, uuid=%id, source_issi=route.source_issi, destination=route.destination, "routed SDS report");
}

async fn handle_private_setup(state: &Arc<AppState>, source: ClientId, id: uuid::Uuid, payload: CallPayload, raw: Vec<u8>) {
    // Prefer the structured CircularCall payload (parsed per Brew v1). Fall back
    // to the conservative raw source/destination pair for any peer that sends a
    // payload we could not fully structure.
    let (source_issi, destination, mnemonic) = match &payload {
        CallPayload::CircularCall(c) => (c.source, c.destination, c.mnemonic.clone()),
        other => match protocol::raw_peer_pair(other) {
            Some((s, d)) => (s, d, None),
            None => {
                warn!(%source, uuid=%id, "private SETUP_REQUEST has no routable source/destination pair");
                return;
            }
        },
    };
    let mut inner = state.inner.write().await;
    let Some(target_client) = inner.subscribers.get(&destination).map(|s| s.client_id) else {
        warn!(%source, uuid=%id, destination, "private call destination not registered");
        return;
    };
    if target_client == source { return; }
    let peers = HashSet::from([target_client]);
    inner.calls.insert(id, ActiveCall { kind: CallKind::Private, owner: source, source_issi, destination, priority: 0, peers: peers.clone() });
    let tx = inner.clients.get(&target_client).map(|c| c.tx.clone());
    drop(inner);
    if let Some(tx) = tx { let _ = tx.send(raw); }
    state.monitor.call_started(id, "private", source_issi, destination, 0).await;
    info!(%source, uuid=%id, source_issi, destination, mnemonic=?mnemonic, "routed private SETUP_REQUEST");
}

async fn route_private_control(state: &Arc<AppState>, source: ClientId, id: uuid::Uuid, raw: Vec<u8>) {
    let inner = state.inner.read().await;
    let Some(call) = inner.calls.get(&id) else { debug!(uuid=%id, "private control for unknown call"); return; };
    if call.kind != CallKind::Private { return; }
    let mut recipients = call.peers.clone();
    recipients.insert(call.owner);
    recipients.remove(&source);
    let txs = recipients.iter().filter_map(|cid| inner.clients.get(cid).map(|c| c.tx.clone())).collect::<Vec<_>>();
    drop(inner);
    for tx in txs { let _ = tx.send(raw.clone()); }
}

async fn route_call_frame(state: &Arc<AppState>, source: ClientId, id: uuid::Uuid, raw: Vec<u8>) {
    let inner = state.inner.read().await;
    let Some(call) = inner.calls.get(&id) else { debug!(uuid=%id, "voice frame for unknown call"); return; };
    let mut allowed = call.peers.contains(&source) || call.owner == source;
    if call.kind == CallKind::Group { allowed = call.owner == source; }
    if !allowed { warn!(%source, uuid=%id, "voice frame from non-participant"); return; }
    let mut recipients = call.peers.clone();
    if call.kind == CallKind::Private { recipients.insert(call.owner); }
    recipients.remove(&source);
    let txs = recipients.iter().filter_map(|cid| inner.clients.get(cid).map(|c| c.tx.clone())).collect::<Vec<_>>();
    drop(inner);
    state.monitor.voice_frame(id).await;
    for tx in txs { let _ = tx.send(raw.clone()); }
}

async fn end_call(state: &Arc<AppState>, source: ClientId, id: uuid::Uuid, raw: Vec<u8>) {
    let mut inner = state.inner.write().await;
    let Some(call) = inner.calls.remove(&id) else { debug!(uuid=%id, "call end for unknown call"); return; };
    let participant = call.owner == source || call.peers.contains(&source);
    if !participant { inner.calls.insert(id, call); return; }
    if call.kind == CallKind::Group && call.owner != source { inner.calls.insert(id, call); return; }
    if call.kind == CallKind::Group && inner.group_floor.get(&call.destination) == Some(&id) { inner.group_floor.remove(&call.destination); }
    let mut recipients = call.peers.clone();
    if call.kind == CallKind::Private { recipients.insert(call.owner); }
    recipients.remove(&source);
    let txs = recipients.iter().filter_map(|cid| inner.clients.get(cid).map(|c| c.tx.clone())).collect::<Vec<_>>();
    drop(inner);
    for tx in txs { let _ = tx.send(raw.clone()); }
    state.monitor.call_ended(id).await;
    info!(%source, uuid=%id, kind=?call.kind, "routed call end");
}

async fn handle_subscriber(state: &Arc<AppState>, source: ClientId, msg: SubscriberMessage) {
    let mut inner = state.inner.write().await;
    match msg.msg_type {
        SUB_REGISTER | SUB_REREGISTER => {
            let previous = inner.subscribers.get(&msg.issi).map(|s| (s.client_id, s.groups.clone()));
            let old_groups = previous.as_ref().map(|(_, groups)| groups.clone()).unwrap_or_default();
            if let Some((old_client, groups)) = previous {
                if old_client != source {
                    for gssi in &groups {
                        if let Some(clients) = inner.group_clients.get_mut(gssi) { clients.remove(&old_client); clients.insert(source); }
                    }
                }
            }
            inner.subscribers.insert(msg.issi, Subscriber { client_id: source, groups: old_groups });
            info!(%source, issi=msg.issi, "subscriber registered");
        }
        SUB_DEREGISTER => {
            if let Some(sub) = inner.subscribers.remove(&msg.issi) {
                if sub.client_id == source {
                    for gssi in sub.groups {
                        let still_present = inner.subscribers.values().any(|other| other.client_id == source && other.groups.contains(&gssi));
                        if !still_present { if let Some(clients) = inner.group_clients.get_mut(&gssi) { clients.remove(&source); } }
                    }
                    info!(%source, issi=msg.issi, "subscriber deregistered");
                } else { inner.subscribers.insert(msg.issi, sub); }
            }
        }
        SUB_AFFILIATE => {
            let owner = inner.subscribers.get(&msg.issi).map(|s| s.client_id);
            if let Some(owner) = owner { if owner != source { warn!(%source, issi=msg.issi, "affiliation from non-owner"); return; } }
            else { inner.subscribers.insert(msg.issi, Subscriber { client_id: source, groups: HashSet::new() }); }
            for gssi in msg.groups {
                if let Some(sub) = inner.subscribers.get_mut(&msg.issi) { sub.groups.insert(gssi); }
                inner.group_clients.entry(gssi).or_default().insert(source);
                info!(%source, issi=msg.issi, gssi, "subscriber affiliated");
            }
        }
        SUB_DEAFFILIATE => {
            if inner.subscribers.get(&msg.issi).map(|s| s.client_id) != Some(source) { return; }
            for gssi in msg.groups {
                if let Some(sub) = inner.subscribers.get_mut(&msg.issi) { sub.groups.remove(&gssi); }
                let still_present = inner.subscribers.values().any(|other| other.client_id == source && other.groups.contains(&gssi));
                if !still_present { if let Some(clients) = inner.group_clients.get_mut(&gssi) { clients.remove(&source); } }
                info!(%source, issi=msg.issi, gssi, "subscriber deaffiliated");
            }
        }
        _ => debug!(%source, msg_type=msg.msg_type, "unknown subscriber message"),
    }
}
