# Private/simplex routing (experimental)

The following Brew call states are recognized and routed by call UUID:

- 4 SETUP_REQUEST
- 5 SETUP_ACCEPT
- 6 SETUP_REJECT
- 7 CALL_ALERT
- 8 CONNECT_REQUEST
- 9 CONNECT_CONFIRM
- 10 CALL_RELEASE
- 12 SIMPLEX_GRANTED
- 13 SIMPLEX_IDLE

`SETUP_REQUEST` establishes the route from the structured `BrewCircularCall` payload (source ISSI, destination ISSI, dialled number, priority, and — on v1 — the talking-party `mnemonic`); for payloads that cannot be fully structured it falls back to the first 8 bytes (`source_issi:u32 LE`, `destination_issi:u32 LE`). If the destination ISSI is registered on another Basestation, the call stays on the Brew side; thereafter control messages and traffic-channel frames may flow in either direction between the two participating cells until `CALL_RELEASE`.

If the destination ISSI is *not* a registered subscriber, the call is offered to the SIP subsystem (Brew -> SIP) instead of being rejected outright: `[[sip.routes]]` entries are matched against a dialled string, which is the `BrewCircularCall`'s ASCII `number` field when the caller set one, falling back to the destination ISSI rendered as decimal otherwise. This is how a mobile terminal dialling an outside-line-style number (e.g. "9" + a 10-digit PSTN number) reaches a SIP trunk: the terminal sends `destination = 0` with the dialled digits in `number` (this is how FlowStation encodes a PBX/phone call — see its `cc_bs/procedures/setup.rs`), a route like `match_pattern = "9*"` selects it, and an optional `strip_prefix = "9"` on the route removes the leading digit before it reaches an empty-`number` `sip_trunk` destination, so the trunk dials the bare 10 digits. See `[[sip.routes]]` in [SIP / VoIP](SIP-VoIP.md).

**Duplex vs. PTT.** `build_circular_call_setup` (the server-originated `SETUP_REQUEST` for a SIP->Brew private call) sets `duplex=1` and `method=1` in the `BrewCircularCall` payload. There is no separate "PBX"/"phone" call type in TETRA CMCE to select instead (`communication` only has `P2p`/`P2Mp`/`P2MpAcked`/`Broadcast`, and `P2p` — already what this server sends — is correct for an individual call whether it's a radio-to-radio call or a bridged PSTN call); what actually matters is `duplex`/`method`. With both left at `0`, FlowStation's `cc_bs` presents the call as simplex with a PTT-style `TransmissionGrant` and non-hook signalling — the mobile terminal can only be "answered" by pressing PTT, never the real accept/green button, and audio doesn't behave like a normal duplex phone call even once picked up that way. `duplex=1` (full duplex) + `method=1` (hook signalling, i.e. the call requires an explicit user answer) make the terminal present and handle it as a genuine duplex phone call.

**Answering a Brew->SIP (MS-originated) call.** When a mobile terminal itself places the call and the SIP/PSTN side answers, the message that tells the MS "connected" is `CALL_CONNECT_REQUEST` (`build_circular_connect_request`, also with `duplex=1`/`method=1`) — *not* `CALL_CONNECT_CONFIRM`. FlowStation's `cc_bs` explicitly ignores `CALL_CONNECT_CONFIRM` for a call where the MS is the calling party (`fsm_on_network_circuit_connect_confirm` checks `calling_over_brew` and returns early otherwise); sending it left the terminal stuck showing "calling..." even after the far end had genuinely answered. `CALL_CONNECT_CONFIRM` remains correct for the opposite direction (SIP->Brew, in response to the ISSI's own `CALL_CONNECT_REQUEST`), where it's already what this server sends. The outbound `INVITE`'s `From`/`Contact` also now identify the call as `sip:<issi>@host` rather than a generic `sip:brew@host`, so the far end sees a real caller identity.

**Codec for a Brew->SIP call.** The transcoder for a Brew-originated leg is *not* started when the `INVITE` is sent — it's started once the SIP peer's `200 OK` actually arrives, using whichever of PCMU/PCMA that answer's own SDP picked (`BrewBridge::start_pending_media`, called from `on_sip_response`'s `200` case), not a guess made before the peer had even answered. Starting the transcoder early at a fixed assumption produced garbled audio in one direction and effectively nothing intelligible in the other whenever the peer answered PCMA instead of PCMU — the same bug the earlier "optimistic answer" note used to describe. The `RtpLeg` for this call, its Brew-side receive channel, and its target list are held in `BridgedLeg::pending_media` until the answer arrives; the leg's remote RTP address is also set explicitly from the answer's SDP at that point (`c=`/`m=audio`), rather than relying only on symmetric-RTP latching from the first inbound packet — the latter still applies as a fallback/NAT-safety net, but no longer as the *only* way this leg learns where to send audio.

Because current upstream Basestation does not yet expose a complete private-call Brew command path, this feature should be considered server-ready/experimental rather than end-to-end validated.
