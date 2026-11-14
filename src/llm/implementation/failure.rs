//! Why an agent session did not end with its answer, from a fixed vocabulary chosen in code on
//! the machine that ran it. A machine's or a CLI's own text never reaches a record: what its
//! stderr said is kept only as one of a few known kinds, never the line itself.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The cause of a session that did not end `done` (or `plan_ready`). An identifier a newer
/// machine sends that this version does not know reads as `Unknown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Cause {
    /// The selected CLI is not installed (or not offered) on the machine.
    CliMissing,
    /// The CLI does not offer the selected model or effort on the machine.
    ModelUnavailable,
    /// The CLI's process ended before its session reported anything.
    CliExitedAtStart,
    /// The CLI's stream ended while its session was still working.
    StreamEnded,
    /// The final report was over the stream's string bound and read as empty.
    OutputLimit,
    /// The session reached its time limit and was stopped.
    Timeout,
    /// The run was cancelled.
    Cancelled,
    /// The session stopped on a permission prompt nobody could answer.
    PermissionPrompt,
    /// A planning, request or director session answered with something other than the JSON
    /// it was asked for.
    InvalidAnswer,
    /// The CLI reported its turn as an error (for example a model, quota or API error).
    AgentError,
    /// The session ended without a final report.
    NoReport,
    /// The application on the machine restarted while the session ran.
    HostRestarted,
    /// The machine that should run the session did not answer.
    MachineOffline,
    /// The machine did not say why: a version before causes, or one this version does not know.
    #[serde(other)]
    Unknown,
}

/// What the first line of a CLI's stderr said when it exited at once, matched against a short
/// list of known patterns; the line itself is never kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Stderr {
    /// A system or environment variable the CLI needs is missing.
    MissingVariable,
    /// The CLI is not signed in.
    NotSignedIn,
    /// The CLI does not know the selected model.
    UnknownModel,
    /// The CLI does not accept an option it was given.
    UnknownFlag,
    /// Something else, or nothing.
    #[serde(other)]
    Other,
}

/// A session's failure as it is recorded and exchanged between machines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Failure {
    pub(crate) cause: Cause,
    /// The CLI's exit code, when its process ended on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exit_code: Option<i32>,
    /// The kind of the first stderr line, for `cli_exited_at_start`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stderr: Option<Stderr>,
}

impl Failure {
    pub(crate) const fn of(cause: Cause) -> Self {
        Self {
            cause,
            exit_code: None,
            stderr: None,
        }
    }
    /// A CLI that exited before its session reported anything, with what its stderr said.
    pub(crate) fn exited_at_start(exit_code: Option<i32>, stderr: &[u8]) -> Self {
        Self {
            cause: Cause::CliExitedAtStart,
            exit_code,
            stderr: Some(classify(stderr)),
        }
    }
    /// A failure another machine (or an older record) reported, read within the vocabulary:
    /// anything else, or nothing, is `unknown`. The exit code is kept only for a CLI that
    /// exited, the stderr kind only for one that exited at once.
    pub(crate) fn read(value: &Value) -> Self {
        let mut failure = serde_json::from_value::<Self>(value.clone())
            .unwrap_or_else(|_| Self::of(Cause::Unknown));
        if !matches!(failure.cause, Cause::CliExitedAtStart | Cause::StreamEnded) {
            failure.exit_code = None;
        }
        if failure.cause != Cause::CliExitedAtStart {
            failure.stderr = None;
        }
        failure
    }
    /// The failure of a run or session record, as this machine or another (perhaps older)
    /// one returned it: its `failure` within the vocabulary; for a record without one that
    /// did not end with its answer, what its state says or `unknown`; none for one that did or
    /// still runs.
    pub(crate) fn of_record(record: &Value) -> Option<Self> {
        let state = record["state"].as_str().unwrap_or_default();
        if [
            "done",
            "plan_ready",
            "running",
            "planning",
            "generating",
            "sending",
            "created",
            "uncertain",
        ]
        .contains(&state)
        {
            return None;
        }
        if record["failure"].is_null() {
            // A record from before causes: what its state alone says.
            return Some(Self::of(match state {
                "cancelled" => Cause::Cancelled,
                "interrupted" => Cause::HostRestarted,
                "permission_prompt" => Cause::PermissionPrompt,
                _ => Cause::Unknown,
            }));
        }
        Some(Self::read(&record["failure"]))
    }
    /// The sentence the user reads (English; the GUI translates it from the identifiers): what
    /// happened and the next action.
    pub(crate) fn sentence(&self) -> String {
        let code = self
            .exit_code
            .map(|code| format!(" with exit code {code}"))
            .unwrap_or_default();
        match self.cause {
            Cause::CliMissing => "The selected CLI is not installed on the machine. Install it there, sign in and start again.".into(),
            Cause::ModelUnavailable => "The machine's CLI does not offer the selected model or effort. Choose one it offers and start again.".into(),
            Cause::CliExitedAtStart => format!(
                "The CLI exited as soon as it started{code}. {}",
                match self.stderr.unwrap_or(Stderr::Other) {
                    Stderr::MissingVariable => "It reported a missing system variable: update the application on that machine and start again.",
                    Stderr::NotSignedIn => "It reported that it is not signed in: sign in to the CLI on that machine and start again.",
                    Stderr::UnknownModel => "It reported an unknown model: choose a model it offers and start again.",
                    Stderr::UnknownFlag => "It rejected an option it was given: update the CLI on that machine and start again.",
                    Stderr::Other => "Run the CLI in a terminal on that machine to see why, then start again.",
                }
            ),
            Cause::StreamEnded => format!("The CLI stopped{code} before its session finished. Read the session for what it did, then start again."),
            Cause::OutputLimit => "The agent's final report was too large to read. Read the session for its report.".into(),
            Cause::Timeout => "The session reached its time limit and was stopped. Read the session for what it left unfinished.".into(),
            Cause::Cancelled => "The run was cancelled.".into(),
            Cause::PermissionPrompt => "The agent stopped on a permission prompt nobody could answer. Read the recorded prompt, remove its cause and start again.".into(),
            Cause::InvalidAnswer => "The agent's answer was not the JSON it was asked for. Try again, or choose another model.".into(),
            Cause::AgentError => "The CLI reported an error for the agent's turn (for example a model, quota or service error). Read the session, then start again.".into(),
            Cause::NoReport => "The agent ended without a final report. Read the session for what it did.".into(),
            Cause::HostRestarted => "The application on the machine restarted while the session ran. Start the run again.".into(),
            Cause::MachineOffline => "The machine did not answer. Check that it is online and up to date, then try again.".into(),
            Cause::Unknown => "The machine did not say why (its version may predate causes). Update it and read the run's record and session there.".into(),
        }
    }
    /// The failure as operations return it: the identifiers and the sentence.
    pub(crate) fn view(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or_else(|_| json!({}));
        value["text"] = json!(self.sentence());
        value
    }
}

