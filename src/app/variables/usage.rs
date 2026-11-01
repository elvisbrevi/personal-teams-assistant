//! What an agent may do with each variable: the ways this installation can use it by reference,
//! whether it can use them now and which need the user's confirmation. Metadata only: it is built
//! from `variable_list` and `variable_consumer_list` replies and never resolves a value.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UsageArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

const NO_PATH: &str = "Nothing uses this variable by reference yet. Do not ask for its value: tell the user which path is missing (a built-in consumer binding, a launcher they approve on their trusted machine, or a delivery destination they approve) and propose it.";

pub(super) async fn usage(host: &Arc<Host>, request: &control::Request) -> Reply {
    let args: UsageArgs = match parse(&request.args) {
        Ok(args) => args,
        Err(error) => return failure(error),
    };
    let mut list = control::Request::new("variable_list");
    list.args = json!({"query": args.query, "limit": args.limit});
    let listed = Box::pin(operate(host, &list)).await;
    if !listed.ok {
        return listed;
    }
    let consumers = Box::pin(consumers::operate(
        host,
        &control::Request::new("variable_consumer_list"),
    ))
    .await;
    if !consumers.ok {
        return consumers;
    }
    let config = match read_config(&host.state.config_path) {
        Ok(config) => config,
        Err(_) => {
            return failure(vault::Error::Storage(
                "The configuration could not be read.".into(),
            ));
        }
    };
    let link = implementation::link(&host.state).ok().flatten();
    let standing = host.state.standing.view(link.as_ref());
    let usable = standing["variables_usable"] == true;
    let launchers_here = consumers.data["launchers_available"] == true;
    let deepseek_bound = consumers.data["consumers"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|c| c["id"] == "deepseek" && !c["binding"].is_null());
    let variables: Vec<Value> = listed.data["variables"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|variable| {
            let id = variable["id"].as_str().unwrap_or_default();
            let name = variable["name"].as_str().unwrap_or_default();
            let mut paths = Vec::new();
            for consumer in consumers.data["consumers"].as_array().into_iter().flatten() {
                if consumer["binding"]["id"] == id {
                    paths.push(json!({
                        "kind": "consumer",
                        "id": consumer["id"],
                        "destination": consumer["destination"],
                        "state": consumer["binding"]["state"],
                        "operations": ["consumer_check"],
                        "confirmation": true,
                        "available_here": usable,
                    }));
                }
            }
            for launcher in consumers.data["launchers"].as_array().into_iter().flatten() {
                let env: Vec<&Value> = launcher["variables"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|v| v["id"] == id)
                    .map(|v| &v["env"])
                    .collect();
                if !env.is_empty() {
                    let approved = launcher["approved"] == true;
                    paths.push(json!({
                        "kind": "launcher",
                        "id": launcher["id"],
                        "env": env,
                        "approved": approved,
                        "operations": ["launcher_run"],
                        "confirmation": true,
                        "available_here": usable && launchers_here && approved,
                    }));
                }
            }
            for reference in application_references(&config, name, deepseek_bound) {
                paths.push(json!({
                    "kind": "application",
                    "reference": reference,
                    "operations": [],
                    "confirmation": false,
                    "available_here": usable,
                }));
            }
            let mut view = json!({
                "id": id,
                "name": name,
                "description": variable["description"],
                "version": variable["version"],
                "usable_now": usable,
                "paths": paths,
            });
            if view["paths"].as_array().is_some_and(Vec::is_empty) {
                view["next"] = json!(NO_PATH);
            }
            view
        })
        .collect();
    let reason = if !usable {
        standing["recovery"].as_str().unwrap_or_default().to_owned()
    } else if !launchers_here {
        "Launchers run only on the owner's trusted machine, from its application window or local CLI.".to_owned()
    } else {
        String::new()
    };
    Reply::success(json!({
        "variables": variables,
        "total": listed.data["total"],
        "here": {
            "standing": standing["state"],
            "variables_usable": usable,
            "launchers": launchers_here,
            "reason": reason,
        },
        "sync": listed.data["sync"],
        "never": "No path returns a value. Never ask the user to paste, reveal or copy a value, and never put one in an argument, an environment export, a file or a handoff document.",
    }))
}

/// The application settings that read a variable of this name by name.
fn application_references(config: &Config, name: &str, deepseek_bound: bool) -> Vec<String> {
    let mut found = Vec::new();
    if name.eq_ignore_ascii_case("DEEPSEEK_API_KEY") && !deepseek_bound {
        found.push("DeepSeek model key (DEEPSEEK_API_KEY)".to_owned());
    }
    for (reference, target) in &config.secrets {
        if target.eq_ignore_ascii_case(name) {
            found.push(format!("[secrets] {reference}"));
        }
    }
    for (field, _) in identity::FIELDS {
        if field.eq_ignore_ascii_case(name) {
            found.push(format!("Microsoft setup ({field})"));
        }
    }
    found
}
