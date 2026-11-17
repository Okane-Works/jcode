use super::*;

type CapturedSystemPromptRequests = Arc<std::sync::Mutex<Vec<(String, Vec<Message>)>>>;

#[derive(Clone, Default)]
struct SystemPromptCaptureProvider {
    requests: CapturedSystemPromptRequests,
}

#[async_trait]
impl Provider for SystemPromptCaptureProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolDefinition],
        system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.requests
            .lock()
            .unwrap()
            .push((system.to_string(), messages.to_vec()));
        let (tx, rx) = tokio_mpsc::channel::<Result<StreamEvent>>(4);
        tokio::spawn(async move {
            let _ = tx.send(Ok(StreamEvent::TextDelta("ok".to_string()))).await;
            let _ = tx
                .send(Ok(StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                }))
                .await;
        });
        Ok(Box::pin(ReceiverStream::new(rx)))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }
}

/// A swarm DM wakes the recipient with a per-turn system reminder. Putting it
/// in the system prompt changed the prefix of every cached message, so each DM
/// cost a full KV cache miss, and the next turn missed again when it vanished.
/// The reminder must ride in the transcript and leave the system prompt alone.
#[tokio::test]
async fn turn_system_reminder_does_not_change_the_system_prompt() {
    let _guard = crate::storage::lock_test_env();
    let capture = SystemPromptCaptureProvider::default();
    let requests = capture.requests.clone();
    let provider: Arc<dyn Provider> = Arc::new(capture);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("first", Vec::new(), None, tx.clone())
        .await
        .expect("plain turn");
    agent
        .run_once_streaming_mpsc(
            "DM from fox: hello",
            Vec::new(),
            Some("You just received a direct swarm message from fox.".to_string()),
            tx.clone(),
        )
        .await
        .expect("reminder turn");
    agent
        .run_once_streaming_mpsc("third", Vec::new(), None, tx)
        .await
        .expect("follow-up turn");

    let injections: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|event| match event {
            ServerEvent::SoftInterruptInjected {
                content,
                display_role,
                ..
            } => {
                assert_eq!(display_role.as_deref(), Some("system"));
                Some(content)
            }
            _ => None,
        })
        .collect();
    assert_eq!(injections.len(), 1, "ordinary prompts must not be echoed");
    assert!(injections[0].contains("direct swarm message from fox"));

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests.iter().all(|(system, _)| system == &requests[0].0),
        "the system prompt must be identical across reminder and plain turns"
    );
    assert!(!requests[1].0.contains("direct swarm message"));

    let reminder_turn = &requests[1].1;
    let last = reminder_turn.last().map(message_text).unwrap_or_default();
    assert!(last.starts_with("<system-reminder>"), "got {last:?}");
    assert!(last.contains("direct swarm message from fox"));

    // The follow-up turn replays the reminder turn unchanged as its prefix.
    let follow_up = &requests[2].1;
    assert!(follow_up.len() > reminder_turn.len());
    for (before, after) in reminder_turn.iter().zip(follow_up.iter()) {
        assert_eq!(message_text(before), message_text(after));
    }
}

#[tokio::test]
async fn pending_alerts_and_reminder_only_turn_publish_context_entries() {
    let _guard = crate::storage::lock_test_env();
    let provider: Arc<dyn Provider> = Arc::new(SystemPromptCaptureProvider::default());
    let mut agent = Agent::new(provider, Registry::empty());
    agent.push_alert("peer alert: inspect the shared file".to_string());
    agent.push_alert("peer alert: inspect the shared file".to_string());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("", vec![], Some("Resume after reload".into()), tx)
        .await
        .unwrap();
    let entries: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|event| match event {
            ServerEvent::SoftInterruptInjected {
                content,
                display_role,
                client_ids,
                ..
            } => {
                assert_eq!(display_role.as_deref(), Some("system"));
                assert!(client_ids.is_empty());
                Some(content)
            }
            _ => None,
        })
        .collect();
    assert_eq!(entries.len(), 3);
    assert!(entries[0].contains("peer alert: inspect the shared file"));
    assert_eq!(
        entries[0], entries[1],
        "repeated alerts remain separate entries"
    );
    assert_eq!(entries[2], "[auto] Resume after reload");
    let persisted = crate::session::Session::load(&agent.session.id).unwrap();
    for entry in entries {
        assert!(
            persisted
                .messages
                .iter()
                .any(|message| content_text(&message.content) == entry
                    && message.display_role == Some(crate::session::StoredDisplayRole::System))
        );
    }
}

#[tokio::test]
async fn late_skill_announcement_publishes_one_new_context_entry() {
    let _guard = crate::storage::lock_test_env();
    let project = tempfile::tempdir().unwrap();
    let provider: Arc<dyn Provider> = Arc::new(SystemPromptCaptureProvider::default());
    let mut agent = Agent::new_with_initial_working_dir(
        provider,
        Registry::empty(),
        Some(project.path().to_str().unwrap()),
    );
    // Freeze the session's initial skill snapshot before installing a skill.
    agent.build_system_prompt_split(None);
    let skill_dir = project.path().join(".jcode/skills/context-entry-test");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: context-entry-test\ndescription: Late announcement test\n---\n\nTest body.\n",
    )
    .unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    for prompt in ["first", "second"] {
        agent
            .run_once_streaming_mpsc(prompt, vec![], None, tx.clone())
            .await
            .unwrap();
    }
    let entries: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|event| match event {
            ServerEvent::SoftInterruptInjected { content, .. } => Some(content),
            _ => None,
        })
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].contains("New skills were installed."));
    assert!(entries[0].contains("context-entry-test"));
}

