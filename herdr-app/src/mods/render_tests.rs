//! Shipped JS descriptions and native rendering regressions.
use super::*;
use gpui::TestAppContext;
use gpui_shell::{ShellRuntime, plugin::DescriptionSession, policy::Policy};
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};


#[gpui::test]
fn mod_text_selection_stays_in_its_shell_root(cx: &mut TestAppContext) {
    use gpui::*;
    use gpui_base::{SelectableText, TextSelection, TextView, TextViewState};
    use gpui_shell::ShellRoot;

    struct Column {
        texts: Vec<Entity<TextViewState>>,
        label: &'static str,
    }
    impl Render for Column {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().flex().flex_col().size_full().gap(px(20.))
                .children(self.texts.iter().map(|text| TextView::new(text).scrollable(false).selectable(true)))
                .child(SelectableText::new("plain", format!("{} plain text", self.label)).document_order(u64::MAX))
        }
    }
    struct Columns {
        roots: Vec<Entity<ShellRoot>>,
    }
    impl Render for Columns {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().flex().size_full().children(self.roots.iter().map(|root| {
                div().w(px(240.)).h_full().child(root.clone())
            }))
        }
    }
    cx.update(gpui_shell::init);
    let mut texts = Vec::new();
    let (_, cx) = cx.add_window_view(|window, cx| {
        let roots = ["LEFT", "CENTER", "RIGHT"].into_iter().map(|label| {
            let blocks = ["first", "middle", "second"].into_iter().map(|block| {
                cx.new(|cx| TextViewState::markdown(&format!("{label} {block} block"), cx))
            }).collect::<Vec<_>>();
            texts.push(blocks.clone());
            let content = cx.new(|_| Column { texts: blocks, label }).into();
            cx.new(|cx| ShellRoot::new(content, window, cx))
        }).collect();
        Columns { roots }
    });
    cx.run_until_parked();
    let bounds = |column: usize, block: usize, cx: &mut VisualTestContext| {
        texts[column][block].read_with(cx, |text, _| text.bounds())
    };
    let start = bounds(1, 0, cx).origin + point(px(1.), px(10.));
    let inside = bounds(1, 2, cx);
    let neighbor = bounds(2, 2, cx);
    for end in [
        point(inside.right() - px(1.), inside.top() + px(10.)),
        point(neighbor.right() - px(1.), neighbor.top() + px(10.)),
        point(px(900.), px(400.)),
        point(px(-20.), px(400.)),
    ] {
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        let copied = cx.update(|window, cx| TextSelection::selected_text(window, cx));
        assert!(copied.contains("CENTER first block"), "{copied:?}");
        assert!(copied.contains("CENTER middle block"), "end {end:?}: {copied:?}");
        assert!(copied.contains("CENTER second block"), "end {end:?}: {copied:?}");
        assert!(!copied.contains("LEFT") && !copied.contains("RIGHT"), "{copied:?}");
        if end.y == px(400.) {
            assert!(copied.contains("CENTER plain text"), "{copied:?}");
        }
        cx.simulate_mouse_move(neighbor.center(), None, Modifiers::default());
        assert_eq!(cx.update(|window, cx| TextSelection::selected_text(window, cx)), copied);
    }
}

