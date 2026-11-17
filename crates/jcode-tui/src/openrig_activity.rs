//! OpenRig activity reporting (harness-runner-contract §4).
//!
//! A seat whose preset declares `runner: true` is expected to post turn
//! boundaries to `POST <base>/api/activity/hooks`, which gives it the
//! authoritative lifecycle-hooks rung instead of window sampling alone
//! (activity reads working/idle instead of unknown). jcode is its own
//! runner: the TUI client observes every turn boundary first-hand.
//!
//! Contract points honored here:
//! - posts go one at a time, in order, with a short timeout, off the
//!   agent's threads (a background thread owns the socket);
//! - `sequence` rises across the whole process, `occurredAt` is supplied;
//! - working evidence is re-posted on meaningful transitions and throttled,
//!   never per streaming delta;
//! - `Stop`/`<preset>_exited` is posted only on observed process exit;
//! - the hook token and run id are scrubbed from this process's
//!   environment at capture so children never inherit them.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

pub const ENV_URL: &str = "OPENRIG_URL";
pub const ENV_HOST: &str = "OPENRIG_HOST";
pub const ENV_PORT: &str = "OPENRIG_PORT";
pub const ENV_TOKEN: &str = "OPENRIG_ACTIVITY_HOOK_TOKEN";
pub const ENV_NODE_ID: &str = "OPENRIG_NODE_ID";
pub const ENV_SESSION_NAME: &str = "OPENRIG_SESSION_NAME";
pub const ENV_GENERATION: &str = "OPENRIG_OCCUPANT_GENERATION";
pub const ENV_RUN_ID: &str = "OPENRIG_RUN_ID";
pub const ENV_RUNTIME: &str = "OPENRIG_RUNTIME";
pub const ENV_HOME: &str = "OPENRIG_HOME";

/// Minimum spacing between working posts; boundaries always post.
const WORKING_MIN_GAP: Duration = Duration::from_secs(2);
/// A repeat of the same working evidence refreshes the 15-second hook
/// window well before it lapses.
const WORKING_REPEAT_GAP: Duration = Duration::from_secs(10);

static CAPTURED: OnceLock<ActivityEnv> = OnceLock::new();
static SENDER: OnceLock<Option<SyncSender<Event>>> = OnceLock::new();
static TURNS: AtomicU64 = AtomicU64::new(0);

/// Read the reporting variables once, scrubbing the credential pair from
/// this process's environment so spawned children (the daemon, editors,
/// nested jcode) never see the install token or run grant.
pub fn capture() -> &'static ActivityEnv {
    CAPTURED.get_or_init(|| {
        let env = ActivityEnv::from_lookup(|key| std::env::var(key).ok());
        for key in [ENV_TOKEN, ENV_RUN_ID] {
            crate::env::remove_var(key);
        }
        env
    })
}

/// Re-export the scrubbed credentials before an in-pane exec (reload or
/// update), so the successor jcode keeps reporting for this seat.
pub fn reexport_for_exec() {
    let env = capture();
    if let Some(token) = env.token.as_deref() {
        crate::env::set_var(ENV_TOKEN, token);
    }
    if let Some(run_id) = env.run_id.as_deref() {
        crate::env::set_var(ENV_RUN_ID, run_id);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivityEnv {
    pub endpoint: Option<Endpoint>,
    pub token: Option<String>,
    pub node_id: Option<String>,
    pub session_name: Option<String>,
    pub generation: Option<String>,
    pub run_id: Option<String>,
    pub runtime: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
}

impl ActivityEnv {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let mut endpoint = get(ENV_URL).as_deref().and_then(parse_http_origin);
        if endpoint.is_none()
            && let Some(port) = get(ENV_PORT).and_then(|p| p.parse::<u16>().ok())
        {
            endpoint = Some(Endpoint {
                host: get(ENV_HOST).unwrap_or_else(|| "127.0.0.1".into()),
                port,
            });
        }
        let token = get(ENV_TOKEN);
        // The endpoint file may fill in a missing base URL, but never the
        // credential: the token must arrive through this process's own
        // environment (tmux session env or a run grant). Otherwise a nested
        // jcode spawned by the seat's tools (which sees the public session
        // name but a scrubbed token) would adopt the install token from the
        // file and post conflicting boundaries for the seat.
        if endpoint.is_none() && token.is_some() {
            let home = get(ENV_HOME)
                .map(std::path::PathBuf::from)
                .or_else(|| dirs::home_dir().map(|home| home.join(".openrig")));
            if let Some(file) = home.and_then(|dir| read_endpoint_file(&dir.join("activity-endpoint.json"))) {
                endpoint = file.0;
            }
        }
        Self {
            endpoint,
            token,
            node_id: get(ENV_NODE_ID),
            session_name: get(ENV_SESSION_NAME),
            generation: get(ENV_GENERATION),
            run_id: get(ENV_RUN_ID),
            runtime: get(ENV_RUNTIME).unwrap_or_else(|| "jcode".into()),
        }
    }

    /// Reporting needs a reachable ingest, a credential, and the seat's
    /// session name (install-token posts resolve the session by name).
    fn enabled(&self) -> bool {
        self.endpoint.is_some() && self.token.is_some() && self.session_name.is_some()
    }
}

/// `http://host:port` (or `http://host`, port 80). Https is not spoken
/// here: local ingest is plain http, and silently failing TLS would look
/// like a dead daemon.
fn parse_http_origin(url: &str) -> Option<Endpoint> {
    let rest = url.strip_prefix("http://")?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains('/') {
        return None;
    }
    // Bracketed IPv6 hosts keep their colons inside the brackets.
    let (host, port) = if let Some(bracketed) = rest.strip_prefix('[') {
        let (host, after) = bracketed.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if after.is_empty() => 80,
            None => return None,
        };
        (host.to_string(), port)
    } else if let Some((host, port)) = rest.rsplit_once(':') {
        (host.to_string(), port.parse().ok()?)
    } else {
        (rest.to_string(), 80)
    };
    (!host.is_empty()).then_some(Endpoint { host, port })
}

