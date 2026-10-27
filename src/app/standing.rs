//! Whether this installation may still act for the account it signed in to.
//!
//! The coordinator is the only authority for an account's variables and machines. A signed-in
//! installation therefore tracks the outcome of its latest exchanges with it: `live` (answered),
//! `offline` (not reachable, so the answer is unknown) or `revoked` (the credential is no longer
//! accepted: the machine was revoked or signed out, or the account was disabled or rebound).
//! Cached configuration stays readable in every state and says how old it is; a new use of an
//! account variable needs a live answer at that moment and never falls back to a local copy, the
//! environment or another account. Nothing here is a cache of a secret.
use super::{
    implementation::{self, Link},
    *,
};
use serde_json::{Value, json};

pub(super) const OFFLINE: &str = "The coordinator cannot be reached right now, so account variables cannot be used. The synchronized configuration stays readable but may be out of date; try again when this computer is online.";
pub(super) const REVOKED: &str = "The coordinator no longer accepts this installation (it was signed out or revoked, or its account changed). Nothing was used. Sign in again with Connect this machine or `pta auth machine login`.";
const SIGNED_OUT: &str = "This installation is not signed in to an assistant account. Sign in with Connect this machine or `pta auth machine login` to restore your account here.";
const UNKNOWN: &str = "The coordinator has not been checked since this host started.";

/// Why the coordinator did not serve an exchange; carried in the error chain so each caller can
/// tell an outage from a refusal without parsing text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The credential is no longer accepted.
    Rejected,
    /// No answer: the network, the coordinator or its host is unavailable.
    Unavailable,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Rejected => "The coordinator no longer accepts this machine.",
            Self::Unavailable => {
                "Machine coordinator is unavailable or this machine was disconnected."
            }
        })
    }
}
impl std::error::Error for Refusal {}

pub(super) fn refusal(error: &anyhow::Error) -> Option<Refusal> {
    error.downcast_ref::<Refusal>().copied()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Observed {
    #[default]
    Unknown,
    Live,
    Offline,
    Revoked,
}

#[derive(Default)]
struct Record {
    link: Option<Link>,
    observed: Observed,
    at: Option<i64>,
    last_live: Option<i64>,
}

#[derive(Default)]
pub(super) struct State {
    record: std::sync::Mutex<Record>,
}

impl State {
    fn record(&self) -> std::sync::MutexGuard<'_, Record> {
        self.record.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Forget what was observed: the installation signed in, out or to another account.
    pub(super) fn reset(&self) {
        *self.record() = Record::default();
    }
    /// Record the outcome of one exchange with the coordinator of `link`. Replies that say
    /// nothing about reachability or acceptance (an operation refusal) leave the state alone.
    pub(super) fn observe<T>(&self, link: &Link, result: &anyhow::Result<T>) -> Observed {
        let observed = match result {
            Ok(_) => Observed::Live,
            Err(error) => match refusal(error) {
                Some(Refusal::Rejected) => Observed::Revoked,
                Some(Refusal::Unavailable) => Observed::Offline,
                None => return self.record().observed,
            },
        };
        let mut record = self.record();
        if record.link.as_ref() != Some(link) {
            *record = Record {
                link: Some(link.clone()),
                ..Record::default()
            };
        }
        let now = chrono::Utc::now().timestamp();
        record.observed = observed;
        record.at = Some(now);
        if observed == Observed::Live {
            record.last_live = Some(now);
        }
        observed
    }
    /// What a client may show: the state, when it was last confirmed and how to recover. Never
    /// a token, a value or the raw transport error.
    pub(super) fn view(&self, link: Option<&Link>) -> Value {
        let record = self.record();
        let matching = link.is_some() && record.link.as_ref() == link;
        let observed = if matching {
            record.observed
        } else {
            Observed::Unknown
        };
        let (state, recovery) = match (link, observed) {
            (None, _) => ("signed_out", SIGNED_OUT),
            (Some(_), Observed::Unknown) => ("unknown", UNKNOWN),
            (Some(_), Observed::Live) => ("live", ""),
            (Some(_), Observed::Offline) => ("offline", OFFLINE),
            (Some(_), Observed::Revoked) => ("revoked", REVOKED),
        };
        json!({
            "state": state,
            "coordinator": link.map(|l| l.origin.as_str()),
            "account_key": link.map(qualifier),
            "checked_at": if matching { record.at } else { None },
            "last_live_at": if matching { record.last_live } else { None },
            "variables_usable": state == "live" || (link.is_none()),
            "recovery": recovery,
        })
    }
}

/// An opaque qualifier of the coordinator, account and machine: it names the account a screen or
/// a file of references belongs to without revealing any identity, and differs for every other.
pub(super) fn qualifier(link: &Link) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(
        [
            b"personal-teams-assistant/account-qualifier/v1".as_slice(),
            link.origin.as_bytes(),
            b"\0",
            link.account.to_string().as_bytes(),
            b"\0",
            link.machine.as_bytes(),
        ]
        .concat(),
    );
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Record an exchange of this installation with its coordinator, and withdraw anything held for
/// the account the moment the coordinator stops accepting it.
pub(super) fn note<T>(state: &DesktopState, result: &anyhow::Result<T>) {
    let Ok(Some(link)) = implementation::link(state) else {
        return;
    };
    if state.standing.observe(&link, result) == Observed::Revoked {
        state.variables.withdraw();
    }
}

/// The installation left its account, signed in to one, or lost it: nothing observed, mirrored
/// or staged for the former account may be shown or used for the next. Stored data on the
/// coordinator is not touched.
pub(super) fn forget_account(state: &DesktopState) {
    state.standing.reset();
    state.variables.forget_catalog();
    state.variables.withdraw();
    state.portable.forget();
}

/// The failure of a use that needs the coordinator, from the typed cause of an exchange.
pub(super) fn denial(error: &anyhow::Error) -> crate::variables::Error {
    match refusal(error) {
        Some(Refusal::Rejected) => crate::variables::Error::Revoked(REVOKED.into()),
        _ => crate::variables::Error::Offline(OFFLINE.into()),
    }
}

#[cfg(test)]
mod tests;
