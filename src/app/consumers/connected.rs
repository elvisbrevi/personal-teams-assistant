//! Consumers on an installation signed in to a coordinator.
//!
//! The account's variables live only on the coordinator. A signed-in installation keeps no value,
//! ciphertext or key and opens no local value store for them: choosing a variable needs the
//! synchronized catalog, and every new use asks the coordinator for the value at that moment
//! (`variable_resolve`, which is also the live check of the machine credential and of the
//! account). An outage denies the use (`dependency_or_network`), a credential the coordinator no
//! longer accepts denies it with the sign-in instructions (`not_ready`); neither falls back to a
//! local store, the environment, another account or a remembered value. Bindings and launchers
//! made here are qualified by the coordinator, account and machine, so they never survive into
//! another account.
use super::*;
use crate::app::{implementation::Link, standing, variables as sync};
use std::time::Instant;

/// How long a value resolved for an operation stays staged for the credential lookup of that
/// operation. Nothing is written to disk.
const STAGE_SECONDS: u64 = 60;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bound {
    consumer: String,
    slot: String,
    entry: String,
    bound_at: i64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    link: Link,
    bindings: Vec<Bound>,
}

/// The bindings of the signed-in account: entry references only, never a value.
pub(super) struct Bindings {
    path: PathBuf,
}

impl Bindings {
    pub(super) fn new(profile: &Path) -> Self {
        Self {
            path: profile.join("variables").join("account-bindings.json"),
        }
    }
    fn load(&self, link: &Link) -> Vec<Bound> {
        let Ok(bytes) = fs::read(&self.path) else {
            return Vec::new();
        };
        match serde_json::from_slice::<File>(&bytes) {
            Ok(file) if file.link == *link => file.bindings,
            _ => {
                // Another account's, or unreadable: never reused.
                let _ = fs::remove_file(&self.path);
                Vec::new()
            }
        }
    }
    fn save(&self, link: &Link, bindings: Vec<Bound>) -> std::result::Result<(), vault::Error> {
        if let Some(parent) = self.path.parent() {
            security::private_dir(parent)?;
        }
        let file = File {
            link: link.clone(),
            bindings,
        };
        write_private(
            &self.path,
            &serde_json::to_string(&file)
                .map_err(|_| vault::Error::Storage("The binding could not be saved.".into()))?,
        )?;
        Ok(())
    }
    pub(super) fn has(&self, link: &Link, consumer: &str, slot: &str) -> bool {
        self.load(link)
            .iter()
            .any(|b| b.consumer == consumer && b.slot == slot)
    }
    fn entry(&self, link: &Link, consumer: &str, slot: &str) -> Option<Bound> {
        self.load(link)
            .into_iter()
            .find(|b| b.consumer == consumer && b.slot == slot)
    }
    fn bind(
        &self,
        link: &Link,
        consumer: &str,
        slot: &str,
        entry: &str,
    ) -> std::result::Result<Bound, vault::Error> {
        let mut all = self.load(link);
        all.retain(|b| !(b.consumer == consumer && b.slot == slot));
        let bound = Bound {
            consumer: consumer.into(),
            slot: slot.into(),
            entry: entry.into(),
            bound_at: chrono::Utc::now().timestamp(),
        };
        let view = Bound {
            consumer: bound.consumer.clone(),
            slot: bound.slot.clone(),
            entry: bound.entry.clone(),
            bound_at: bound.bound_at,
        };
        all.push(bound);
        self.save(link, all)?;
        Ok(view)
    }
    fn unbind(
        &self,
        link: &Link,
        consumer: &str,
        slot: &str,
    ) -> std::result::Result<bool, vault::Error> {
        let mut all = self.load(link);
        let before = all.len();
        all.retain(|b| !(b.consumer == consumer && b.slot == slot));
        let removed = all.len() != before;
        if removed {
            self.save(link, all)?;
        }
        Ok(removed)
    }
}