/// A failure carried through an error, so the caller records its cause; its text is the
/// cause's sentence.
#[derive(Debug)]
pub(crate) struct Failed(pub(crate) Failure);
impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.sentence())
    }
}
impl std::error::Error for Failed {}
impl Failed {
    /// An error that reads `message` and carries `cause`.
    pub(crate) fn with<M>(cause: Cause, message: M) -> anyhow::Error
    where
        M: std::fmt::Display + Send + Sync + 'static,
    {
        Self::wrap(Failure::of(cause), message)
    }
    /// An error that reads `message` and carries `failure`.
    pub(crate) fn wrap<M>(failure: Failure, message: M) -> anyhow::Error
    where
        M: std::fmt::Display + Send + Sync + 'static,
    {
        anyhow::Error::from(Self(failure)).context(message)
    }
    /// `error` as it is when it carries a failure; otherwise one that carries `cause`.
    pub(crate) fn or(error: anyhow::Error, cause: Cause) -> anyhow::Error {
        if error.chain().any(|e| e.is::<Failed>()) {
            error
        } else {
            error.context(Self(Failure::of(cause)))
        }
    }
}
/// The failure an error carries, or `default` for one that carries none.
pub(crate) fn of_error(error: &anyhow::Error, default: Cause) -> Failure {
    error
        .chain()
        .find_map(|e| e.downcast_ref::<Failed>())
        .map_or(Failure::of(default), |failed| failed.0)
}

