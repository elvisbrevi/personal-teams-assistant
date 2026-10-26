//! Structured host failures: retain causes and locations without retaining error text or inputs.
use super::*;
use serde_json::{Value, json};
use std::{cell::RefCell, future::Future, panic::Location, time::Duration};

tokio::task_local! {
    static CAUSES: RefCell<Vec<Value>>;
}

pub(super) async fn capture<F: Future<Output = control::Reply>>(
    operation: F,
) -> (control::Reply, Vec<Value>) {
    CAUSES
        .scope(RefCell::new(Vec::new()), async {
            let reply = operation.await;
            (reply, CAUSES.with(|causes| causes.take()))
        })
        .await
}

pub(super) fn capture_error(error: &anyhow::Error, location: &Location<'_>) {
    let chain = error.chain().take(8).map(|source| {
        if let Some(error) = source.downcast_ref::<azure_cli::LoginError>() {
            json!({"type":"azure_cli_login", "details":error})
        } else if let Some(error) = source.downcast_ref::<std::io::Error>() {
            json!({"type":"io", "kind":format!("{:?}", error.kind()), "os_code":error.raw_os_error()})
        } else if let Some(error) = source.downcast_ref::<reqwest::Error>() {
            json!({"type":"http", "status":error.status().map(|s| s.as_u16()), "timeout":error.is_timeout(), "connect":error.is_connect(), "decode":error.is_decode()})
        } else if let Some(error) = source.downcast_ref::<serde_json::Error>() {
            json!({"type":"json", "category":format!("{:?}", error.classify()), "line":error.line(), "column":error.column()})
        } else if let Some(error) = source.downcast_ref::<toml::de::Error>() {
            json!({"type":"toml", "span":error.span()})
        } else if let Some(rusqlite::Error::SqliteFailure(error, _)) = source.downcast_ref::<rusqlite::Error>() {
            json!({"type":"sqlite", "code":format!("{:?}", error.code), "extended_code":error.extended_code})
        } else if let Some(error) = source.downcast_ref::<SetupError>() {
            json!({"type":"precondition", "reason":error.0})
        } else if let Some(error) = source.downcast_ref::<Rejected>() {
            json!({"type":"precondition", "reason":error.to_string()})
        } else if let Some(error) = source.downcast_ref::<tokio::task::JoinError>() {
            json!({"type":"task", "cancelled":error.is_cancelled(), "panic":error.is_panic()})
        } else {
            json!({"type":"internal"})
        }
    }).collect::<Vec<_>>();
    // Function adapters such as map_err(fail) report a core shim as their caller. Retain
    // bounded application frame names as well; stack formatting never includes arguments.
    let frames = std::backtrace::Backtrace::force_capture()
        .to_string()
        .lines()
        .filter(|line| {
            line.contains("personal_teams_assistant::app::")
                && !line.contains("::diagnostics::")
                && !line.contains("::app::fail")
        })
        .take(8)
        .map(|line| line.trim().to_owned())
        .collect::<Vec<_>>();
    let cause =
        json!({"file":location.file(), "line":location.line(), "frames":frames, "chain":chain});
    // Error Display/Debug and arguments can contain credentials, authorization URLs or message content.
    tracing::warn!(event="app_error_cause", detail=%cause);
    let _ = CAUSES.try_with(|causes| {
        if causes.borrow().len() < 8 {
            causes.borrow_mut().push(cause);
        }
    });
}

pub(super) fn record(
    state: &DesktopState,
    method: &str,
    reply: &control::Reply,
    elapsed: Duration,
    causes: Vec<Value>,
) {
    let detail = detail(method, reply, elapsed, causes);
    tracing::warn!(event="app_operation_failed", detail=%detail);
    let result = (|| -> Result<()> {
        let config = read_config(&state.config_path)?;
        fs::create_dir_all(&config.server.data_dir)?;
        Store::append_event(
            &config.server.data_dir.join("assistant.db"),
            "app_operation_failed",
            &detail.to_string(),
        )
    })();
    if let Err(error) = result {
        capture_error(&error, Location::caller());
        tracing::warn!(event = "app_error_log_unavailable");
    }
}

fn detail(method: &str, reply: &control::Reply, elapsed: Duration, causes: Vec<Value>) -> Value {
    let method =
        if method.len() <= 80 && method.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            method
        } else {
            "unknown"
        };
    json!({"operation":method, "code":reply.code, "exit_code":reply.exit_code, "elapsed_ms":elapsed.as_millis().min(u64::MAX as u128) as u64, "causes":causes})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn diagnostics_persist_causes_without_inputs_or_changing_jobs() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("assistant.db");
        let store = Store::open(&path).unwrap();
        // An error log must not trigger startup recovery of a job already being processed.
        store.begin_simulation("synthetic-job").unwrap();
        let (reply, causes) = capture(async {
            let error =
                anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
                    .context(
                        "token=synthetic-secret code ABCD12345 https://example.test/?code=private",
                    );
            fail(error);
            control::Reply::error("operation_failed", 1, "synthetic-private-message")
        })
        .await;
        let detail = detail(
            "begin_azure_cli_login",
            &reply,
            Duration::from_millis(30000),
            causes,
        );
        Store::append_event(&path, "app_operation_failed", &detail.to_string()).unwrap();
        let rows = Store::inspect(&path, true, 10, None, false).unwrap();
        let saved = rows[0]["detail"].as_str().unwrap();
        assert!(saved.contains("PermissionDenied"));
        assert!(saved.contains("begin_azure_cli_login"));
        assert!(saved.contains("30000"));
        for secret in [
            "synthetic-secret",
            "ABCD12345",
            "example.test",
            "synthetic-private-message",
        ] {
            assert!(!saved.contains(secret));
        }
        assert_eq!(
            Store::inspect(&path, false, 10, None, false).unwrap()[0]["status"],
            "processing"
        );
    }
}
