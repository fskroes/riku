//! The live local-session pipeline shared by the Board and the Collector.
//!
//! [`Engine`] owns the runtime-dependent work around the runtime-agnostic
//! `collector` crate: discovery, filesystem watching, periodic status refresh,
//! diff enrichment, machine stamping, and broadcasting complete local events.
//! Callers use its small interface to take a snapshot, subscribe to local changes,
//! or resolve a local transcript; transport and presentation stay in their
//! adapters.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use collector::{
    Change, ClaudeSource, CodexSource, DiffCache, Event, Session, SessionSource, SessionStore,
    WatchGuard,
};
use tokio::sync::broadcast;
use tracing::{info, warn};

/// How often the Engine re-evaluates status so a quiet session can become
/// `Finished` without another filesystem event.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// A running local-session pipeline. Dropping it stops its filesystem watcher.
pub struct Engine {
    store: Arc<Mutex<SessionStore>>,
    events: broadcast::Sender<Event>,
    diff_cache: Arc<DiffCache>,
    machine: Arc<str>,
    // The guard is intentionally private: callers only need the Engine's
    // snapshot/event interface, but it must live as long as that interface does.
    _watch_guard: Option<WatchGuard>,
}

impl Engine {
    /// Discover sessions below the Claude and optional Codex roots, then start the
    /// live watcher and periodic refresh loop. Must be called inside a Tokio
    /// runtime because the refresh loop is spawned onto it.
    pub fn start(
        claude_root: PathBuf,
        codex_root: Option<PathBuf>,
        machine: impl Into<Arc<str>>,
    ) -> Self {
        // Canonicalize so filesystem event paths match discovery keys. A root that
        // does not exist yet keeps its given spelling; discovery and watching then
        // degrade gracefully.
        let claude_root = std::fs::canonicalize(&claude_root).unwrap_or(claude_root);
        let codex_root = codex_root.map(|root| std::fs::canonicalize(&root).unwrap_or(root));

        let mut sources: Vec<Box<dyn SessionSource>> =
            vec![Box::new(ClaudeSource::new(claude_root))];
        if let Some(root) = codex_root {
            sources.push(Box::new(CodexSource::new(root)));
        }
        let roots: Vec<PathBuf> = sources.iter().flat_map(|source| source.roots()).collect();
        let store = Arc::new(Mutex::new(SessionStore::new(sources)));
        let (events, _) = broadcast::channel::<Event>(1024);
        let diff_cache = Arc::new(DiffCache::new());
        let machine = machine.into();

        let count = store.lock().unwrap().scan(Utc::now()).len();
        info!(?roots, sessions = count, machine = %machine, "discovered sessions");

        let watch_guard = start_watch(
            &roots,
            store.clone(),
            events.clone(),
            diff_cache.clone(),
            machine.clone(),
        );
        spawn_refresh(
            store.clone(),
            events.clone(),
            diff_cache.clone(),
            machine.clone(),
        );

        Engine {
            store,
            events,
            diff_cache,
            machine,
            _watch_guard: watch_guard,
        }
    }

    /// A current, fully enriched and machine-stamped view of local sessions.
    /// This can invoke git, so async adapters should call it on a blocking worker.
    pub fn snapshot(&self) -> Vec<Session> {
        let mut sessions = self.store.lock().unwrap().snapshot(Utc::now());
        for session in &mut sessions {
            self.diff_cache.enrich(session);
            stamp(session, &self.machine);
        }
        sessions
    }

    /// Subscribe to complete local-session events. Every `Upsert` is enriched and
    /// stamped before it is sent; a lagged receiver can recover with [`snapshot`](Self::snapshot).
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Resolve a local session's trusted transcript path for deep-linking.
    pub fn find_by_id(&self, id: &str) -> Option<(PathBuf, Session)> {
        self.store.lock().unwrap().find_by_id(id, Utc::now())
    }

    /// Return local sessions in one working directory. The Board uses this to
    /// build Work Links without exposing the store itself.
    pub fn sessions_in(&self, cwd: &str) -> Vec<Session> {
        self.store
            .lock()
            .unwrap()
            .snapshot(Utc::now())
            .into_iter()
            .filter(|session| session.cwd.as_deref() == Some(cwd))
            .collect()
    }
}

