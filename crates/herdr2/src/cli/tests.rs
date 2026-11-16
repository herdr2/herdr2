use super::*;
use clap::CommandFactory;

fn parsed(args: &[&str]) -> Cmd {
    Cli::try_parse_from([&["h2"], args].concat()).unwrap().cmd.unwrap()
}

#[test]
fn table_orders_children_under_parent_and_shows_columns() {
    let r = json!({
        "agents": [
            {"address": "a1", "name": "lead", "agent": "claude", "state": "live", "turn": 2, "status": {"state": "working"}, "cwd": "/work/proj"},
            {"address": "a2", "name": "worker", "parent": "a1", "argv": ["codex-acp"], "state": "live", "turn": 1, "inbox_len": 3, "status": {"state": "blocked"}, "pending_permission": {"title": "Run rm -rf"}, "cwd": "/work/proj"},
        ],
        "machines": [],
        "hidden_finished": 0,
    });
    let rendered = table(&r);
    assert!(rendered.contains("ADDRESS") && rendered.contains("DETAIL"));
    assert!(rendered.contains("lead") && rendered.contains("  a2"));
    assert!(rendered.contains("approve: Run rm -rf"));
}

#[test]
fn get_keeps_an_unsent_first_message_visible_to_people() {
    let mut a = json!({"address": "a1", "argv": ["pi-acp"], "state": "live", "cwd": "/tmp"});
    assert!(!describe(&a).contains("first message"));
    a["first_message"] = json!({"sent": false, "reason": "agent closed"});
    assert!(describe(&a).contains("first message NOT sent: agent closed"));
}

#[test]
fn every_api_method_has_a_command_or_is_api_only() {
    Cli::command().debug_assert();
    for method in crate::api::METHOD_NAMES {
        let covered = COMMANDS.iter().any(|(_, m)| m == method) || API_ONLY.contains(method);
        assert!(covered, "{method} has no command and is not marked API-only");
    }
    for (command, _) in COMMANDS {
        let mut cmd = Cli::command();
        for word in command.split(' ') {
            cmd = cmd.find_subcommand(word).unwrap_or_else(|| panic!("no command `{command}`")).clone();
        }
    }
}

#[test]
fn a_command_line_becomes_the_api_request() {
    let Cmd::Send(p) = parsed(&["send", "a3", "hi there", "--if", "idle", "--steer", "--wait", "--timeout", "2.5"])
    else {
        panic!()
    };
    let want = json!({
        "agent": "a3",
        "text": "hi there",
        "steer": true,
        "if": "idle",
        "wait": true,
        "timeout": 2.5,
        "caller": null
    });
    assert_eq!(to(&p), want);
    assert!(serde_json::from_value::<SendParams>(want).is_ok(), "and the API reads it back");

    let Cmd::Wait(p) = parsed(&["wait", "a3", "--output", "ready"]) else { panic!() };
    assert_eq!(to(&p)["output"], "ready");

    let Cmd::Run(p) = parsed(&[
        "run",
        "--agent",
        "claude",
        "--prompt",
        "fix tests",
        "--model",
        "sonnet",
        "--permissions",
        "approve-all",
        "--no-mcp",
        "--label",
        "a=1",
        "--label",
        "b=x=y",
        "--label",
        "a=2",
    ]) else {
        panic!()
    };
    let req = to(&p);
    assert_eq!(req["agent"], "claude");
    assert_eq!(req["prompt"], "fix tests");
    assert_eq!(req["model"], "sonnet");
    assert_eq!(req["permissions"], "approve-all");
    assert_eq!(req["no_mcp"], true);
    assert_eq!(req["labels"], json!({"a": "2", "b": "x=y"}));

    let Cmd::Approve(p) = parsed(&["approve", "a2", "--option", "allow-once"]) else { panic!() };
    assert_eq!(p.agent, "a2");
    assert_eq!(p.option.as_deref(), Some("allow-once"));
}

#[test]
fn a_wait_prints_what_happened() {
    let one = |reason: &str| {
        json!({"agent": "a1", "ended": reason != "timeout", "reason": reason, "status": "working", "turn": 2, "exit_code": 42, "children": []})
    };
    assert!(describe_wait(&one("timeout")).starts_with("a1 still running: working"));
    assert!(describe_wait(&one("exit")).starts_with("a1 exited (exit 42"));
    let all = json!({"agent": "a0", "reason": "children", "children": [one("exit")]});
    assert!(describe_wait(&all).starts_with("a1 exited"));
    let none = json!({"agent": "a0", "reason": "children", "children": []});
    assert_eq!(describe_wait(&none), "no children are working");
    let message = json!({"agent": "a0", "reason": "message", "from": "a2", "children": []});
    assert!(describe_wait(&message).contains("a2"), "{}", describe_wait(&message));
    let mut interrupted = one("message");
    interrupted["ended"] = false.into();
    interrupted["from"] = "a1".into();
    assert_eq!(
        describe_wait(&interrupted),
        "a message from a1 (queued in inbox)\na1 still running: working, turn 2 (wait again to keep waiting)"
    );
}

#[test]
fn older_compares_versions_as_numbers() {
    assert!(older("h2 0.1.9", "h2 0.2.0") && older("h2 0.2.0", "h2 0.10.0"));
    assert!(!older("h2 0.2.0", "h2 0.2.0") && !older("h2 0.10.0", "h2 0.9.9") && !older("h2 dev", "h2 0.2.0"));
}

#[test]
fn labels_need_a_key() {
    assert!(Cli::try_parse_from(["h2", "label", "a1", "nokey"]).is_err());
    assert!(Cli::try_parse_from(["h2", "label", "a1", "=v"]).is_err());
    let Cmd::Label(p) = parsed(&["label", "a1", "gone="]) else { panic!() };
    assert_eq!(to(&p)["labels"], json!({"gone": ""}), "an empty value removes it");
}

#[test]
fn connect_counts_the_plain_lines_the_core_answers() {
    for (line, answered, last) in [
        (r#"{"jsonrpc":"2.0","id":1,"method":"doc.get"}"#, true, false),
        (r#"{"jsonrpc":"2.0","id":null,"method":"doc.get"}"#, true, false),
        (r#"{"jsonrpc":"2.0","method":"doc.get"}"#, false, false),
        (r#"{"foo":1}"#, true, false),
        ("[1,2]", true, false),
        ("not json", true, false),
        (" \r", false, false),
        (r#"{"id":1,"method":"events.subscribe","params":{}}"#, true, true),
        (r#"{"id":1,"method":"hello","params":{"framed":true}}"#, true, true),
        (r#"{"id":1,"method":"hello","params":{"framed":false}}"#, true, false),
    ] {
        assert_eq!(plain_line(format!("{line}\n").as_bytes()), (answered, last), "{line}");
    }
}
