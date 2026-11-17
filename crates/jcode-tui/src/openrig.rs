//! OpenRig community-harness launch protocol.
//!
//! OpenRig (<https://openrig.dev>) runs third-party agents in its seats from a
//! preset under `$OPENRIG_HOME/community-harnesses/<name>.yaml`. With
//! `launch_result: true`, OpenRig hands the launcher a per-attempt record path
//! and expects one JSON record naming the harness's own conversation id:
//!
//! ```json
//! { "format": 1, "attempt": "<OPENRIG_LAUNCH_ATTEMPT>", "harness": "jcode",
//!   "nativeRef": "<session id>", "producer": "jcode" }
//! ```
//!
//! With `resume: true` the saved id comes back in `OPENRIG_RESUME_REF`, and a
//! conversation that no longer exists is reported as `"error": "missing"` so
//! OpenRig never silently replaces it. `passes_model` sets `OPENRIG_MODEL` and
//! `first_input` points `OPENRIG_FIRST_INPUT` at a file holding the seat's
//! first prompt. jcode reads all of these directly, so the preset command is
//! just `jcode`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

pub const ENV_LAUNCH_RESULT: &str = "OPENRIG_LAUNCH_RESULT";
pub const ENV_LAUNCH_ATTEMPT: &str = "OPENRIG_LAUNCH_ATTEMPT";
pub const ENV_RESUME_REF: &str = "OPENRIG_RESUME_REF";
pub const ENV_MODEL: &str = "OPENRIG_MODEL";
pub const ENV_FIRST_INPUT: &str = "OPENRIG_FIRST_INPUT";
/// Preset name to report. OpenRig rejects a record whose harness differs from
/// the preset file name, so a preset named other than `jcode` sets this.
pub const ENV_HARNESS: &str = "JCODE_OPENRIG_HARNESS";

const PRODUCER: &str = "jcode";
static REPORTED: AtomicBool = AtomicBool::new(false);
static CAPTURED: OnceLock<LaunchEnv> = OnceLock::new();

/// Read the protocol variables once and remove them from this process's
/// environment, so a daemon this client spawns, the bash tool, and nested
/// jcode processes never inherit and overwrite this seat's identity.
pub fn capture() -> &'static LaunchEnv {
    CAPTURED.get_or_init(|| {
        let env = LaunchEnv::from_env();
        for key in [ENV_LAUNCH_RESULT, ENV_LAUNCH_ATTEMPT, ENV_RESUME_REF, ENV_MODEL, ENV_FIRST_INPUT, ENV_HARNESS] {
            crate::env::remove_var(key);
        }
        env
    })
}

/// Launch request read from the environment OpenRig sets for this process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchEnv {
    pub result_path: Option<PathBuf>,
    pub attempt: Option<String>,
    pub resume_ref: Option<String>,
    pub model: Option<String>,
    pub first_input: Option<PathBuf>,
    pub harness: String,
}

impl LaunchEnv {
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        Self {
            result_path: get(ENV_LAUNCH_RESULT).map(PathBuf::from),
            attempt: get(ENV_LAUNCH_ATTEMPT),
            resume_ref: get(ENV_RESUME_REF),
            model: get(ENV_MODEL),
            first_input: get(ENV_FIRST_INPUT).map(PathBuf::from),
            harness: get(ENV_HARNESS).unwrap_or_else(|| PRODUCER.to_string()),
        }
    }

    fn reporter(&self) -> Option<(&Path, &str)> {
        Some((self.result_path.as_deref()?, self.attempt.as_deref()?))
    }

    pub fn is_openrig_launch(&self) -> bool {
        self.reporter().is_some()
            || self.resume_ref.is_some()
            || self.model.is_some()
            || self.first_input.is_some()
    }
}

fn record_json(env: &LaunchEnv, attempt: &str, native_ref: Option<&str>) -> serde_json::Value {
    let mut record = serde_json::json!({
        "format": 1,
        "attempt": attempt,
        "harness": env.harness,
        "producer": PRODUCER,
    });
    match native_ref {
        Some(id) => record["nativeRef"] = id.into(),
        None => record["error"] = "missing".into(),
    }
    record
}