/// Point `user_prompt_submit` at a script that appends each stdin event to
/// `record` (one JSON object per line) and returns `context`.
#[cfg(unix)]
fn prompt_context_hook(dir: &std::path::Path, context: &str) -> (impl Drop + use<>, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    struct EnvReset(Option<std::ffi::OsString>);
    impl Drop for EnvReset {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => crate::env::set_var("JCODE_HOOK_USER_PROMPT_SUBMIT", value),
                None => crate::env::remove_var("JCODE_HOOK_USER_PROMPT_SUBMIT"),
            }
            crate::config::invalidate_config_cache();
        }
    }
    let record = dir.join("events.jsonl");
    let script = dir.join("prompt-context.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\ncat >> '{record}'\necho >> '{record}'\nprintf '%s' '{{\"systemMessage\":\"user notice\",\"hookSpecificOutput\":{{\"additionalContext\":\"{context}\"}}}}'\n",
            record = record.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let reset = EnvReset(std::env::var_os("JCODE_HOOK_USER_PROMPT_SUBMIT"));
    crate::env::set_var("JCODE_HOOK_USER_PROMPT_SUBMIT", &script);
    // The config cache only re-reads env overrides on its check interval.
    crate::config::invalidate_config_cache();
    (reset, record)
}

#[cfg(unix)]
fn recorded_prompt_events(record: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(record)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("hook stdin is one JSON object"))
        .collect()
}

/// A genuine prompt runs `user_prompt_submit`; its context lands in the
/// transcript right after the user message as a system-role reminder and is
/// part of the provider request. Machine-authored turns never run the hook.
#[cfg(unix)]
#[tokio::test]
async fn user_prompt_submit_context_follows_the_prompt_and_skips_synthetic_turns() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().unwrap();
    let (_env, record) = prompt_context_hook(temp.path(), "recalled: the deploy target is fly");
    let capture = SystemPromptCaptureProvider::default();
    let requests = capture.requests.clone();
    let provider: Arc<dyn Provider> = Arc::new(capture);
    let mut agent = Agent::new(provider, Registry::empty());
    let session_id = agent.session.id.clone();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    agent
        .run_once_streaming_mpsc("where do we deploy?", Vec::new(), None, tx.clone())
        .await
        .expect("user turn");

    let events = recorded_prompt_events(&record);
    assert_eq!(events.len(), 1, "one hook run per user prompt");
    let event = &events[0];
    assert_eq!(event["event"], "user_prompt_submit");
    assert_eq!(event["prompt"], "where do we deploy?");
    assert_eq!(event["session_id"], session_id.as_str());
    assert_eq!(event["source"], "chat");
    assert_eq!(
        event["transcript_path"],
        crate::session::session_path(&session_id)
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );

    let expected = "<system-reminder>\nrecalled: the deploy target is fly\n</system-reminder>";
    let persisted = crate::session::Session::load(&session_id).unwrap();
    let prompt_index = persisted
        .messages
        .iter()
        .position(|message| content_text(&message.content).starts_with("where do we deploy?"))
        .expect("user message persisted");
    assert_eq!(persisted.messages[prompt_index].display_role, None);
    let reminder = &persisted.messages[prompt_index + 1];
    assert_eq!(content_text(&reminder.content), expected);
    assert_eq!(
        reminder.display_role,
        Some(crate::session::StoredDisplayRole::System)
    );
    assert!(
        !persisted
            .messages
            .iter()
            .any(|message| content_text(&message.content).contains("user notice")),
        "systemMessage is for the user and must not reach the model"
    );

    {
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let last = requests[0].1.last().map(message_text).unwrap_or_default();
        assert_eq!(last, expected, "the provider request ends with the context");
    }
    let injected: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|event| match event {
            ServerEvent::SoftInterruptInjected { content, .. } => Some(content),
            _ => None,
        })
        .collect();
    assert_eq!(injected, [expected], "clients see the context entry once");

    // Reminder-only continuation, a machine-authored message, and a failover
    // retry are not user submissions.
    agent
        .run_once_streaming_mpsc(
            "",
            Vec::new(),
            Some("Resume after reload".into()),
            tx.clone(),
        )
        .await
        .expect("continuation turn");
    agent
        .run_once_streaming_mpsc_with_display_role(
            "background task finished",
            Vec::new(),
            None,
            tx.clone(),
            Some(crate::session::StoredDisplayRole::BackgroundTask),
        )
        .await
        .expect("background wake turn");
    let lease = agent.claim_server_turn().expect("turn lease");
    agent
        .continue_turn_after_failover(&lease, None, tx)
        .await
        .expect("failover retry");
    drop(lease);
    assert_eq!(
        recorded_prompt_events(&record).len(),
        1,
        "synthetic turns must not run user_prompt_submit"
    );
}

/// `jcode run` goes through `run_once`, which must deliver the same context.
#[cfg(unix)]
#[tokio::test]
async fn user_prompt_submit_context_reaches_the_run_once_path() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().unwrap();
    let (_env, record) = prompt_context_hook(temp.path(), "recalled for run");
    let capture = SystemPromptCaptureProvider::default();
    let requests = capture.requests.clone();
    let provider: Arc<dyn Provider> = Arc::new(capture);
    let mut agent = Agent::new(provider, Registry::empty());

    agent.run_once("one shot prompt").await.expect("run turn");

    let events = recorded_prompt_events(&record);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["prompt"], "one shot prompt");
    assert_eq!(events[0]["source"], "run");
    let requests = requests.lock().unwrap();
    let last = requests[0].1.last().map(message_text).unwrap_or_default();
    assert_eq!(
        last,
        "<system-reminder>\nrecalled for run\n</system-reminder>"
    );
}
