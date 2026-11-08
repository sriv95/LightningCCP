//! Parity of the registered command tree with Python's: `fixtures/command_tree.json` is
//! `tree.get_commands()` of the bot with every cog loaded, each `to_dict()` (regenerate with
//! `fixtures/dump_command_tree.py`). The Rust side is poise's `create_application_commands` for `bot::commands()`,
//! the payload `register_globally` sends. Fields Discord treats as absent (null) are dropped on both sides.
use serde_json::{Value, json};

/// Drops what Discord reads as absent: nulls, empty lists, `false` of `autocomplete` and `nsfw`, empty
/// localizations. Numbers compare as floats (poise sends `800.0` where discord.py sends `800`).
fn norm(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            let subcommand = matches!(m.get("type").and_then(Value::as_u64), Some(1 | 2));
            Value::Object(
                m.into_iter()
                    .filter(|(k, v)| match v {
                        Value::Null => false,
                        Value::Array(a) => !a.is_empty(),
                        Value::Object(o) => !o.is_empty(),
                        // `required` means nothing on a subcommand or group; poise sends false, discord.py nothing
                        Value::Bool(false) => {
                            (k == "required" && !subcommand) || k == "dm_permission"
                        }
                        _ => true,
                    })
                    // discord.py sends the permission bits as an int, poise as a string; both are read the same
                    .map(|(k, v)| match (k.as_str(), &v) {
                        ("default_member_permissions", Value::Number(n)) => {
                            (k.clone(), json!(n.to_string()))
                        }
                        _ => (k, norm(v)),
                    })
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.into_iter().map(norm).collect()),
        Value::Number(n) => json!(n.as_f64()),
        other => other,
    }
}

/// A top-level command in the new-API form poise sends: chat input is the default `type`, and discord.py's
/// legacy `dm_permission: false` is poise's `contexts: [guild]` (`true` is the default, no contexts).
fn modern(mut cmd: Value) -> Value {
    let m = cmd.as_object_mut().unwrap();
    m.remove("type");
    match m.remove("dm_permission") {
        Some(Value::Bool(false)) => drop(m.insert("contexts".into(), json!([0.0]))),
        _ => {}
    }
    cmd
}

/// Every path where the two values differ, one line each.
fn diff(path: &str, py: &Value, rs: &Value, out: &mut Vec<String>) {
    match (py, rs) {
        (Value::Object(a), Value::Object(b)) => {
            for k in a.keys().chain(b.keys().filter(|k| !a.contains_key(*k))) {
                diff(
                    &format!("{path}.{k}"),
                    a.get(k).unwrap_or(&Value::Null),
                    b.get(k).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                let name = x
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or(i.to_string());
                diff(&format!("{path}[{name}]"), x, y, out);
            }
        }
        _ if py != rs => out.push(format!("{path}: python {py} / rust {rs}")),
        _ => {}
    }
}

fn rust_tree() -> Value {
    let commands = lightningccp::bot::commands();
    let tree = norm(
        serde_json::to_value(poise::builtins::create_application_commands(&commands)).unwrap(),
    );
    Value::Array(
        tree.as_array()
            .unwrap()
            .iter()
            .cloned()
            .map(modern)
            .collect(),
    )
}

fn python_tree() -> Value {
    let tree = norm(serde_json::from_str(include_str!("fixtures/command_tree.json")).unwrap());
    Value::Array(
        tree.as_array()
            .unwrap()
            .iter()
            .cloned()
            .map(modern)
            .collect(),
    )
}

#[test]
fn command_tree_matches_python() {
    let (py, rs) = (python_tree(), rust_tree());
    let mut diffs = vec![];
    diff("", &py, &rs, &mut diffs);
    assert!(
        diffs.is_empty(),
        "{} differences:\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

/// The names, in Python's registration order.
#[test]
fn command_names_in_python_order() {
    let names: Vec<String> = lightningccp::bot::commands()
        .iter()
        .map(|c| c.name.to_string())
        .collect();
    let python: Vec<String> = python_tree()
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().into())
        .collect();
    assert_eq!(names, python);
    assert_eq!(json!(names).as_array().unwrap().len(), 14);
}

/// `Services::connect` opens and migrates the DB, builds every service; `close` stops the loops and closes the DB.
#[tokio::test]
async fn services_connect_and_close() {
    use lightningccp::config::Config;
    use lightningccp::services::Services;
    let dir = std::env::temp_dir().join(format!("lccp-wiring-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Services::connect(&Config { token: "x".into(), db_path: dir.join("bot.db") }).await.unwrap();
    assert!(services.problems.require().is_err() && services.contests.contests().is_empty());
    services.loops.lock().unwrap().push(tokio::spawn(std::future::pending()));
    services.close().await;
    assert!(services.loops.lock().unwrap().is_empty());
    assert!(services.db.get_handle(1, 1).await.is_err(), "the database is closed");
    std::fs::remove_dir_all(&dir).ok();
}