fn read_endpoint_file(path: &std::path::Path) -> Option<(Option<Endpoint>, String)> {
    let parsed: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let token = parsed.get("token")?.as_str()?.trim().to_string();
    if token.is_empty() {
        return None;
    }
    let endpoint = parsed.get("baseUrl").and_then(|v| v.as_str()).and_then(parse_http_origin);
    Some((endpoint, token))
}


#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    /// The seat submitted its Nth prompt (`UserPromptSubmit`/`accepted`).
    Prompt { turn: u64 },
    /// The agent produced work evidence (`active` or `PreToolUse`).
    Working { hook_event: &'static str, subtype: String },
    /// The turn ended (`Stop`/reason).
    Stopped { subtype: &'static str },
    /// The client process is exiting (`Stop`/`<runtime>_exited`).
    Exited,
}

fn sender() -> Option<&'static SyncSender<Event>> {
    SENDER
        .get_or_init(|| {
            let env = capture();
            if !env.enabled() {
                return None;
            }
            let (tx, rx) = std::sync::mpsc::sync_channel(256);
            std::thread::Builder::new()
                .name("openrig-activity".into())
                .spawn(move || reporter_loop(env, rx))
                .ok()?;
            Some(tx)
        })
        .as_ref()
}

fn enqueue(event: Event) {
    let Some(tx) = sender() else { return };
    match tx.try_send(event) {
        Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
    }
}

/// The seat accepted a submitted message as its next prompt.
pub fn prompt_submitted() {
    let turn = TURNS.fetch_add(1, Ordering::SeqCst) + 1;
    enqueue(Event::Prompt { turn });
}

/// Classify a server event into working evidence and post it; returns
/// whether this event hints the turn is ending by cancellation.
pub fn observe_server_event(event: &crate::protocol::ServerEvent, processing: bool) -> bool {
    use crate::protocol::ServerEvent;
    match event {
        ServerEvent::TurnEvent { event, .. } => return observe_server_event(event, processing),
        ServerEvent::Interrupted | ServerEvent::TurnStopped { .. } => return true,
        ServerEvent::ToolStart { name, .. } if processing => {
            enqueue(Event::Working { hook_event: "PreToolUse", subtype: name.clone() });
        }
        ServerEvent::TextDelta { .. }
        | ServerEvent::ReasoningDelta { .. }
        | ServerEvent::ToolExec { .. }
        | ServerEvent::BatchProgress { .. }
            if processing =>
        {
            enqueue(Event::Working { hook_event: "active", subtype: "agent_message".into() });
        }
        _ => {}
    }
    false
}

/// The turn ended; `cancelled` when the stop came from an interrupt.
pub fn turn_ended(cancelled: bool) {
    enqueue(Event::Stopped { subtype: if cancelled { "cancelled" } else { "end_turn" } });
}

/// The client is exiting for good (not a reload exec). Posts
/// `Stop`/`<runtime>_exited` and drains the queue, bounded by a short
/// deadline so quitting never hangs on a dead daemon.
pub fn report_exited() {
    let Some(tx) = sender() else { return };
    let _ = tx.try_send(Event::Exited);
    // The reporter posts with second-scale timeouts; give it a moment to
    // flush the exit boundary, then leave regardless.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && EXIT_FLUSHED.load(Ordering::SeqCst) == 0 {
        std::thread::sleep(Duration::from_millis(25));
    }
}

static EXIT_FLUSHED: AtomicU64 = AtomicU64::new(0);