/// This machine's name, never empty. Board and Collector both use it to label
/// sessions consistently; a caller may still supply an explicit name to `start`.
pub fn local_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|hostname| hostname.into_string().ok())
        .filter(|hostname| !hostname.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Stamp a session unless it already carries a remote machine identity.
pub fn stamp(session: &mut Session, machine: &str) {
    if session.machine.is_none() {
        session.machine = Some(machine.to_string());
    }
}

fn finalize(event: &mut Event, diff_cache: &DiffCache, machine: &str) {
    if let Event::Upsert(session) = event {
        diff_cache.enrich(session);
        stamp(session, machine);
    }
}

fn start_watch(
    roots: &[PathBuf],
    store: Arc<Mutex<SessionStore>>,
    events: broadcast::Sender<Event>,
    diff_cache: Arc<DiffCache>,
    machine: Arc<str>,
) -> Option<WatchGuard> {
    match collector::watch(roots, move |change| {
        let event = {
            let mut store = store.lock().unwrap();
            match change {
                Change::Modified(path) => store.ingest(&path, Utc::now()),
                Change::Removed(path) => store.remove(&path),
            }
        };
        // The watcher owns a dedicated thread, keeping git work off the runtime.
        if let Some(mut event) = event {
            finalize(&mut event, &diff_cache, &machine);
            let _ = events.send(event);
        }
    }) {
        Ok(guard) => Some(guard),
        Err(error) => {
            warn!(error = %error, "failed to start filesystem watcher; live updates disabled");
            None
        }
    }
}

fn spawn_refresh(
    store: Arc<Mutex<SessionStore>>,
    events: broadcast::Sender<Event>,
    diff_cache: Arc<DiffCache>,
    machine: Arc<str>,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let mut refreshed = store.lock().unwrap().refresh(Utc::now());
            let cache = diff_cache.clone();
            let machine = machine.clone();
            let events_to_send = tokio::task::spawn_blocking(move || {
                for event in &mut refreshed {
                    finalize(event, &cache, &machine);
                }
                refreshed
            })
            .await
            .expect("diff enrichment does not panic");
            for event in events_to_send {
                let _ = events.send(event);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{local_hostname, stamp, Engine};
    use chrono::Utc;
    use collector::{Session, Status, Tool};

    fn bare_session() -> Session {
        Session {
            id: "s".into(),
            tool: Tool::Claude,
            project: "p".into(),
            model: None,
            branch: None,
            cwd: None,
            tokens_in: 0,
            tokens_out: 0,
            activity: None,
            last_event_at: Utc::now(),
            status: Status::Active,
            attention_reason: None,
            cost_usd: None,
            diff: None,
            machine: None,
        }
    }

    #[test]
    fn stamps_an_unlabelled_session_without_overwriting_a_remote_label() {
        let mut session = bare_session();
        stamp(&mut session, "loki.local");
        assert_eq!(session.machine.as_deref(), Some("loki.local"));
        stamp(&mut session, "forge-desktop");
        assert_eq!(session.machine.as_deref(), Some("loki.local"));
    }

    #[test]
    fn local_hostname_is_never_empty() {
        assert!(!local_hostname().is_empty());
    }

    #[tokio::test]
    async fn snapshot_exposes_an_enriched_machine_stamped_local_session() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("-Users-x-repos-foo");
        fs::create_dir_all(&project).unwrap();
        let transcript = serde_json::json!({
            "type": "assistant",
            "sessionId": "engine-1",
            "timestamp": "2026-07-19T10:00:00Z",
            "cwd": "/Users/x/repos/foo",
            "gitBranch": "main",
            "message": {
                "model": "claude-opus-4-8",
                "usage": { "input_tokens": 100, "output_tokens": 10 },
                "content": [{ "type": "text", "text": "through the engine" }]
            }
        })
        .to_string();
        fs::write(project.join("session.jsonl"), format!("{transcript}\n")).unwrap();

        let engine = Engine::start(root.path().to_path_buf(), None, "loki.local");
        let sessions = engine.snapshot();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "engine-1");
        assert_eq!(sessions[0].machine.as_deref(), Some("loki.local"));
    }
}
