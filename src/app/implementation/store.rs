//! SQLite is authoritative for execution states and native agent sessions.
use super::*;
use rusqlite::{Connection, params};

pub(super) struct Database(std::sync::Mutex<Connection>);
impl Database {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        security::protect_file(path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS agent_runs (
                id TEXT PRIMARY KEY, machine_id TEXT NOT NULL, provider TEXT NOT NULL,
                model TEXT NOT NULL, effort TEXT NOT NULL,
                work_item_provider TEXT NOT NULL, work_item_id INTEGER NOT NULL,
                work_item_project TEXT NOT NULL, work_item_url TEXT NOT NULL,
                session_id TEXT, state TEXT NOT NULL, requested_at TEXT NOT NULL,
                started_at TEXT, finished_at TEXT, payload TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS agent_runs_item ON agent_runs(work_item_url, requested_at);
            CREATE INDEX IF NOT EXISTS agent_runs_session ON agent_runs(provider, session_id);
            CREATE INDEX IF NOT EXISTS agent_runs_state ON agent_runs(state);",
        )?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS agent_run_events (sequence INTEGER PRIMARY KEY AUTOINCREMENT,run_id TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS agent_session_index (run_id TEXT PRIMARY KEY,machine_id TEXT NOT NULL,item_url TEXT NOT NULL,requested_at TEXT NOT NULL,metadata TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS agent_session_item ON agent_session_index(item_url,requested_at);
            BEGIN IMMEDIATE;
            INSERT INTO agent_run_events(run_id) SELECT id FROM agent_runs WHERE state IN ('running','planning');
            UPDATE agent_runs SET state='interrupted' WHERE state IN ('running','planning');
            COMMIT;")?;
        Ok(Self(std::sync::Mutex::new(db)))
    }
    pub(super) fn insert(&self, run: &Run) -> Result<()> {
        let launch = &run.launch;
        let request = &launch.request;
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute("INSERT INTO agent_runs (id,machine_id,provider,model,effort,work_item_provider,work_item_id,work_item_project,work_item_url,state,requested_at,payload) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![launch.id,request.machine,request.provider,request.model,request.effort,request.item.provider,request.item.id,request.item.project,launch.detail.item.url,run.state,run.created_at,serde_json::to_string(run)?])?;
        tx.execute(
            "INSERT INTO agent_run_events(run_id) VALUES (?1)",
            [&launch.id],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn list(
        &self,
        url: Option<&str>,
        id: Option<&str>,
        active: bool,
    ) -> Result<Vec<Run>> {
        let db = self.0.lock().unwrap();
        let mut statement=db.prepare("SELECT payload,state,session_id,started_at,finished_at FROM agent_runs WHERE (?1 IS NULL OR work_item_url=?1) AND (?2 IS NULL OR id=?2) AND (?3=0 OR state IN ('running','planning')) ORDER BY requested_at DESC LIMIT CASE WHEN ?3=1 THEN -1 ELSE 100 END")?;
        let rows = statement.query_map(params![url, id, active], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut runs = Vec::new();
        for row in rows {
            let (payload, state, session_id, started_at, finished_at) = row?;
            let mut run: Run = serde_json::from_str(&payload)?;
            run.state = state;
            run.session_id = session_id;
            run.started_at = started_at;
            run.finished_at = finished_at;
            runs.push(run);
        }
        Ok(runs)
    }
    pub(super) fn get(&self, id: &str) -> Result<Option<Run>> {
        Ok(self.list(None, Some(id), false)?.pop())
    }
    pub(super) fn session(&self, id: &str, session: Option<&str>) -> Result<()> {
        if let Some(session) = session {
            ensure!(
                (1..=200).contains(&session.len())
                    && session
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
                "Invalid native agent session ID."
            );
        }
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        if tx.execute("UPDATE agent_runs SET session_id=COALESCE(?2,session_id),started_at=COALESCE(started_at,?3) WHERE id=?1 AND state IN ('running','planning')",params![id,session,chrono::Utc::now().to_rfc3339()])?>0 { tx.execute("INSERT INTO agent_run_events(run_id) VALUES (?1)",[id])?; }
        tx.commit()?;
        Ok(())
    }
    pub(super) fn finish(
        &self,
        id: &str,
        state: &str,
        output: &str,
        plan: Option<Value>,
    ) -> Result<()> {
        let mut run = self.get(id)?.context("Execution record unavailable.")?;
        if !["running", "planning"].contains(&run.state.as_str()) {
            return Ok(());
        }
        run.state = state.into();
        run.output = output.into();
        run.plan = plan;
        let finished = chrono::Utc::now().to_rfc3339();
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        if tx.execute("UPDATE agent_runs SET state=?2,finished_at=?3,payload=?4 WHERE id=?1 AND state IN ('running','planning')",params![id,state,finished,serde_json::to_string(&run)?])?>0 { tx.execute("INSERT INTO agent_run_events(run_id) VALUES (?1)",[id])?; }
        tx.commit()?;
        Ok(())
    }
    pub(super) fn changes(&self, cursor: i64) -> Result<(i64, Vec<SessionSummary>)> {
        let events = {
            let db = self.0.lock().unwrap();
            let mut query=db.prepare("SELECT sequence,run_id FROM agent_run_events WHERE sequence>?1 ORDER BY sequence LIMIT 50")?;
            query
                .query_map([cursor], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        let next = events.last().map_or(cursor, |e| e.0);
        let mut runs = Vec::new();
        for id in events
            .into_iter()
            .map(|e| e.1)
            .collect::<std::collections::BTreeSet<_>>()
        {
            if let Some(run) = self.get(&id)? {
                runs.push(run.into());
            }
        }
        Ok((next, runs))
    }
    pub(super) fn cache(&self, machine: &str, sessions: Vec<SessionSummary>) -> Result<()> {
        ensure!(sessions.len() <= 100, "Too many session updates.");
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        for session in sessions {
            uuid(&session.id)?;
            ensure!(
                session.machine == machine
                    && ["codex", "claude", "opencode"].contains(&session.provider.as_str()),
                "Invalid agent session owner."
            );
            crate::tools::todo::validate_link(&session.item.url)?;
            let metadata = serde_json::to_string(&session)?;
            ensure!(metadata.len() <= 6_000, "Session metadata is too large.");
            tx.execute("INSERT INTO agent_session_index(run_id,machine_id,item_url,requested_at,metadata) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(run_id) DO UPDATE SET metadata=excluded.metadata WHERE machine_id=excluded.machine_id",params![session.id,machine,session.item.url,session.created_at,metadata])?;
        }
        tx.commit()?;
        Ok(())
    }
    pub(super) fn sessions(&self, urls: &[String]) -> Result<Vec<SessionSummary>> {
        let mut sessions = BTreeMap::new();
        for url in urls {
            for run in self.list(Some(url), None, false)? {
                sessions.insert(run.launch.id.clone(), SessionSummary::from(run));
            }
            let cached = {
                let db = self.0.lock().unwrap();
                let mut query=db.prepare("SELECT metadata FROM agent_session_index WHERE item_url=?1 ORDER BY requested_at DESC LIMIT 100")?;
                query
                    .query_map([url], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?
            };
            for cached in cached {
                let session: SessionSummary = serde_json::from_str(&cached)?;
                sessions.entry(session.id.clone()).or_insert(session);
            }
        }
        let mut sessions: Vec<_> = sessions.into_values().collect();
        sessions.sort_by(|a, b| {
            let active = |s: &SessionSummary| ["running", "planning"].contains(&s.state.as_str());
            active(b)
                .cmp(&active(a))
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        // Cards need a bounded index; complete run records remain in agent_runs.
        let mut counts = BTreeMap::<String, usize>::new();
        sessions.retain(|session| {
            let count = counts.entry(session.item.url.clone()).or_default();
            *count += 1;
            *count <= 3
        });
        Ok(sessions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Run {
        serde_json::from_value(json!({"launch":{"id":"11111111-1111-4111-8111-111111111111","request":{"machine":"22222222-2222-4222-8222-222222222222","item":{"provider":"azure","id":7,"project":"Payments","source":"azure","task":true},"context":"Synthetic task","directories":["/work"],"provider":"codex","model":"test-model","effort":"high"},"detail":{"item":{"id":7,"title":"Synthetic task","url":"https://dev.azure.com/example/Payments/_workitems/edit/7","project":"Payments","state":"Active","kind":"Task","created_at":null,"dates":[]},"fields":[],"comments":[],"relations":[],"history":[],"partial":false,"warnings":[]},"language":"en","planning":false},"state":"running","created_at":"2026-10-24T12:00:00Z","finished_at":null,"output":"","plan":null})).unwrap()
    }
    #[test]
    fn session_and_times_survive_completion_restart_and_remote_indexing() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("runs.db");
        let db = Database::open(&path).unwrap();
        let run = sample();
        let id = &run.launch.id;
        db.insert(&run).unwrap();
        assert!(db.get(id).unwrap().unwrap().started_at.is_none());
        db.session(id, None).unwrap();
        db.session(id, Some("33333333-3333-4333-8333-333333333333"))
            .unwrap();
        let running = db.get(id).unwrap().unwrap();
        assert_eq!(running.state, "running");
        assert!(running.session_id.is_some());
        assert!(running.started_at.is_some());
        assert!(running.finished_at.is_none());
        db.finish(id, "done", "Implemented and checked.", None)
            .unwrap();
        assert!(db.insert(&run).is_err());
        let (cursor, updates) = db.changes(0).unwrap();
        assert!(cursor >= 3);
        assert_eq!(updates.len(), 1);
        assert!(db.changes(cursor).unwrap().1.is_empty());
        let coordinator = Database::open(&root.path().join("coordinator.db")).unwrap();
        coordinator
            .cache(&run.launch.request.machine, updates)
            .unwrap();
        let indexed = coordinator
            .sessions(std::slice::from_ref(&run.launch.detail.item.url))
            .unwrap();
        assert_eq!(indexed[0].state, "done");
        assert!(indexed[0].finished_at.is_some());
        drop(db);
        let restarted = Database::open(&path).unwrap();
        let done = restarted.get(id).unwrap().unwrap();
        assert_eq!(done.state, "done");
        assert_eq!(done.session_id, running.session_id);
        assert_eq!(done.started_at, running.started_at);
        assert!(done.finished_at.is_some());
    }
    #[test]
    fn an_interrupted_session_retains_its_native_id_without_an_invented_finish_time() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("runs.db");
        let run = sample();
        {
            let db = Database::open(&path).unwrap();
            db.insert(&run).unwrap();
            db.session(&run.launch.id, Some("33333333-3333-4333-8333-333333333333"))
                .unwrap();
        }
        let db = Database::open(&path).unwrap();
        let restored = db.get(&run.launch.id).unwrap().unwrap();
        assert_eq!(restored.state, "interrupted");
        assert!(restored.session_id.is_some());
        assert!(restored.finished_at.is_none());
    }
}