struct ReporterState {
    sequence: u64,
    turn: Option<u64>,
    last_working: Option<(String, Instant)>,
    last_any_working: Option<Instant>,
    failures: u64,
}

impl ReporterState {
    fn new() -> Self {
        Self { sequence: 0, turn: None, last_working: None, last_any_working: None, failures: 0 }
    }

    /// Working posts are throttled; boundaries always pass.
    fn admit(&mut self, event: &Event, now: Instant) -> bool {
        let Event::Working { hook_event, subtype } = event else {
            if matches!(event, Event::Stopped { .. } | Event::Exited) {
                self.last_working = None;
                self.last_any_working = None;
            }
            return true;
        };
        let key = format!("{hook_event}/{subtype}");
        if let Some((last_key, at)) = &self.last_working
            && *last_key == key
            && now.duration_since(*at) < WORKING_REPEAT_GAP
        {
            return false;
        }
        if let Some(at) = self.last_any_working
            && now.duration_since(at) < WORKING_MIN_GAP
        {
            return false;
        }
        self.last_working = Some((key, now));
        self.last_any_working = Some(now);
        true
    }
}

fn reporter_loop(env: &'static ActivityEnv, rx: Receiver<Event>) {
    let mut state = ReporterState::new();
    while let Ok(event) = rx.recv() {
        let exiting = matches!(event, Event::Exited);
        if state.admit(&event, Instant::now()) {
            if let Event::Prompt { turn } = &event {
                state.turn = Some(*turn);
            }
            state.sequence += 1;
            let body = hook_body(env, &event, state.sequence, state.turn);
            match post_hook(env, &body) {
                Ok(status) if (200..300).contains(&status) => state.failures = 0,
                Ok(status) => note_failure(&mut state, &format!("ingest refused with status {status}")),
                Err(err) => note_failure(&mut state, &err),
            }
        }
        if exiting {
            break;
        }
    }
    EXIT_FLUSHED.store(1, Ordering::SeqCst);
}

fn note_failure(state: &mut ReporterState, detail: &str) {
    state.failures += 1;
    if state.failures == 1 || state.failures.is_multiple_of(50) {
        crate::logging::warn(&format!(
            "OpenRig activity post failed ({} consecutive): {detail}",
            state.failures
        ));
    }
}

fn hook_body(env: &ActivityEnv, event: &Event, sequence: u64, turn: Option<u64>) -> serde_json::Value {
    let (hook_event, subtype, turn_id) = match event {
        Event::Prompt { turn } => ("UserPromptSubmit", "accepted".to_string(), Some(*turn)),
        Event::Working { hook_event, subtype } => (*hook_event, subtype.clone(), turn),
        Event::Stopped { subtype } => ("Stop", (*subtype).to_string(), turn),
        Event::Exited => ("Stop", format!("{}_exited", env.runtime), None),
    };
    let mut body = serde_json::json!({
        "runtime": env.runtime,
        "hookEvent": hook_event,
        "subtype": subtype,
        "sequence": sequence,
        "occurredAt": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    });
    if let Some(name) = env.session_name.as_deref() {
        body["sessionName"] = name.into();
    }
    if let Some(node) = env.node_id.as_deref() {
        body["nodeId"] = node.into();
    }
    if let Some(generation) = env.generation.as_deref() {
        body["generation"] = generation.into();
    }
    if let Some(turn) = turn_id {
        body["turnId"] = turn.to_string().into();
    }
    body
}