/// Write via a sibling temp file and rename, so OpenRig's poller never reads a
/// half-written record.
fn write_record(path: &Path, record: &serde_json::Value) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("jcode-{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(record)?)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Report the session this launch opened. Only the first id is reported.
pub fn report_session(session_id: &str) {
    report_with(capture(), Some(session_id));
}

/// Report that the requested conversation is gone and nothing was started.
pub fn report_missing() {
    report_with(capture(), None);
}

pub fn report_with(env: &LaunchEnv, native_ref: Option<&str>) -> bool {
    let Some((path, attempt)) = env.reporter() else {
        return false;
    };
    if REPORTED.swap(true, Ordering::SeqCst) {
        return false;
    }
    match write_record(path, &record_json(env, attempt, native_ref)) {
        Ok(()) => true,
        Err(err) => {
            crate::logging::warn(&format!(
                "OpenRig launch record {} not written: {err}",
                path.display()
            ));
            false
        }
    }
}

/// The seat's first prompt, if OpenRig staged one.
pub fn read_first_input(env: &LaunchEnv) -> Option<String> {
    let text = std::fs::read_to_string(env.first_input.as_deref()?).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Variables that name one particular seat. The shared daemon must never carry
/// them: whichever seat happened to spawn it would otherwise lend its identity
/// to every other session's shell tools (`rig whoami`, `rig send` senders).
pub const SEAT_SCOPED_ENV: &[&str] = &[
    "OPENRIG_SESSION_NAME",
    "OPENRIG_NODE_ID",
    "OPENRIG_OCCUPANT_GENERATION",
    "OPENRIG_RUNTIME",
    "OPENRIG_TRANSCRIPTS_ENABLED",
    "OPENRIG_TRANSCRIPTS_PATH",
    "OPENRIG_TRANSCRIPTS_LINES",
    "OPENRIG_TRANSCRIPTS_POLL_INTERVAL_SECONDS",
    "RIGGED_SESSION_NAME",
    "RIGGED_NODE_ID",
];

/// Public seat identity forwarded into this session's tool environment, so a
/// shell tool running on the shared daemon acts as this seat. Only the public
/// address and node id (plus where to reach OpenRig) are forwarded: never the
/// activity-hook token, run grant, or occupant generation.
const SEAT_IDENTITY_ENV: &[&str] = &[
    "OPENRIG_SESSION_NAME",
    "OPENRIG_NODE_ID",
    "OPENRIG_URL",
    "OPENRIG_HOST",
    "OPENRIG_PORT",
    "OPENRIG_HOME",
];

static SEAT_IDENTITY: OnceLock<std::collections::HashMap<String, String>> = OnceLock::new();

/// Whether this client runs as an OpenRig seat. A seat keeps its idle screen
/// byte-silent: OpenRig reads any pane output as work, which would override
/// the seat's own idle report. Decorative periodic output (rotating tips,
/// terminal background polling) is suppressed.
pub fn is_seat() -> bool {
    !seat_identity().is_empty()
}

/// This client's seat identity overlay, read once at startup. Empty unless
/// the process was launched as an OpenRig seat (session name present).
pub fn seat_identity() -> &'static std::collections::HashMap<String, String> {
    SEAT_IDENTITY.get_or_init(|| seat_identity_from_lookup(|key| std::env::var(key).ok()))
}

pub fn seat_identity_from_lookup(
    get: impl Fn(&str) -> Option<String>,
) -> std::collections::HashMap<String, String> {
    let get = |key: &str| get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    if get("OPENRIG_SESSION_NAME").is_none() {
        return std::collections::HashMap::new();
    }
    SEAT_IDENTITY_ENV
        .iter()
        .filter_map(|key| Some((key.to_string(), get(key)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> LaunchEnv {
        let map: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        LaunchEnv::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn parses_protocol_env_and_defaults_harness() {
        let e = env(&[(ENV_RESUME_REF, " session_x "), (ENV_MODEL, ""), (ENV_LAUNCH_ATTEMPT, "a")]);
        assert_eq!(e.resume_ref.as_deref(), Some("session_x"));
        assert_eq!(e.model, None);
        assert_eq!(e.harness, "jcode");
        assert!(e.reporter().is_none());
        assert!(e.is_openrig_launch());
        assert!(!env(&[]).is_openrig_launch());
    }

    #[test]
    fn record_matches_openrig_schema() {
        let e = env(&[(ENV_HARNESS, "jcode-opus")]);
        let ok = record_json(&e, "att", Some("session_fox_1"));
        assert_eq!(ok["format"], 1);
        assert_eq!(ok["attempt"], "att");
        assert_eq!(ok["harness"], "jcode-opus");
        assert_eq!(ok["nativeRef"], "session_fox_1");
        assert_eq!(ok["producer"], "jcode");
        let gone = record_json(&e, "att", None);
        assert_eq!(gone["error"], "missing");
        assert!(gone.get("nativeRef").is_none());
    }

    #[test]
    fn writes_record_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("att.json");
        write_record(&path, &record_json(&env(&[]), "att", Some("s1"))).unwrap();
        let read: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(read["nativeRef"], "s1");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn reads_trimmed_first_input() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.txt");
        std::fs::write(&path, "  hello seat \n").unwrap();
        let e = env(&[(ENV_FIRST_INPUT, path.to_str().unwrap())]);
        assert_eq!(read_first_input(&e).as_deref(), Some("hello seat"));
    }
}
