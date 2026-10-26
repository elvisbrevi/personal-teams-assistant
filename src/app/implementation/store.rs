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
        db.execute_batch("CREATE TABLE IF NOT EXISTS request_runs (id TEXT PRIMARY KEY, state TEXT NOT NULL, requested_at TEXT NOT NULL, payload TEXT NOT NULL);
            UPDATE request_runs SET state=CASE WHEN state='sending' THEN 'uncertain' ELSE 'interrupted' END WHERE state IN ('generating','sending');")?;
        Ok(Self(std::sync::Mutex::new(db)))
    }
    pub(super) fn request_insert(&self, record: &Value) -> Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT INTO request_runs(id,state,requested_at,payload) VALUES (?1,?2,?3,?4)",
            params![
                record["id"].as_str(),
                record["state"].as_str(),
                record["created_at"].as_str(),
                record.to_string()
            ],
        )?;
        Ok(())
    }
    pub(super) fn request_update(
        &self,
        id: &str,
        change: impl FnOnce(&mut Value) -> Result<()>,
    ) -> Result<()> {
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        let (payload, state): (String, String) = tx.query_row(
            "SELECT payload,state FROM request_runs WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut value: Value = serde_json::from_str(&payload)?;
        value["state"] = json!(state);
        change(&mut value)?;
        tx.execute(
            "UPDATE request_runs SET state=?2,payload=?3 WHERE id=?1",
            params![id, value["state"].as_str(), value.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn requests(&self, id: Option<&str>) -> Result<Vec<Value>> {
        let db = self.0.lock().unwrap();
        let mut query = db.prepare("SELECT payload,state FROM request_runs WHERE (?1 IS NULL OR id=?1) ORDER BY requested_at DESC LIMIT 100")?;
        let rows = query.query_map([id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        rows.map(|row| {
            let (payload, state) = row?;
            let mut value: Value = serde_json::from_str(&payload)?;
            value["state"] = json!(state);
            Ok(value)
        })
        .collect()
    }
    pub(super) fn history(&self) -> Result<Vec<Value>> {
        let mut runs: BTreeMap<String, Value> = self
            .list(None, None, false)?
            .into_iter()
            .map(|r| {
                let id = r.launch.id.clone();
                (id, serde_json::to_value(SessionSummary::from(r)).unwrap())
            })
            .collect();
        let db = self.0.lock().unwrap();
        let mut query = db.prepare(
            "SELECT metadata FROM agent_session_index ORDER BY requested_at DESC LIMIT 100",
        )?;
        for row in query.query_map([], |r| r.get::<_, String>(0))? {
            let value: Value = serde_json::from_str(&row?)?;
            if let Some(id) = value["id"].as_str() {
                runs.entry(id.into()).or_insert(value);
            }
        }
        drop(query);
        drop(db);
        for mut value in self.requests(None)? {
            value.as_object_mut().unwrap().remove("context");
            value.as_object_mut().unwrap().remove("target");
            value.as_object_mut().unwrap().remove("submission");
            value.as_object_mut().unwrap().remove("draft");
            runs.insert(value["id"].as_str().unwrap().into(), value);
        }
        let mut runs: Vec<_> = runs.into_values().collect();
        runs.sort_by(|a, b| b["created_at"].as_str().cmp(&a["created_at"].as_str()));
        Ok(runs)
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
    pub(super) fn remote(
        &self,
        id: &str,
        access: crate::llm::implementation::mobile::Access,
    ) -> Result<()> {
        let value = serde_json::to_string(&access)?;
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        if tx.execute(
            "UPDATE agent_runs SET payload=json_set(payload,'$.remote',json(?2)) WHERE id=?1",
            params![id, value],
        )? > 0
        {
            tx.execute("INSERT INTO agent_run_events(run_id) VALUES (?1)", [id])?;
        } else {
            tx.execute(
                "UPDATE request_runs SET payload=json_set(payload,'$.remote',json(?2)) WHERE id=?1",
                params![id, value],
            )?;
        }
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
            if let Some(url) = session.remote.as_ref().and_then(|a| a.url.as_ref()) {
                ensure!(
                    session.provider == "claude",
                    "Invalid remote session provider."
                );
                crate::llm::implementation::mobile::claude_url(url)?;
            }
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
        db.remote(
            id,
            crate::llm::implementation::mobile::Access {
                status: "connected".into(),
                environment_id: Some("env_synthetic".into()),
                ..Default::default()
            },
        )
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
        assert_eq!(
            indexed[0]
                .remote
                .as_ref()
                .unwrap()
                .environment_id
                .as_deref(),
            Some("env_synthetic")
        );
        assert!(indexed[0].finished_at.is_some());
        drop(db);
        let restarted = Database::open(&path).unwrap();
        let done = restarted.get(id).unwrap().unwrap();
        assert_eq!(done.state, "done");
        assert_eq!(done.session_id, running.session_id);
        assert_eq!(done.remote.as_ref().unwrap().status, "connected");
        assert_eq!(done.started_at, running.started_at);
        assert!(done.finished_at.is_some());
    }
    #[test]
    fn request_intents_sessions_and_uncertainty_survive_restart_without_replay() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("runs.db");
        let id = "44444444-4444-4444-8444-444444444444";
        let generating = json!({"id":id,"state":"generating","created_at":"2026-10-25T12:00:00Z","session_id":null,"submission":{"context":"Synthetic request"}});
        {
            let db = Database::open(&path).unwrap();
            db.request_insert(&generating).unwrap();
            assert!(db.request_insert(&generating).is_err());
            db.request_update(id, |r| {
                r["state"] = json!("sending");
                r["session_id"] = json!("55555555-5555-4555-8555-555555555555");
                Ok(())
            })
            .unwrap();
        }
        let db = Database::open(&path).unwrap();
        let record = db.requests(Some(id)).unwrap().pop().unwrap();
        assert_eq!(record["state"], "uncertain");
        assert_eq!(record["session_id"], "55555555-5555-4555-8555-555555555555");
        assert!(db.request_insert(&generating).is_err());
        let mut second = generating.clone();
        second["id"] = json!("66666666-6666-4666-8666-666666666666");
        db.request_insert(&second).unwrap();
        drop(db);
        let db = Database::open(&path).unwrap();
        assert_eq!(
            db.requests(second["id"].as_str()).unwrap()[0]["state"],
            "interrupted"
        );
        let history = db.history().unwrap();
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|r| r.get("submission").is_none()));
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