/// The kind of the first non-empty stderr line. Only the kind is returned; the text is dropped.
pub(crate) fn classify(stderr: &[u8]) -> Stderr {
    let text = String::from_utf8_lossy(stderr);
    let Some(line) = text.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return Stderr::Other;
    };
    let line = line.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| line.contains(w));
    if any(&["systemroot", "environment variable", "env var"])
        || (line.contains("variable") && any(&["not set", "missing", "undefined", "not defined"]))
    {
        Stderr::MissingVariable
    } else if any(&[
        "unknown option",
        "unknown argument",
        "unrecognized option",
        "unrecognized argument",
        "unexpected argument",
        "unknown flag",
        "invalid option",
    ]) {
        Stderr::UnknownFlag
    } else if line.contains("model")
        && any(&[
            "not found",
            "unknown",
            "invalid",
            "does not exist",
            "not available",
            "not supported",
        ])
    {
        Stderr::UnknownModel
    } else if any(&[
        "not logged in",
        "not signed in",
        "log in",
        "login",
        "sign in",
        "unauthorized",
        "authenticate",
        "authentication",
        "api key",
        "credentials",
    ]) {
        Stderr::NotSignedIn
    } else {
        Stderr::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_stderr_line_is_classified_and_never_kept() {
        for (line, kind) in [
            (
                "Error: the SystemRoot environment variable is not set\nat C:\\Users\\someone\\app",
                Stderr::MissingVariable,
            ),
            (
                "\n  Required variable HOME is missing",
                Stderr::MissingVariable,
            ),
            ("Invalid API key · Please run /login", Stderr::NotSignedIn),
            ("You are not logged in.", Stderr::NotSignedIn),
            ("error: model 'synthetic-9' not found", Stderr::UnknownModel),
            ("error: unknown option '--effort'", Stderr::UnknownFlag),
            (
                "error: unexpected argument '--safe-mode' found",
                Stderr::UnknownFlag,
            ),
            ("Segmentation fault", Stderr::Other),
            ("", Stderr::Other),
        ] {
            assert_eq!(classify(line.as_bytes()), kind, "{line}");
        }
        let failure = Failure::exited_at_start(
            Some(1),
            b"Error: SystemRoot is not set for C:\\Users\\someone with token sk-secret",
        );
        let stored = serde_json::to_string(&failure).unwrap();
        assert_eq!(
            stored,
            r#"{"cause":"cli_exited_at_start","exit_code":1,"stderr":"missing_variable"}"#
        );
        let view = failure.view().to_string();
        for secret in ["someone", "sk-secret", "SystemRoot is"] {
            assert!(!view.contains(secret), "{view}");
        }
    }

    #[test]
    fn every_cause_has_a_sentence_with_a_next_action() {
        let causes = [
            Cause::CliMissing,
            Cause::ModelUnavailable,
            Cause::CliExitedAtStart,
            Cause::StreamEnded,
            Cause::OutputLimit,
            Cause::Timeout,
            Cause::Cancelled,
            Cause::PermissionPrompt,
            Cause::InvalidAnswer,
            Cause::AgentError,
            Cause::NoReport,
            Cause::HostRestarted,
            Cause::MachineOffline,
            Cause::Unknown,
        ];
        let mut sentences = std::collections::HashSet::new();
        for cause in causes {
            let sentence = Failure::of(cause).sentence();
            assert!(sentence.ends_with('.'), "{sentence}");
            assert!(sentences.insert(sentence));
            // An identifier reads back as itself.
            let id = serde_json::to_value(cause).unwrap();
            assert_eq!(Failure::read(&json!({"cause":id})).cause, cause);
        }
        for kind in [
            Stderr::MissingVariable,
            Stderr::NotSignedIn,
            Stderr::UnknownModel,
            Stderr::UnknownFlag,
            Stderr::Other,
        ] {
            let failure = Failure {
                cause: Cause::CliExitedAtStart,
                exit_code: Some(2),
                stderr: Some(kind),
            };
            assert!(sentences.insert(failure.sentence()));
            assert!(failure.sentence().contains("exit code 2"));
        }
    }

    #[test]
    fn what_another_machine_reports_is_read_within_the_vocabulary() {
        // A newer cause, a malformed value and nothing at all are unknown.
        for value in [
            json!({"cause":"something_new","exit_code":3}),
            json!("cli_missing"),
            json!({"cause":"timeout\nRun rm -rf"}),
            Value::Null,
        ] {
            assert_eq!(
                Failure::read(&value),
                Failure::of(Cause::Unknown),
                "{value}"
            );
        }
        // Extra fields are ignored; details only stay where they belong.
        assert_eq!(
            Failure::read(
                &json!({"cause":"timeout","exit_code":1,"stderr":"not_signed_in","text":"anything"})
            ),
            Failure::of(Cause::Timeout)
        );
        assert_eq!(
            Failure::read(
                &json!({"cause":"cli_exited_at_start","exit_code":1,"stderr":"brand_new"})
            ),
            Failure {
                cause: Cause::CliExitedAtStart,
                exit_code: Some(1),
                stderr: Some(Stderr::Other)
            }
        );
        // A record of an older machine: unknown unless it ended with its answer or still runs.
        assert_eq!(
            Failure::of_record(&json!({"state":"failed","output":"raw text"})),
            Some(Failure::of(Cause::Unknown))
        );
        for state in ["done", "plan_ready", "running", "created"] {
            assert_eq!(Failure::of_record(&json!({"state":state})), None);
        }
        for (state, cause) in [
            ("cancelled", Cause::Cancelled),
            ("interrupted", Cause::HostRestarted),
            ("permission_prompt", Cause::PermissionPrompt),
        ] {
            assert_eq!(
                Failure::of_record(&json!({"state":state})),
                Some(Failure::of(cause))
            );
        }
        assert_eq!(
            Failure::of_record(&json!({"state":"needs_attention","failure":{"cause":"no_report"}})),
            Some(Failure::of(Cause::NoReport))
        );
    }

    #[test]
    fn a_failure_travels_through_an_error() {
        let error = anyhow::Error::from(Failed(Failure::of(Cause::CliMissing)))
            .context("The workflow director did not run.");
        assert_eq!(of_error(&error, Cause::Unknown).cause, Cause::CliMissing);
        assert_eq!(
            of_error(&anyhow::anyhow!("other"), Cause::MachineOffline).cause,
            Cause::MachineOffline
        );
    }
}