struct Staged(PathBuf);
impl Staged {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = crate::test_tmp().join(format!(
            "herdr-render-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        let staged = Self(path);
        for (folder, files) in [("mods", DEFAULT_MODS), ("frame", FRAME_MODS)] {
            for (name, bytes) in files {
                let path = staged.0.join(folder).join(name);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, bytes).unwrap();
            }
        }
        staged
    }
    fn entries(&self) -> Vec<String> {
        let mut entries = Vec::new();
        for folder in ["mods", "frame"] {
            for entry in std::fs::read_dir(self.0.join(folder)).unwrap() {
                let entry = entry.unwrap();
                assert!(entry.path().join("main.js").is_file(), "missing main.js: {:?}", entry.path());
                entries.push(format!("{folder}/{}", entry.file_name().to_str().unwrap()));
            }
        }
        entries.sort();
        entries
    }
    fn wrapper(&self, entry: &str, setup: &str) -> PathBuf {
        let dir = self.0.clone();
        std::fs::write(dir.join("main.js"), format!(
            "import Real from './{entry}/main.js'; export default class Scenario extends Real {{ init(props, cx) {{ super.init(props, cx); {setup} }} }}"
        )).unwrap();
        dir
    }
}
impl Drop for Staged {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[derive(Default)]
struct Calls {
    unexpected: Vec<String>,
    effects: Vec<(String, Vec<Value>)>,
    shared: BTreeMap<String, Value>,
    interrupted: Vec<String>,
    files: BTreeMap<String, String>,
    images: BTreeMap<String, String>,
    reads: Vec<String>,
    writes: Vec<(String, String, Value, Value)>,
}

fn updates(app: &str, core: &str) -> Value {
    json!({
        "app": {"version":"0.1.0", "updates":app != "source", "ready": (app == "ready").then_some("0.1.1")},
        "core": {"version":"0.1.5", "ready":(core == "ready").then_some("0.1.6"), "managed":(core == "managed").then_some("homebrew"), "error":(core == "error").then_some("offline"), "updating":core == "updating"},
        "checked":1700000000, "checks":app != "source", "checking":false
    })
}

fn fixture(populated: bool, props: Value, update: Value) -> BTreeMap<String, Value> {
    let program =
        json!({"key":"pi", "name":"Pi", "icon":null, "tint":true, "tile":"#334455", "color":"#aabbcc", "shell":false, "agent":true});
    let mut values = BTreeMap::from([
        ("props".into(), props),
        ("checking".into(), json!(false)),
        ("machines".into(), json!([{ "name": null, "layout": "ready" }])),
        ("spaces".into(), json!([])), ("agents".into(), json!([])), ("terminals".into(), json!([])), ("panes".into(), json!([])), ("focused".into(), Value::Null),
        ("layout".into(), serde_json::from_str(include_str!("../../defaults/layouts/main.json")).unwrap()),
        ("layouts".into(), json!(crate::window::shipped_layouts())),
        ("theme".into(), serde_json::to_value(&*crate::settings::theme()).unwrap()),
        ("settings".into(), serde_json::to_value(crate::settings::Settings::default()).unwrap()),
        ("themes".into(), json!([{"name":"graphite", "colors": &*crate::settings::theme()}])),  ("fonts".into(), json!(["Monospace", "Sans"])),
        ("updates".into(), update),
        ("telemetry".into(), json!({"enabled":true,"setting":true,"sends":true,"off_because":null,"endpoint":"https://invalid.example", "draft":{},"queued":[],"last_sent":null})),
        ("window".into(), json!({"platform":"linux","titlebar":"none","buttons":true,"maximized":false,"fullscreen":false,"active":true})),
        ("placement".into(), Value::Null),
        ("home".into(), json!("/fixture")), ("appHome".into(), json!("/fixture/app")),
        ("keys".into(), Value::Array(serde_json::from_str::<BTreeMap<String, BTreeMap<String, String>>>(include_str!("../../defaults/keys.json")).unwrap().into_iter().flat_map(|(section, bindings)| bindings.into_iter().map(move |(keys, action)| json!({"section":section,"keys":keys,"action":action}))).collect())),
        ("commands".into(), Value::Array(crate::commands::COMMANDS.iter().map(|c| json!({"name":c.name,"about":c.about,"args":c.args.iter().map(|a| json!({"name":a.name,"required":a.required})).collect::<Vec<_>>()})).collect())),
    ]);
    if populated {
        values.insert("spaces".into(), json!([
            {"id":"s1","index":1,"name":"Local","folder":"/fixture/project","machine":null,"branch":"main","active":true,"status":"working","tabs":[{"id":"tab1","index":1,"name":"Build","active":true,"status":"working","panes":1,"zoomed":false,"settled":false,"settled_at":null,"changed_at":1700000000000i64,"working_since":1700000000000i64}]},
            {"id":"s2","index":2,"name":"Remote","folder":"/remote/project","machine":"remote","branch":null,"active":false,"status":"done","tabs":[{"id":"tab2","index":1,"name":"Done","active":true,"status":"done","panes":1,"zoomed":false,"settled":false,"settled_at":null,"changed_at":null,"working_since":null}]}
        ]));
        values.insert("panes".into(), json!([{"pane":"p1","terminal":"t1","tab":"tab1","title":"Build","cwd":"/fixture/project","branch":"main","status":"working","app":"pi","program":program,"focused":true,"zoomed":false}, {"pane":"p2","terminal":"t2","tab":"tab2","title":"Remote task","cwd":"/remote/project","branch":null,"status":"done","app":"pi","program":program,"focused":false,"zoomed":false}]));
        values.insert("terminals".into(), json!([{"address":"t1","name":"Build","title":"Build","argv":["pi"],"parent":null,"labels":{},"state":"running","status":"working","report":{"app":"pi"},"program":program,"cwd":"/fixture/project"}, {"address":"t2","name":"Remote task","title":"Remote task","argv":["pi"],"parent":null,"labels":{},"state":"running","status":"done","report":{"app":"pi"},"program":program,"cwd":"/remote/project"}]));
        values.insert("agents".into(), json!([{"address":"t1","pane":"p1","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Build","status":"working","app":"pi","program":program,"title":"Build","seq":1,"focused":true}, {"address":"t2","pane":"p2","space":"s2","space_name":"Remote","tab":"tab2","tab_index":1,"tab_name":"Done","status":"done","app":"pi","program":program,"title":"Remote task","seq":2,"focused":false}]));
        values.insert(
            "focused".into(),
            json!({"space":"s1","tab":"tab1","pane":"p1","terminal":"t1","agent":"t1","shown":null,"cwd":"/fixture/project"}),
        );
    }
    values
}

fn module(values: BTreeMap<String, Value>, calls: Rc<RefCell<Calls>>) -> HostModule {
    let mut module = HostModule::new("herdr");
    for (name, value) in values {
        module = module.function(name, move |_| Ok(to_host(&value)));
    }
    module = module.function("feedbackPreview", |a| {
        Ok(to_host(
            &json!({"available":true,"body":{"message":a.string(0)?,"email":a.string(1)?}}),
        ))
    });
    let shared = calls.clone();
    module = module.function("shared", move |a| {
        Ok(shared.borrow().shared.get(a.string(0)?).map(to_host).unwrap_or(HostValue::Null))
    });
    let shared = calls.clone();
    module = module.function("setShared", move |a| {
        shared.borrow_mut().shared.insert(a.string(0)?.into(), from_host(a.value(1)?));
        Ok(HostValue::Null)
    });
    for name in ["run", "playSound", "setAgentOrder", "setWindowTitle", "setBadge", "openOverlay", "closeOverlay", "notify", "presentUpdateNotes"] {
        let calls = calls.clone();
        module = module.function(name, move |a| {
            if name == "run" {
                let command = a.string(0)?;
                let command = crate::commands::find(command).unwrap_or_else(|| panic!("unknown shipped mod command {command}"));
                let args = a.get(1).map(from_host).unwrap_or(Value::Null);
                crate::commands::validate_args(command, &args).unwrap();
            }
            calls
                .borrow_mut()
                .effects
                .push((name.into(), (0..2).filter_map(|i| a.get(i).map(from_host)).collect()));
            Ok(if name == "openOverlay" || name == "notify" {
                "fixture-overlay".into()
            } else {
                HostValue::Null
            })
        });
    }
    for name in ["feedbackPasteImage", "feedbackClearImages", "feedbackAttachImages"] {
        module = module.function(name, |_| Ok(HostValue::Null));
    }
    module = module.async_function("feedbackPickImages", |_| Ok(async { Ok(HostValue::Array(vec![])) }));
    module = module.async_function("knownAgents", |_| Ok(async { Ok(HostValue::Array(vec![])) }));
    module = module.async_function("pickFolder", |_| Ok(async { Ok(HostValue::Null) }));
    module = module.async_function("nextCommand", |_| Ok(std::future::pending::<gpui_shell::HostResult>()));
    module = module.cancellable_async_function("watchFolder", HostValue::Array(vec![]), |_| Ok(std::future::pending::<gpui_shell::HostResult>()));
    for name in ["exec", "hc", "h2", "readTextFile", "readImageDataUrl", "writeTextFile", "sendFeedback"] {
        let calls = calls.clone();
        module = module.async_function(name, move |a| {
            let first = a.get(0).map(from_host).unwrap_or(Value::Null);
            let reply = match name {
                "readImageDataUrl" => Some(
                    first
                        .as_str()
                        .and_then(|p| calls.borrow().images.get(p).cloned())
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                ),
                "readTextFile" if first == "/fixture/app/settings.json" || first == "/fixture/.ssh/config" => Some(Value::Null),
                "readTextFile" if first.as_str().is_some_and(|p| calls.borrow().files.contains_key(p)) => {
                    first.as_str().and_then(|p| {
                        calls.borrow_mut().reads.push(p.to_owned());
                        calls.borrow().files.get(p).cloned()
                    }).map(Value::String)
                }
                "readTextFile" => first
                    .as_str()
                    .and_then(|p| p.strip_prefix("/fixture/app/layouts/"))
                    .and_then(|p| p.strip_suffix(".json"))
                    .filter(|n| crate::window::shipped_layouts().contains(n))
                    .map(|_| json!("{}")),
                "writeTextFile" => {
                    let text = a.get(1).map(from_host).unwrap_or(Value::Null);
                    let expected = a.get(2).map(from_host).unwrap_or(Value::Null);
                    let on = a
                        .get(3)
                        .map(from_host)
                        .and_then(|v| v.get("on").cloned())
                        .unwrap_or(Value::Null);
                    if let (Some(path), Some(text_str)) = (first.as_str(), text.as_str()) {
                        let mut c = calls.borrow_mut();
                        c.writes.push((path.to_owned(), text_str.to_owned(), expected, on));
                        c.files.insert(path.to_owned(), text_str.to_owned());
                        Some(Value::Null)
                    } else {
                        None
                    }
                }
                "hc" | "h2" if first == json!(["machine", "ls", "--json"]) => {
                    Some(json!({"code":0,"stdout":"{\"machines\":[{\"name\":\"remote\",\"state\":\"online\"}]}","stderr":""}))
                }
                "hc" | "h2" if first == json!(["machine", "add", "host", "--json"]) => {
                    Some(json!({"code":1,"stdout":"{\"error\":{\"kind\":\"host_key\",\"message\":\"unknown host key\"}}","stderr":""}))
                }
                "hc" | "h2" if first == json!(["read", "--json", "t1"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "complete": true,
                            "how": "transcript",
                            "entries": [
                                {"kind":"user","turn":1,"text":"Fix CLI layout","ts_ms":1700000000000i64},
                                {"kind":"assistant","turn":1,"text":"Updated the layout.","ts_ms":1700000001000i64}
                            ]
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["get", "--json", "t1"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "address": "t1",
                            "state": "live",
                            "cwd": "/fixture/project"
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["read", "--json", "t_idle"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "complete": true,
                            "how": "screen",
                            "entries": [],
                            "text": "────────────────────────────────────\n ▐▛███▛█ Claude Code\n❯ Try \"fix lint errors\""
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["read", "--json", "t_snap"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "complete": true,
                            "how": "screen",
                            "entries": [],
                            "text": "────────────────────────────────────\nbuild succeeded in 2.4s"
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["read", "--json", "t_rich"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "complete": true,
                            "how": "transcript",
                            "entries": [
                                {"kind":"user","turn":1,"text":"编译+安装","ts_ms":1700000000000i64},
                                {"kind":"thought","turn":1,"text":"Running release build and checking targets.","ts_ms":1700000001000i64},
                                {"kind":"tool_call","turn":1,"id":"tc1","title":"cargo xwin build --release","tool_kind":"execute","status":"completed","ts_ms":1700000002000i64},
                                {"kind":"assistant","turn":1,"text":"已完成编译与安装，结果如下：\n\n| 目标 | 状态 | 产物 |\n|---|---|---|\n| Linux Core | 通过 | `herdr2` |\n| Windows App | 通过 | `herdr2.exe` |\n\n1. 验证单测：\n\n   ```sh\n   cargo test -p herdr2\n   ```\n\n```rust\nfn main() {\n    println!(\"ok\");\n}\n```","ts_ms":1700000003000i64},
                                {"kind":"user","turn":2,"text":"请对比一下：第一行：检查对齐；\n第二行：检查多行用户气泡宽度与边框是否重叠。","ts_ms":1700000010000i64},
                                {"kind":"assistant","turn":2,"text":"已确认气泡、表格和代码块均对齐且无双重边框。","ts_ms":1700000011000i64}
                            ]
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["read", "--json", "a1"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "complete": false,
                            "entries": [
                                {"kind":"user","turn":1,"from":"t1","text":"<h2 from=\"t1\">Fix bug</h2>","ts_ms":1700000000000i64},
                                {"kind":"thought","turn":1,"text":"Inspecting parser state.","ts_ms":1700000001000i64},
                                {"kind":"tool_call","turn":1,"id":"tc1","title":"Read src/main.rs","tool_kind":"read","status":"completed","ts_ms":1700000002000i64},
                                {"kind":"plan","turn":1,"items":["Check inputs","Add regression test"],"ts_ms":1700000003000i64},
                                {"kind":"assistant","turn":1,"text":"Found the issue.\n\n```rust\nfn fix() -> bool { true }\n```","ts_ms":1700000004000i64},
                                {"kind":"stderr","text":"warn: unused helper","ts_ms":1700000005000i64},
                                {"kind":"error","turn":2,"message":"Sandbox warning","ts_ms":1700000006000i64}
                            ]
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["get", "--json", "a1"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "address": "a1",
                            "config_options": [
                                {"id":"model","name":"Model","category":"model","kind":"select","current_value":"gpt-5.4","options":[{"value":"gpt-5.4","name":"GPT-5.4"},{"value":"gpt-5.3-codex","name":"GPT-5.3 Codex"}]},
                                {"id":"reasoning_effort","name":"Reasoning Effort","category":"thought_level","kind":"select","current_value":"high","options":[{"value":"low","name":"Low"},{"value":"high","name":"High"}]},
                                {"id":"fast-mode","name":"Fast Mode","category":"model_config","kind":"boolean","current_value":true},
                                {"id":"mode","name":"Mode","category":"mode","kind":"select","current_value":"code","options":[{"value":"code","name":"Code"},{"value":"architect","name":"Architect"}]}
                            ]
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["config", "--json", "a1", "--id", "fast-mode", "--bool", "true"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "address": "a1",
                            "config_options": [
                                {"id":"model","name":"Model","category":"model","kind":"select","current_value":"gpt-5.4","options":[{"value":"gpt-5.4","name":"GPT-5.4"},{"value":"gpt-5.3-codex","name":"GPT-5.3 Codex"}]},
                                {"id":"fast-mode","name":"Fast Mode","category":"model_config","kind":"boolean","current_value":true}
                            ]
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["inbox", "edit", "--json", "a1", "102", "Updated second prompt"]) => {
                    calls.borrow_mut().effects.push(("inbox.edit".into(), vec![first.clone()]));
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "address": "a1",
                            "inbox_len": 2,
                            "inbox": [
                                {"id": 101, "text": "First queued prompt", "queued_ms": 1700000000000i64},
                                {"id": 102, "text": "Updated second prompt", "from": "t1", "queued_ms": 1700000060000i64}
                            ]
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["inbox", "rm", "--json", "a1", "101"]) => {
                    calls.borrow_mut().effects.push(("inbox.rm".into(), vec![first.clone()]));
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "address": "a1",
                            "inbox_len": 1,
                            "inbox": [
                                {"id": 102, "text": "Updated second prompt", "from": "t1", "queued_ms": 1700000060000i64}
                            ]
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["interrupt", "--json", "a1"]) => {
                    calls.borrow_mut().interrupted.push("a1".into());
                    Some(json!({"code":0,"stdout":"{\"outcome\":\"stopped\"}","stderr":""}))
                }
                "hc" | "h2" if first == json!(["interrupt", "--json", "t1"]) => {
                    calls.borrow_mut().interrupted.push("t1".into());
                    Some(json!({"code":0,"stdout":"{\"outcome\":\"stopped\"}","stderr":""}))
                }
                "hc" | "h2" if first == json!(["checkpoint", "ls", "--json", "t1"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "target": "t1",
                            "isolation": {"level": "isolated_worktree", "blockers": []},
                            "checkpoints": [
                                {"turn": 0, "ts_ms": 1000, "files": []},
                                {"turn": 1, "prompt": "Fix CLI layout", "ts_ms": 2000, "files": [{"path": "src/main.rs", "added": 2, "deleted": 1}]}
                            ]
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["checkpoint", "diff", "--json", "t1", "--to", "1"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "turn": 1,
                            "patch": "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,2 @@\n-old\n+new\n+added\n"
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                // The files panel lists a folder with sh: kind and name pairs.
                "exec" if first == "sh" && a.get(1).map(from_host).and_then(|v| v.get(2).cloned()) == Some(json!("files")) => {
                    Some(json!({"code":0,"stdout":"d\u{0}src\u{0}f\u{0}README.md\u{0}","stderr":""}))
                }
                "exec"
                    if first == "git"
                        && [
                            json!(["rev-parse", "--show-toplevel"]),
                            json!(["rev-parse", "--path-format=absolute", "--git-common-dir"]),
                            json!(["status", "--porcelain=v2", "--branch"]),
                            json!(["--no-optional-locks", "-c", "core.fsmonitor=false", "status", "--porcelain=v2", "--branch"]),
                            json!(["--no-optional-locks", "-c", "core.fsmonitor=false", "status", "--porcelain"]),
                            json!(["rev-parse", "--abbrev-ref", "HEAD"]),
                        ]
                        .contains(&a.get(1).map(from_host).unwrap_or(Value::Null)) =>
                {
                    Some(json!({"code":128,"stdout":"","stderr":"fixture: not a git repository"}))
                }
                _ => None,
            };
            let result = if let Some(reply) = reply {
                Ok(to_host(&reply))
            } else {
                let message = format!("unexpected {name}: {first}, {:?}", a.get(1));
                calls.borrow_mut().unexpected.push(message.clone());
                Err(HostError::new(message))
            };
            Ok(std::future::ready(result))
        });
    }
    module.declarations(DECLARATIONS)
}

#[gpui::test]
fn mod_copies_share_code_without_sharing_props_state_or_lifetime(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let root = staged.0.join("cache");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.js"), r#"
import { View, div } from 'gpui-kit';
import { props, mark, next } from 'copy';
const evaluation = mark();
const row = globalThis.__template(value => div().child(value));
export default class Panel extends View {
  init(_, cx) {
    if (props() === 'bad') throw Error('bad props');
    this.count = 0;
    this.count++;
    this.async = 'waiting';
    cx.spawn(async cx => { await next(); this.async = props(); cx.notify(); });
  }
  render() { return row(props() + ':' + this.count + ':' + evaluation + ':' + this.async); }
}
"#).unwrap();
    let marks = Rc::new(Cell::new(0));
    let policy = |name: &'static str| {
        let marks = marks.clone();
        Policy::new().with_host_module(HostModule::new("copy")
            .function("props", move |_| Ok(name.into()))
            .function("mark", move |_| { marks.set(marks.get() + 1); Ok((marks.get() as f64).into()) })
            .async_function("next", |_| Ok(std::future::ready(Ok(HostValue::Null))))
        ).unwrap()
    };
    let runtime = ShellRuntime::new_isolated().unwrap();
    let window = cx.add_window(|_, _| Empty);
    let (first, mut second) = window.update(cx, |_, window, cx| {
        let mut first = DescriptionSession::load(runtime.clone(), &root, policy("A"), window, cx).unwrap();
        let mut second = DescriptionSession::load(runtime.clone(), &root, policy("B"), window, cx).unwrap();
        assert!(first.describe(window, cx).unwrap().contains("A:1:1"));
        assert!(second.describe(window, cx).unwrap().contains("B:1:1"));
        assert!(DescriptionSession::load(runtime.clone(), &root, policy("bad"), window, cx).is_err());
        (first, second)
    }).unwrap();
    cx.run_until_parked();
    drop(first);
    cx.run_until_parked();
    window.update(cx, |_, window, cx| {
        let tree = second.describe(window, cx).unwrap();
        assert!(tree.contains("B:1:1:B"), "{tree}");
    }).unwrap();
    drop(second);
    cx.run_until_parked();
    window.update(cx, |_, window, cx| {
        let mut reopened = DescriptionSession::load(runtime.clone(), &root, policy("C"), window, cx).unwrap();
        assert!(reopened.describe(window, cx).unwrap().contains("C:1:1"));
    }).unwrap();
    assert_eq!(marks.get(), 1, "reopening and failed init must reuse evaluated code");
    window.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();
}

#[gpui::test]
fn mod_code_revisions_share_successes_and_failures_without_poisoning_old_copies(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let root = staged.0.join("cache");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.js"), r#"
import { View, div } from 'gpui-kit';
import { version } from './part.js';
export default class Panel extends View { render() { return div().child('version:' + version); } }
"#).unwrap();
    std::fs::write(root.join("part.js"), "import { mark } from 'copy'; mark(); export const version = 1;").unwrap();
    let marks = Rc::new(Cell::new(0));
    let policy = || {
        let marks = marks.clone();
        Policy::new().with_host_module(HostModule::new("copy").function("mark", move |_| {
            marks.set(marks.get() + 1); Ok(HostValue::Null)
        })).unwrap()
    };
    let runtime = ShellRuntime::new_isolated().unwrap();
    let window = cx.add_window(|_, _| Empty);
    window.update(cx, |_, window, cx| {
        let mut old = DescriptionSession::load(runtime.clone(), &root, policy(), window, cx).unwrap();
        for version in [2, 3] {
            std::fs::write(root.join("part.js"), format!("import {{ mark }} from 'copy'; mark(); export const version = {version};{}", " ".repeat(version))).unwrap();
            for _ in 0..20 {
                let mut new = DescriptionSession::load(runtime.clone(), &root, policy(), window, cx).unwrap();
                assert!(new.describe(window, cx).unwrap().contains(&format!("version:{version}")));
            }
        }
        assert_eq!(marks.get(), 3);
        std::fs::write(root.join("part.js"), "import { mark } from 'copy'; mark(); throw Error('broken save'); export const version = 4;").unwrap();
        for _ in 0..20 {
            assert!(DescriptionSession::load(runtime.clone(), &root, policy(), window, cx).is_err());
        }
        assert_eq!(marks.get(), 4, "a failed revision evaluates once, not on every open");
        assert!(old.describe(window, cx).unwrap().contains("version:1"));
        std::fs::write(root.join("part.js"), "import { mark } from 'copy'; mark(); export const version = 5;").unwrap();
        let mut fixed = DescriptionSession::load(runtime.clone(), &root, policy(), window, cx).unwrap();
        assert!(fixed.describe(window, cx).unwrap().contains("version:5"));
        assert_eq!(marks.get(), 5);
        assert!(DescriptionSession::load(runtime.clone(), &root, Policy::new(), window, cx).is_err(),
            "an ungranted host module must not reuse another policy's linked class");
    }).unwrap();
    window.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();
}

#[gpui::test]
fn mod_host_imports_keep_sync_async_and_missing_export_surfaces_distinct(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let root = staged.0.join("cache");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("main.js"), r#"
import { View, div } from 'gpui-kit';
import { value } from 'copy';
export default class Panel extends View {
  init() { this.kind = typeof value(); }
  render() { return div().child('kind:' + this.kind); }
}
"#).unwrap();
    let runtime = ShellRuntime::new_isolated().unwrap();
    let window = cx.add_window(|_, _| Empty);
    window.update(cx, |_, window, cx| {
        let sync = HostModule::new("copy").function("value", |_| Ok(HostValue::Number(1.0)));
        let mut a = DescriptionSession::load(runtime.clone(), &root, Policy::new().with_host_module(sync).unwrap(), window, cx).unwrap();
        assert!(a.describe(window, cx).unwrap().contains("kind:number"));
        let asynchronous = HostModule::new("copy").async_function("value", |_| Ok(std::future::ready(Ok(HostValue::Null))));
        let mut b = DescriptionSession::load(runtime.clone(), &root, Policy::new().with_host_module(asynchronous).unwrap(), window, cx).unwrap();
        assert!(b.describe(window, cx).unwrap().contains("kind:object"));
        let missing = HostModule::new("copy").function("other", |_| Ok(HostValue::Null));
        assert!(DescriptionSession::load(runtime.clone(), &root, Policy::new().with_host_module(missing).unwrap(), window, cx).is_err());
        assert!(a.describe(window, cx).unwrap().contains("kind:number"));
    }).unwrap();
    window.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();
}

struct Empty;
impl gpui::Render for Empty {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        gpui::div()
    }
}

fn describe(
    cx: &mut TestAppContext,
    staged: &Staged,
    entry: &str,
    populated: bool,
    props: Value,
    update: Value,
    setup: Option<&str>,
) -> anyhow::Result<String> {
    describe_with_values(cx, staged, entry, fixture(populated, props, update), setup)
}

fn describe_with_values(
    cx: &mut TestAppContext,
    staged: &Staged,
    entry: &str,
    values: BTreeMap<String, Value>,
    setup: Option<&str>,
) -> anyhow::Result<String> {
    let calls = Rc::new(RefCell::new(Calls::default()));
    static NEXT_STORE: AtomicU64 = AtomicU64::new(0);
    let policy = Policy::new()
        .with_storage_path(staged.0.join(format!("store-{}.json", NEXT_STORE.fetch_add(1, Ordering::Relaxed))))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(values, calls.clone()))?;
    let runtime = ShellRuntime::new_isolated()?;
    let root = setup.map(|s| staged.wrapper(entry, s)).unwrap_or_else(|| staged.0.join(entry));
    let window = cx.add_window(|_, _| Empty);
    let result = window.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(runtime, root, policy, window, cx)?;
        let first = session.describe(window, cx)?;
        let second = session.describe(window, cx)?;
        assert!(!first.is_empty() && !second.is_empty(), "empty description: {entry}");
        Ok(second)
    })?;
    window.update(cx, |_, window, _| window.remove_window())?;
    // localStorage writes land on the executor: let them finish while the
    // staged folder is still there, or they recreate it after its drop.
    cx.run_until_parked();
    assert!(calls.borrow().unexpected.is_empty(), "{entry}: {:?}", calls.borrow().unexpected);
    result
}

fn props() -> Value {
    json!({"overlay":"o1","pane":"p1","terminal":"t1","space":"s1","tab":"pane","target":{"pane":"p1","terminal":"t1","id":"s1","space":"s1"},"title":"Fixture","value":"Name","message":"A prefilled message","command":{"name":"space.rename","args":{"id":"s1"}},"notification":{"title":"Finished","body":"Build done","action":{"name":"app.help"}},"action":"new"})
}

#[gpui::test]
fn shipped_initial_descriptions(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let start = Instant::now();
    let staged = Staged::new();
    let entries = staged.entries();
    let mut count = 0;
    for entry in &entries {
        for populated in [false, true] {
            describe(cx, &staged, entry, populated, props(), updates("current", "current"), None)
                .unwrap_or_else(|e| panic!("{entry}/{}: {e:#}", if populated { "populated" } else { "empty" }));
            count += 1;
        }
    }
    eprintln!(
        "{count} initial-description cases ({} entries, each twice): {:?}",
        entries.len(),
        start.elapsed()
    );
}

/// A mod's localStorage write lands on the executor; one that lands after the
/// staged folder's drop recreates it, and the folder outlives the test.
#[gpui::test]
fn a_described_mod_leaves_no_folder_behind(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let path = staged.0.clone();
    describe(cx, &staged, "mods/agents", true, props(), updates("current", "current"), None).unwrap();
    drop(staged);
    cx.run_until_parked();
    assert!(!path.exists(), "left behind: {:?}", std::fs::read_dir(&path).map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>()));
}

#[gpui::test]
fn brand_leaves_room_for_macos_traffic_lights_only_outside_full_screen(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    for (fullscreen, inset) in [(false, true), (true, false)] {
        let mut values = fixture(true, props(), updates("current", "current"));
        values.insert("window".into(), json!({"platform":"macos","titlebar":"none","fullscreen":fullscreen,"buttons":false}));
        let tree = describe_with_values(cx, &staged, "mods/workspace-chrome", values, None).unwrap();
        let root = tree.lines().next().unwrap();
        assert_eq!(root.contains(".pl[Number(76.0)]"), inset, "fullscreen={fullscreen}: {tree}");
    }
}

#[gpui::test]
fn tabs_leave_room_for_macos_traffic_lights(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    for (platform, titlebar, fullscreen, width, lights, zoom, expected) in [
        ("macos", "none", false, 44, true, 1.0, 32),
        ("macos", "none", false, 44, true, 0.5, 108),
        ("macos", "none", false, 44, true, 2.0, 10),
        ("macos", "none", false, 60, true, 1.0, 16),
        ("macos", "none", false, 76, true, 1.0, 10),
        ("macos", "none", false, 268, true, 1.0, 10),
        ("macos", "none", true, 44, true, 1.0, 10),
        ("macos", "system", false, 44, true, 1.0, 10),
        ("macos", "none", false, 44, false, 1.0, 10),
        ("linux", "none", false, 44, true, 1.0, 10),
        ("windows", "none", false, 44, true, 1.0, 10),
    ] {
        let mut values = fixture(true, props(), updates("current", "current"));
        values.insert("window".into(), json!({"platform":platform,"titlebar":titlebar,"fullscreen":fullscreen,"buttons":false}));
        values.get_mut("settings").unwrap()["zoom"] = json!(zoom);
        let layout = values.get_mut("layout").unwrap();
        layout["children"][0]["size"]["px"] = json!(width);
        if !lights {
            layout.as_object_mut().unwrap().remove("traffic_lights");
        }
        let tree = describe_with_values(cx, &staged, "mods/tabs", values, None).unwrap();
        let root = tree.lines().next().unwrap();
        assert!(root.contains(&format!(".pl[Number({expected}.0)]")), "{platform}/{titlebar}/{fullscreen}/{width}/{lights}: {tree}");
        assert!(tree.contains("tab-tab1"), "first tab must remain visible: {tree}");
    }
}

#[gpui::test]
fn windows_tab_buttons_use_matching_vector_icons(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    for platform in ["windows", "linux"] {
        for maximized in [false, true] {
            let mut values = fixture(true, props(), updates("current", "current"));
            values.insert("window".into(), json!({"platform":platform,"buttons":true,"maximized":maximized}));
            let tree = describe_with_values(cx, &staged, "mods/tabs", values, None).unwrap();
            if platform == "windows" {
                for icon in ["win-min.svg", if maximized { "win-restore.svg" } else { "win-max.svg" }, "win-close.svg"] {
                    assert!(tree.contains(icon), "missing {icon}: {tree}");
                }
            } else {
                assert!(!tree.contains("mods/tabs/win-"), "{tree}");
                assert!(tree.contains("mods/pane/close.svg"), "{tree}");
            }
        }
    }
}

#[gpui::test]
fn font_pickers_contain_scroll_on_the_wrapper(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    for which in ["font", "ui_font"] {
        let mut p = props();
        p["section"] = json!("look");
        let tree = describe(
            cx, &staged, "frame/settings", true, p, updates("current", "current"),
            Some(&format!("this.picking = '{which}';")),
        ).unwrap();
        let scroll = tree.lines().find(|line| line.contains(&format!("st-fl-{which}"))).unwrap();
        assert!(scroll.contains("overflow_y_scrollbar"), "{tree}");
        assert!(!scroll.contains("on_scroll_wheel"), "the native scroller must receive the event: {tree}");
        assert_eq!(tree.matches("on_scroll_wheel").count(), 1, "the wrapper must stop scroll chaining: {tree}");
    }
}

struct RootedView(gpui::Entity<gpui_shell::ShellRoot>);
impl gpui::Render for RootedView {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        self.0.clone()
    }
}

#[gpui::test]
fn a_mod_hears_a_pinch_only_over_its_element(cx: &mut TestAppContext) {
    use gpui::{Modifiers, PinchEvent, TouchPhase, VisualTestContext, point, px};

    cx.update(gpui_shell::init);
    let staged = Staged::new();
    std::fs::write(staged.0.join("main.js"), r#"
import { View, div } from "gpui-kit";
import { v_flex } from "gpui-base";
export default class Demo extends View {
  init() { this.log = []; }
  render() {
    return v_flex().w(300).h(200)
      .child(div().w(100).h(50).on_pinch((event, cx) => {
        this.log.push(`${event.delta}:${event.touch_phase}:${event.local_position.x}:${event.modifiers.platform}`);
        cx.notify();
      }))
      .child(div().child(`log=${this.log.join(" ")}`));
  }
}
"#).unwrap();
    let runtime = ShellRuntime::new_isolated().unwrap();
    let window = cx.add_window(|window, cx| {
        RootedView(runtime.try_load(&staged.0, window, cx).unwrap())
    });
    let mut context = VisualTestContext::from_window(window.into(), cx);
    context.update(|window, cx| window.draw(cx).clear(cx));
    let view: gpui::Entity<gpui_shell::ScriptView> = window
        .root(&mut context)
        .unwrap()
        .read_with(&context, |root, cx| root.0.read(cx).content().clone().downcast().unwrap());

    let pinch = |x: f32, delta: f32, phase: TouchPhase, platform: bool| PinchEvent {
        position: point(px(x), px(20.)),
        delta,
        modifiers: Modifiers { platform, ..Modifiers::default() },
        phase,
    };
    // A hitbox counts as hovered only once the last input is the pointer.
    context.simulate_mouse_move(point(px(20.), px(20.)), None, Modifiers::default());
    context.simulate_event(pinch(20., 0.5, TouchPhase::Moved, true));
    // Outside the element: GPUI only calls the listener on its hitbox.
    context.simulate_event(pinch(250., 0.25, TouchPhase::Moved, false));
    context.simulate_event(pinch(30., -0.25, TouchPhase::Ended, false));
    context.run_until_parked();
    context.update(|window, cx| window.draw(cx).clear(cx));

    let tree = context.update(|_, cx| {
        view.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(tree.contains("log=0.5:moved:20:true -0.25:ended:30:false"), "{tree}");
}

#[gpui::test]
fn long_theme_names_stay_inside_their_card(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let long = "catppuccin-macchiato";
    let mut p = props();
    p["section"] = json!("look");
    let mut values = fixture(true, p, updates("current", "current"));
    let theme = values["theme"].clone();
    values.insert("themes".into(), json!([{ "name": long, "light": false, "colors": theme }]));
    let mut settings = values["settings"].clone();
    settings["theme_dark"] = json!(long);
    values.insert("settings".into(), settings);
    let tree = describe_with_values(cx, &staged, "frame/settings", values, None).unwrap();
    let lines: Vec<&str> = tree.lines().collect();
    let card = lines.iter().position(|l| l.contains(&format!("st-t-{long}"))).unwrap();
    let row = lines[card..].iter().find(|l| l.trim_start().starts_with("h_flex") && l.contains("min_w")).unwrap();
    assert!(row.contains(".w_full"), "the name row must fill the card: {row}");
    assert!(row.contains(".min_w[Number(0.0)]"), "the name row must be allowed to shrink: {row}");
    let name = lines[card..].iter().find(|l| l.contains("truncate")).unwrap();
    assert!(name.contains(":tooltip[Str(\"catppuccin macchiato\")]"), "a cut name needs its full text on hover: {name}");
    let check = lines[card..].iter().find(|l| l.contains("check.svg")).unwrap();
    assert!(check.contains(".flex_none"), "the selected check must not shrink: {check}");
}

#[gpui::test]
fn confirm_dialog_background_follows_the_frame_corners(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let mut p = props();
    p["title"] = json!("Close space?");
    p["command"] = json!({"name": "space.close", "args": {"id": "s1"}});
    let tree = describe(cx, &staged, "mods/confirm", true, p, updates("current", "current"), None).unwrap();
    let root = tree.lines().find(|l| l.contains(":id[Str(\"confirm\")]")).unwrap();
    assert!(root.contains(".rounded[Number(8.0)]"), "the dialog background must follow the frame's corners: {root}");
}

#[gpui::test]
fn pane_header_divider_faces_the_terminal(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    for (position, reveal) in [("bottom", "always"), ("top", "always"), ("", "always"), ("bottom", "hover"), ("top", "hover")] {
        let mut values = fixture(true, props(), updates("current", "current"));
        values.insert("layout".into(), json!({
            "id": "root", "type": "column", "children": [
                {"id": "hidden", "type": "block", "hidden": true,
                 "content": {"tab": "active", "pane": {"header": {"position": "bottom"}}}},
                {"id": "stack", "type": "stack", "active": "shown", "children": [
                    {"id": "inactive", "type": "block",
                     "content": {"tab": "active", "pane": {"header": {"position": "bottom"}}}},
                    {"id": "shown", "type": "block",
                     "content": {"tab": "active", "pane": {"header": {"position": position, "reveal": reveal}}}}
                ]}
            ]
        }));
        if position.is_empty() {
            values.get_mut("layout").unwrap()["children"][1]["children"][1]["content"]["pane"]["header"].as_object_mut().unwrap().remove("position");
        }
        let tree = describe_with_values(cx, &staged, "mods/pane", values, None).unwrap();
        let head = tree.lines().find(|l| l.contains("pane-head-")).unwrap();
        let (edge, outside) = if position == "bottom" { ("t", "b") } else { ("b", "t") };
        assert!(head.contains(&format!(".border_{edge}[Number(1.0)]")), "{position}: {head}");
        assert!(!head.contains(&format!(".border_{outside}[")), "{position}: {head}");
    }
}

#[gpui::test]
fn layout_previews_stay_inside_their_cards(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let mut p = props();
    p["section"] = json!("look");
    let mut values = fixture(true, p, updates("current", "current"));
    values.insert("layouts".into(), json!(["main"]));
    let tree = describe_with_values(
        cx, &staged, "frame/settings", values,
        Some("this.previews = { main: 'layouts/portrait.png' }; this.previewsOf = 'main';"),
    ).unwrap();
    let lines: Vec<&str> = tree.lines().collect();
    let image = lines.iter().position(|l| l.contains("layouts/portrait.png")).unwrap();
    assert!(lines[image].contains(".h[Number(62.0)]"), "a preview keeps the card's height: {}", lines[image]);
    assert!(lines[image - 1].contains(".overflow_hidden"), "a preview cannot spill out of its card: {}", lines[image - 1]);
}

#[gpui::test]
fn check_materializes_retained_elements_in_a_rendered_view(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    for body in [
        "v_flex().h(400).child(v_virtual_list('rows', 0, 28, (i) => 'a', () => { throw new Error('deferred rows ran'); }))",
        "v_flex().h(400).child(v_virtual_list('rows', 1, 28, (i) => 'a', () => { throw new Error('deferred rows ran'); }))",
        "div().w(100).h(50).left(10).transition('left', 200)",
        "div().w(100).h(50).transition('width', 200)",
        "div().w(100).h(50).left(10).spring('left')",
        "div().opacity(0.5).transition('opacity', 200)",
    ] {
        std::fs::write(staged.0.join("main.js"), format!(
            "import {{ View, div }} from 'gpui-kit'; import {{ v_flex, v_virtual_list }} from 'gpui-base'; export default class Demo extends View {{ render() {{ return {body}; }} }}"
        )).unwrap();
        let runtime = ShellRuntime::new_isolated().unwrap();
        let window = cx.add_window(|_, _| Empty);
        let description = window.update(cx, |_, window, cx| runtime.check(&staged.0, window, cx)).unwrap().unwrap();
        assert!(!description.is_empty(), "{body}");
        gpui::AnyWindowHandle::from(window).update(cx, |_, window, _| window.remove_window()).unwrap();
    }
}

#[gpui::test]
fn long_space_names_use_the_sidebar_width(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let name = "quarterly infrastructure cost review";
    let mut values = fixture(true, props(), updates("current", "current"));
    let spaces = values.get_mut("spaces").unwrap().as_array_mut().unwrap();
    spaces[0]["name"] = json!(name);
    // A worktree of the first space's repo, drawn nested under it.
    spaces.insert(1, json!({"id":"s3","index":2,"name":"portal rewrite","folder":"/fixture/portal","machine":null,"branch":null,"active":false,"status":"idle","tabs":[]}));
    // The remote space is on main in a repo named after it: its tail has nothing to show.
    let git = "this.git[JSON.stringify([null, '/fixture/project'])] = { name: 'herdr', branch: 'feature/rate-limiting', ahead: 0, behind: 0, repo: JSON.stringify([null, '/fixture/project/.git']), linked: false };
        this.git[JSON.stringify([null, '/fixture/portal'])] = { name: 'herdr', branch: 'portal-rewrite', ahead: 0, behind: 0, repo: JSON.stringify([null, '/fixture/project/.git']), linked: true };
        this.git[JSON.stringify(['remote', '/remote/project'])] = { name: 'remote', branch: 'main', ahead: 0, behind: 0, repo: JSON.stringify(['remote', '/remote/project/.git']), linked: false };";
    let tree = describe_with_values(cx, &staged, "mods/spaces", values, Some(git)).unwrap();
    let lines: Vec<&str> = tree.lines().collect();
    let text = lines.iter().position(|l| l.contains(&format!("text {name:?}"))).unwrap();
    let label = lines[text - 1];
    assert!(!label.contains(".max_w"), "a wider sidebar must show more of the name: {label}");
    assert!(!label.contains(".flex_shrink[Number(0.0)]"), "the name must give way when the row is full: {label}");
    assert!(label.contains(".min_w[Number(48.0)]"), "the name shrinks, but a short one stays readable: {label}");
    let tail = lines[text + 1..].iter().find(|l| l.contains(".justify_end")).unwrap();
    assert!(tail.contains(".flex_auto") && !tail.contains(".flex_1"), "the git tail must start from its text's width, so both sides give way in proportion: {tail}");
    assert!(tail.contains(".min_w[Number(48.0)]"), "the git tail shrinks, but a short one stays readable: {tail}");
    let remote = lines.iter().position(|l| l.contains("text \"Remote\"")).unwrap();
    let empty = lines[remote + 1..].iter().find(|l| l.contains(".justify_end")).unwrap();
    assert!(empty.contains(".min_w[Number(0.0)]"), "an empty git tail must not take room from the name: {empty}");
    let icon = lines.iter().find(|l| l.contains("branch.svg")).expect("the worktree space is nested");
    assert!(icon.contains(".flex_none"), "a worktree's icon keeps its size while the name gives way: {icon}");
}

#[gpui::test]
fn rebuilt_machine_offers_connect_only_for_identity_mismatch(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    for (state, why, offered) in [
        ("unreachable", "a different core answers there (\"new-id\")", true),
        ("unreachable", "connection refused", false),
        ("unreachable", "a different core answers there", false),
        ("connected", "a different core answers there (\"new-id\")", false),
    ] {
        let setup = format!(
            "this.machines = [{{name:'cloud', state:{}, why:{}, target:'host'}}]; this.folded.add('cloud');",
            json!(state), json!(why)
        );
        let tree = describe(cx, &staged, "mods/spaces", false, props(), updates("current", "current"), Some(&setup)).unwrap();
        assert_eq!(tree.contains("machine-connect-cloud"), offered, "{state}/{why}: {tree}");
        assert_eq!(tree.contains("rebuilt ·"), offered, "{tree}");
        assert_eq!(tree.contains("This machine was rebuilt or replaced. Click to connect to the new one."), offered, "{tree}");
        assert!(!tree.contains("machine-reconnect-"), "reconnect belongs in the machine row: {tree}");
    }
}

#[gpui::test]
fn machine_logout_warning_tracks_current_remote_state(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    for state in ["false", "true", "null", "undefined"] {
        let setup = format!("this.hosts = []; this.added = [{{name:'cloud', target:'host', keeps_running_after_logout:{state}, logout_linger_user:'-alice'}}];");
        let tree = describe(cx, &staged, "mods/machine-add", false, props(), updates("current", "current"), Some(&setup)).unwrap();
        assert_eq!(tree.contains("Agents on cloud stop when you disconnect"), state == "false", "{tree}");
        assert_eq!(tree.contains("ma-logout-copy-cloud"), state == "false", "{tree}");
        if state == "false" { assert!(tree.contains("sudo loginctl enable-linger -- -alice"), "{tree}"); }
        let setup = format!("this.machines = [{{name:'cloud', target:'host', state:'connected', keeps_running_after_logout:{state}}}];");
        let tree = describe(cx, &staged, "mods/spaces", false, props(), updates("current", "current"), Some(&setup)).unwrap();
        assert_eq!(tree.contains("machine-logout-cloud"), state == "false", "{tree}");
    }
}

#[gpui::test]
fn machine_picker_keeps_long_lists_scrollable(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let tree = describe(
        cx, &staged, "mods/machine-add", false, props(), updates("current", "current"),
        Some("this.hosts = Array.from({length: 30}, (_, i) => ({alias: `host-${i}`, aliases: []}));"),
    ).unwrap();
    let scroll = tree.lines().find(|line| line.contains("ma-hosts")).unwrap();
    assert!(scroll.contains("overflow_y_scroll") && !scroll.contains("overflow_y_scrollbar"), "{tree}");
    assert!(scroll.contains(".min_h[Number(0.0)]"), "{tree}");
    assert!(tree.contains("ma-row-host-29"), "{tree}");
    assert_eq!(tree.matches(".flex_none").count(), 60, "rows must not shrink into the viewport: {tree}");
}

#[gpui::test]
fn machine_add_needing_input_keeps_the_picker_and_shows_instructions(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let calls = Rc::new(RefCell::new(Calls::default()));
    let policy = Policy::new().with_host_module(module(fixture(false, props(), updates("current", "current")), calls.clone())).unwrap();
    let root = staged.wrapper("mods/machine-add", "this.typed = 'host'; this.add('host', cx);");
    let window = cx.add_window(|_, _| Empty);
    let mut session = window.update(cx, |_, window, cx| {
        DescriptionSession::load(ShellRuntime::new_isolated().unwrap(), root, policy, window, cx).unwrap()
    }).unwrap();
    cx.run_until_parked();
    window.update(cx, |_, window, cx| {
        let tree = session.describe(window, cx).unwrap();
        assert!(tree.contains("machine-add"), "{tree}");
        assert!(tree.contains("ma-message-host"), "{tree}");
        assert!(tree.contains("hc machine add host"), "{tree}");
        window.remove_window();
    }).unwrap();
    let calls = calls.borrow();
    assert!(calls.unexpected.is_empty(), "{:?}", calls.unexpected);
    assert!(!calls.effects.iter().any(|(name, _)| name == "openOverlay" || name == "closeOverlay"), "the picker stays open without a setup dialog");
}

#[gpui::test]
fn option_key_settings_are_only_shown_on_macos(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    for platform in ["linux", "windows", "macos"] {
        let mut p = props();
        p["section"] = json!("keyboard");
        let mut values = fixture(true, p, updates("current", "current"));
        values.insert("window".into(), json!({"platform":platform}));
        let tree = describe_with_values(cx, &staged, "frame/settings", values, None).unwrap();
        for value in ["auto", "on", "off", "left", "right"] {
            assert_eq!(tree.contains(&format!("st-option-{value}")), platform == "macos", "{platform}: {tree}");
        }
        assert_eq!(tree.contains("st-s-keyboard"), platform == "macos", "{platform}: {tree}");
    }
}

#[gpui::test]
fn settings_sections_and_updates(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    for section in ["look", "machines", "drove", "alerts", "privacy"] {
        let mut p = props();
        p["section"] = json!(section);
        let tree = describe(cx, &staged, "frame/settings", true, p, updates("current", "current"), None).unwrap();
        assert!(!tree.contains("st-s-updates") && !tree.contains("st-check-app") && !tree.contains("st-app-update") && !tree.contains("st-check-core"), "updates UI must stay suppressed in {section}: {tree}");
    }
    let mut p = props();
    p["section"] = json!("updates");
    let fallback = describe(cx, &staged, "frame/settings", true, p, updates("ready", "ready"), None).unwrap();
    assert!(!fallback.contains("st-s-updates") && !fallback.contains("st-check-app") && !fallback.contains("st-app-update") && !fallback.contains("st-check-core"), "updates section must fall back without update UI: {fallback}");
}

#[gpui::test]
fn animated_images_advance_with_explicit_and_implicit_ids(cx: &mut TestAppContext) {
    use gpui::{Styled as _, http_client::{AsyncBody, FakeHttpClient, Response}};
    use image::{Delay, Frame, Rgba, RgbaImage, codecs::gif::GifEncoder};
    use std::{sync::Arc, time::Duration};

    cx.update(gpui_shell::init);
    let mut bytes = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut bytes);
        for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
            encoder.encode_frame(Frame::from_parts(RgbaImage::from_pixel(2, 2, Rgba(color)), 0, 0, Delay::from_numer_denom_ms(20, 1))).unwrap();
        }
    }
    let (ready, waiting) = async_channel::unbounded::<()>();
    let client = FakeHttpClient::create(move |_| {
        let bytes = bytes.clone();
        let waiting = waiting.clone();
        async move {
            waiting.recv().await.unwrap();
            Ok(Response::builder().status(200).body(AsyncBody::from(bytes)).unwrap())
        }
    });
    cx.update(|cx| cx.set_http_client(client));
    let staged = Staged::new();
    for explicit in [false, true] {
        let runtime = ShellRuntime::new_isolated().unwrap();
        let url = format!("https://images.example/animation-{explicit}.gif");
        let source = format!("import {{ View, image }} from 'gpui-kit'; export default class Demo extends View {{ render() {{ return image('{url}'){}.size(20); }} }}", if explicit { ".id('clip')" } else { "" });
        std::fs::write(staged.0.join("main.js"), source).unwrap();
        let context = cx.add_empty_window();
        let view = context.update(|window, cx| {
            let root = runtime.try_load(&staged.0, window, cx).unwrap();
            root.read(cx).content().clone()
        });
        struct AnimationRoot(gpui::AnyView);
        impl gpui::Render for AnimationRoot {
            fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
                self.0.clone().cached(gpui::StyleRefinement::default().size_full())
            }
        }
        // Load while inactive, then tick frames explicitly so the executor can park.
        context.deactivate_window();
        let root = context.update(|window, cx| window.replace_root(cx, |_, _| AnimationRoot(view.clone())));
        context.run_until_parked();
        // A cached subtree must keep its pending load across an unrelated parent draw.
        context.update(|_, cx| root.update(cx, |_, cx| cx.notify()));
        context.run_until_parked();
        ready.try_send(()).unwrap();
        context.run_until_parked();
        let resource = gpui::Resource::Uri(url.into());
        let still: Arc<gpui::RenderImage> = context.update(|_, cx| cx.fetch_asset::<gpui::ImgResourceLoader>(&resource).unwrap().unwrap());
        assert_eq!(still.frame_count(), 1);
        let data = context.update(|window, _| window.displayed_images().into_iter().find(|image| image.frame_count() == 2)).unwrap();
        assert_eq!(data.as_bytes(0).unwrap().as_ptr(), still.as_bytes(0).unwrap().as_ptr());
        context.update(|window, cx| {
            window.replace_root(cx, |_, _| Empty);
            window.activate_window();
        });
        context.run_until_parked();
        context.update(|window, cx| { window.replace_root(cx, |_, _| AnimationRoot(view.clone())); });
        let draw = |context: &mut gpui::VisualTestContext| {
            context.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
        };
        draw(context);
        assert!(context.update(|window, _| window.is_window_active()));
        assert_eq!(data.frame_count(), 2);
        assert!(context.update(|window, _| window.has_image_frame_atlas_entry(&data, 0)));
        assert!(!context.update(|window, _| window.has_image_frame_atlas_entry(&data, 1)));
        // GPUI's image clock is wall time, not the test executor's clock.
        std::thread::sleep(Duration::from_millis(25));
        draw(context);
        assert!(context.update(|window, _| window.has_image_frame_atlas_entry(&data, 1)), "the next animation frame must paint (explicit id: {explicit})");
        assert!(context.update(|window, _| window.has_image_frame_atlas_entry(&data, 0)));
        std::thread::sleep(Duration::from_millis(25));
        draw(context);
        assert!(context.update(|window, _| window.has_image_frame_atlas_entry(&data, 0)));
        assert!(context.update(|window, _| window.has_image_frame_atlas_entry(&data, 1)));
        context.update(|window, cx| { window.replace_root(cx, |_, _| Empty); });
        context.deactivate_window();
    }
}

#[gpui::test]
fn settings_keep_notes_in_the_whats_new_window(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let notes = json!([
        {"version":"1.8.0","date":"2026-10-26","items":[{"headline":"Newest headline","detail":"Newest details","fixed":false,"media":[]},{"headline":"","detail":"Newest fix","fixed":true,"media":[]}],"spotlight":true},
        {"version":"1.7.0","date":"2026-10-25","items":[{"headline":"Older headline","detail":"Older details","fixed":false,"media":[]},{"headline":"Another older headline","detail":"Hidden detail","fixed":false,"media":[]}],"spotlight":true}
    ]);
    let mut update = updates("ready", "current");
    update["notes"] = notes.clone();
    let mut p = props();
    p["section"] = json!("look");
    let tree = describe(cx, &staged, "frame/settings", true, p, update.clone(), None).unwrap();
    assert!(!tree.contains("st-whats-new") && !tree.contains("st-app-update") && !tree.contains("st-s-updates"));
    assert!(!tree.contains("Newest details") && !tree.contains("Older headline"));
    let mut p = props();
    p["notes"] = notes;
    let tree = describe(cx, &staged, "frame/whatsnew", true, p, update, None).unwrap();
    assert!(tree.contains("Newest details") && tree.contains("Older details"));
    assert!(tree.contains("Also fixed") && tree.contains("Newest fix"));
    assert!(tree.contains("Oct 26") && !tree.contains("2026-10-26"));
}

#[gpui::test]
fn local_state_and_overlay_initial_descriptions(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let start = Instant::now();
    let staged = Staged::new();
    for page in 1..=4 {
        let tree = describe(
            cx,
            &staged,
            "frame/welcome",
            true,
            props(),
            updates("current", "current"),
            Some(&format!("this.page = {page};")),
        )
        .unwrap();
        let expected = ["Herdr runs your terminals in the background", "Add your other machines", "Choose your panels", "Make it yours"][page - 1];
        assert!(tree.contains(expected), "welcome page {page}: {tree}");
    }
    for setup in ["", "this.sent = true;", "this.error = 'Fixture error';"] {
        let tree = describe(
            cx,
            &staged,
            "frame/feedback",
            true,
            props(),
            updates("current", "current"),
            Some(setup),
        )
        .unwrap();
        assert!(tree.contains("feedback"), "{tree}");
        if setup.is_empty() {
            assert!(
                tree.contains("Input #") && tree.contains("Textarea #") && tree.contains("A prefilled message"),
                "{tree}"
            );
        } else {
            assert!(
                tree.contains(if setup.contains("sent") { "Sent. Thanks" } else { "Fixture error" }),
                "{tree}"
            );
        }
    }
    for query in ["settings", "no-such-command"] {
        let tree = describe(
            cx,
            &staged,
            "frame/palette",
            true,
            props(),
            updates("current", "current"),
            Some(&format!("this.query = {query:?};")),
        )
        .unwrap();
        // The query sits in a native text field; what the list shows tells it apart.
        let expected = if query == "settings" { "Settings" } else { "Nothing matches" };
        assert!(tree.contains(expected), "palette query {query}: {tree}");
    }
    // Reviewed overlay contract table, not extracted from arbitrary JS call sites.
    let scenarios = [
        ("mods/menu", json!({"tab":"pane","target":{"pane":"p1","terminal":"t1"}})),
        ("mods/menu", json!({"tab":"tab","target":{"id":"tab1","space":"s1"}})),
        ("mods/menu", json!({"tab":"space","target":{"id":"s1"}})),
        ("mods/prompt", json!({"machines":true})),
        (
            "mods/confirm",
            json!({"title":"Close space?","command":{"name":"space.close","args":{"id":"s1"}}}),
        ),
        (
            "mods/toast",
            json!({"title":"Done","body":"Build complete","level":"success","action":{"name":"app.help"}}),
        ),
        ("mods/peek", json!({"terminal":"t1"})),
        ("mods/peek", json!({"terminal":"missing"})),
        ("mods/worktree", json!({"space":"s1","action":"new"})),
        ("mods/worktree", json!({"space":"s1","action":"remove"})),
    ];
    let entries = staged.entries();
    for (entry, patch) in scenarios {
        assert!(entries.iter().any(|e| e == entry), "orphan scenario: {entry}");
        let mut p = props();
        p.as_object_mut().unwrap().extend(patch.as_object().unwrap().clone());
        describe(cx, &staged, entry, true, p, updates("current", "current"), None).unwrap();
    }
    eprintln!("18 local-state/overlay cases, each twice: {:?}", start.elapsed());
}

#[gpui::test]
fn duplicate_element_negative_control(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let dir = staged.0.join("negative");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("main.js"), "import { View, div } from 'gpui-kit'; export default class Bad extends View { render() { const child = div().child('reused'); return div().child(div().child(child)).child(div().child(child)); } }").unwrap();
    let error = describe(cx, &staged, "negative", false, props(), updates("current", "current"), None).unwrap_err();
    assert!(format!("{error:#}").contains("already added to a parent"), "{error:#}");
}

#[test]
fn shipped_layouts_and_keys() {
    let staged = Staged::new();
    let mods: BTreeSet<_> = staged
        .entries()
        .into_iter()
        .filter_map(|p| p.strip_prefix("mods/").map(str::to_owned))
        .collect();
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("defaults/layouts");
    let mut layouts = BTreeSet::new();
    fn references(v: &Value, mods: &BTreeSet<String>) {
        match v {
            Value::Object(o) => {
                if let Some(Value::String(name)) = o.get("mod") {
                    assert!(mods.contains(name), "unknown layout mod {name}");
                }
                for v in o.values() {
                    references(v, mods);
                }
            }
            Value::Array(a) => {
                for v in a {
                    references(v, mods);
                }
            }
            _ => {}
        }
    }
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        serde_json::from_str::<Node>(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        references(&serde_json::from_str::<Value>(&text).unwrap(), &mods);
        layouts.insert(path.file_stem().unwrap().to_str().unwrap().to_owned());
    }
    assert_eq!(layouts, crate::window::shipped_layouts().into_iter().map(str::to_owned).collect());
    let mod_commands = ["ribbon.next"];
    for text in [
        include_str!("../../defaults/keys.json"),
        include_str!("../../defaults/keys.macos.json"),
    ] {
        crate::keys::parse(text).unwrap();
        let sections: BTreeMap<String, BTreeMap<String, String>> = serde_json::from_str(text).unwrap();
        for bindings in sections.values() {
            for action in bindings.values() {
                let name = action.split(':').next().unwrap();
                if let Some(command) = crate::commands::find(name) {
                    let args = match action.split_once(':') {
                        Some((_, value)) => {
                            assert!(!command.args.is_empty(), "unexpected shipped key argument {action}");
                            crate::commands::shorthand(command, value)
                        }
                        None => Value::Null,
                    };
                    crate::commands::validate_args(command, &args).unwrap();
                    assert!(crate::commands::missing(command, &args).is_none(), "missing shipped key argument {action}");
                }
                assert!(
                    action == "terminal"
                        || action.starts_with("send:")
                        || crate::commands::COMMANDS.iter().any(|c| c.name == name)
                        || (mod_commands.contains(&name) && mods.contains(name.split('.').next().unwrap())),
                    "unknown shipped key action {action}"
                );
            }
        }
    }
}

#[gpui::test]
fn picker_scrolls_the_selection_into_view(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let mut values = fixture(true, props(), updates("current", "current"));
    let tabs: Vec<Value> = (1..=12)
        .map(|i| json!({"id":format!("tab{i}"),"index":i,"name":format!("Task {i}"),"active":i==1,"status":"done","panes":1,"zoomed":false,"settled":false,"settled_at":null,"changed_at":null,"working_since":null}))
        .collect();
    values.insert("spaces".into(), json!([
        {"id":"s1","index":1,"name":"Local","folder":"/fixture/project","machine":null,"branch":"main","active":true,"status":"working","tabs":tabs}
    ]));
    // Selecting the last row (with the arrows) must scroll it into the visible 320px.
    let tree = describe_with_values(cx, &staged, "mods/picker", values, Some("this.at = 999; cx.notify();")).unwrap();
    let top = tree.lines()
        .find(|l| l.contains("pk-space-here"))
        .unwrap_or_else(|| panic!("selected row missing: {tree}"));
    let px: f64 = top.split(".top[Number(").nth(1).unwrap_or_else(|| panic!("selected row must be placed inside the scrolled list: {tree}")).split(")").next().unwrap().parse().unwrap();
    assert!(px <= 320.0 - 32.0, "selected row at {px}px is below the visible list: {tree}");
}

#[gpui::test]
fn status_bar_shows_the_real_palette_key(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let mut bound = fixture(true, props(), updates("current", "current"));
    bound.insert("keys".into(), json!([{"section":"global","keys":"ctrl-p","action":"app.palette"}]));
    let tree = describe_with_values(cx, &staged, "mods/workspace-status", bound, None).unwrap();
    assert!(tree.contains("Ctrl+P"), "hint must show the real binding: {tree}");
    assert!(!tree.contains("Shift P"), "stale hardcoded hint: {tree}");

    let mut unbound = fixture(true, props(), updates("current", "current"));
    unbound.insert("keys".into(), json!([{"section":"global","keys":"ctrl-k","action":"app.help"}]));
    let tree = describe_with_values(cx, &staged, "mods/workspace-status", unbound, None).unwrap();
    assert!(tree.contains("commands"), "unbound hint still offers commands: {tree}");
    assert!(!tree.contains("Shift P"), "unbound hint must not invent a key: {tree}");
}

#[gpui::test]
fn status_bar_counts_agents_in_the_singular_and_the_plural(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let agent = |n: u32| json!({"address":format!("t{n}"),"pane":format!("p{n}"),"space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Build","status":"blocked","app":"pi","program":null,"title":"Build","seq":n,"focused":n==1});

    let mut one = fixture(true, props(), updates("current", "current"));
    one.insert("agents".into(), json!([agent(1)]));
    let tree = describe_with_values(cx, &staged, "mods/workspace-status", one, None).unwrap();
    assert!(tree.contains("1 agent") && !tree.contains("1 agents"), "one agent is singular: {tree}");
    assert!(tree.contains("1 needs you"), "one blocked agent needs you: {tree}");

    let mut two = fixture(true, props(), updates("current", "current"));
    two.insert("agents".into(), json!([agent(1), agent(2)]));
    let tree = describe_with_values(cx, &staged, "mods/workspace-status", two, None).unwrap();
    assert!(tree.contains("2 agents"), "two agents are plural: {tree}");
    assert!(tree.contains("2 need you") && !tree.contains("2 needs you"), "two blocked agents need you: {tree}");
}

#[gpui::test]
fn acp_peek_renders_t3_conversation_and_permission_card(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let program = json!({"key":"pi", "name":"Pi", "icon":null, "tint":true, "tile":"#334455", "color":"#aabbcc", "shell":false, "agent":true});
    let mut values = fixture(true, json!({"overlay":"o1","terminal":"a1","acp":true}), updates("current", "current"));
    values.insert("terminals".into(), json!([
        {"address":"t1","name":"Build","title":"Build","argv":["pi"],"parent":null,"labels":{},"state":"running","status":"working","report":{"app":"pi"},"program":program,"cwd":"/fixture/project"},
        {"address":"a1","name":"Reviewer","title":"Reviewer","argv":["pi"],"parent":"t1","labels":{},"state":"running","status":"blocked","turn":2,"transcript_seq":4,"inbox_len":2,"pending_permission":{"request_id":"req-1","title":"Run cargo test","options":[{"option_id":"once","name":"Allow once"}]},"report":{"app":"pi"},"program":program,"cwd":"/fixture/project"}
    ]));
    values.insert("agents".into(), json!([
        {"address":"t1","pane":"p1","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Build","status":"working","app":"pi","program":program,"title":"Build","seq":1,"focused":true},
        {"address":"a1","pane":"","space":"s1","space_name":"Local","tab":"","tab_index":0,"tab_name":"","status":"blocked","app":"pi","program":program,"title":"Reviewer","seq":2,"focused":false}
    ]));

    let calls = Rc::new(RefCell::new(Calls::default()));
    let policy = Policy::new()
        .with_storage_path(staged.0.join("store-acp-peek.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(values, calls.clone()))
        .unwrap();
    let runtime = ShellRuntime::new_isolated().unwrap();
    let root = staged.0.join("mods/peek");
    let window = cx.add_window(|_, _| Empty);
    let mut session = window.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(runtime, root, policy, window, cx).unwrap();
        let _initial = session.describe(window, cx).unwrap();
        session
    }).unwrap();

    // Let async `h2(["read", "--json", "a1"])` resolve and trigger re-render.
    cx.run_until_parked();
    let tree = window.update(cx, |_, window, cx| session.describe(window, cx).unwrap()).unwrap();
    window.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();

    assert!(calls.borrow().unexpected.is_empty(), "unexpected host calls: {:?}", calls.borrow().unexpected);
    assert!(!tree.contains("<h2"), "raw <h2 envelope leaked into rendered tree: {tree}");
    assert!(tree.contains("Fix bug"), "cleaned user prompt missing: {tree}");
    assert!(tree.contains("from Build"), "parent badge missing: {tree}");
    assert!(tree.contains("1 tool call · 1 thought · 1 plan"), "activity group summary missing: {tree}");
    assert!(tree.contains("copy-as-4-code-1"), "code card copy button missing: {tree}");
    assert!(!tree.contains("Diagnostic stderr"), "noisy stderr diagnostic drawer should be hidden: {tree}");
    assert!(tree.contains("Sandbox warning"), "error card missing: {tree}");
    assert!(tree.contains("Run cargo test"), "permission card title missing: {tree}");
    assert!(tree.contains("perm-opt-0"), "permission approve option missing: {tree}");
    assert!(tree.contains("2 queued"), "queued inbox pill missing: {tree}");
    assert!(tree.contains("acp-stage"), "floating transcript stage missing: {tree}");
    assert!(tree.contains("acp-u-fold-u-0"), "delegated prompt FoldedMessageHeader missing: {tree}");
    assert!(tree.contains("acp-composer-card"), "composer card missing: {tree}");
    assert!(tree.contains("acp-input-area"), "spacious composer input area missing: {tree}");
    assert!(tree.contains("acp-send"), "composer send button missing: {tree}");
    assert!(tree.contains("wrap-as-4-code-1"), "code card wrap toggle missing: {tree}");
    assert!(tree.contains("acp-cfg-bar"), "capability pill bar missing: {tree}");
    assert!(tree.contains("GPT-5.4"), "model pill missing: {tree}");
    assert!(tree.contains("High"), "reasoning pill missing: {tree}");
    assert!(tree.contains("Fast"), "fast-mode pill missing: {tree}");
    assert!(tree.contains("Code"), "mode pill missing: {tree}");
    assert!(tree.contains("acp-mount-tab") && tree.contains("Open as Tab"), "Open as Tab button missing in dialog: {tree}");
    assert!(tree.contains("acp-split-right") && tree.contains("Split right"), "Split right button missing in dialog: {tree}");
}

#[gpui::test]
fn acp_peek_tab_mode_renders_background_action_without_dialog_close(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let program = json!({"key":"pi", "name":"Pi", "icon":null, "tint":true, "tile":"#334455", "color":"#aabbcc", "shell":false, "agent":true});
    let mut values = fixture(true, json!({"terminal":"a1","acp":true,"tab":true,"pane":"p2"}), updates("current", "current"));
    values.insert("terminals".into(), json!([
        {"address":"a1","name":"Reviewer","title":"Reviewer","argv":["pi"],"parent":null,"labels":{},"state":"live","status":"idle","turn":1,"transcript_seq":1,"report":{"app":"pi"},"program":program,"cwd":"/fixture/project"}
    ]));
    values.insert("agents".into(), json!([
        {"address":"a1","acp":true,"pane":"p2","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Reviewer","status":"idle","app":"pi","program":program,"title":"Reviewer","seq":1,"focused":true}
    ]));

    let calls = Rc::new(RefCell::new(Calls::default()));
    let policy = Policy::new()
        .with_storage_path(staged.0.join("store-acp-tab.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(values, calls.clone()))
        .unwrap();
    let runtime = ShellRuntime::new_isolated().unwrap();
    let root = staged.0.join("mods/peek");
    let window = cx.add_window(|_, _| Empty);
    let mut session = window.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(runtime, root, policy, window, cx).unwrap();
        let _initial = session.describe(window, cx).unwrap();
        session
    }).unwrap();

    cx.run_until_parked();
    let tree = window.update(cx, |_, window, cx| session.describe(window, cx).unwrap()).unwrap();
    window.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();

    assert!(calls.borrow().unexpected.is_empty(), "unexpected host calls: {:?}", calls.borrow().unexpected);
    assert!(!tree.contains("acp-header"), "duplicate acp-header should be omitted in tab mode: {tree}");
    assert!(!tree.contains("acp-close\""), "dialog close button should be omitted in tab mode: {tree}");
    assert!(tree.contains("GPT-5.4"), "model pill missing in tab mode: {tree}");

    let mut pane_values = fixture(true, json!({"pane":"p2","terminal":"a1"}), updates("current", "current"));
    pane_values.insert("terminals".into(), json!([
        {"address":"a1","acp":true,"name":"Reviewer","state":"live","status":"working","program":program,"context_usage":{"used":28000,"size":200000}}
    ]));
    pane_values.insert("panes".into(), json!([
        {"pane":"p2","acp":true,"mod":"peek","terminal":"a1","tab":"tab1","title":"Reviewer","cwd":"/fixture/project","status":"working","program":program,"context_usage":{"used":28000,"size":200000},"focused":true,"zoomed":false,"movable":false}
    ]));
    let pane_tree = describe_with_values(cx, &staged, "mods/pane", pane_values.clone(), Some("this.hovered = true;")).unwrap();
    assert!(!pane_tree.contains("Reviewer"), "sole ACP tab pane omits duplicate agent name below tab pill: {pane_tree}");
    assert!(pane_tree.contains("ph-type-p2"), "sole ACP pane header missing type badge: {pane_tree}");
    assert!(pane_tree.contains("ph-time-p2"), "sole ACP pane header missing execution time badge: {pane_tree}");
    assert!(pane_tree.contains("Ctx 14%"), "ACP pane header missing context usage ratio: {pane_tree}");
    assert!(pane_tree.contains("ph-stop-p2"), "ACP pane header missing inline Stop button: {pane_tree}");
    assert!(pane_tree.contains("ph-r-p2"), "ACP pane header missing split-right button: {pane_tree}");
    assert!(pane_tree.contains("ph-x-p2"), "ACP pane header missing background button: {pane_tree}");

    // When split alongside another pane in the same tab, the ACP pane header identifies itself by name & ACP badge.
    pane_values.insert("panes".into(), json!([
        {"pane":"p1","acp":false,"terminal":"t1","tab":"tab1","title":"zsh","cwd":"/fixture/project","status":null,"program":null,"focused":false,"zoomed":false,"movable":true},
        {"pane":"p2","acp":true,"mod":"peek","terminal":"a1","tab":"tab1","title":"Reviewer","cwd":"/fixture/project","status":"working","program":program,"context_usage":{"used":28000,"size":200000},"focused":true,"zoomed":false,"movable":true}
    ]));
    let split_tree = describe_with_values(cx, &staged, "mods/pane", pane_values, None).unwrap();
    assert!(split_tree.contains("Reviewer"), "split ACP pane header missing agent name: {split_tree}");
    assert!(split_tree.contains("ACP"), "split ACP pane header missing ACP badge: {split_tree}");
    assert!(split_tree.contains("ph-time-p2"), "split ACP pane header missing execution time badge: {split_tree}");
}

#[gpui::test]
fn acp_capability_popup_opens_anchored_above_trigger_without_warnings(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let program = json!({"key":"pi", "name":"Pi", "icon":null, "tint":true, "tile":"#334455", "color":"#aabbcc", "shell":false, "agent":true});
    let mut values = fixture(true, json!({"terminal":"a1","acp":true,"tab":true,"pane":"p2"}), updates("current", "current"));
    let config_options = json!([
        {"id":"model","name":"Model","category":"model","kind":"select","current_value":"gpt-5.4","options":[{"value":"gpt-5.4","name":"GPT-5.4"},{"value":"gpt-5.3-codex","name":"GPT-5.3 Codex"}]},
        {"id":"fast-mode","name":"Fast Mode","category":"model_config","kind":"boolean","current_value":false}
    ]);
    values.insert("terminals".into(), json!([
        {"address":"a1","name":"Reviewer","title":"Reviewer","argv":["pi"],"parent":null,"labels":{},"state":"live","status":"idle","turn":1,"transcript_seq":1,"config_options":config_options,"report":{"app":"pi"},"program":program,"cwd":"/fixture/project"}
    ]));
    values.insert("agents".into(), json!([
        {"address":"a1","acp":true,"pane":"p2","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Reviewer","status":"idle","app":"pi","program":program,"title":"Reviewer","seq":1,"focused":true}
    ]));

    let tree = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        values.clone(),
        Some("this.openConfigPopover(this.configOptionsFor(this.term())[0], cx);"),
    ).unwrap();

    assert!(tree.contains("Popup \"acp-popup-model\""), "anchored Popup missing: {tree}");
    assert!(tree.contains("acp-cfg-popover-model"), "animated popover card missing: {tree}");
    assert!(tree.contains("GPT-5.3 Codex"), "model choices missing inside open popup: {tree}");

    // Toggling Fast Mode while Model popup is open immediately flips Fast to On and animates Model closed.
    let after_fast = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        values,
        Some("const opts = this.configOptionsFor(this.term()); this.openConfigPopover(opts[0], cx); this.setConfigOption(opts[1], true, cx);"),
    ).unwrap();
    assert!(!after_fast.contains("chevron-up.svg"), "model trigger should close immediately on fast toggle: {after_fast}");
    assert!(after_fast.contains("Fast Mode: On"), "fast-mode should toggle optimistically: {after_fast}");
    assert!(
        !after_fast.contains("acp-cfg-popover-model") || after_fast.contains(":transition(opacity, 130ms, 0ms, ease-in)"),
        "model popup must unmount or be in its closing animation: {after_fast}"
    );
}

#[gpui::test]
fn acp_peek_renders_and_mutates_queued_inbox_items(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let program = json!({"key":"pi", "name":"Pi", "icon":null, "tint":true, "tile":"#334455", "color":"#aabbcc", "shell":false, "agent":true});
    let mut values = fixture(true, json!({"terminal":"a1","acp":true,"tab":true,"pane":"p2"}), updates("current", "current"));
    let inbox = json!([
        {"id": 101, "text": "First queued prompt", "queued_ms": 1700000000000i64},
        {"id": 102, "text": "<h2 from=\"t1\">\nSecond queued prompt\n</h2>", "from": "t1", "queued_ms": 1700000060000i64}
    ]);
    values.insert("terminals".into(), json!([
        {"address":"t1","name":"Build","title":"Build","argv":["zsh"],"parent":null,"labels":{},"state":"live","status":"idle","program":program,"cwd":"/fixture/project"},
        {"address":"a1","name":"Reviewer","title":"Reviewer","argv":["pi"],"parent":"t1","labels":{},"state":"live","status":"working","turn":2,"transcript_seq":4,"inbox_len":2,"inbox":inbox,"report":{"app":"pi"},"program":program,"cwd":"/fixture/project"}
    ]));
    values.insert("agents".into(), json!([
        {"address":"a1","acp":true,"pane":"p2","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Reviewer","status":"working","app":"pi","program":program,"title":"Reviewer","seq":1,"focused":true}
    ]));

    let tree = describe_with_values(cx, &staged, "mods/peek", values.clone(), None).unwrap();
    assert!(tree.contains("acp-inbox-queue"), "queued inbox container missing: {tree}");
    assert!(tree.contains("Input queue (2)"), "queue header missing count: {tree}");
    assert!(tree.contains("First queued prompt"), "first queued item missing: {tree}");
    assert!(tree.contains("Second queued prompt"), "second queued item missing: {tree}");
    assert!(tree.contains("Build (t1)"), "friendly sender label missing: {tree}");
    assert!(tree.contains("acp-inbox-edit-101") && tree.contains("acp-inbox-rm-101"), "edit/remove buttons missing: {tree}");
    assert!(!tree.contains("<h2"), "raw <h2 envelope leaked into queue UI: {tree}");

    // Inline editor renders for item #102 and saves back via `h2 inbox edit`.
    let editing_tree = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        values.clone(),
        Some("const items = this.inboxItemsFor(this.term()); this.startEditInboxItem(items[1], cx);"),
    ).unwrap();
    assert!(editing_tree.contains("acp-inbox-editor-102"), "inline editor missing: {editing_tree}");
    assert!(editing_tree.contains("acp-inbox-save-102") && editing_tree.contains("acp-inbox-cancel-102"), "editor save/cancel buttons missing: {editing_tree}");