/// One plain HTTP/1.1 POST to the local ingest. A tiny hand-rolled client
/// keeps this crate free of an async runtime hop and never blocks longer
/// than its socket timeouts.
fn post_hook(env: &ActivityEnv, body: &serde_json::Value) -> Result<u16, String> {
    let endpoint = env.endpoint.as_ref().ok_or("no endpoint")?;
    let token = env.token.as_deref().ok_or("no token")?;
    let addr = (endpoint.host.as_str(), endpoint.port)
        .to_socket_addrs()
        .map_err(|err| format!("resolve {}: {err}", endpoint.host))?
        .next()
        .ok_or_else(|| format!("resolve {}: no address", endpoint.host))?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_millis(750))
        .map_err(|err| format!("connect {addr}: {err}"))?;
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
    let payload = body.to_string();
    let request = format!(
        "POST /api/activity/hooks HTTP/1.1\r\nHost: {}:{}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        endpoint.host,
        endpoint.port,
        payload.len(),
    );
    let mut stream = stream;
    stream.write_all(request.as_bytes()).map_err(|err| format!("write: {err}"))?;
    let mut response = [0u8; 256];
    let read = stream.read(&mut response).map_err(|err| format!("read: {err}"))?;
    let line = std::str::from_utf8(&response[..read]).unwrap_or("");
    line.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("malformed response: {:?}", line.lines().next().unwrap_or("")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> ActivityEnv {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        ActivityEnv::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn parses_url_and_host_port_endpoints() {
        assert_eq!(
            parse_http_origin("http://127.0.0.1:7433"),
            Some(Endpoint { host: "127.0.0.1".into(), port: 7433 })
        );
        assert_eq!(
            parse_http_origin("http://[::1]:7433"),
            Some(Endpoint { host: "::1".into(), port: 7433 })
        );
        assert_eq!(parse_http_origin("https://127.0.0.1:7433"), None);
        assert_eq!(parse_http_origin("http://127.0.0.1:7433/path"), None);

        let e = env(&[
            (ENV_PORT, "7433"),
            (ENV_TOKEN, "tok"),
            (ENV_SESSION_NAME, "lead@workshop"),
        ]);
        assert_eq!(e.endpoint, Some(Endpoint { host: "127.0.0.1".into(), port: 7433 }));
        assert!(e.enabled());
        assert!(!env(&[(ENV_TOKEN, "tok")]).enabled());
    }

    #[test]
    fn hook_body_matches_contract_shape() {
        let e = env(&[
            (ENV_URL, "http://127.0.0.1:7433"),
            (ENV_TOKEN, "tok"),
            (ENV_SESSION_NAME, "lead@workshop"),
            (ENV_NODE_ID, "node1"),
            (ENV_GENERATION, "gen1"),
            (ENV_RUNTIME, "jcode"),
        ]);
        let body = hook_body(&e, &Event::Prompt { turn: 7 }, 3, Some(7));
        assert_eq!(body["hookEvent"], "UserPromptSubmit");
        assert_eq!(body["subtype"], "accepted");
        assert_eq!(body["turnId"], "7");
        assert_eq!(body["sequence"], 3);
        assert_eq!(body["sessionName"], "lead@workshop");
        assert_eq!(body["nodeId"], "node1");
        assert_eq!(body["generation"], "gen1");
        assert_eq!(body["runtime"], "jcode");
        assert!(body["occurredAt"].as_str().unwrap().ends_with('Z'));

        let exited = hook_body(&e, &Event::Exited, 9, Some(7));
        assert_eq!(exited["hookEvent"], "Stop");
        assert_eq!(exited["subtype"], "jcode_exited");
        assert!(exited.get("turnId").is_none());

        let stop = hook_body(&e, &Event::Stopped { subtype: "end_turn" }, 10, Some(7));
        assert_eq!(stop["subtype"], "end_turn");
        assert_eq!(stop["turnId"], "7");
    }

    #[test]
    fn throttles_repeat_working_but_always_admits_boundaries() {
        let mut state = ReporterState::new();
        let t0 = Instant::now();
        let working = Event::Working { hook_event: "active", subtype: "agent_message".into() };
        assert!(state.admit(&working, t0));
        assert!(!state.admit(&working, t0 + Duration::from_secs(3)));
        assert!(state.admit(&working, t0 + Duration::from_secs(11)));
        // A different tool within the global gap is still spaced out.
        let tool = Event::Working { hook_event: "PreToolUse", subtype: "bash".into() };
        assert!(!state.admit(&tool, t0 + Duration::from_secs(12)));
        assert!(state.admit(&tool, t0 + Duration::from_secs(14)));
        // Boundaries always pass and reset the throttle.
        assert!(state.admit(&Event::Stopped { subtype: "end_turn" }, t0 + Duration::from_secs(14)));
        assert!(state.admit(&working, t0 + Duration::from_secs(14)));
    }

    #[test]
    fn endpoint_file_supplies_missing_base_only_with_env_token() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("activity-endpoint.json"),
            r#"{"baseUrl":"http://127.0.0.1:7433","token":"filetok"}"#,
        )
        .unwrap();
        let home = dir.path().to_str().unwrap().to_string();
        // With an env token, the file fills the endpoint.
        let map: HashMap<String, String> = [
            (ENV_HOME.to_string(), home.clone()),
            (ENV_TOKEN.to_string(), "envtok".to_string()),
            (ENV_SESSION_NAME.to_string(), "lead@workshop".to_string()),
        ]
        .into();
        let e = ActivityEnv::from_lookup(|k| map.get(k).cloned());
        assert_eq!(e.token.as_deref(), Some("envtok"));
        assert_eq!(e.endpoint, Some(Endpoint { host: "127.0.0.1".into(), port: 7433 }));
        assert!(e.enabled());
        // Without an env token, the file never supplies the credential: a
        // nested jcode inside the seat must not adopt the install token.
        let map: HashMap<String, String> = [
            (ENV_HOME.to_string(), home),
            (ENV_SESSION_NAME.to_string(), "lead@workshop".to_string()),
        ]
        .into();
        let nested = ActivityEnv::from_lookup(|k| map.get(k).cloned());
        assert_eq!(nested.token, None);
        assert!(!nested.enabled());
    }
}