/// A staged value of a credential, or why it could not be obtained.
pub(super) enum Staged {
    Value(Secret, Instant),
    Denied(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolved {
    variable: vault::Metadata,
    value: String,
}

/// A coordinator refusal as a store error, restricted to the kinds the caller may act on.
fn refused(reply: &Value) -> vault::Error {
    let message = reply["message"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(400)
        .collect::<String>();
    match reply["code"].as_str() {
        Some("state_conflict") => vault::Error::Conflict(message),
        Some("invalid_input") => vault::Error::Input(message),
        _ => vault::Error::Storage("The coordinator could not resolve the variable.".into()),
    }
}

/// The value of one entry of the signed-in account, asked of the coordinator now.
pub(super) async fn resolve(
    state: &DesktopState,
    entry: &str,
    expected: Option<u64>,
) -> std::result::Result<(vault::Metadata, Secret), vault::Error> {
    let id = uuid::Uuid::parse_str(entry)
        .map_err(|_| input("The variable ID must be a UUID."))?
        .hyphenated()
        .to_string();
    let reply = match sync::call_account(
        state,
        "variable_resolve",
        &json!({"id":id,"version":expected}),
    )
    .await
    {
        Ok(Some(reply)) => reply,
        Ok(None) => return Err(vault::Error::Revoked(standing::REVOKED.into())),
        Err(error) => return Err(standing::denial(&error)),
    };
    if reply["ok"] != true {
        // A credential the coordinator's host no longer knows is as good as a refused one.
        if reply["message"]
            .as_str()
            .is_some_and(|m| m.contains("Machine is not registered"))
        {
            return Err(vault::Error::Revoked(standing::REVOKED.into()));
        }
        return Err(refused(&reply));
    }
    let resolved: Resolved = serde_json::from_value(reply["data"].clone()).map_err(|_| {
        vault::Error::Storage("The coordinator returned an unexpected reply.".into())
    })?;
    // The reply must be for exactly what was asked: never a different entry or version.
    if resolved.variable.id != id
        || expected.is_some_and(|version| version != resolved.variable.version)
        || vault::validate_value(&resolved.value).is_err()
    {
        return Err(vault::Error::Storage(
            "The coordinator returned an unexpected reply.".into(),
        ));
    }
    Ok((resolved.variable, Secret::new(resolved.value)))
}

fn binding_view(bound: &Bound, cache: &[vault::Metadata]) -> Value {
    let variable = cache.iter().find(|v| v.id == bound.entry);
    json!({
        "state": if variable.is_some() { "ok" } else { "unverified" },
        "id": bound.entry,
        "variable": variable.map(|v| json!({"name":v.name,"version":v.version})),
        "bound_at": bound.bound_at,
    })
}

fn list(host: &Host, link: &Link, launchers: bool) -> std::result::Result<Value, vault::Error> {
    let state = &host.state.variables.consumers;
    let cache = sync::cached(host, link);
    let consumers: Vec<Value> = BUILT_IN
        .iter()
        .map(|consumer| {
            let binding = state
                .bindings
                .entry(link, consumer.id, consumer.slot)
                .map(|b| binding_view(&b, &cache));
            json!({
                "id":consumer.id,
                "title":consumer.title,
                "summary":consumer.summary,
                "destination":consumer.destination,
                "slot":consumer.slot,
                "operations":consumer.operations,
                "binding":binding,
            })
        })
        .collect();
    let mut result = json!({"consumers":consumers,"launchers_available":launchers});
    if launchers {
        let mut views = Vec::new();
        for launcher in state.launchers_of(host, link).list()? {
            let variables: Vec<Value> = launcher
                .env
                .iter()
                .map(|(env, entry)| {
                    let found = cache.iter().find(|v| v.id == *entry);
                    json!({"env":env,"id":entry,
                        "state": if found.is_some() { "ok" } else { "unverified" },
                        "variable": found.map(|v| json!({"name":v.name,"version":v.version}))})
                })
                .collect();
            views.push(json!({
                "id":launcher.id,
                "description":launcher.description,
                "program":launcher.program,
                "args":launcher.args,
                "directory":launcher.directory,
                "timeout_seconds":launcher.timeout_seconds,
                "variables":variables,
            }));
        }
        result["launchers"] = json!(views);
    }
    Ok(result)
}

pub(super) async fn run(
    host: &Arc<Host>,
    link: &Link,
    method: &str,
    args: &Value,
    launchers: bool,
) -> std::result::Result<Value, vault::Error> {
    let state = &host.state.variables.consumers;
    match method {
        "variable_consumer_list" => list(host, link, launchers),
        "variable_consumer_bind" => {
            let a: BindArgs = parse(args)?;
            let consumer = consumer_of(&a.consumer)?;
            let (metadata, value) = resolve(&host.state, &a.variable, a.version).await?;
            rules::validate_value(consumer, value.expose())?;
            drop(value);
            let bound = state
                .bindings
                .bind(link, consumer.id, consumer.slot, &metadata.id)?;
            Ok(json!({
                "consumer":consumer.id,
                "binding":binding_view(&bound, std::slice::from_ref(&metadata)),
                "restart_required":restart_needed(host).await,
            }))
        }
        "variable_consumer_unbind" => {
            // Removing a local authorization never needs the coordinator.
            let a: ConsumerArgs = parse(args)?;
            let consumer = consumer_of(&a.consumer)?;
            let removed = state.bindings.unbind(link, consumer.id, consumer.slot)?;
            Ok(
                json!({"consumer":consumer.id,"unbound":removed,"restart_required":restart_needed(host).await}),
            )
        }
        "variable_consumer_check" => {
            let a: ConsumerArgs = parse(args)?;
            let consumer = consumer_of(&a.consumer)?;
            let Some(binding) = state.bindings.entry(link, consumer.id, consumer.slot) else {
                return Err(input(
                    "No variable is bound to this consumer. Choose one with `pta consumers bind`.",
                ));
            };
            let (metadata, value) = resolve(&host.state, &binding.entry, a.version).await?;
            rules::validate_value(consumer, value.expose())?;
            let origin = state.origin(consumer);
            let result = rules::check_at(&origin, &value).await;
            drop(value);
            Ok(json!({
                "consumer":consumer.id,
                "operation":"check",
                "destination":consumer.destination,
                "reference":{"id":metadata.id,"name":metadata.name,"version":metadata.version},
                "result":result,
            }))
        }
        "variable_launcher_define" => {
            let a: DefineArgs = parse(args)?;
            let directory = implementation::directories(host, std::slice::from_ref(&a.directory))
                .await
                .map_err(|error| input(error.to_string()))?
                .remove(0);
            let cache = sync::cached(host, link);
            for entry in a.env.values() {
                if !cache.iter().any(|v| v.id == *entry) {
                    return Err(input(
                        "A referenced variable is not in this account's synchronized catalog. Synchronize and choose it again.",
                    ));
                }
            }
            let launcher = rules::define(
                Definition {
                    id: a.id,
                    description: a.description,
                    program: a.program,
                    args: a.args,
                    directory,
                    env: a.env,
                    timeout_seconds: a.timeout_seconds,
                },
                chrono::Utc::now().timestamp(),
            )?;
            let _edit = state.edit.lock().unwrap_or_else(|e| e.into_inner());
            state.launchers_of(host, link).add(launcher.clone())?;
            Ok(json!({"launcher":{
                "id":launcher.id,
                "program":launcher.program,
                "directory":launcher.directory,
                "timeout_seconds":launcher.timeout_seconds,
                "variables":launcher.env.keys().collect::<Vec<_>>(),
            }}))
        }
        "variable_launcher_remove" => {
            let a: LauncherArgs = parse(args)?;
            let _edit = state.edit.lock().unwrap_or_else(|e| e.into_inner());
            let removed = state.launchers_of(host, link).remove(&a.id)?;
            Ok(json!({"launcher":a.id,"removed":removed}))
        }
        _ => {
            let a: LauncherArgs = parse(args)?;
            let _busy = state
                .busy
                .try_lock()
                .map_err(|_| vault::Error::Conflict("A launcher is already running.".into()))?;
            let launcher = state.launchers_of(host, link).get(&a.id)?;
            if let Some(unknown) = a
                .versions
                .keys()
                .find(|env| !launcher.env.contains_key(*env))
            {
                return Err(input(format!(
                    "{unknown} is not one of this launcher's variables."
                )));
            }
            let confined =
                implementation::directories(host, std::slice::from_ref(&launcher.directory))
                    .await
                    .map_err(|error| input(error.to_string()))?;
            if confined.first() != Some(&launcher.directory) {
                return Err(vault::Error::Conflict(
                    "The approved working directory changed. Define the launcher again.".into(),
                ));
            }
            // Resolve every value first, online: any denied, deleted, stale or unreachable
            // reference stops the run before a start.
            let mut values: Vec<(String, Secret)> = Vec::new();
            let mut used = Vec::new();
            for (env, entry) in &launcher.env {
                let (metadata, value) =
                    resolve(&host.state, entry, a.versions.get(env).copied()).await?;
                used.push(
                    json!({"env":env,"id":metadata.id,"name":metadata.name,"version":metadata.version}),
                );
                values.push((env.clone(), value));
            }
            let result = rules::run(&launcher, values).await?;
            Ok(json!({
                "launcher":launcher.id,
                "operation":"run",
                "variables":used,
                "result":result,
                "output":"not captured",
            }))
        }
    }
}

/// Resolve, for the operation about to start, every credential an account variable supplies on
/// this installation. A failure is remembered per credential and surfaces only if the operation
/// actually needs that credential, so an outage does not stop work that does not use it.
pub(in crate::app) async fn stage(state: &DesktopState) {
    let Ok(Some(link)) = implementation::link(state) else {
        return;
    };
    let consumers = &state.variables.consumers;
    for consumer in BUILT_IN {
        let Some(binding) = consumers.bindings.entry(&link, consumer.id, consumer.slot) else {
            consumers.unstage_one(consumer.credential);
            continue;
        };
        let resolved = resolve(state, &binding.entry, None).await;
        // A late answer is dropped if the installation signed in to another account meanwhile.
        if implementation::link(state).ok().flatten().as_ref() != Some(&link) {
            return;
        }
        let outcome = match resolved {
            Ok((_, value)) => match rules::validate_value(consumer, value.expose()) {
                Ok(()) => Staged::Value(value, Instant::now()),
                Err(error) => Staged::Denied(error.to_string()),
            },
            Err(error) => Staged::Denied(error.to_string()),
        };
        consumers.stage(consumer.credential, outcome);
    }
}

impl State {
    pub(super) fn stage(&self, credential: &str, staged: Staged) {
        self.staged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(credential.into(), staged);
    }
    pub(super) fn unstage_one(&self, credential: &str) {
        self.staged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(credential);
    }
    /// Forget every staged value: authority was lost or the account changed.
    pub(in crate::app) fn unstage(&self) {
        self.staged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }
    /// The staged value of a credential while it is fresh.
    pub(super) fn staged(&self, credential: &str) -> std::result::Result<String, String> {
        let mut staged = self.staged.lock().unwrap_or_else(|e| e.into_inner());
        match staged.get(credential) {
            Some(Staged::Value(value, at)) if at.elapsed().as_secs() < STAGE_SECONDS => {
                Ok(value.expose().to_owned())
            }
            Some(Staged::Denied(message)) => Err(message.clone()),
            Some(Staged::Value(..)) => {
                staged.remove(credential);
                Err("The account value was not resolved for this operation. Retry once the coordinator is reachable.".into())
            }
            None => Err("The account value was not resolved for this operation. Retry once the coordinator is reachable.".into()),
        }
    }
    pub(super) fn launchers_of(&self, host: &Host, link: &Link) -> rules::Launchers {
        let profile = host.state.config_path.parent().unwrap();
        rules::Launchers::account(profile, &standing::qualifier(link))
    }
}