    // Saving edit then removing item #101 mutates the rendered queue cleanly.
    let after_edit = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        values.clone(),
        Some("const items = this.inboxItemsFor(this.term()); this.startEditInboxItem(items[1], cx); this.inboxEditInput.set_value('Updated second prompt'); this.saveEditInboxItem(cx);"),
    ).unwrap();
    assert!(after_edit.contains("Updated second prompt"), "optimistic/saved edit missing: {after_edit}");

    let after_rm = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        values,
        Some("this.removeInboxItem(101, cx);"),
    ).unwrap();
    assert!(!after_rm.contains("First queued prompt"), "removed queued item still visible: {after_rm}");
    assert!(after_rm.contains("Input queue (1)"), "remaining count should drop to 1: {after_rm}");
}

#[gpui::test]
fn brand_and_panel_buttons_render_by_default(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    let chrome = describe(cx, &staged, "mods/workspace-chrome", false, props(), updates("current", "current"), None).unwrap();
    assert!(chrome.contains("Herdr2"), "top-left sidebar header must say Herdr2: {chrome}");
    assert!(!chrome.contains("text \"Herdr\""), "stale Herdr label in workspace-chrome: {chrome}");

    let tabs = describe(cx, &staged, "mods/tabs", false, props(), updates("current", "current"), None).unwrap();
    assert!(tabs.contains("panel-files"), "Files button missing in top-right tab bar: {tabs}");
    assert!(tabs.contains("panel-changes"), "Changes button missing in top-right tab bar: {tabs}");
    assert!(tabs.contains("panel-checkpoints"), "Checkpoints button missing in top-right tab bar: {tabs}");

    let calls = Rc::new(RefCell::new(Calls::default()));
    let policy = Policy::new()
        .with_host_module(module(fixture(true, props(), updates("current", "current")), calls.clone()))
        .unwrap();
    let runtime = ShellRuntime::new_isolated().unwrap();
    let root = staged.wrapper("frame/welcome", "this.page = 3;");
    let window = cx.add_window(|_, _| Empty);
    let welcome = window.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(runtime, root, policy, window, cx).unwrap();
        let tree = session.describe(window, cx).unwrap();
        window.remove_window();
        tree
    }).unwrap();
    assert!(welcome.contains("Choose your panels"), "{welcome}");
    assert!(
        !calls.borrow().shared.contains_key("workspace.panels"),
        "welcome page 3 must not reset workspace.panels to []"
    );

    let settings = describe(cx, &staged, "frame/settings", true, json!({"section":"look"}), updates("current", "current"), None).unwrap();
    assert!(settings.contains("st-panel-files"), "Files panel toggle missing in Settings Look: {settings}");
    assert!(settings.contains("st-panel-changes"), "Changes panel toggle missing in Settings Look: {settings}");
    assert!(settings.contains("st-panel-checkpoints"), "Checkpoints panel toggle missing in Settings Look: {settings}");
}

#[gpui::test]
fn claude_acp_peek_exposes_dangerously_skip_permissions_in_mode_and_banner(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let program = json!({"key":"claude", "name":"Claude Code", "icon":null, "tint":false, "tile":"#1c1d21", "color":"#ffffff", "shell":false, "agent":true});
    let mut values = fixture(true, json!({"terminal":"a1","acp":true,"tab":true,"pane":"p2"}), updates("current", "current"));
    let config_options = json!([
        {"id":"model","name":"Model","category":"model","kind":"select","current_value":"sonnet","options":[{"value":"sonnet","name":"Sonnet"}]},
        {"id":"mode","name":"Mode","category":"mode","kind":"select","current_value":"default","options":[
            {"value":"default","name":"Manual","description":"Always ask before making changes"},
            {"value":"acceptEdits","name":"Accept edits","description":"Automatically accept all file edits"},
            {"value":"plan","name":"Plan","description":"Create a plan before making changes"},
            {"value":"auto","name":"Auto","description":"Claude handles permission decisions"},
            {"value":"bypassPermissions","name":"Bypass permissions","description":"Accepts all permissions"}
        ]}
    ]);
    values.insert("terminals".into(), json!([
        {"address":"a1","acp":true,"agent":"claude","mode":"default","name":"Claude","title":"Claude","argv":["claude-agent-acp"],"parent":null,"labels":{},"state":"live","status":"blocked","turn":1,"transcript_seq":1,"pending_permission":{"request_id":"req-1","title":"Run bash","options":[{"option_id":"allow-once","name":"Yes"}]},"config_options":config_options,"report":{"app":"claude-code"},"program":program,"cwd":"/fixture/project"}
    ]));
    values.insert("agents".into(), json!([
        {"address":"a1","agent":"claude","acp":true,"pane":"p2","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Claude","status":"blocked","app":"claude-code","program":program,"title":"Claude","seq":1,"focused":true}
    ]));

    let tree = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        values.clone(),
        Some("const mode = this.configOptionsFor(this.term()).find((o) => o.id === 'mode'); this.openConfigPopover(mode, cx);"),
    ).unwrap();

    assert!(tree.contains("perm-skip-all"), "dangerously-skip-permissions quick approve button missing in permission banner: {tree}");
    assert!(tree.contains("acp-cfg-popover-mode"), "mode popover missing: {tree}");
    assert!(tree.contains("acp-cfg-choice-mode-4"), "5th Claude mode choice missing in mode menu: {tree}");
    assert!(tree.contains("dangerously-skip-permissions"), "dangerously-skip-permissions choice missing in mode menu: {tree}");
    let list_line = tree.lines().find(|l| l.contains("acp-cfg-list")).unwrap();
    assert!(list_line.contains(".max_h[Number(420.0)]"), "mode choice list max_h must expand to avoid clipping 5th choice: {list_line}");

    let mut menu_values = values;
    menu_values.insert("props".into(), json!({"tab":"agent","target":{"terminal":"a1","pane":"p2"}}));
    let menu_tree = describe_with_values(cx, &staged, "mods/menu", menu_values.clone(), None).unwrap();
    assert!(menu_tree.contains("Permissions: dangerously-skip-permissions"), "ACP agent right-click menu missing quick bypass option: {menu_tree}");

    if let Some(list) = menu_values.get_mut("terminals").and_then(Value::as_array_mut) {
        list[0]["mode"] = json!("bypassPermissions");
    }
    let bypass_menu_tree = describe_with_values(cx, &staged, "mods/menu", menu_values, None).unwrap();
    assert!(bypass_menu_tree.contains("✓ dangerously-skip-permissions (Reset)"), "ACP agent right-click menu missing active bypass reset label: {bypass_menu_tree}");
}

#[gpui::test]
fn agents_status_button_and_manual_status_menus_render(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    let agents_tree = describe(cx, &staged, "mods/agents", true, props(), updates("current", "current"), None).unwrap();
    assert!(agents_tree.contains("status-t1"), "agents panel missing status button for t1: {agents_tree}");
    assert!(agents_tree.contains("ag-recycle"), "agents panel missing recycle bin button: {agents_tree}");

    let mut trash_props = props();
    trash_props.as_object_mut().unwrap().insert("trashMenu".into(), json!(true));
    let trash_tree = describe(cx, &staged, "mods/agents", true, trash_props, updates("current", "current"), None).unwrap();
    assert!(trash_tree.contains("Recycle Bin (0)"), "trashMenu missing header: {trash_tree}");

    let mut status_props = props();
    status_props.as_object_mut().unwrap().extend([
        ("statusMenu".into(), json!(true)),
        ("terminal".into(), json!("t1")),
    ]);
    let status_menu_tree = describe(cx, &staged, "mods/agents", true, status_props, updates("current", "current"), None).unwrap();
    for label in ["Idle", "Working", "Needs you", "Done", "Auto detect"] {
        assert!(status_menu_tree.contains(label), "statusMenu missing {label}: {status_menu_tree}");
    }

    let mut agent_menu_props = props();
    agent_menu_props.as_object_mut().unwrap().extend([
        ("tab".into(), json!("agent")),
        ("target".into(), json!({"terminal": "t1", "pane": "p1"})),
    ]);
    let agent_menu_tree = describe(cx, &staged, "mods/menu", true, agent_menu_props.clone(), updates("current", "current"), None).unwrap();
    assert!(agent_menu_tree.contains("Rename"), "agent context menu missing Rename: {agent_menu_tree}");
    for removed in ["Status: Idle", "Status: Working", "Status: Needs you", "Status: Done", "Status: Auto detect"] {
        assert!(!agent_menu_tree.contains(removed), "agent context menu should not include {removed}: {agent_menu_tree}");
    }

    let mut zh_values = fixture(true, agent_menu_props, updates("current", "current"));
    zh_values.get_mut("settings").unwrap()["language"] = json!("zh-CN");
    let zh_menu_tree = describe_with_values(cx, &staged, "mods/menu", zh_values, None).unwrap();
    assert!(zh_menu_tree.contains("重命名"), "zh-CN agent context menu missing 重命名: {zh_menu_tree}");
}

#[gpui::test]
fn simplified_chinese_language_renders_localized_chrome_and_settings(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    let zh_values = |mod_props: Value| {
        let mut values = fixture(true, mod_props, updates("current", "current"));
        values.get_mut("settings").unwrap()["language"] = json!("zh-CN");
        values
    };

    let settings_tree = describe_with_values(cx, &staged, "frame/settings", zh_values(props()), None).unwrap();
    for expected in ["界面语言", "简体中文", "外观"] {
        assert!(settings_tree.contains(expected), "zh-CN settings missing {expected}: {settings_tree}");
    }

    let tabs_tree = describe_with_values(cx, &staged, "mods/tabs", zh_values(props()), None).unwrap();
    for expected in ["文件", "变更", "终端", "tab-time-tab1"] {
        assert!(tabs_tree.contains(expected), "zh-CN tabs missing {expected}: {tabs_tree}");
    }

    let pane_zh_tree = describe_with_values(cx, &staged, "mods/pane", zh_values(props()), None).unwrap();
    for expected in ["终端", "ph-type-p1", "ph-time-p1"] {
        assert!(pane_zh_tree.contains(expected), "zh-CN pane header missing {expected}: {pane_zh_tree}");
    }

    let agents_tree = describe_with_values(cx, &staged, "mods/agents", zh_values(props()), None).unwrap();
    assert!(agents_tree.contains("智能体"), "zh-CN agents header missing: {agents_tree}");

    let chrome_tree = describe_with_values(
        cx,
        &staged,
        "mods/workspace-chrome",
        zh_values(json!({"footer": true})),
        None,
    ).unwrap();
    for expected in ["设置", "添加主机"] {
        assert!(chrome_tree.contains(expected), "zh-CN footer missing {expected}: {chrome_tree}");
    }

    let menu_tree = describe_with_values(
        cx,
        &staged,
        "mods/menu",
        zh_values(json!({"tab": "global"})),
        None,
    ).unwrap();
    for expected in ["新建工作区", "Language: 简体中文"] {
        assert!(menu_tree.contains(expected), "zh-CN global menu missing {expected}: {menu_tree}");
    }
}

#[gpui::test]
fn tab_menu_structured_view_toggle_and_pty_peek_render_cleanly(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let program = json!({"key":"claude", "name":"Claude Code", "icon":null, "tint":false, "tile":"#1c1d21", "color":"#ffffff", "shell":false, "agent":true});

    let mut off_values = fixture(true, json!({"tab":"tab","target":{"id":"tab1","space":"s1"}}), updates("current", "current"));
    off_values.insert("panes".into(), json!([
        {"pane":"p1","terminal":"t1","tab":"tab1","title":"Claude Code","cwd":"/fixture/project","status":"idle","program":program,"structured":false,"focused":true,"zoomed":false,"movable":false}
    ]));
    let off_menu = describe_with_values(cx, &staged, "mods/menu", off_values.clone(), None).unwrap();
    assert!(off_menu.contains("Structured chat view"), "tab menu missing Structured chat view toggle: {off_menu}");
    assert!(off_menu.contains("mi-sw-"), "tab menu missing visual toggle switch element: {off_menu}");

    let mut on_values = off_values;
    on_values.insert("panes".into(), json!([
        {"pane":"p1","terminal":"t1","tab":"tab1","title":"Claude Code","cwd":"/fixture/project","status":"working","program":program,"structured":true,"focused":true,"zoomed":false,"movable":false}
    ]));
    on_values.get_mut("settings").unwrap()["language"] = json!("zh-CN");
    let on_menu = describe_with_values(cx, &staged, "mods/menu", on_values, None).unwrap();
    assert!(on_menu.contains("结构化对话视图"), "zh-CN tab menu missing 结构化对话视图: {on_menu}");
    assert!(on_menu.contains("mi-sw-"), "zh-CN tab menu missing visual toggle switch element: {on_menu}");

    let mut peek_values = fixture(true, json!({"terminal":"t1","acp":true,"pty":true,"tab":true,"pane":"p1"}), updates("current", "current"));
    peek_values.insert("terminals".into(), json!([
        {"address":"t1","name":"Claude Code","title":"Claude Code","argv":["claude"],"parent":null,"labels":{"herdr.view":"peek"},"state":"live","status":"blocked","turn":1,"program":program,"cwd":"/fixture/project"}
    ]));
    peek_values.insert("agents".into(), json!([
        {"address":"t1","pane":"p1","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Claude Code","status":"blocked","app":"claude-code","program":program,"title":"Claude Code","seq":1,"focused":true}
    ]));
    peek_values.insert("panes".into(), json!([
        {"pane":"p1","terminal":"t1","tab":"tab1","title":"Claude Code","cwd":"/fixture/project","status":"blocked","program":program,"structured":true,"focused":true,"zoomed":false,"movable":false}
    ]));
    let calls = Rc::new(RefCell::new(Calls::default()));
    let policy = Policy::new()
        .with_storage_path(staged.0.join("store-pty-peek.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(peek_values, calls.clone()))
        .unwrap();
    let runtime = ShellRuntime::new_isolated().unwrap();
    let root = staged.0.join("mods/peek");
    let window = cx.add_window(|_, _| Empty);
    let mut session = window.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(runtime, root, policy, window, cx).unwrap();
        let _initial = session.describe(window, cx).unwrap();
        session
    }).unwrap();
    cx.run_until_parked();
    let peek_tree = window.update(cx, |_, window, cx| session.describe(window, cx).unwrap()).unwrap();
    window.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();

    assert!(calls.borrow().unexpected.is_empty(), "unexpected host calls: {:?}", calls.borrow().unexpected);

    assert!(peek_tree.contains("acp-switch-terminal"), "PTY peek missing Terminal view pill: {peek_tree}");
    assert!(peek_tree.contains("CLI Agent"), "PTY peek missing CLI Agent badge: {peek_tree}");
    assert!(peek_tree.contains("pty-blocked-banner"), "blocked PTY peek missing interactive confirmation banner: {peek_tree}");
    assert!(peek_tree.contains("pty-open-terminal"), "blocked PTY peek missing Open terminal button: {peek_tree}");
    assert!(peek_tree.contains("Fix CLI layout"), "PTY peek missing parsed user entry: {peek_tree}");
    assert!(peek_tree.contains("Updated the layout."), "PTY peek missing parsed assistant entry: {peek_tree}");

    let mut tabs_values = fixture(true, json!({}), updates("current", "current"));
    tabs_values.insert("panes".into(), json!([
        {"pane":"p1","terminal":"t1","tab":"tab1","title":"Claude Code","cwd":"/fixture/project","status":"working","program":program,"structured":true,"focused":true,"zoomed":false,"movable":false}
    ]));
    let tabs_tree = describe_with_values(cx, &staged, "mods/tabs", tabs_values.clone(), None).unwrap();
    assert!(tabs_tree.contains("Terminal Agent (Structured View)"), "structured terminal tab badge tooltip missing: {tabs_tree}");
    assert!(tabs_tree.contains("tab-time-tab1"), "structured terminal tab execution timer missing: {tabs_tree}");

    tabs_values.insert("props".into(), json!({"pane":"p1","terminal":"t1"}));
    let pane_tree = describe_with_values(cx, &staged, "mods/pane", tabs_values, None).unwrap();
    assert!(pane_tree.contains("ph-type-p1"), "structured PTY pane header missing agent type badge: {pane_tree}");
    assert!(pane_tree.contains("ph-time-p1"), "structured PTY pane header missing execution timer: {pane_tree}");
    assert!(pane_tree.contains("ph-stop-p1"), "structured PTY pane header missing Stop button: {pane_tree}");
    assert!(pane_tree.contains("ph-view-p1"), "structured PTY pane header missing 1-click Terminal toggle: {pane_tree}");

    let mut load_and_describe = |addr: &str, turn: u64, status: &str| {
        let mut pv = fixture(true, json!({"terminal":addr,"acp":true,"pty":true,"tab":true,"pane":"p1"}), updates("current", "current"));
        pv.insert("terminals".into(), json!([
            {"address":addr,"name":"Claude Code","title":"Claude Code","argv":["claude"],"parent":null,"labels":{"herdr.view":"peek"},"state":"live","status":status,"turn":turn,"program":program,"cwd":"/fixture/project"}
        ]));
        pv.insert("agents".into(), json!([
            {"address":addr,"pane":"p1","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Claude Code","status":status,"app":"claude-code","program":program,"title":"Claude Code","seq":1,"focused":true}
        ]));
        pv.insert("panes".into(), json!([
            {"pane":"p1","terminal":addr,"tab":"tab1","title":"Claude Code","cwd":"/fixture/project","status":status,"program":program,"structured":true,"focused":true,"zoomed":false,"movable":false}
        ]));
        let c = Rc::new(RefCell::new(Calls::default()));
        let pol = Policy::new()
            .with_storage_path(staged.0.join(format!("store-pty-{addr}.json")))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(pv, c.clone()))
            .unwrap();
        let rt = ShellRuntime::new_isolated().unwrap();
        let w = cx.add_window(|_, _| Empty);
        let mut s = w.update(cx, |_, window, cx| {
            let mut s = DescriptionSession::load(rt, staged.0.join("mods/peek"), pol, window, cx).unwrap();
            let _ = s.describe(window, cx).unwrap();
            s
        }).unwrap();
        cx.run_until_parked();
        let tree = w.update(cx, |_, window, cx| s.describe(window, cx).unwrap()).unwrap();
        w.update(cx, |_, window, _| window.remove_window()).unwrap();
        cx.run_until_parked();
        assert!(c.borrow().unexpected.is_empty(), "unexpected host calls for {addr}: {:?}", c.borrow().unexpected);
        tree
    };

    let idle_tree = load_and_describe("t_idle", 0, "idle");
    assert!(idle_tree.contains("No messages yet. Send a prompt below to begin."), "turn-0 idle PTY must show empty state: {idle_tree}");
    assert!(!idle_tree.contains("▐▛███▛█"), "turn-0 idle PTY leaked raw welcome banner: {idle_tree}");

    let snap_tree = load_and_describe("t_snap", 1, "done");
    assert!(snap_tree.contains("Terminal snapshot"), "screen fallback missing Terminal snapshot header: {snap_tree}");
    assert!(snap_tree.contains("Copy"), "screen fallback missing Copy pill: {snap_tree}");
    assert!(snap_tree.contains("pty-snap-open-terminal"), "screen fallback missing Open terminal pill: {snap_tree}");
}

#[gpui::test]
fn escape_interrupts_working_acp_and_structured_pty_peek(cx: &mut TestAppContext) {
    use gpui::VisualTestContext;
    use std::ops::Deref;

    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let program = json!({"key":"claude", "name":"Claude Code", "icon":null, "tint":false, "tile":"#1c1d21", "color":"#ffffff", "shell":false, "agent":true});

    let mut acp_values = fixture(true, json!({"terminal":"a1","acp":true,"tab":true,"pane":"p2"}), updates("current", "current"));
    acp_values.insert("terminals".into(), json!([
        {"address":"a1","acp":true,"agent":"claude","name":"Claude","title":"Claude","argv":["claude-agent-acp"],"parent":null,"labels":{},"state":"live","status":"working","turn":1,"transcript_seq":1,"program":program,"cwd":"/fixture/project"}
    ]));
    acp_values.insert("agents".into(), json!([
        {"address":"a1","acp":true,"pane":"p2","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Claude","status":"working","app":"claude-code","program":program,"title":"Claude","seq":1,"focused":true}
    ]));
    acp_values.insert("panes".into(), json!([
        {"pane":"p2","acp":true,"mod":"peek","terminal":"a1","tab":"tab1","title":"Claude","cwd":"/fixture/project","status":"working","program":program,"focused":true,"zoomed":false,"movable":false}
    ]));

    let tree = describe_with_values(cx, &staged, "mods/peek", acp_values.clone(), None).unwrap();
    assert!(tree.contains("acp-stop"), "working tab composer should render Stop · Esc pill: {tree}");

    let calls = Rc::new(RefCell::new(Calls::default()));
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-esc-acp.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(acp_values, calls.clone()))
            .unwrap(),
    );
    let runtime = ShellRuntime::new_isolated().unwrap();
    let window = cx.add_window(|window, cx| RootedView(runtime.try_load(&staged.0.join("mods/peek"), window, cx).unwrap()));
    let mut visual = VisualTestContext::from_window(*window.deref(), cx);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert_eq!(calls.borrow().interrupted, vec!["a1"], "unexpected: {:?}", calls.borrow().unexpected);
    visual.update(|window, _| window.remove_window());
    cx.run_until_parked();

    let mut pty_values = fixture(true, json!({"terminal":"t1","acp":true,"pty":true,"tab":true,"pane":"p1"}), updates("current", "current"));
    pty_values.insert("terminals".into(), json!([
        {"address":"t1","name":"Claude Code","title":"Claude Code","argv":["claude"],"parent":null,"labels":{"herdr.view":"peek"},"state":"live","status":"working","turn":1,"program":program,"cwd":"/fixture/project"}
    ]));
    pty_values.insert("agents".into(), json!([
        {"address":"t1","pane":"p1","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Claude Code","status":"working","app":"claude-code","program":program,"title":"Claude Code","seq":1,"focused":true}
    ]));
    pty_values.insert("panes".into(), json!([
        {"pane":"p1","terminal":"t1","tab":"tab1","title":"Claude Code","cwd":"/fixture/project","status":"working","program":program,"structured":true,"focused":true,"zoomed":false,"movable":false}
    ]));

    let pty_tree = describe_with_values(cx, &staged, "mods/peek", pty_values.clone(), None).unwrap();
    assert!(pty_tree.contains("acp-stop"), "working structured PTY composer should render Stop · Esc pill: {pty_tree}");

    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-esc-pty.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(pty_values, calls.clone()))
            .unwrap(),
    );
    let pty_runtime = ShellRuntime::new_isolated().unwrap();
    let pty_window = cx.add_window(|window, cx| RootedView(pty_runtime.try_load(&staged.0.join("mods/peek"), window, cx).unwrap()));
    let mut pty_visual = VisualTestContext::from_window(*pty_window.deref(), cx);
    pty_visual.update(|window, cx| window.draw(cx).clear(cx));
    pty_visual.run_until_parked();
    pty_visual.simulate_keystrokes("escape");
    pty_visual.run_until_parked();
    assert_eq!(calls.borrow().interrupted, vec!["a1", "t1"], "unexpected: {:?}", calls.borrow().unexpected);
    pty_visual.update(|window, _| window.remove_window());
    gpui_shell::policy::set_default(Policy::new());
    cx.run_until_parked();
}

#[gpui::test]
fn settings_drove_renders_policies_codex_fast_and_english_drove_label(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    let en_tree = describe(
        cx,
        &staged,
        "frame/settings",
        true,
        json!({ "section": "drove" }),
        updates("current", "current"),
        Some("this.droveData = { config: { roles: { reviewer: { label: 'Reviewer', access: 'readonly', primary: { agent: 'codex', model: 'gpt-6.1-sol', effort: 'high', fast: true }, fallbacks: [] } }, ai: { enabled: true, verifyDigest: true, handoverCompact: false, autoTitle: true, taskEnrich: true, conflictPolicy: 'warn', questionMode: 'draft', loopDetect: 'steer_once' } }, aiStatus: { switches: { verifyDigest: true, handoverCompact: false, autoTitle: true, taskEnrich: true, conflictPolicy: 'warn', questionMode: 'draft', loopDetect: 'steer_once' } }, agents: { fullProbe: true, agents: [{ id: 'codex', installed: true, models: ['gpt-6.1-sol'] }] } };"),
    )
    .unwrap();
    for expected in [
        "CONTROL-PLANE AI POLICIES",
        "Verify Digest",
        "Handover Compact",
        "Conflict Policy",
        "Loop Detect",
        "⚡ Fast",
        "Reasoning Effort",
        "Context Size",
        "Max Output Tokens",
        "st-dr-ai-eff-group",
        "st-drp-ai-context",
        "st-drp-ai-max-tokens",
    ] {
        assert!(en_tree.contains(expected), "EN drove settings missing {expected}: {en_tree}");
    }
    assert!(!en_tree.contains("Send feedback"), "Send feedback should be suppressed: {en_tree}");

    let mut zh_values = fixture(true, json!({ "section": "drove" }), updates("current", "current"));
    zh_values.get_mut("settings").unwrap()["language"] = json!("zh-CN");
    let zh_tree = describe_with_values(
        cx,
        &staged,
        "frame/settings",
        zh_values,
        Some("this.droveData = { config: { roles: { reviewer: { label: 'Reviewer', access: 'readonly', primary: { agent: 'codex', model: null, effort: 'high', fast: false }, fallbacks: [] } }, ai: { enabled: true } }, aiStatus: { switches: { verifyDigest: true, handoverCompact: true, autoTitle: true, taskEnrich: true, conflictPolicy: 'warn', questionMode: 'draft', loopDetect: 'off' } }, agents: { fullProbe: true, agents: [{ id: 'codex', installed: true, models: ['gpt-6.1-sol'] }] } };"),
    )
    .unwrap();
    for expected in ["Drove", "控制面 AI 策略开关", "Verify 失败摘要", "交接上下文压缩", "死循环检测", "思考强度", "上下文大小", "最大输出长度"] {
        assert!(zh_tree.contains(expected), "zh-CN drove settings missing {expected}: {zh_tree}");
    }
    assert!(!zh_tree.contains("编队"), "Settings should show English Drove in zh-CN: {zh_tree}");
    assert!(!zh_tree.contains("发送反馈"), "Send feedback should be suppressed in zh-CN: {zh_tree}");
}

#[gpui::test]
fn split_pane_names_merged_tabs_and_windows_shells(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let claude_prog = json!({"key":"claude","name":"Claude Code","icon":null,"tint":false,"tile":"#1c1d21","color":"#ffffff","shell":false,"agent":true});
    let shell_prog = json!({"key":"shell","name":"Shell","icon":null,"tint":true,"tile":"#16241a","color":"#4ec95f","shell":true,"agent":false});

    // 1. Split tab panes show each pane's custom_name / name, inline rename button, and inline input when renaming.
    let mut split_values = fixture(true, json!({"pane":"p1","terminal":"t1"}), updates("current", "current"));
    split_values.insert("spaces".into(), json!([
        {"id":"s1","index":1,"name":"Local","folder":"/fixture/project","machine":null,"branch":"main","active":true,"status":"idle","tabs":[{"id":"tab1","index":1,"name":"tab1","named":false,"active":true,"status":"idle","panes":2,"zoomed":false,"settled":false,"settled_at":null}]}
    ]));
    split_values.insert("panes".into(), json!([
        {"pane":"p1","terminal":"t1","tab":"tab1","title":"zsh","custom_name":"Frontend","cwd":"/fixture/project","status":"idle","program":shell_prog,"focused":true,"zoomed":false,"movable":true},
        {"pane":"p2","acp":true,"mod":"peek","terminal":"a1","tab":"tab1","title":"Reviewer","custom_name":"Reviewer","cwd":"/fixture/project","status":"idle","program":claude_prog,"focused":false,"zoomed":false,"movable":true}
    ]));
    split_values.insert("terminals".into(), json!([
        {"address":"t1","name":"zsh","labels":{"herdr.name":"Frontend"},"state":"live","status":"idle","program":shell_prog,"cwd":"/fixture/project"},
        {"address":"a1","acp":true,"name":"Reviewer","labels":{"herdr.name":"Reviewer"},"state":"live","status":"idle","program":claude_prog,"cwd":"/fixture/project"}
    ]));
    let pane_tree = describe_with_values(cx, &staged, "mods/pane", split_values.clone(), None).unwrap();
    assert!(pane_tree.contains("Frontend"), "split pane 1 name missing: {pane_tree}");
    assert!(pane_tree.contains("ph-ren-p1"), "split pane 1 rename trigger missing: {pane_tree}");

    let rename_tree = describe_with_values(cx, &staged, "mods/pane", split_values.clone(), Some("this.startRename('t1', 'Frontend', cx);")).unwrap();
    assert!(rename_tree.contains("ph-ren-input-p1"), "inline rename Input missing in split pane: {rename_tree}");

    // 2. Pane context menu exposes Rename action.
    let menu_tree = describe_with_values(
        cx,
        &staged,
        "mods/menu",
        fixture(true, json!({"tab":"pane","target":{"pane":"p1","terminal":"t1"}}), updates("current", "current")),
        None,
    ).unwrap();
    assert!(menu_tree.contains("Rename"), "pane context menu should expose Rename: {menu_tree}");

    // 3. Merged tab shows combined pane names and persistent merged badge.
    let tabs_tree = describe_with_values(cx, &staged, "mods/tabs", split_values, None).unwrap();
    assert!(tabs_tree.contains("Frontend + Reviewer"), "merged tab title should combine pane names: {tabs_tree}");
    assert!(tabs_tree.contains("tab-merged-tab1"), "merged tab badge element missing: {tabs_tree}");
    assert!(tabs_tree.contains("◫ 2"), "merged tab badge label missing: {tabs_tree}");

    // 4. New Menu omits dangerously-skip-permissions toggle and shows Windows shell choices on Windows.
    let mut nm_values = fixture(true, json!({}), updates("current", "current"));
    nm_values.insert("window".into(), json!({"platform":"windows","titlebar":"none","buttons":true,"maximized":false,"fullscreen":false,"active":true}));
    let nm_calls = Rc::new(RefCell::new(Calls::default()));
    nm_calls.borrow_mut().shared.insert("new-menu.agent-mode".into(), json!("acp"));
    let nm_policy = Policy::new()
        .with_storage_path(staged.0.join("store-nm-win.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(nm_values.clone(), nm_calls.clone()))
        .unwrap();
    let nm_wrapper = staged.wrapper(
        "mods/new-menu",
        "this.launchAgent({ key: 'claude', name: 'Claude Code', icon: 'icons/claude.svg', tint: false, installed: true, run: ['claude'] }, { id: 's1', folder: '/fixture/project' }, true);",
    );
    let nm_runtime = ShellRuntime::new_isolated().unwrap();
    let nm_win = cx.add_window(|_, _| Empty);
    let nm_tree = nm_win
        .update(cx, |_, window, cx| {
            let mut session = DescriptionSession::load(nm_runtime, nm_wrapper, nm_policy, window, cx).unwrap();
            let _ = session.describe(window, cx).unwrap();
            session.describe(window, cx).unwrap()
        })
        .unwrap();
    nm_win.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();
    assert!(!nm_tree.contains("nm-acp-skip"), "ACP skip toggle should be removed: {nm_tree}");
    assert!(!nm_tree.contains("dangerously-skip-permissions"), "dangerously-skip-permissions should be hidden: {nm_tree}");
    assert!(!nm_tree.contains("nm-shell-wsl"), "WSL shell option should be removed on Windows: {nm_tree}");
    assert!(!nm_tree.contains("WSL"), "WSL label should be removed on Windows: {nm_tree}");
    for expected in ["nm-win-shells", "nm-shell-powershell", "nm-shell-git-bash", "PowerShell", "Git Bash", "CMD"] {
        assert!(nm_tree.contains(expected), "Windows shell picker in new-menu missing {expected}: {nm_tree}");
    }
    assert!(
        nm_calls.borrow().effects.iter().any(|(fn_name, args)| {
            fn_name == "run"
                && args.first().and_then(Value::as_str) == Some("tab.new")
                && args.get(1).and_then(|v| v.get("acp")).and_then(Value::as_bool) == Some(true)
                && args.get(1).and_then(|v| v.get("agent")).and_then(Value::as_str) == Some("claude")
        }),
        "ACP launch should dispatch tab.new with acp: true so workspace jumps to the new tab: {:?}",
        nm_calls.borrow().effects
    );

    // 4b. Antigravity (`agy`) is enabled in ACP mode and launches `agent: "agy"`.
    let agy_calls = Rc::new(RefCell::new(Calls::default()));
    agy_calls.borrow_mut().shared.insert("new-menu.agent-mode".into(), json!("acp"));
    let agy_policy = Policy::new()
        .with_storage_path(staged.0.join("store-nm-agy.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(nm_values.clone(), agy_calls.clone()))
        .unwrap();
    let agy_agent_fixture = "[{ key: 'antigravity', name: 'Antigravity', icon: 'icons/antigravity.svg', tint: false, installed: true, run: ['agy', '--dangerously-skip-permissions'] }]";
    std::fs::write(
        staged.0.join("main.js"),
        format!(
            "import Real from './mods/new-menu/main.js'; export default class Scenario extends Real {{ \
             init(p, cx) {{ super.init(p, cx); this.launchAgent({agy_agent_fixture}[0], {{ id: 's1', folder: '/fixture/project' }}, true); }} \
             render() {{ this.agents = {agy_agent_fixture}; return super.render(); }} }}"
        ),
    )
    .unwrap();
    let agy_runtime = ShellRuntime::new_isolated().unwrap();
    let agy_win = cx.add_window(|_, _| Empty);
    let agy_tree = agy_win
        .update(cx, |_, window, cx| {
            let mut session = DescriptionSession::load(agy_runtime, staged.0.clone(), agy_policy, window, cx).unwrap();
            let _ = session.describe(window, cx).unwrap();
            session.describe(window, cx).unwrap()
        })
        .unwrap();
    agy_win.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();
    assert!(agy_tree.contains("Antigravity"), "Antigravity row missing in new-menu ACP mode: {agy_tree}");
    assert!(!agy_tree.contains("CLI only"), "Antigravity must be ACP-enabled, not CLI only: {agy_tree}");
    assert!(!agy_tree.contains("仅终端"), "Antigravity must be ACP-enabled, not 仅终端: {agy_tree}");
    assert!(
        agy_calls.borrow().effects.iter().any(|(fn_name, args)| {
            fn_name == "run"
                && args.first().and_then(Value::as_str) == Some("tab.new")
                && args.get(1).and_then(|v| v.get("acp")).and_then(Value::as_bool) == Some(true)
                && args.get(1).and_then(|v| v.get("agent")).and_then(Value::as_str) == Some("agy")
                && args.get(1).and_then(|v| v.get("folder")).and_then(Value::as_str) == Some("/fixture/project")
        }),
        "Antigravity ACP launch should map antigravity -> agent: \"agy\": {:?}",
        agy_calls.borrow().effects
    );

    // 5. Settings exposes Default terminal shell picker on Windows and fixed Fast column slot across all Drove candidate rows.
    let mut st_look_values = fixture(true, json!({ "section": "look" }), updates("current", "current"));
    st_look_values.insert("window".into(), json!({"platform":"windows","titlebar":"none","buttons":true,"maximized":false,"fullscreen":false,"active":true}));
    let look_tree = describe_with_values(cx, &staged, "frame/settings", st_look_values, None).unwrap();
    assert!(look_tree.contains("Default terminal shell"), "Windows Look settings missing Default terminal shell: {look_tree}");
    assert!(look_tree.contains("st-drp-term-shell"), "Windows Look settings missing st-drp-term-shell picker: {look_tree}");

    let aligned_tree = describe(
        cx,
        &staged,
        "frame/settings",
        true,
        json!({ "section": "drove" }),
        updates("current", "current"),
        Some("this.droveAiConfigMode = 'models'; this.droveRolesBackup = { mode: 'models', roles: {} }; this.droveData = { config: { roles: { reviewer: { label: 'Reviewer', access: 'readonly', primary: { agent: 'claude', model: 'claude-sonnet-4-5', effort: 'high', strengths: ['Security Audit', 'Architecture Review'] }, fallbacks: [{ agent: 'codex', model: 'gpt-5.4', effort: 'xhigh', fast: true, strengths: ['Concurrency & Locks'] }, { agent: 'agy', model: 'gemini-4-carbon-high', effort: 'high' }, { agent: 'cursor', model: 'grok-4.7', effort: 'high', context: '500k' }, { agent: 'cursor', model: 'grok-4.7-high', effort: 'high', context: '256k' }] } }, ai: { enabled: true, baseUrl: 'https://api.deepseek.com', model: 'deepseek-chat' } }, aiStatus: { switches: {} }, agents: { fullProbe: true, agents: [{ id: 'claude', installed: true, models: ['claude-sonnet-4-5', 'claude-opus-4-6'] }, { id: 'codex', installed: true, models: ['gpt-5.4'] }, { id: 'agy', installed: true, models: ['gemini-4-carbon-high'] }, { id: 'cursor', installed: true, models: ['grok-4.7', 'grok-4.7-high'], modelContexts: { 'grok-4.7': ['256k', '500k'] } }] } };"),
    )
    .unwrap();
    assert!(aligned_tree.contains("st-dr-ai-models-btn"), "Roles header should render AI Models button: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-ai-strengths-btn"), "Roles header should render AI Strengths button: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-ai-roles-undo"), "Roles header should render Restore button when backup exists: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-ai-roles-panel"), "Expanded AI Auto-Config panel should render: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-ai-roles-prompt"), "Expanded AI panel should render tall multiline Textarea: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-ai-roles-run"), "Expanded AI Auto-Config panel should render Generate button: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-str-chip-reviewer-0-0"), "Candidate strengths chip should render: {aligned_tree}");
    assert!(aligned_tree.contains("Security Audit"), "Candidate strength label should render: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-str-add-reviewer-c0"), "Candidate strength add picker should render: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-eff-reviewer-0-high"), "3-level model (claude-sonnet-4-5) should show High effort: {aligned_tree}");
    assert!(!aligned_tree.contains("st-dr-eff-reviewer-0-xhigh"), "3-level model (claude-sonnet-4-5) must NOT show xhigh effort: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-eff-reviewer-1-xhigh"), "4-level model (gpt-5.4) should show xhigh effort: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-fast-reviewer-0"), "Non-Codex candidate row should reserve aligned Fast slot: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-fast-reviewer-1"), "Codex candidate row should render Fast toggle: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-eff-group-reviewer-2"), "Agy candidate row should render effort container: {aligned_tree}");
    assert!(aligned_tree.contains("st-drp-reviewer-c3-ctx"), "Cursor candidate row should render context picker: {aligned_tree}");
    assert!(aligned_tree.contains("Ctx 500K"), "Cursor candidate row should show selected 500K context: {aligned_tree}");
    let eff_group_3 = aligned_tree.lines().find(|l| l.contains("st-dr-eff-group-reviewer-3")).unwrap();
    assert!(!eff_group_3.contains("0.35"), "Base Cursor model effort selector should stay enabled: {eff_group_3}");
    let eff_pill_3 = aligned_tree.lines().find(|l| l.contains("st-dr-eff-reviewer-3-low")).unwrap();
    assert!(eff_pill_3.contains("on_click"), "Base Cursor model effort pill should remain clickable: {eff_pill_3}");
    assert!(eff_pill_3.contains("cursor_pointer"), "Base Cursor model effort pill should show pointer cursor: {eff_pill_3}");
    let eff_group_4 = aligned_tree.lines().find(|l| l.contains("st-dr-eff-group-reviewer-4")).unwrap();
    assert!(eff_group_4.contains("0.35"), "Embedded-effort Cursor model should gray out effort selector: {eff_group_4}");
    assert!(
        eff_group_4.contains("Selected Cursor model already includes reasoning effort"),
        "Embedded-effort Cursor model should explain disabled effort in tooltip: {eff_group_4}"
    );
    let eff_pill_4 = aligned_tree.lines().find(|l| l.contains("st-dr-eff-reviewer-4-high")).unwrap();
    assert!(!eff_pill_4.contains("on_click"), "Embedded-effort Cursor model effort pill must not be clickable: {eff_pill_4}");
    assert!(aligned_tree.contains("st-drp-reviewer-c4-ctx"), "Embedded-effort Cursor row should still render context picker: {aligned_tree}");
    assert!(aligned_tree.contains("Ctx 256K"), "Embedded-effort Cursor row should show selected 256K context: {aligned_tree}");
    for idx in 0..5 {
        let popup_line = aligned_tree
            .lines()
            .find(|l| l.contains(&format!("st-drpp-reviewer-c{idx}-agent")))
            .unwrap_or_else(|| panic!("missing agent picker popup for row {idx}: {aligned_tree}"));
        assert!(
            popup_line.contains(".w[Number(118.0)]")
                && popup_line.contains(".min_w[Number(118.0)]")
                && popup_line.contains(".max_w[Number(118.0)]")
                && popup_line.contains(".flex_none"),
            "Agent picker row {idx} must enforce fixed 118px width so rows align vertically: {popup_line}"
        );
        let trigger_line = aligned_tree
            .lines()
            .find(|l| l.contains(&format!("st-drp-reviewer-c{idx}-agent")))
            .unwrap_or_else(|| panic!("missing agent picker trigger for row {idx}: {aligned_tree}"));
        assert!(
            trigger_line.contains(".w_full"),
            "Agent picker trigger row {idx} must stretch full width of its 118px popup box: {trigger_line}"
        );
    }
}

#[gpui::test]
fn text_view_markdown_renders_local_data_and_http_images(cx: &mut TestAppContext) {
    use base64::Engine as _;
    use gpui::{VisualTestContext, http_client::{AsyncBody, FakeHttpClient, Response}};
    use gpui_shell::HttpRequestGrant;
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
        ops::Deref as _,
        sync::Arc,
        task::{Context, Poll, Wake},
        thread::Thread,
        time::Duration,
    };

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        struct ThreadWaker(Thread);
        impl Wake for ThreadWaker {
            fn wake(self: Arc<Self>) { self.0.unpark(); }
        }
        let waker = Arc::new(ThreadWaker(std::thread::current())).into();
        let mut tcx = Context::from_waker(&waker);
        let mut fut = std::pin::pin!(fut);
        loop {
            match fut.as_mut().poll(&mut tcx) {
                Poll::Ready(val) => return val,
                Poll::Pending => std::thread::park_timeout(Duration::from_millis(5)),
            }
        }
    }

    let encode_png = |width: u32, rgba: [u8; 4]| -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let pixels: Vec<u8> = (0..width).flat_map(|_| rgba).collect();
            writer.write_image_data(&pixels).unwrap();
        }
        out
    };

    let png_2 = encode_png(2, [220, 40, 40, 255]);
    let png_3 = encode_png(3, [40, 220, 40, 255]);
    let png_4 = encode_png(4, [40, 40, 220, 255]);
    let png_5 = encode_png(5, [220, 220, 40, 255]);
    let png_6 = encode_png(6, [220, 40, 220, 255]);
    let png_7 = encode_png(7, [40, 220, 220, 255]);

    // 1. Verify production gpui_shell::http_client GET + RedirectPolicy handling.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server_png = png_6.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            let first_line = req.lines().next().unwrap_or("");
            if first_line.starts_with("GET /redirect ") {
                stream
                    .write_all(b"HTTP/1.1 302 Found\r\nLocation: /final.png\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
            } else {
                let mut reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    server_png.len()
                )
                .into_bytes();
                reply.extend_from_slice(&server_png);
                stream.write_all(&reply).unwrap();
            }
        }
    });

    let real_client = gpui_shell::http_client("herdr2-test");
    let no_follow = block_on(real_client.get(
        &format!("http://{addr}/redirect"),
        ().into(),
        false,
    ))
    .unwrap();
    assert_eq!(no_follow.status().as_u16(), 302);
    assert_eq!(no_follow.headers().get("location").unwrap().to_str().unwrap(), "/final.png");

    let followed = block_on(real_client.get(
        &format!("http://{addr}/redirect"),
        ().into(),
        true,
    ))
    .unwrap();
    assert_eq!(followed.status().as_u16(), 200);
    assert_eq!(
        followed.headers().get("content-length").unwrap().to_str().unwrap(),
        png_6.len().to_string(),
    );
    server.join().unwrap();

    // 2. Verify end-to-end TextView.markdown image resolution across policy boundaries.
    cx.update(gpui_shell::init);
    let remote_png = png_6.clone();
    let fake_client = FakeHttpClient::create(move |req| {
        let remote_png = remote_png.clone();
        async move {
            if req.uri().to_string() == "https://images.example/remote.png" {
                Ok(Response::builder()
                    .status(200)
                    .header("content-type", "image/png")
                    .body(AsyncBody::from(remote_png))
                    .unwrap())
            } else {
                Ok(Response::builder()
                    .status(404)
                    .body(AsyncBody::from(Vec::new()))
                    .unwrap())
            }
        }
    });
    cx.update(|cx| cx.set_http_client(fake_client));

    let staged = Staged::new();
    let ws = staged.0.join("ws");
    let docs = ws.join("docs");
    let sub = docs.join("sub");
    let assets = ws.join("assets");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(&assets).unwrap();
    std::fs::write(sub.join("rel.png"), &png_2).unwrap();
    std::fs::write(assets.join("root.png"), &png_3).unwrap();
    std::fs::write(sub.join("file.png"), &png_4).unwrap();
    let outside = staged.0.join("outside.png");
    std::fs::write(&outside, &png_7).unwrap();

    let file_uri = format!("file://{}", sub.join("file.png").display());
    let data_uri = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png_5)
    );
    let md_doc = format!(
        "![rel](./sub/rel.png)\n\n![root](/assets/root.png)\n\n![file]({file_uri})\n\n<img src=\"{data_uri}\" height=\"24\">\n\n![remote](https://images.example/remote.png)\n\n![outside](../../outside.png)"
    );
    let md_json = serde_json::to_string(&md_doc).unwrap();
    let denied_opts = json!({ "allow_data_images": false });
    let allowed_opts = json!({
        "base_dir": docs.to_string_lossy(),
        "root_dir": ws.to_string_lossy(),
        "allow_data_images": true,
    });

    let denied_dir = staged.0.join("tv-denied");
    std::fs::create_dir_all(&denied_dir).unwrap();
    std::fs::write(
        denied_dir.join("main.js"),
        format!(
            "import {{ View, div }} from 'gpui-kit';\nimport {{ TextView }} from 'gpui-base';\nexport default class Doc extends View {{ render() {{ return div().size_full().child(TextView.markdown('doc-denied', {md_json}, {denied_opts}).scrollable(false)); }} }}\n"
        ),
    )
    .unwrap();

    gpui_shell::policy::set_default(Policy::new().with_capabilities(Capabilities::new()));
    let runtime_denied = ShellRuntime::new_isolated().unwrap();
    let win_denied = cx.add_window(|window, cx| RootedView(runtime_denied.try_load(&denied_dir, window, cx).unwrap()));
    let mut visual_denied = VisualTestContext::from_window(*win_denied.deref(), cx);
    for _ in 0..4 {
        visual_denied.update(|window, cx| window.draw(cx).clear(cx));
        visual_denied.run_until_parked();
    }
    let denied_images = visual_denied.update(|window, _| window.displayed_images());
    assert!(denied_images.is_empty(), "expected no images without grants/opt-in, got {}", denied_images.len());
    visual_denied.update(|window, _| window.remove_window());
    cx.run_until_parked();

    let allowed_dir = staged.0.join("tv-allowed");
    std::fs::create_dir_all(&allowed_dir).unwrap();
    std::fs::write(
        allowed_dir.join("main.js"),
        format!(
            "import {{ View, div }} from 'gpui-kit';\nimport {{ TextView }} from 'gpui-base';\nexport default class Doc extends View {{ render() {{ return div().size_full().child(TextView.markdown('doc-allowed', {md_json}, {allowed_opts}).scrollable(false)); }} }}\n"
        ),
    )
    .unwrap();

    gpui_shell::policy::set_default(
        Policy::new().with_capabilities(
            Capabilities::new()
                .read_roots([ws.clone()])
                .http_requests([HttpRequestGrant::new("images.example", ["GET"], ["/remote.png"], [] as [&str; 0])]),
        ),
    );
    let runtime_allowed = ShellRuntime::new_isolated().unwrap();
    let win_allowed = cx.add_window(|window, cx| RootedView(runtime_allowed.try_load(&allowed_dir, window, cx).unwrap()));
    let mut visual_allowed = VisualTestContext::from_window(*win_allowed.deref(), cx);
    for _ in 0..4 {
        visual_allowed.update(|window, cx| window.draw(cx).clear(cx));
        visual_allowed.run_until_parked();
    }
    let allowed_sizes: Vec<_> = visual_allowed
        .update(|window, _| window.displayed_images())
        .into_iter()
        .map(|img| img.size(0))
        .collect();
    for width in [2, 3, 4, 5, 6] {
        let expected = gpui::size(width.into(), 1.into());
        assert!(
            allowed_sizes.contains(&expected),
            "expected rendered image width {width}x1 in {:?}",
            allowed_sizes
        );
    }
    assert!(
        !allowed_sizes.contains(&gpui::size(7.into(), 1.into())),
        "outside read_root image 7x1 must not render: {:?}",
        allowed_sizes
    );

    visual_allowed.update(|window, _| window.remove_window());
    gpui_shell::policy::set_default(Policy::new());
    cx.run_until_parked();
}

#[gpui::test]
fn files_mod_shows_collapsible_tree_and_edits_markdown_in_preview(cx: &mut TestAppContext) {
    use gpui::{Modifiers, VisualTestContext, point, px};
    use std::ops::Deref as _;

    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let initial_md = "# Title\n\n- [ ] Ship images\n\nBody paragraph.\n";
    let data_png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAYAAAD0In+KAAAAEUlEQVR4nGMUMgn7z8DAwAAACQ0BnfdmzRUAAAAASUVORK5CYII=";

    // 1. Right-side tree + Markdown preview without Split, Wrap, or Open-in-Tab.
    let calls_open = Rc::new(RefCell::new(Calls::default()));
    calls_open.borrow_mut().files.insert("/fixture/project/README.md".into(), initial_md.into());
    let policy_open = Policy::new()
        .with_storage_path(staged.0.join("store-files-open.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(fixture(true, json!({ "file": "README.md" }), updates("current", "current")), calls_open.clone()))
        .unwrap();
    let runtime_open = ShellRuntime::new_isolated().unwrap();
    let win_open = cx.add_window(|_, _| Empty);
    let mut sess_open = win_open
        .update(cx, |_, window, cx| {
            let mut session = DescriptionSession::load(runtime_open, staged.0.join("mods/files"), policy_open, window, cx).unwrap();
            let _ = session.describe(window, cx).unwrap();
            session
        })
        .unwrap();
    cx.run_until_parked();
    let tree_open = win_open.update(cx, |_, window, cx| sess_open.describe(window, cx).unwrap()).unwrap();
    win_open.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();

    assert!(calls_open.borrow().unexpected.is_empty(), "unexpected host calls: {:?}", calls_open.borrow().unexpected);
    for expected in [
        "files-split",
        "files-tree",
        "README.md",
        "src",
        "files-collapse-tree",
        "files-toggle-tree",
        "mods/files/panel-right.svg",
        "files-md-view",
        "files-md-preview",
        "files-md-code",
        "files-md-block-0",
        "files-md-block-1",
        "files-md-block-2",
        "files-task-1-0",
    ] {
        assert!(tree_open.contains(expected), "side-by-side files view missing {expected}: {tree_open}");
    }
    let md_pos = tree_open.find("files-md-view").expect("files-md-view missing");
    let tree_pos = tree_open.find("files-tree").expect("files-tree missing");
    assert!(md_pos < tree_pos, "file tree must appear to the right of Markdown viewer: {tree_open}");
    assert!(!tree_open.contains("files-md-split"), "Split mode button must be removed: {tree_open}");
    assert!(!tree_open.contains("files-wrap"), "Wrap button must be removed: {tree_open}");
    assert!(!tree_open.contains("files-open-tab"), "Open in Tab button must be removed: {tree_open}");

    // 2. Collapsible file tree hides treeSidebar while keeping Markdown preview & toggle button.
    let calls_collapsed = Rc::new(RefCell::new(Calls::default()));
    calls_collapsed.borrow_mut().files.insert("/fixture/project/README.md".into(), initial_md.into());
    let policy_collapsed = Policy::new()
        .with_storage_path(staged.0.join("store-files-collapsed.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(fixture(true, json!({ "file": "README.md" }), updates("current", "current")), calls_collapsed.clone()))
        .unwrap();
    let collapsed_dir = staged.wrapper("mods/files", "this.setTreeOpen(false, cx);");
    let runtime_collapsed = ShellRuntime::new_isolated().unwrap();
    let win_collapsed = cx.add_window(|_, _| Empty);
    let mut sess_collapsed = win_collapsed
        .update(cx, |_, window, cx| {
            let mut session = DescriptionSession::load(runtime_collapsed, collapsed_dir, policy_collapsed, window, cx).unwrap();
            let _ = session.describe(window, cx).unwrap();
            session
        })
        .unwrap();
    cx.run_until_parked();
    let tree_collapsed = win_collapsed.update(cx, |_, window, cx| sess_collapsed.describe(window, cx).unwrap()).unwrap();
    win_collapsed.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();

    assert!(calls_collapsed.borrow().unexpected.is_empty(), "unexpected host calls: {:?}", calls_collapsed.borrow().unexpected);
    assert!(!tree_collapsed.contains("files-tree"), "collapsed tree must not build files-tree: {tree_collapsed}");
    assert!(tree_collapsed.contains("files-md-view"), "collapsed tree must keep files-md-view: {tree_collapsed}");
    assert!(tree_collapsed.contains("files-toggle-tree"), "collapsed tree must keep expand toggle: {tree_collapsed}");
    assert_eq!(calls_collapsed.borrow().shared.get("files.treeOpen"), Some(&json!(false)));

    // 3. Real mouse click + real keystroke editing in Markdown Preview, Escape commit, and Ctrl+S save (local workspace).
    let calls_edit = Rc::new(RefCell::new(Calls::default()));
    calls_edit.borrow_mut().files.insert("/fixture/project/README.md".into(), initial_md.into());
    calls_edit.borrow_mut().shared.insert("files.treeOpen".into(), json!(false));
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-files-edit.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(fixture(true, json!({ "file": "README.md" }), updates("current", "current")), calls_edit.clone()))
            .unwrap(),
    );
    let runtime_edit = ShellRuntime::new_isolated().unwrap();
    let win_edit = cx.add_window(|window, cx| RootedView(runtime_edit.try_load(&staged.0.join("mods/files"), window, cx).unwrap()));
    let view_edit: gpui::Entity<gpui_shell::ScriptView> = win_edit
        .read_with(cx, |root, cx| root.0.read(cx).content().clone().downcast().unwrap())
        .unwrap();
    let mut visual = VisualTestContext::from_window(*win_edit.deref(), cx);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));

    visual.simulate_click(point(px(80.), px(60.)), Modifiers::default());
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let opened_editor_tree = visual.update(|_, cx| {
        view_edit.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(opened_editor_tree.contains("files-md-block-editor"), "clicking #files-md-block-0 must open inline block editor: {opened_editor_tree}");
    assert!(!opened_editor_tree.contains("files-md-block-done"), "Typora block editor must not show header chrome/Done button: {opened_editor_tree}");

    visual.simulate_keystrokes("!");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let dirty_tree = visual.update(|_, cx| {
        view_edit.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(dirty_tree.contains("files-save"), "Save button must appear after typing in block editor: {dirty_tree}");

    // Down arrow at end of heading block seamlessly commits and moves cursor into next block.
    visual.simulate_keystrokes("down");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let after_down_tree = visual.update(|_, cx| {
        view_edit.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(after_down_tree.contains("# Title!"), "Down arrow must commit previous block edit: {after_down_tree}");
    assert!(after_down_tree.contains("files-md-block-editor"), "Down arrow must enter editing on next block: {after_down_tree}");

    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let committed_tree = visual.update(|_, cx| {
        view_edit.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(!committed_tree.contains("files-md-block-editor"), "Escape must commit and close block editor: {committed_tree}");
    assert!(committed_tree.contains("# Title!"), "committed block edit missing updated heading: {committed_tree}");

    visual.simulate_click(point(px(300.), px(18.)), Modifiers::default());
    visual.simulate_keystrokes("ctrl-s");
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));
    let saved_tree = visual.update(|_, cx| {
        view_edit.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert_eq!(
        calls_edit.borrow().writes,
        vec![(
            "/fixture/project/README.md".to_owned(),
            "# Title!\n\n- [ ] Ship images\n\nBody paragraph.\n".to_owned(),
            json!(initial_md),
            Value::Null,
        )],
        "unexpected local writes: {:?}",
        calls_edit.borrow().writes,
    );
    assert!(!saved_tree.contains("files-save"), "Save button should disappear after Ctrl+S save: {saved_tree}");
    assert!(calls_edit.borrow().unexpected.is_empty(), "unexpected host calls: {:?}", calls_edit.borrow().unexpected);

    visual.update(|window, _| window.remove_window());
    gpui_shell::policy::set_default(Policy::new());
    cx.run_until_parked();

    // 4. Remote / WSL workspace editing in Markdown Preview passes { on: "remote" } to writeTextFile.
    let mut remote_values = fixture(true, json!({ "file": "README.md" }), updates("current", "current"));
    let program = json!({"key":"pi", "name":"Pi", "icon":null, "tint":true, "tile":"#334455", "color":"#aabbcc", "shell":false, "agent":true});
    remote_values.insert("spaces".into(), json!([
        {"id":"s1","index":1,"name":"Local","folder":"/fixture/project","machine":null,"branch":"main","active":false,"status":"working","tabs":[{"id":"tab1","index":1,"name":"Build","active":true,"status":"working","panes":1,"zoomed":false,"settled":false,"settled_at":null}]},
        {"id":"s2","index":2,"name":"Remote","folder":"/remote/project","machine":"remote","branch":null,"active":true,"status":"done","tabs":[{"id":"tab2","index":1,"name":"Done","active":true,"status":"done","panes":1,"zoomed":false,"settled":false,"settled_at":null}]}
    ]));
    remote_values.insert("panes".into(), json!([
        {"pane":"p2","terminal":"t2","tab":"tab2","title":"Remote task","cwd":"/remote/project","branch":null,"status":"done","app":"pi","program":program,"focused":true,"zoomed":false}
    ]));
    remote_values.insert("focused".into(), json!({"space":"s2","tab":"tab2","pane":"p2","terminal":"t2","agent":"t2","shown":null,"cwd":"/remote/project"}));

    let calls_remote = Rc::new(RefCell::new(Calls::default()));
    calls_remote.borrow_mut().files.insert("/remote/project/README.md".into(), initial_md.into());
    calls_remote.borrow_mut().shared.insert("files.treeOpen".into(), json!(false));
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-files-remote.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(remote_values, calls_remote.clone()))
            .unwrap(),
    );
    let runtime_remote = ShellRuntime::new_isolated().unwrap();
    let win_remote = cx.add_window(|window, cx| RootedView(runtime_remote.try_load(&staged.0.join("mods/files"), window, cx).unwrap()));
    let view_remote: gpui::Entity<gpui_shell::ScriptView> = win_remote
        .read_with(cx, |root, cx| root.0.read(cx).content().clone().downcast().unwrap())
        .unwrap();
    let mut remote_visual = VisualTestContext::from_window(*win_remote.deref(), cx);
    remote_visual.update(|window, cx| window.draw(cx).clear(cx));
    remote_visual.run_until_parked();
    remote_visual.update(|window, cx| window.draw(cx).clear(cx));

    remote_visual.simulate_click(point(px(80.), px(60.)), Modifiers::default());
    remote_visual.run_until_parked();
    remote_visual.update(|window, cx| window.draw(cx).clear(cx));
    let remote_editor_tree = remote_visual.update(|_, cx| {
        view_remote.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(remote_editor_tree.contains("files-md-block-editor"), "remote workspace click must open block editor: {remote_editor_tree}");

    remote_visual.simulate_keystrokes("!");
    remote_visual.simulate_keystrokes("escape");
    remote_visual.run_until_parked();
    remote_visual.update(|window, cx| window.draw(cx).clear(cx));
    remote_visual.simulate_click(point(px(300.), px(18.)), Modifiers::default());
    remote_visual.simulate_keystrokes("ctrl-s");
    remote_visual.run_until_parked();
    remote_visual.update(|window, cx| window.draw(cx).clear(cx));
    assert_eq!(
        calls_remote.borrow().writes,
        vec![(
            "/remote/project/README.md".to_owned(),
            "# Title!\n\n- [ ] Ship images\n\nBody paragraph.\n".to_owned(),
            json!(initial_md),
            json!("remote"),
        )],
        "unexpected remote writes: {:?}",
        calls_remote.borrow().writes,
    );
    assert!(calls_remote.borrow().unexpected.is_empty(), "unexpected host calls: {:?}", calls_remote.borrow().unexpected);

    remote_visual.update(|window, _| window.remove_window());
    gpui_shell::policy::set_default(Policy::new());
    cx.run_until_parked();

    // 5. Source code file (.rs) is directly editable and savable, with Wrap/Open-in-Tab removed.
    let initial_rs = "fn main() {}\n";
    let calls_code = Rc::new(RefCell::new(Calls::default()));
    calls_code.borrow_mut().files.insert("/fixture/project/src/main.rs".into(), initial_rs.into());
    calls_code.borrow_mut().shared.insert("files.treeOpen".into(), json!(false));
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-files-code.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(fixture(true, json!({ "file": "src/main.rs" }), updates("current", "current")), calls_code.clone()))
            .unwrap(),
    );
    let runtime_code = ShellRuntime::new_isolated().unwrap();
    let win_code = cx.add_window(|window, cx| RootedView(runtime_code.try_load(&staged.0.join("mods/files"), window, cx).unwrap()));
    let view_code: gpui::Entity<gpui_shell::ScriptView> = win_code
        .read_with(cx, |root, cx| root.0.read(cx).content().clone().downcast().unwrap())
        .unwrap();
    let mut code_visual = VisualTestContext::from_window(*win_code.deref(), cx);
    code_visual.update(|window, cx| window.draw(cx).clear(cx));
    code_visual.run_until_parked();
    code_visual.update(|window, cx| window.draw(cx).clear(cx));

    let code_tree = code_visual.update(|_, cx| {
        view_code.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(code_tree.contains("files-code-view"), "source file missing files-code-view: {code_tree}");
    assert!(!code_tree.contains("files-wrap"), "source file toolbar must not have Wrap button: {code_tree}");
    assert!(!code_tree.contains("files-open-tab"), "source file toolbar must not have Open in Tab button: {code_tree}");

    code_visual.simulate_click(point(px(120.), px(80.)), Modifiers::default());
    code_visual.simulate_keystrokes("/");
    code_visual.run_until_parked();
    code_visual.update(|window, cx| window.draw(cx).clear(cx));
    let dirty_code_tree = code_visual.update(|_, cx| {
        view_code.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(dirty_code_tree.contains("files-save"), "Save button must appear after typing in source code editor: {dirty_code_tree}");

    code_visual.simulate_keystrokes("ctrl-s");
    code_visual.run_until_parked();
    code_visual.update(|window, cx| window.draw(cx).clear(cx));
    assert!(
        calls_code.borrow().writes.len() == 1
            && calls_code.borrow().writes[0].0 == "/fixture/project/src/main.rs"
            && calls_code.borrow().writes[0].1.contains('/'),
        "expected modified src/main.rs write with '/', got {:?}",
        calls_code.borrow().writes,
    );
    assert!(calls_code.borrow().unexpected.is_empty(), "unexpected host calls: {:?}", calls_code.borrow().unexpected);

    code_visual.update(|window, _| window.remove_window());
    gpui_shell::policy::set_default(Policy::new());
    cx.run_until_parked();

    // 6. Binary image preview renders files-image-view via readImageDataUrl.
    let calls_img = Rc::new(RefCell::new(Calls::default()));
    calls_img.borrow_mut().images.insert("/fixture/project/hero.png".into(), data_png.into());
    let policy_img = Policy::new()
        .with_storage_path(staged.0.join("store-files-img.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(fixture(true, json!({ "file": "hero.png" }), updates("current", "current")), calls_img.clone()))
        .unwrap();
    let runtime_img = ShellRuntime::new_isolated().unwrap();
    let win_img = cx.add_window(|_, _| Empty);
    let mut sess_img = win_img
        .update(cx, |_, window, cx| {
            let mut session = DescriptionSession::load(runtime_img, staged.0.join("mods/files"), policy_img, window, cx).unwrap();
            let _ = session.describe(window, cx).unwrap();
            session
        })
        .unwrap();
    cx.run_until_parked();
    let img_tree = win_img.update(cx, |_, window, cx| sess_img.describe(window, cx).unwrap()).unwrap();
    win_img.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();

    assert!(calls_img.borrow().unexpected.is_empty(), "unexpected host calls: {:?}", calls_img.borrow().unexpected);
    assert!(img_tree.contains("files-image-view"), "binary image preview missing files-image-view: {img_tree}");
    assert!(img_tree.contains("PNG"), "binary image preview missing PNG badge: {img_tree}");

    // 7. Typora direct visual table cell editing, fenced code block card, and debounced auto-save.
    let spec_md = "| Name | Role |\n| :--- | ---: |\n| Alice | Dev |\n\n```rust\nfn demo() {}\n```\n";
    let calls_typora = Rc::new(RefCell::new(Calls::default()));
    calls_typora.borrow_mut().files.insert("/fixture/project/SPEC.md".into(), spec_md.into());
    calls_typora.borrow_mut().shared.insert("files.treeOpen".into(), json!(false));
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-files-typora.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(fixture(true, json!({ "file": "SPEC.md" }), updates("current", "current")), calls_typora.clone()))
            .unwrap(),
    );
    let runtime_typora = ShellRuntime::new_isolated().unwrap();
    let win_typora = cx.add_window(|window, cx| RootedView(runtime_typora.try_load(&staged.0.join("mods/files"), window, cx).unwrap()));
    let view_typora: gpui::Entity<gpui_shell::ScriptView> = win_typora
        .read_with(cx, |root, cx| root.0.read(cx).content().clone().downcast().unwrap())
        .unwrap();
    let mut typora_visual = VisualTestContext::from_window(*win_typora.deref(), cx);
    typora_visual.update(|window, cx| window.draw(cx).clear(cx));
    typora_visual.run_until_parked();
    typora_visual.update(|window, cx| window.draw(cx).clear(cx));

    let typora_init_tree = typora_visual.update(|_, cx| {
        view_typora.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    for expected in [
        "files-md-table-0",
        "files-md-cell-0-h-0",
        "files-md-cell-0-0-1",
        "files-md-code-block-1",
        "files-md-code-lang-1",
        "files-md-code-body-1",
    ] {
        assert!(typora_init_tree.contains(expected), "Typora preview missing {expected}: {typora_init_tree}");
    }

    // Click header cell (0, h, 0) ("Name"), type "!", wait for debounced auto-save without pressing Ctrl+S.
    typora_visual.simulate_click(point(px(80.), px(64.)), Modifiers::default());
    typora_visual.run_until_parked();
    typora_visual.update(|window, cx| window.draw(cx).clear(cx));
    let cell_open_tree = typora_visual.update(|_, cx| {
        view_typora.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(cell_open_tree.contains("files-md-table-cell-editor"), "clicking table cell must open inline input: {cell_open_tree}");

    typora_visual.simulate_keystrokes("!");
    typora_visual.run_until_parked();
    typora_visual.executor().advance_clock(std::time::Duration::from_millis(1000));
    typora_visual.run_until_parked();
    typora_visual.update(|window, cx| window.draw(cx).clear(cx));
    let autosaved_tree = typora_visual.update(|_, cx| {
        view_typora.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(autosaved_tree.contains("files-md-table-cell-editor"), "debounced auto-save must keep active cell editor open: {autosaved_tree}");
    assert_eq!(calls_typora.borrow().writes.len(), 1, "expected 1 debounced auto-save write: {:?}", calls_typora.borrow().writes);
    assert!(calls_typora.borrow().writes[0].1.contains("Name!"), "auto-saved table missing 'Name!': {:?}", calls_typora.borrow().writes);

    // Click fenced code block body below the table to open direct code-body editor (without raw ``` markers).
    typora_visual.simulate_click(point(px(120.), px(175.)), Modifiers::default());
    typora_visual.run_until_parked();
    typora_visual.update(|window, cx| window.draw(cx).clear(cx));
    let code_open_tree = typora_visual.update(|_, cx| {
        view_typora.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(code_open_tree.contains("files-md-code-editor"), "clicking code block body must open inline code editor: {code_open_tree}");
    assert!(calls_typora.borrow().unexpected.is_empty(), "unexpected host calls: {:?}", calls_typora.borrow().unexpected);

    typora_visual.update(|window, _| window.remove_window());
    gpui_shell::policy::set_default(Policy::new());
    cx.run_until_parked();

    // 8. Multi-file switching: same-language EditorState reuse and immediate cached Markdown restoration.
    let calls_switch = Rc::new(RefCell::new(Calls::default()));
    {
        let mut f = calls_switch.borrow_mut();
        f.files.insert("/fixture/project/README.md".into(), initial_md.into());
        f.files.insert("/fixture/project/src/main.rs".into(), "fn main() {}\n".into());
        f.files.insert("/fixture/project/src/lib.rs".into(), "pub fn lib_fn() -> usize { 42 }\n".into());
        f.shared.insert("files.treeOpen".into(), json!(false));
    }
    let switch_dir = staged.0.clone();
    std::fs::write(
        switch_dir.join("main.js"),
        r#"import Real from './mods/files/main.js';
import { div } from 'gpui-kit';
export default class SwitchTest extends Real {
  init(props, cx) {
    super.init(props, cx);
    this.trackedFile = null;
    this.trackedCode = null;
    this.codeReusedCount = 0;
  }
  render(cx) {
    const out = super.render(cx);
    if (this.file && !this.loadingFile && !this.fileError) {
      if (this.trackedFile !== this.file) {
        if (this.trackedFile && this.trackedCode && this.code && this.trackedCode === this.code) {
          this.codeReusedCount++;
        }
        this.trackedFile = this.file;
        this.trackedCode = this.code;
      } else if (!this.trackedCode && this.code) {
        this.trackedCode = this.code;
      }
    }
    return div()
      .size_full()
      .id(`switch-reused-${this.codeReusedCount}`)
      .on_key_down((e, cx) => {
        if (e.key === "1") this.show("README.md", cx);
        else if (e.key === "2") this.show("src/main.rs", cx);
        else if (e.key === "3") this.show("src/lib.rs", cx);
      })
      .child(out);
  }
}
"#,
    )
    .unwrap();
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-files-switch.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(fixture(true, json!({ "file": "README.md" }), updates("current", "current")), calls_switch.clone()))
            .unwrap(),
    );
    let runtime_switch = ShellRuntime::new_isolated().unwrap();
    let win_switch = cx.add_window(|window, cx| RootedView(runtime_switch.try_load(&switch_dir, window, cx).unwrap()));
    let view_switch: gpui::Entity<gpui_shell::ScriptView> = win_switch
        .read_with(cx, |root, cx| root.0.read(cx).content().clone().downcast().unwrap())
        .unwrap();
    let mut switch_visual = VisualTestContext::from_window(*win_switch.deref(), cx);
    switch_visual.update(|window, cx| window.draw(cx).clear(cx));
    switch_visual.run_until_parked();
    switch_visual.update(|window, cx| window.draw(cx).clear(cx));

    // Focus the panel cleanly, then switch to src/main.rs -> src/lib.rs (same Rust language).
    switch_visual.simulate_click(point(px(20.), px(20.)), Modifiers::default());
    switch_visual.simulate_keystrokes("2");
    switch_visual.run_until_parked();
    switch_visual.update(|window, cx| window.draw(cx).clear(cx));
    let main_tree = switch_visual.update(|_, cx| {
        view_switch.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(main_tree.contains("text \"main.rs\""), "expected main.rs in tree: {main_tree}");
    assert!(main_tree.contains("files-code-view"), "expected files-code-view for src/main.rs: {main_tree}");
    assert!(!main_tree.contains("Loading file"), "must not flash loading indicator: {main_tree}");

    switch_visual.simulate_keystrokes("3");
    switch_visual.run_until_parked();
    switch_visual.update(|window, cx| window.draw(cx).clear(cx));
    let lib_tree = switch_visual.update(|_, cx| {
        view_switch.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(lib_tree.contains("text \"lib.rs\""), "expected lib.rs in tree: {lib_tree}");
    assert!(lib_tree.contains("files-code-view"), "expected files-code-view for src/lib.rs: {lib_tree}");
    assert!(lib_tree.contains("switch-reused-1"), "expected EditorState reuse across Rust files: {lib_tree}");

    // Switching back to cached README.md updates synchronously on the first draw before async tasks run.
    switch_visual.simulate_keystrokes("1");
    switch_visual.update(|window, cx| window.draw(cx).clear(cx));
    let cached_md_tree = switch_visual.update(|_, cx| {
        view_switch.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(cached_md_tree.contains("files-md-view"), "cached Markdown must render on first frame: {cached_md_tree}");
    assert!(cached_md_tree.contains("# Title"), "cached Markdown content missing on first frame: {cached_md_tree}");
    assert!(!cached_md_tree.contains("Loading file"), "cached Markdown switch must not flash Loading: {cached_md_tree}");

    switch_visual.run_until_parked();
    assert!(calls_switch.borrow().unexpected.is_empty(), "unexpected host calls: {:?}", calls_switch.borrow().unexpected);
    assert!(calls_switch.borrow().reads.iter().any(|p| p.ends_with("src/main.rs")), "expected read of src/main.rs");
    assert!(calls_switch.borrow().reads.iter().any(|p| p.ends_with("src/lib.rs")), "expected read of src/lib.rs");

    switch_visual.update(|window, _| window.remove_window());
    gpui_shell::policy::set_default(Policy::new());
    cx.run_until_parked();
}

#[gpui::test]
fn side_panel_multiple_side_chats_and_handoff_dialog_render_cleanly(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let claude = json!({"key":"claude", "name":"Claude Code", "icon":null, "tint":false, "tile":"#1c1d21", "color":"#ffffff", "shell":false, "agent":true});
    let codex = json!({"key":"codex", "name":"Codex", "icon":null, "tint":false, "tile":"#1c1d21", "color":"#ffffff", "shell":false, "agent":true});
    let mut values = fixture(true, json!({"sidePanel": true}), updates("current", "current"));
    values.insert("checking".into(), json!(true));
    values.insert("shared".into(), json!({"side-chat.active": "wsl/a4j"}));
    values.insert("terminals".into(), json!([
        {"address":"wsl/t2t","parent":null,"state":"live","status":"idle","program":claude,"cwd":"/tmp/test","labels":{}},
        {"address":"wsl/a4h","acp":true,"agent":"claude","parent":"wsl/t2t","lineage":{"kind":"side-chat","parent":"wsl/t2t"},"state":"live","status":"idle","turn":1,"program":claude,"cwd":"/tmp/test","labels":{}},
        {"address":"wsl/a4j","acp":true,"agent":"codex","parent":"wsl/t2t","lineage":{"kind":"side-chat","parent":"wsl/t2t"},"state":"live","status":"idle","turn":1,"program":codex,"cwd":"/tmp/test","labels":{}}
    ]));
    values.insert("panes".into(), json!([
        {"pane":"p1","tab":"tab1","terminal":"wsl/t2t","title":"Claude","status":"idle","program":claude,"focused":true}
    ]));
    values.insert("agents".into(), json!([
        {"address":"wsl/t2t","pane":"p1","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Claude","status":"idle","program":claude,"title":"Claude","seq":1,"focused":true}
    ]));

    // Render side-chat panel with 2 live side-chats, expanded handoff context card, AND a completed user+assistant turn.
    let tree = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        values.clone(),
        Some("this.loadedTerminal = 'wsl/a4j'; this.expandedGroups['ho-exp-ho-0'] = true; this.entries = [{kind:'handoff',turn:0,handoff_kind:'side-chat',from_address:'wsl/t2t',from_provider:'claude',from_turns:'1-3',context_markdown:'# Context\\nTurn 1'},{kind:'user',turn:1,text:'check?'},{kind:'assistant',turn:1,text:'all good'}]; this.loadedOnce = true;"),
    ).unwrap();
    assert!(tree.contains("side-chat-switcher"), "expected side-chat switcher with 2 side-chats: {tree}");
    assert!(tree.contains("side-sw-wsl/a4h"), "expected first side-chat pill: {tree}");
    assert!(tree.contains("side-sw-wsl/a4j"), "expected second side-chat pill: {tree}");
    assert!(tree.contains("acp-ho-toggle-ho-0"), "expected expandable handoff context card: {tree}");
    assert!(tree.contains("acp-ho-md-ho-0"), "expected expanded handoff markdown view: {tree}");
    assert!(!tree.contains("acp-side-new"), "top + New button must be removed from side-chat header: {tree}");
    assert!(!tree.contains("acp-fork-turn-"), "side-chat must not expose assistant fork button: {tree}");
    assert!(!tree.contains("acp-fork-from-"), "side-chat must not expose user fork button: {tree}");

    // Render CLI side-chat terminal header AND CLI side-chat structured chat view toggle buttons.
    let mut cli_values = values.clone();
    cli_values.insert("shared".into(), json!({"side-chat.active": "wsl/t30"}));
    cli_values.insert("terminals".into(), json!([
        {"address":"wsl/t2t","parent":null,"state":"live","status":"idle","program":claude,"cwd":"/tmp/test","labels":{}},
        {"address":"wsl/a4h","acp":true,"agent":"claude","parent":"wsl/t2t","lineage":{"kind":"side-chat","parent":"wsl/t2t"},"state":"live","status":"idle","turn":1,"program":claude,"cwd":"/tmp/test","labels":{}},
        {"address":"wsl/t30","acp":false,"parent":"wsl/t2t","lineage":{"kind":"side-chat","parent":"wsl/t2t"},"state":"live","status":"idle","program":codex,"cwd":"/tmp/test","labels":{}}
    ]));
    let mut cli_hdr_values = cli_values.clone();
    cli_hdr_values.insert("props".into(), json!({"sidePanel": true, "sideTermHeader": true}));
    let cli_hdr_tree = describe_with_values(cx, &staged, "mods/peek", cli_hdr_values, None).unwrap();
    assert!(cli_hdr_tree.contains("side-term-header"), "CLI side-chat terminal mode should render compact header: {cli_hdr_tree}");
    assert!(cli_hdr_tree.contains("side-to-chat"), "CLI side-chat terminal mode should expose switch-to-chat button: {cli_hdr_tree}");
    assert!(cli_hdr_tree.contains("side-chat-switcher"), "CLI side-chat terminal header should include multi-side switcher: {cli_hdr_tree}");
    assert!(!cli_hdr_tree.contains("acp-side-new"), "CLI side-chat terminal header must not have + New button: {cli_hdr_tree}");

    let cli_chat_tree = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        cli_values,
        Some("this.loadedTerminal = 'wsl/t30'; this.entries = [{kind:'user',turn:1,text:'hi'},{kind:'assistant',turn:1,text:'yo'}]; this.loadedOnce = true;"),
    ).unwrap();
    assert!(cli_chat_tree.contains("side-to-term"), "CLI side-chat chat view should expose switch-back-to-terminal button: {cli_chat_tree}");
    assert!(!cli_chat_tree.contains("acp-side-new"), "CLI side-chat chat view must not have + New button: {cli_chat_tree}");
    assert!(!cli_chat_tree.contains("acp-fork-turn-"), "CLI side-chat chat view must not expose fork button: {cli_chat_tree}");

    // Render handoff dialog in side-chat mode and verify both ACP and CLI runtime chips exist and Merge Back tab is absent.
    let mut ho_values = values;
    ho_values.insert("props".into(), json!({"overlay":"handoff","source":"wsl/t2t","kind":"side-chat"}));
    let ho_tree = describe_with_values(cx, &staged, "mods/handoff", ho_values, None).unwrap();
    assert!(ho_tree.contains("ho-rt-acp"), "expected ACP option in side-chat dialog: {ho_tree}");
    assert!(ho_tree.contains("ho-rt-pty"), "expected CLI Terminal option in side-chat dialog: {ho_tree}");
    assert!(!ho_tree.contains("ho-mode-merge-back"), "merge-back tab must not appear in creation dialog: {ho_tree}");
}
