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

fn module(mut values: BTreeMap<String, Value>, calls: Rc<RefCell<Calls>>) -> HostModule {
    if let Some(Value::Object(map)) = values.remove("shared") {
        let mut s = calls.borrow_mut();
        for (k, v) in map {
            s.shared.insert(k, v);
        }
    }
    let mut module = HostModule::new("herdr");
    for (name, value) in values {
        module = module.function(name, move |_| Ok(to_host(&value)));
    }
    let shared = calls.clone();
    module = module.function("shared", move |a| {
        Ok(shared.borrow().shared.get(a.string(0)?).map(to_host).unwrap_or(HostValue::Null))
    });
    let shared = calls.clone();
    module = module.function("setShared", move |a| {
        shared.borrow_mut().shared.insert(a.string(0)?.into(), from_host(a.value(1)?));
        Ok(HostValue::Null)
    });
    for name in ["run", "playSound", "setAgentOrder", "setWindowTitle", "setBadge", "openOverlay", "closeOverlay", "notify"] {
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
    module = module.function("pasteClipboardAttachments", |_| Ok(HostValue::Array(vec![])));
    module = module.function("prepareAttachments", |a| {
        let paths = a
            .get(0)
            .map(from_host)
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let out: Vec<Value> = paths
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .map(|p| {
                let name = p.rsplit(['/', '\\']).next().unwrap_or(&p).to_string();
                let is_img = name.ends_with(".png") || name.ends_with(".jpg") || name.ends_with(".webp");
                if is_img {
                    json!({ "kind": "image", "path": p, "preview": null, "name": name, "mimeType": "image/png", "bytes": 2048 })
                } else {
                    json!({ "kind": "resource", "path": p, "name": name, "mimeType": "text/markdown", "bytes": 512 })
                }
            })
            .collect();
        Ok(to_host(&Value::Array(out)))
    });
    module = module.async_function("pickAttachments", |_| Ok(async { Ok(HostValue::Array(vec![])) }));
    module = module.async_function("knownAgents", |_| Ok(async { Ok(HostValue::Array(vec![])) }));
    module = module.async_function("pickFolder", |_| Ok(async { Ok(HostValue::Null) }));
    module = module.async_function("nextCommand", |_| Ok(std::future::pending::<gpui_shell::HostResult>()));
    module = module.cancellable_async_function("watchFolder", HostValue::Array(vec![]), |_| Ok(std::future::pending::<gpui_shell::HostResult>()));
    for name in ["exec", "hc", "h2", "readTextFile", "readImageDataUrl", "writeTextFile"] {
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
                "hc" | "h2" if first == json!(["ai", "status", "--json"]) => {
                    Some(json!({"code":0,"stdout":"{\"configured\":true,\"enabled\":true,\"model\":\"gpt-5.4\",\"baseUrl\":\"https://api.example.com/v1\"}","stderr":""}))
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
                "hc" | "h2" if first == json!(["read", "--json", "t_large_working"]) => {
                    let mut entries = Vec::new();
                    let mut ts = 1_795_250_000_000i64;
                    for turn in 1..=26 {
                        entries.push(json!({
                            "kind": "user",
                            "turn": turn,
                            "text": format!("Turn {turn}: inspect `herdr-app/defaults/mods/peek/main.js:120` and fix working CLI chat view rendering."),
                            "ts_ms": ts
                        }));
                        ts += 1000;
                        for step in 0..4 {
                            entries.push(json!({
                                "kind": "thought",
                                "turn": turn,
                                "text": format!("**Step {step}** Checking transcript rendering and QuickJS budget for turn {turn}."),
                                "ts_ms": ts
                            }));
                            entries.push(json!({
                                "id": "",
                                "kind": "tool_call",
                                "turn": turn,
                                "title": format!("read_file /home/zzh0838/Project/herdr2/herdr-app/defaults/mods/peek/main.js #{step}"),
                                "tool_kind": "read",
                                "status": "completed",
                                "detail": format!("line 1: fn sample_{turn}_{step}() {{}}\nline 2: return ok;\n"),
                                "ts_ms": ts + 100
                            }));
                            ts += 500;
                        }
                        if turn < 26 {
                            entries.push(json!({
                                "kind": "assistant",
                                "turn": turn,
                                "text": format!("Finished turn {turn} after inspecting `herdr-app/src/window.rs:1600` and `herdr-app/defaults/mods/peek/acp_model.js:900`.\n\n```js\nconst turn = {turn};\n```"),
                                "ts_ms": ts
                            }));
                        } else {
                            // Active working turn with many steps (defaultExpanded = true while working)
                            for extra in 0..45 {
                                entries.push(json!({
                                    "id": "",
                                    "kind": "tool_call",
                                    "turn": turn,
                                    "title": format!("run_command cargo test step_{extra}"),
                                    "tool_kind": "execute",
                                    "status": if extra == 44 { "in_progress" } else { "completed" },
                                    "detail": format!("cargo test --locked step_{extra}\npassed"),
                                    "ts_ms": ts + extra * 100
                                }));
                            }
                        }
                    }
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "complete": true,
                            "how": "transcript",
                            "session_meta": {
                                "model": "gemini-4-neon-high",
                                "provider": "agy",
                                "session_id": "test-large-working"
                            },
                            "entries": entries,
                            "text": "working..."
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
                "hc" | "h2" if first == json!(["read", "--json", "a_rb"]) => {
                    let rewound = calls.borrow().shared.contains_key("checkpoint.sync");
                    let entries = if rewound {
                        json!([
                            {"kind":"user","turn":1,"text":"Add add/sub","ts_ms":1700000000000i64},
                            {"kind":"assistant","turn":1,"text":"Implemented add and sub.","ts_ms":1700000001000i64},
                            {"kind":"rewind","to_turn":1,"restore_files":true,"ts_ms":1700000009000i64}
                        ])
                    } else {
                        json!([
                            {"kind":"user","turn":1,"text":"Add add/sub","ts_ms":1700000000000i64},
                            {"kind":"assistant","turn":1,"text":"Implemented add and sub.","ts_ms":1700000001000i64},
                            {"kind":"user","turn":2,"text":"Add trigonometry","ts_ms":1700000002000i64},
                            {"kind":"assistant","turn":2,"text":"Implemented sin and cos.","ts_ms":1700000003000i64}
                        ])
                    };
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "complete": true,
                            "how": "turn",
                            "entries": entries
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["get", "--json", "a_rb"]) => {
                    let rewound = calls.borrow().shared.contains_key("checkpoint.sync");
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "address": "a_rb",
                            "state": "live",
                            "turn": if rewound { 1 } else { 2 },
                            "cwd": "/fixture/project",
                            "config_options": []
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["read", "--json", "a_media"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "complete": true,
                            "how": "transcript",
                            "entries": [
                                {
                                    "kind": "user",
                                    "turn": 1,
                                    "text": "Check this screenshot and spec",
                                    "attachments": [
                                        {"kind": "image", "mime_type": "image/png", "name": "bug-shot.png", "path": "/fixture/files/bug-shot.png", "bytes": 2048},
                                        {"kind": "resource", "mime_type": "text/markdown", "name": "spec.md", "path": "/fixture/files/spec.md", "text": "# Spec\nFix header overlap.", "bytes": 128}
                                    ],
                                    "ts_ms": 1700000000000i64
                                },
                                {
                                    "kind": "tool_call",
                                    "turn": 1,
                                    "id": "tc-shot",
                                    "title": "Capture UI preview",
                                    "tool_kind": "read",
                                    "status": "completed",
                                    "attachments": [
                                        {"kind": "image", "mime_type": "image/png", "name": "after.png", "path": "/fixture/files/after.png", "bytes": 4096}
                                    ],
                                    "ts_ms": 1700000001000i64
                                },
                                {
                                    "kind": "assistant",
                                    "turn": 1,
                                    "text": "Updated layout and verified screenshot.",
                                    "attachments": [
                                        {"kind": "image", "mime_type": "image/png", "name": "verified.png", "path": "/fixture/files/verified.png", "bytes": 3072},
                                        {"kind": "resource_link", "mime_type": "text/html", "name": "Design doc", "uri": "https://example.com/design"}
                                    ],
                                    "ts_ms": 1700000002000i64
                                }
                            ]
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["get", "--json", "a_media"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "address": "a_media",
                            "state": "live",
                            "turn": 1,
                            "cwd": "/fixture/project",
                            "prompt_capabilities": {"image": true, "audio": false, "embeddedContext": true},
                            "config_options": []
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["read", "--json", "a_hist"]) || first == json!(["read", "--json", "t_hist"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "complete": true,
                            "entries": [
                                {"kind":"user","turn":1,"text":"first prompt","ts_ms":1700000000000i64},
                                {"kind":"assistant","turn":1,"text":"Done 1","ts_ms":1700000001000i64},
                                {"kind":"user","turn":2,"text":"multi line one\nmulti line two","ts_ms":1700000002000i64},
                                {"kind":"assistant","turn":2,"text":"Done 2","ts_ms":1700000003000i64},
                                {"kind":"user","turn":3,"text":"latest prompt","ts_ms":1700000004000i64},
                                {"kind":"assistant","turn":3,"text":"Done 3","ts_ms":1700000005000i64}
                            ]
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first == json!(["get", "--json", "a_hist"]) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "address": "a_hist",
                            "config_options": []
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
                "hc" | "h2" if first == json!(["inbox", "send", "--json", "a1", "102"]) => {
                    calls.borrow_mut().effects.push(("inbox.send".into(), vec![first.clone()]));
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "address": "a1",
                            "inbox_len": 1,
                            "inbox": [
                                {"id": 101, "text": "First queued prompt", "queued_ms": 1700000000000i64}
                            ]
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first.as_array().is_some_and(|arr| arr.first().and_then(Value::as_str) == Some("ai") && arr.get(1).and_then(Value::as_str) == Some("polish-prompt")) => {
                    calls.borrow_mut().effects.push(("h2.polish".into(), vec![first.clone()]));
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "usedAi": true,
                            "polished": "Polished prompt with clear constraints",
                            "summary": "Clarified intent and constraints"
                        })).unwrap(),
                        "stderr": ""
                    }))
                }
                "hc" | "h2" if first.as_array().is_some_and(|arr| arr.first().and_then(Value::as_str) == Some("send") && arr.get(1).and_then(Value::as_str) == Some("--json")) => {
                    calls.borrow_mut().effects.push(("h2.send".into(), vec![first.clone()]));
                    Some(json!({"code":0,"stdout":"{\"status\":\"working\",\"turn\":2,\"started\":true,\"inbox_len\":0}","stderr":""}))
                }
                "hc" | "h2" if first == json!(["interrupt", "--json", "a1"]) => {
                    calls.borrow_mut().interrupted.push("a1".into());
                    Some(json!({"code":0,"stdout":"{\"outcome\":\"stopped\"}","stderr":""}))
                }
                "hc" | "h2" if first == json!(["interrupt", "--json", "t1"]) => {
                    calls.borrow_mut().interrupted.push("t1".into());
                    Some(json!({"code":0,"stdout":"{\"outcome\":\"stopped\"}","stderr":""}))
                }
                "hc" | "h2" if first.as_array().is_some_and(|arr| arr.first().and_then(Value::as_str) == Some("drove") && arr.get(1).and_then(Value::as_str) == Some("settings")) => {
                    Some(json!({
                        "code": 0,
                        "stdout": serde_json::to_string(&json!({
                            "agents": {
                                "agents": [
                                    {
                                        "id": "claude",
                                        "installed": true,
                                        "models": ["claude-opus-4-7", "claude-sonnet-4-6", "opus", "sonnet"],
                                        "defaultModel": "claude-opus-4-7",
                                        "efforts": ["low", "medium", "high", "xhigh", "max"],
                                        "defaultEffort": "high",
                                        "modelEfforts": {}
                                    },
                                    {
                                        "id": "codex",
                                        "installed": true,
                                        "models": ["gpt-5.4", "gpt-5.3-codex"],
                                        "defaultModel": "gpt-5.4",
                                        "efforts": ["low", "medium", "high", "xhigh"],
                                        "defaultEffort": "high",
                                        "modelEfforts": {}
                                    },
                                    {
                                        "id": "agy",
                                        "installed": true,
                                        "models": [
                                            "gemini-4-carbon-high",
                                            "gemini-4-carbon-medium",
                                            "gemini-4-carbon-low",
                                            "gemini-4-neon-xhigh",
                                            "gemini-4-neon-high",
                                            "gemini-4-neon-medium",
                                            "gemini-4-neon-low",
                                            "gemini-pro-orchestrator"
                                        ],
                                        "defaultModel": "gemini-4-carbon-high",
                                        "efforts": ["low", "medium", "high", "xhigh"],
                                        "defaultEffort": "high",
                                        "modelEfforts": {
                                            "gemini-4-carbon": ["low", "medium", "high"],
                                            "gemini-4-neon": ["low", "medium", "high", "xhigh"]
                                        }
                                    }
                                ]
                            }
                        })).unwrap(),
                        "stderr": ""
                    }))
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
                            json!(["rev-parse", "--show-toplevel", "--show-prefix"]),
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
    p["detail"] = json!("Worktree warning");
    p["items"] = json!([
        {
            "address": "wsl/a2",
            "name": "fork-one",
            "branch": "herdr/fork-fk1",
            "path": "/home/u/.local/state/herdr2/worktrees/forks/repo-fk1",
            "space_name": "repo-fk1",
            "live": true
        },
        {
            "address": "wsl/a3",
            "name": "fork-two",
            "branch": "herdr/fork-fk2",
            "path": "/home/u/.local/state/herdr2/worktrees/forks/repo-fk2",
            "space_name": "repo-fk2",
            "live": false
        }
    ]);
    p["command"] = json!({"name": "agent.empty_trash", "args": {"confirmed": true}});
    let tree = describe(cx, &staged, "mods/confirm", true, p, updates("current", "current"), None).unwrap();
    let root = tree.lines().find(|l| l.contains(":id[Str(\"confirm\")]")).unwrap();
    assert!(root.contains(".rounded[Number(8.0)]"), "the dialog background must follow the frame's corners: {root}");
    assert!(!root.contains(".size_full"), "fit-content confirm dialog must not use size_full: {root}");
    let detail = tree.lines().find(|l| l.contains(":id[Str(\"confirm-detail\")]")).unwrap();
    assert!(!detail.contains(".overflow_y_scrollbar"), "fit-content detail must not collapse via flex_1 scroll container: {detail}");
    assert!(tree.contains("fork-one"), "confirm dialog must render first forked agent name: {tree}");
    assert!(tree.contains("WT · herdr/fork-fk1"), "confirm dialog must render first branch badge: {tree}");
    assert!(tree.contains("/home/u/.local/state/herdr2/worktrees/forks/repo-fk1"), "confirm dialog must render first path: {tree}");
    assert!(tree.contains("fork-two"), "confirm dialog must render second forked agent name: {tree}");
    assert!(tree.contains("WT · herdr/fork-fk2"), "confirm dialog must render second branch badge: {tree}");
    assert!(tree.contains("/home/u/.local/state/herdr2/worktrees/forks/repo-fk2"), "confirm dialog must render second path: {tree}");
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
        assert!(tree.contains("h2 machine add --install host"), "{tree}");
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
        assert!(tree.contains("st-s-keyboard"), "Shortcuts section must be visible on {platform}: {tree}");
        assert!(tree.contains("st-shortcuts"), "Shortcuts editor must render on {platform}: {tree}");
        assert!(tree.contains("st-key-global-prompt.polish"), "AI prompt polish shortcut row missing on {platform}: {tree}");
        assert!(tree.contains("st-key-global-prompt.polish_undo"), "Undo AI polish shortcut row missing on {platform}: {tree}");
        assert!(tree.contains("st-key-global-tab.new"), "New tab shortcut row missing on {platform}: {tree}");
        assert!(tree.contains("st-key-global-terminal.open"), "New terminal shortcut row missing on {platform}: {tree}");
        assert!(tree.contains("st-key-global-mod.open:flash-note"), "Flash note shortcut row missing on {platform}: {tree}");
        assert!(tree.contains("st-keys-reset-all"), "Reset all shortcuts button missing on {platform}: {tree}");
    }
}

#[gpui::test]
fn settings_sections_and_updates(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    for section in ["look", "keyboard", "machines", "ai", "drove", "alerts", "updates"] {
        let mut p = props();
        p["section"] = json!(section);
        let tree = describe(cx, &staged, "frame/settings", true, p, updates("current", "current"), None).unwrap();
        assert!(!tree.contains("st-s-privacy") && !tree.contains("st-telemetry"), "upstream telemetry privacy tab must stay removed in {section}: {tree}");
        assert!(tree.contains("st-s-updates"), "self-hosted updates tab should render in {section}: {tree}");
    }
    let mut p = props();
    p["section"] = json!("updates");
    let mut up_state = updates("ready", "ready");
    up_state["last_error"] = json!("curl: (7) Failed to connect");
    up_state["machines"] = json!([
        {"name":"linux-ok","target":"dev@10.0.0.1","version":"0.1.6","state":"connected","raw_state":"connected","why":null,"ready":null,"needs_update":false,"unreachable":false,"updating":false,"error":null},
        {"name":"linux-down","target":"dev@10.0.0.2","version":"0.1.4","state":"needs_update","raw_state":"unreachable","why":"ssh timed out","ready":"0.1.6","needs_update":true,"unreachable":true,"updating":false,"error":null}
    ]);
    let up = describe(cx, &staged, "frame/settings", true, p, up_state, None).unwrap();
    assert!(
        up.contains("st-check-updates")
            && up.contains("st-ai-update")
            && up.contains("st-ai-update-retry")
            && up.contains("st-releases-url")
            && up.contains("st-update-hosts")
            && up.contains("st-update-host-local")
            && up.contains("st-update-host-linux-ok")
            && up.contains("st-update-host-linux-down")
            && up.contains("needs_update")
            && !up.contains("st-ui-mode-auto")
            && !up.contains("st-gh-releases-url"),
        "updates section should render check button, AI update buttons, URL input, and local/remote host versions with needs_update: {up}"
    );
    let mut p = props();
    p["section"] = json!("privacy");
    let fallback = describe(cx, &staged, "frame/settings", true, p, updates("current", "current"), None).unwrap();
    assert!(!fallback.contains("st-s-privacy") && !fallback.contains("st-telemetry"), "removed privacy section must fall back cleanly: {fallback}");
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
fn local_state_and_overlay_initial_descriptions(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let start = Instant::now();
    let staged = Staged::new();
    for page in 1..=5 {
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
        let expected = [
            "Terminals and agents that stay alive in the background",
            "Choose your panels",
            "Core AI & other machines",
            "Detect agents & configure ACP",
            "Make it yours & h2 CLI",
        ][page - 1];
        assert!(tree.contains(expected), "welcome page {page}: {tree}");
        assert!(tree.contains("Welcome to Herdr2"), "welcome header on page {page}: {tree}");
        if page == 3 {
            assert!(tree.contains("DeepSeek Official"), "welcome page 3 DeepSeek provider: {tree}");
            assert!(tree.contains("Custom (OpenAI / Claude)"), "welcome page 3 Custom OpenAI/Claude provider: {tree}");
            assert!(tree.contains("Fetch Models"), "welcome page 3 Fetch Models button: {tree}");
            assert!(tree.contains("Skip"), "welcome page 3 Skip button: {tree}");
            assert!(!tree.contains("deepseek-reasoner"), "welcome page 3 must not hardcode deepseek-reasoner chip: {tree}");
            assert!(!tree.contains("deepseek-chat"), "welcome page 3 must not default model to deepseek-chat: {tree}");
        }
        if page == 4 {
            assert!(tree.contains("w-agents-card"), "welcome page 4 Agents & ACP detection card: {tree}");
            assert!(tree.contains("w-acp-config-card"), "welcome page 4 ACP configuration card: {tree}");
            assert!(tree.contains("Auto-install managed ACP adapters"), "welcome page 4 auto-install option: {tree}");
            assert!(tree.contains("Default ACP permissions"), "welcome page 4 default permissions option: {tree}");
            assert!(tree.contains("Auto-inject herdr2 MCP server"), "welcome page 4 inject-mcp option: {tree}");
        }
    }
    let zh_tree = describe(
        cx,
        &staged,
        "frame/welcome",
        true,
        props(),
        updates("current", "current"),
        Some("this.languageOverride = 'zh-CN'; this.page = 1;"),
    )
    .unwrap();
    assert!(zh_tree.contains("欢迎使用 Herdr2"), "welcome zh-CN header: {zh_tree}");
    assert!(zh_tree.contains("真终端与无头智能体，关闭窗口也在后台运行"), "welcome zh-CN hero: {zh_tree}");
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
    let mut wt_remove_props = props();
    wt_remove_props
        .as_object_mut()
        .unwrap()
        .extend(json!({"space":"s1","action":"remove"}).as_object().unwrap().clone());
    let wt_remove_tree = describe(
        cx,
        &staged,
        "mods/worktree",
        true,
        wt_remove_props,
        updates("current", "current"),
        Some(
            "this.repo = { top: '/home/u/.herdr/worktrees/repo/feat-demo', main: '/home/u/repo', name: 'repo', linked: true }; this.branch = 'feat/demo'; this.branchUnmerged = true; this.deleteBranch = true;",
        ),
    )
    .unwrap();
    assert!(
        wt_remove_tree.contains("Also delete local branch") && wt_remove_tree.contains("feat/demo") && wt_remove_tree.contains("Unmerged"),
        "worktree remove dialog must render branch deletion toggle: {wt_remove_tree}"
    );
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
    assert!(tree.contains("acp-attach"), "composer attach button missing: {tree}");
}

#[gpui::test]
fn acp_peek_renders_multimodal_attachments_and_composer_drafts(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let program = json!({"key":"pi", "name":"Pi", "icon":null, "tint":true, "tile":"#334455", "color":"#aabbcc", "shell":false, "agent":true});
    let mut values = fixture(true, json!({"overlay":"o1","terminal":"a_media","acp":true}), updates("current", "current"));
    values.insert("terminals".into(), json!([
        {"address":"a_media","name":"Visual","title":"Visual","argv":["pi"],"parent":null,"labels":{},"state":"live","status":"idle","turn":1,"transcript_seq":3,"prompt_capabilities":{"image":true,"audio":false,"embeddedContext":true},"report":{"app":"pi"},"program":program,"cwd":"/fixture/project"}
    ]));
    values.insert("agents".into(), json!([
        {"address":"a_media","pane":"p1","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Visual","status":"idle","app":"pi","program":program,"title":"Visual","seq":1,"focused":true}
    ]));

    let data_png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAYAAAD0In+KAAAAEUlEQVR4nGMUMgn7z8DAwAAACQ0BnfdmzRUAAAAASUVORK5CYII=";
    let calls = Rc::new(RefCell::new(Calls::default()));
    calls.borrow_mut().images.insert("/fixture/files/bug-shot.png".into(), data_png.into());
    calls.borrow_mut().images.insert("/fixture/files/verified.png".into(), data_png.into());
    calls.borrow_mut().images.insert("/fixture/files/after.png".into(), data_png.into());

    let policy = Policy::new()
        .with_storage_path(staged.0.join("store-acp-media.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(values, calls.clone()))
        .unwrap();
    let runtime = ShellRuntime::new_isolated().unwrap();
    let root = staged.wrapper(
        "mods/peek",
        "this.handleDropFiles(['/tmp/dropped.png', '/tmp/report.md'], cx);",
    );
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
    assert!(tree.contains("acp-media-u-0-att-0") && tree.contains("bug-shot.png"), "user image card missing: {tree}");
    assert!(tree.contains("acp-res-u-0-att-1") && tree.contains("spec.md"), "user resource card missing: {tree}");
    assert!(tree.contains("acp-media-as-2-att-0") && tree.contains("verified.png"), "assistant image card missing: {tree}");
    assert!(tree.contains("acp-res-as-2-att-1") && tree.contains("Design doc"), "assistant resource_link card missing: {tree}");
    assert!(tree.contains("acp-draft-attachments"), "composer draft attachments bar missing: {tree}");
    assert!(tree.contains("dropped.png") && tree.contains("report.md"), "dropped draft attachments missing: {tree}");
    assert!(tree.contains("acp-attach"), "composer attach button missing: {tree}");
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
    assert!(tree.contains("acp-inbox-send-101") && tree.contains("acp-inbox-edit-101") && tree.contains("acp-inbox-rm-101"), "send-now/edit/remove buttons missing: {tree}");
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

    let after_send_now = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        values.clone(),
        Some("this.sendInboxItemNow(102, cx);"),
    ).unwrap();
    assert!(!after_send_now.contains("Second queued prompt"), "sent-now queued item still visible: {after_send_now}");
    assert!(after_send_now.contains("Input queue (1)"), "remaining count should drop to 1 after send now: {after_send_now}");

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
fn acp_peek_syncs_conversation_after_checkpoint_rollback_in_shared_runtime(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let sibling_dir = staged.0.join("mods/sibling-microtask");
    std::fs::create_dir_all(&sibling_dir).unwrap();
    std::fs::write(
        sibling_dir.join("main.js"),
        r#"
import { View, div } from "gpui-kit";
import { shared } from "herdr";
export default class Sibling extends View {
  render() {
    const sync = shared("checkpoint.sync") ?? "";
    // Queue a bare microtask before peek renders in the same shared QuickJS runtime.
    Promise.resolve().then(() => {});
    return div().child(`sibling:${sync}`);
  }
}
"#,
    )
    .unwrap();

    let program = json!({"key":"claude", "name":"Claude", "icon":null, "tint":false, "tile":"#1c1d21", "color":"#ffffff", "shell":false, "agent":true});
    let calls = Rc::new(RefCell::new(Calls::default()));

    let mut peek_values = fixture(true, json!({"terminal":"a_rb","acp":true,"tab":true,"pane":"p2"}), updates("current", "current"));
    peek_values.insert("terminals".into(), json!([
        {"address":"a_rb","acp":true,"name":"Claude","title":"Claude","argv":["claude-agent-acp"],"parent":null,"labels":{},"state":"live","status":"idle","turn":2,"transcript_seq":2,"checkpoint_seq":2,"report":{"app":"claude-code"},"program":program,"cwd":"/fixture/project"}
    ]));
    peek_values.insert("agents".into(), json!([
        {"address":"a_rb","acp":true,"pane":"p2","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Claude","status":"idle","app":"claude-code","program":program,"title":"Claude","seq":1,"focused":true}
    ]));
    let sibling_values = fixture(true, json!({}), updates("current", "current"));

    let peek_policy = Policy::new().with_host_module(module(peek_values, calls.clone())).unwrap();
    let sibling_policy = Policy::new().with_host_module(module(sibling_values, calls.clone())).unwrap();

    let runtime = ShellRuntime::new_isolated().unwrap();
    let window = cx.add_window(|_, _| Empty);

    let (mut sibling, mut peek) = window
        .update(cx, |_, window, cx| {
            let sibling = DescriptionSession::load(runtime.clone(), &sibling_dir, sibling_policy, window, cx).unwrap();
            let peek = DescriptionSession::load(runtime.clone(), &staged.0.join("mods/peek"), peek_policy, window, cx).unwrap();
            (sibling, peek)
        })
        .unwrap();
    cx.run_until_parked();

    let before = window
        .update(cx, |_, window, cx| {
            let _ = sibling.describe(window, cx).unwrap();
            peek.describe(window, cx).unwrap()
        })
        .unwrap();
    assert!(before.contains("Add trigonometry"), "initial turn 2 prompt missing: {before}");
    assert!(before.contains("Implemented sin and cos."), "initial turn 2 reply missing: {before}");

    // Simulate checkpoint rollback from mods/checkpoints updating shared("checkpoint.sync")
    // and refreshing both mods in the same frame (sibling renders first with empty props).
    calls.borrow_mut().shared.insert("checkpoint.sync".into(), json!("a_rb:1:1700000009999"));
    window
        .update(cx, |_, window, cx| {
            let _ = sibling.describe(window, cx).unwrap();
            let _ = peek.describe(window, cx).unwrap();
        })
        .unwrap();
    cx.run_until_parked();

    let after = window
        .update(cx, |_, window, cx| peek.describe(window, cx).unwrap())
        .unwrap();
    assert!(after.contains("Implemented add and sub."), "turn 1 reply should remain after rollback: {after}");
    assert!(after.contains("Restored to turn 1"), "rewind divider missing after rollback: {after}");
    assert!(!after.contains("Add trigonometry"), "stale turn 2 prompt still rendered after rollback: {after}");
    assert!(!after.contains("Implemented sin and cos."), "stale turn 2 reply still rendered after rollback: {after}");

    window.update(cx, |_, window, _| window.remove_window()).unwrap();
    cx.run_until_parked();
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
    let root = staged.wrapper("frame/welcome", "this.page = 2;");
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
        "welcome panels step must not reset workspace.panels to []"
    );

    let welcome_agents_root = staged.wrapper(
        "frame/welcome",
        "this.page = 4; this.aiSkipped = true; this.knownAgentsList = [{ key: 'claude', name: 'Claude Code', installed: true }, { key: 'codex', name: 'Codex', installed: true }]; this.adapterStatus = { autoInstall: true, defaultPermissions: 'approve-reads', injectMcp: true, adapters: [{ preset_id: 'claude', binary_name: 'claude-agent-acp', companion_cli: 'claude', companion_installed: true, installed: true, source: 'managed', state: 'ready', version: '0.9.0' }, { preset_id: 'codex', binary_name: 'codex-acp', companion_cli: 'codex', companion_installed: true, installed: false, source: 'missing', state: 'missing' }] }; this.agentProbe = { fullProbe: true, agents: [{ id: 'claude', installed: true, models: ['claude-opus-4-6'] }] };",
    );
    let welcome_agents_window = cx.add_window(|_, _| Empty);
    let welcome_agents_runtime = ShellRuntime::new_isolated().unwrap();
    let welcome_agents_policy = Policy::new()
        .with_host_module(module(fixture(true, props(), updates("current", "current")), calls.clone()))
        .unwrap();
    let welcome_agents = welcome_agents_window.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(welcome_agents_runtime, welcome_agents_root, welcome_agents_policy, window, cx).unwrap();
        let tree = session.describe(window, cx).unwrap();
        window.remove_window();
        tree
    }).unwrap();
    assert!(welcome_agents.contains("w-agents-page"), "Welcome page 4 must render Agents & ACP setup: {welcome_agents}");
    assert!(welcome_agents.contains("w-agents-install-missing"), "Welcome page 4 must offer installing missing adapters: {welcome_agents}");
    assert!(welcome_agents.contains("w-acp-config-card"), "Welcome page 4 must render ACP configuration card: {welcome_agents}");
    assert!(welcome_agents.contains("w-acp-auto-install"), "Welcome page 4 must render auto-install toggle: {welcome_agents}");
    assert!(welcome_agents.contains("w-acp-inject-mcp"), "Welcome page 4 must render inject-mcp toggle: {welcome_agents}");

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
        "Open AI Settings →",
        "st-dr-open-ai",
    ] {
        assert!(en_tree.contains(expected), "EN drove settings missing {expected}: {en_tree}");
    }
    assert!(!en_tree.contains("Send feedback"), "Send feedback should be suppressed: {en_tree}");

    // Dedicated top-level Settings -> AI tab renders model params, core semantic switches, and cache stats.
    let ai_en_tree = describe(
        cx,
        &staged,
        "frame/settings",
        true,
        json!({ "section": "ai" }),
        updates("current", "current"),
        Some("this.aiStatus = { enabled: true, configured: true, baseUrl: 'https://api.example.com/v1', model: 'gpt-4o-mini', effort: 'low', contextWindow: '64k', maxTokens: 1024, switches: { ptyStatusArbitration: true, notesDigest: true, promptCacheEnabled: true, redactSecrets: true }, usage: { totalCalls: 12, localCacheHits: 5, sessionsReused: 3, activeSessions: 1, inputTokens: 18000, cachedInputTokens: 14000, outputTokens: 900, cacheHitRatio: 0.777, promptCacheAutoDegraded: true, opMetrics: { pty_status: { calls: 6, localCacheHits: 4, failures: 0, parseErrors: 0, lowConfidenceRejects: 1, totalLatencyMs: 2400 } } } };"),
    )
    .unwrap();
    for expected in [
        "CORE AI ASSISTANT",
        "Reasoning Effort",
        "Context Size",
        "Max Output Tokens",
        "st-ai-eff-group",
        "st-drp-core-ai-context",
        "st-drp-core-ai-max-tokens",
        "PTY Terminal State Arbitration",
        "Child Note Error Digest",
        "Prompt KV-Cache Optimization",
        "st-ai-prompt-mode-group",
        "st-ai-pm-shared_prefix",
        "st-ai-pm-compact",
        "st-ai-pm-auto",
        "st-ai-pm-redetect",
        "Shared Prefix",
        "Compact",
        "Re-detect Automatically",
        "Outbound Secret Redaction",
        "st-ai-sw-redact",
        "KV Cache Hit Rate",
        "77.7%",
        "st-ai-cache-degraded",
        "st-ai-op-metrics",
        "pty_status",
    ] {
        assert!(ai_en_tree.contains(expected), "EN AI settings missing {expected}: {ai_en_tree}");
    }

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
    for expected in ["Drove", "控制面 AI 策略开关", "Verify 失败摘要", "交接上下文压缩", "死循环检测", "前往 AI 设置 →"] {
        assert!(zh_tree.contains(expected), "zh-CN drove settings missing {expected}: {zh_tree}");
    }
    assert!(!zh_tree.contains("编队"), "Settings should show English Drove in zh-CN: {zh_tree}");
    assert!(!zh_tree.contains("发送反馈"), "Send feedback should be suppressed in zh-CN: {zh_tree}");

    let mut ai_zh_values = fixture(true, json!({ "section": "ai" }), updates("current", "current"));
    ai_zh_values.get_mut("settings").unwrap()["language"] = json!("zh-CN");
    let ai_zh_tree = describe_with_values(
        cx,
        &staged,
        "frame/settings",
        ai_zh_values,
        Some("this.aiStatus = { enabled: true, configured: true, baseUrl: 'https://api.example.com/v1', model: 'gpt-4o-mini', effort: 'low', switches: { ptyStatusArbitration: true, notesDigest: true, promptCacheEnabled: true }, usage: { totalCalls: 4, localCacheHits: 2, sessionsReused: 1, activeSessions: 1, inputTokens: 4000, cachedInputTokens: 3000, outputTokens: 200, cacheHitRatio: 0.75 } };"),
    )
    .unwrap();
    for expected in [
        "核心 AI 助手",
        "思考强度",
        "上下文大小",
        "最大输出长度",
        "非协议终端语义状态仲裁",
        "父子通知异常智能摘要",
        "长共享前缀",
        "精简提示词",
        "自动",
        "重新自动识别",
        "KV 缓存命中率",
    ] {
        assert!(ai_zh_tree.contains(expected), "zh-CN AI settings missing {expected}: {ai_zh_tree}");
    }
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

    // 4. New Menu omits dangerously-skip-permissions toggle and shows only installed Windows shells on Windows.
    let mut nm_values = fixture(true, json!({}), updates("current", "current"));
    nm_values.insert("window".into(), json!({"platform":"windows","titlebar":"none","buttons":true,"maximized":false,"fullscreen":false,"active":true,"shells":["powershell","pwsh","git-bash","cmd"]}));
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
    for expected in ["nm-win-shells", "nm-shell-powershell", "nm-shell-pwsh", "nm-shell-git-bash", "nm-shell-cmd", "PowerShell", "PowerShell 7", "Git Bash", "CMD"] {
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

    // 4a. New Menu filters Windows shell chips to only those installed on the local machine.
    let mut nm_subset_values = fixture(true, json!({}), updates("current", "current"));
    nm_subset_values.insert("window".into(), json!({"platform":"windows","titlebar":"none","buttons":true,"maximized":false,"fullscreen":false,"active":true,"shells":["powershell","cmd"]}));
    nm_subset_values.insert("settings".into(), json!({"shell":"pwsh"}));
    let nm_subset_tree = describe_with_values(cx, &staged, "mods/new-menu", nm_subset_values, None).unwrap();
    for expected in ["nm-win-shells", "nm-shell-powershell", "nm-shell-cmd", "PowerShell", "CMD"] {
        assert!(nm_subset_tree.contains(expected), "Installed Windows shells missing {expected}: {nm_subset_tree}");
    }
    for hidden in ["nm-shell-pwsh", "PowerShell 7", "nm-shell-git-bash", "Git Bash"] {
        assert!(!nm_subset_tree.contains(hidden), "Uninstalled Windows shell {hidden} should be hidden: {nm_subset_tree}");
    }

    let mut nm_empty_shells_values = fixture(true, json!({}), updates("current", "current"));
    nm_empty_shells_values.insert("window".into(), json!({"platform":"windows","titlebar":"none","buttons":true,"maximized":false,"fullscreen":false,"active":true,"shells":[]}));
    let nm_empty_shells_tree = describe_with_values(cx, &staged, "mods/new-menu", nm_empty_shells_values, None).unwrap();
    assert!(!nm_empty_shells_tree.contains("nm-win-shells"), "Empty installed Windows shells list should hide chip row: {nm_empty_shells_tree}");

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
        Some("this.droveAiConfigMode = 'models'; this.droveRolesBackup = { mode: 'models', roles: {} }; this.droveData = { config: { note: 'Avoid GLM between 14:00 and 18:00', roles: { reviewer: { label: 'Reviewer', access: 'readonly', primary: { agent: 'claude', model: 'claude-sonnet-4-5', effort: 'high', strengths: ['Security Audit', 'Architecture Review'], note: 'Prefer for security audits' }, fallbacks: [{ agent: 'codex', model: 'gpt-5.4', effort: 'xhigh', fast: true, strengths: ['Concurrency & Locks'] }, { agent: 'agy', model: 'gemini-4-carbon-high', effort: 'high' }, { agent: 'cursor', model: 'grok-4.7', effort: 'high', context: '500k' }, { agent: 'cursor', model: 'grok-4.7-high', effort: 'high', context: '256k' }] } }, ai: { enabled: true, baseUrl: 'https://api.deepseek.com', model: 'deepseek-chat' } }, aiStatus: { switches: {} }, agents: { fullProbe: true, agents: [{ id: 'claude', installed: true, models: ['claude-sonnet-4-5', 'claude-opus-4-6'] }, { id: 'codex', installed: true, models: ['gpt-5.4'] }, { id: 'agy', installed: true, models: ['gemini-4-carbon-high'] }, { id: 'cursor', installed: true, models: ['grok-4.7', 'grok-4.7-high'], modelContexts: { 'grok-4.7': ['256k', '500k'] } }] } }; this.syncDroveInputs(this.droveData);"),
    )
    .unwrap();
    assert!(aligned_tree.contains("st-dr-ai-models-btn"), "Roles header should render AI Models button: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-ai-strengths-btn"), "Roles header should render AI Strengths button: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-ai-notes-btn"), "Roles header should render AI Notes button: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-ai-roles-undo"), "Roles header should render Restore button when backup exists: {aligned_tree}");
    assert!(!aligned_tree.contains("st-dr-role-note-card"), "Roles section should not render redundant global Routing Note card: {aligned_tree}");
    assert!(!aligned_tree.contains("st-dr-role-note-ai"), "Redundant global Routing Note AI button should not render: {aligned_tree}");
    assert!(!aligned_tree.contains("st-dr-role-note-input"), "Redundant global Routing Note textarea should not render: {aligned_tree}");
    assert!(!aligned_tree.contains("st-dr-role-note-save"), "Redundant global Routing Note save button should not render: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-ai-roles-panel"), "Expanded AI Auto-Config panel should render: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-ai-roles-prompt"), "Expanded AI panel should render tall multiline Textarea: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-ai-roles-run"), "Expanded AI Auto-Config panel should render Generate button: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-str-chip-reviewer-0-0"), "Candidate strengths chip should render: {aligned_tree}");
    assert!(aligned_tree.contains("Security Audit"), "Candidate strength label should render: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-str-add-reviewer-c0"), "Candidate strength add picker should render: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-cnote-btn-reviewer-c0"), "Candidate note trigger should render: {aligned_tree}");
    assert!(aligned_tree.contains("Prefer for security audits"), "Candidate note preview should render: {aligned_tree}");
    assert!(aligned_tree.contains("st-dr-cnote-btn-reviewer-c1"), "Empty candidate note trigger should render: {aligned_tree}");
    let cnote_popup_tree = describe(
        cx,
        &staged,
        "frame/settings",
        true,
        json!({ "section": "drove" }),
        updates("current", "current"),
        Some("this.picking = 'drove-note:reviewer-c0'; this.droveData = { config: { roles: { reviewer: { label: 'Reviewer', access: 'readonly', primary: { agent: 'claude', model: 'claude-sonnet-4-5', effort: 'high' }, fallbacks: [] } }, ai: { enabled: true } }, aiStatus: { switches: {} }, agents: { fullProbe: true, agents: [{ id: 'claude', installed: true, models: ['claude-sonnet-4-5'] }] } };"),
    )
    .unwrap();
    assert!(cnote_popup_tree.contains("st-dr-cnote-ai-reviewer-c0"), "Candidate note popup should render AI Note button: {cnote_popup_tree}");
    let str_popup_tree = describe(
        cx,
        &staged,
        "frame/settings",
        true,
        json!({ "section": "drove" }),
        updates("current", "current"),
        Some("this.picking = 'drove-str:reviewer-c0'; this.droveData = { config: { roles: { reviewer: { label: 'Reviewer', access: 'readonly', primary: { agent: 'claude', model: 'claude-sonnet-4-5', effort: 'high', strengths: ['Security Audit'] }, fallbacks: [] } }, ai: { enabled: true } }, aiStatus: { switches: {} }, agents: { fullProbe: true, agents: [{ id: 'claude', installed: true, models: ['claude-sonnet-4-5'] }] } };"),
    )
    .unwrap();
    assert!(str_popup_tree.contains("st-dr-str-ai-reviewer-c0"), "Candidate strengths popup should render AI Configure button: {str_popup_tree}");
    let drove_notice_tree = describe(
        cx,
        &staged,
        "frame/settings",
        true,
        json!({ "section": "drove" }),
        updates("current", "current"),
        Some("this.droveData = { config: { roles: { coder: { label: 'Coder', access: 'writable', primary: { agent: 'claude', model: 'opus', effort: 'high' }, fallbacks: [] } }, ai: { enabled: true } }, aiStatus: { switches: {} }, agents: { fullProbe: true, agents: [{ id: 'claude', installed: true, models: ['opus'] }, { id: 'cursor', installed: true, models: [], error: 'ACP probe timed out after 60s' }] } };"),
    )
    .unwrap();
    assert!(drove_notice_tree.contains("st-s-agents"), "Settings sidebar should render dedicated Agents & ACP section: {drove_notice_tree}");
    assert!(drove_notice_tree.contains("st-dr-goto-agents"), "Drove top bar should link to Agents & ACP section: {drove_notice_tree}");
    assert!(drove_notice_tree.contains("st-dr-agents-notice"), "Drove should show concise notice linking to Agents & ACP on probe errors: {drove_notice_tree}");
    assert!(!drove_notice_tree.contains("st-dr-agent-errors"), "Full probe error card belongs in dedicated Agents & ACP section: {drove_notice_tree}");

    let probe_err_tree = describe(
        cx,
        &staged,
        "frame/settings",
        true,
        json!({ "section": "agents" }),
        updates("current", "current"),
        Some("this.droveData = { config: { roles: { coder: { label: 'Coder', access: 'writable', primary: { agent: 'claude', model: 'opus', effort: 'high' }, fallbacks: [] } }, ai: { enabled: true } }, aiStatus: { switches: {} }, agents: { fullProbe: true, agents: [{ id: 'claude', installed: true, models: ['opus'] }, { id: 'cursor', installed: true, models: [], error: 'ACP probe timed out after 60s' }] } }; this.adapterStatus = { adapters_dir: '/home/.local/share/herdr2/adapters', auto_install: true, default_permissions: 'approve-reads', inject_mcp: true, adapters: [{ preset_id: 'claude', binary_name: 'claude-agent-acp', companion_cli: 'claude', companion_installed: true, installed: true, source: 'managed', state: 'ready', version: '0.9.0' }, { preset_id: 'codex', binary_name: 'codex-acp', companion_cli: 'codex', companion_installed: true, installed: false, source: 'npx_fallback', state: 'missing' }] };"),
    )
    .unwrap();
    assert!(probe_err_tree.contains("st-agents-page"), "Agents & ACP page should render: {probe_err_tree}");
    assert!(!probe_err_tree.contains("st-ag-adapters-card"), "Duplicate managed ACP adapters card should be unified into st-ag-list-card: {probe_err_tree}");
    assert!(probe_err_tree.contains("st-ag-list-card"), "Unified supported agents & ACP card should render: {probe_err_tree}");
    assert!(probe_err_tree.contains("st-ag-table-header"), "Unified agents table header should render: {probe_err_tree}");
    assert!(probe_err_tree.contains("st-ag-filter-bar"), "Agent filter tabs should render: {probe_err_tree}");
    assert!(probe_err_tree.contains("st-ag-row-claude"), "Installed agent row should render: {probe_err_tree}");
    assert!(probe_err_tree.contains("st-ag-row-codex"), "Missing-ACP agent row should render: {probe_err_tree}");
    assert!(probe_err_tree.contains("st-ag-row-inst-codex"), "Missing managed ACP adapter row should expose one-click install button: {probe_err_tree}");
    assert!(probe_err_tree.contains("st-ag-row-gemini"), "Uninstalled supported agent should be shown by default: {probe_err_tree}");
    assert!(probe_err_tree.contains("st-ag-row-cli-gemini"), "Uninstalled supported agent should expose one-click CLI install button: {probe_err_tree}");
    assert!(probe_err_tree.contains("st-ag-row-ai-cursor"), "Errored agent row should expose direct AI fix button: {probe_err_tree}");
    assert!(probe_err_tree.contains("st-ag-config-card"), "ACP configuration card should render: {probe_err_tree}");
    assert!(probe_err_tree.contains("st-dr-agent-errors"), "Probe error card should render in Agents & ACP: {probe_err_tree}");
    assert!(!probe_err_tree.contains("st-dr-probe-ai-btn"), "AI Diagnose button should be removed: {probe_err_tree}");
    assert!(probe_err_tree.contains("st-dr-probe-agent-fix-btn"), "Probe error card should render Fix with AI button: {probe_err_tree}");
    let probe_fix_tree = describe(
        cx,
        &staged,
        "frame/settings",
        true,
        json!({ "section": "agents" }),
        updates("current", "current"),
        Some("this.droveProbeFixOpen = true; this.droveProbeFixAgent = 'codex'; this.droveProbeFixModel = 'gpt-5.4'; this.droveProbeFixEffort = 'xhigh'; this.droveData = { config: { roles: { coder: { label: 'Coder', access: 'writable', primary: { agent: 'claude', model: 'opus', effort: 'high' }, fallbacks: [] } }, ai: { enabled: true } }, aiStatus: { switches: {} }, agents: { fullProbe: true, agents: [{ id: 'claude', installed: true, models: ['claude-opus-4-6'] }, { id: 'codex', installed: true, models: ['gpt-5.4'] }, { id: 'cursor', installed: true, models: [], error: 'ACP probe timed out after 60s' }] } };"),
    )
    .unwrap();
    assert!(probe_fix_tree.contains("st-dr-probe-fix-panel"), "Fix-with-Agent selector panel should render when open: {probe_fix_tree}");
    assert!(probe_fix_tree.contains("st-drp-probe-fix-agent"), "Fix panel should render agent type picker: {probe_fix_tree}");
    assert!(probe_fix_tree.contains("st-drp-probe-fix-model"), "Fix panel should render model picker: {probe_fix_tree}");
    assert!(probe_fix_tree.contains("st-dr-probe-fix-eff-group"), "Fix panel should render reasoning effort group: {probe_fix_tree}");
    assert!(probe_fix_tree.contains("st-dr-probe-fix-eff-auto"), "Fix panel should render Auto effort pill: {probe_fix_tree}");
    assert!(probe_fix_tree.contains("st-dr-probe-fix-eff-xhigh"), "4-level model (gpt-5.4) should render xhigh effort pill: {probe_fix_tree}");
    assert!(probe_fix_tree.contains("st-dr-probe-fix-launch"), "Fix panel should render Start Agent button: {probe_fix_tree}");
    let probe_fix_embedded_tree = describe(
        cx,
        &staged,
        "frame/settings",
        true,
        json!({ "section": "agents" }),
        updates("current", "current"),
        Some("this.droveProbeFixOpen = true; this.droveProbeFixAgent = 'agy'; this.droveProbeFixModel = 'gemini-4-carbon-high'; this.droveProbeFixEffort = 'high'; this.droveData = { config: { roles: { coder: { label: 'Coder', access: 'writable', primary: { agent: 'claude', model: 'opus', effort: 'high' }, fallbacks: [] } }, ai: { enabled: true } }, aiStatus: { switches: {} }, agents: { fullProbe: true, agents: [{ id: 'agy', installed: true, models: ['gemini-4-carbon-high'] }, { id: 'cursor', installed: true, models: [], error: 'ACP probe timed out after 60s' }] } };"),
    )
    .unwrap();
    let fix_eff_group = probe_fix_embedded_tree
        .lines()
        .find(|l| l.contains("st-dr-probe-fix-eff-group"))
        .unwrap();
    assert!(fix_eff_group.contains("0.35"), "Embedded-effort model should dim fix panel effort group: {fix_eff_group}");
    let fix_eff_high = probe_fix_embedded_tree
        .lines()
        .find(|l| l.contains("st-dr-probe-fix-eff-high"))
        .unwrap();
    assert!(!fix_eff_high.contains("on_click"), "Embedded-effort model should disable clicking effort pills: {fix_eff_high}");
    let ai_install_tree = describe(
        cx,
        &staged,
        "frame/settings",
        true,
        json!({ "section": "agents" }),
        updates("current", "current"),
        Some("this.agentAiInstallTargetId = 'agy'; this.agentAiInstallAgent = 'codex'; this.agentAiInstallModel = 'gpt-5.4'; this.agentAiInstallEffort = 'xhigh'; this.droveData = { config: { roles: {} }, aiStatus: { switches: {} }, agents: { fullProbe: true, agents: [{ id: 'claude', installed: true, models: ['claude-opus-4-6'] }, { id: 'codex', installed: true, models: ['gpt-5.4'] }] } };"),
    )
    .unwrap();
    assert!(ai_install_tree.contains("st-ag-ai-install-panel"), "AI Install selector panel should render when open: {ai_install_tree}");
    assert!(ai_install_tree.contains("st-drp-agent-install-agent"), "AI Install panel should render agent type picker: {ai_install_tree}");
    assert!(ai_install_tree.contains("st-drp-agent-install-model"), "AI Install panel should render model picker: {ai_install_tree}");
    assert!(ai_install_tree.contains("st-ag-ai-install-eff-group"), "AI Install panel should render reasoning effort group: {ai_install_tree}");
    assert!(ai_install_tree.contains("st-ag-ai-install-eff-xhigh"), "4-level model (gpt-5.4) should render xhigh effort pill in AI Install panel: {ai_install_tree}");
    assert!(ai_install_tree.contains("st-ag-ai-install-launch"), "AI Install panel should render Start AI Install button: {ai_install_tree}");
    let ai_install_acp_tree = describe(
        cx,
        &staged,
        "frame/settings",
        true,
        json!({ "section": "agents" }),
        updates("current", "current"),
        Some("this.agentAiInstallTargetId = 'hermes'; this.agentAiInstallAgent = 'agy'; this.agentAiInstallModel = 'gemini-4-carbon-high'; this.agentAiInstallEffort = 'high'; this.droveFixKnownAgents = { __local__: [{ key: 'hermes', name: 'Hermes', installed: true, run: ['hermes'] }, { key: 'antigravity', name: 'Antigravity', installed: true, run: ['agy'] }] }; this.droveData = { config: { roles: {} }, aiStatus: { switches: {} }, agents: { fullProbe: true, agents: [{ id: 'agy', installed: true, models: ['gemini-4-carbon-high'] }] } };"),
    )
    .unwrap();
    assert!(ai_install_acp_tree.contains("st-ag-ai-install-panel"), "ACP AI Install selector panel should render under separate-ACP row: {ai_install_acp_tree}");
    assert!(ai_install_acp_tree.contains("Start AI Install ACP"), "Separate-ACP AI Install panel should show ACP launch button: {ai_install_acp_tree}");
    let inst_eff_group = ai_install_acp_tree
        .lines()
        .find(|l| l.contains("st-ag-ai-install-eff-group"))
        .unwrap();
    assert!(inst_eff_group.contains("0.35"), "Embedded-effort model should dim AI Install panel effort group: {inst_eff_group}");
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
fn files_mod_renders_mermaid_diagrams_and_math_formulas_as_valid_svgs(cx: &mut TestAppContext) {
    use gpui::VisualTestContext;
    use std::ops::Deref as _;
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let rich_md = concat!(
        "# Architecture & Math\n\n",
        "Inline formula $E = mc^2$ and $\\frac{a+b}{c}$ with price $5 and $10.\n\n",
        "```mermaid\n",
        "flowchart TD\n",
        "  subgraph Core[\"核心引擎\"]\n",
        "    A([开始]) --> B{判断?}\n",
        "    B -->|是| C[执行]\n",
        "    B -.->|否| D[(存储)]\n",
        "  end\n",
        "```\n\n",
        "```mermaid\n",
        "sequenceDiagram\n",
        "  participant U as User\n",
        "  participant S as Server\n",
        "  U->>+S: Request\n",
        "  S-->>-U: Response\n",
        "```\n\n",
        "$$\n",
        "\\int_{0}^{\\infty} e^{-x^2} \\, dx = \\frac{\\sqrt{\\pi}}{2}\n",
        "$$\n\n",
        "```math\n",
        "\\begin{pmatrix} 1 & 2 \\\\ 3 & 4 \\end{pmatrix}\n",
        "```\n\n",
        "| Metric | Formula |\n",
        "| --- | --- |\n",
        "| Quadratic | $ax^2 + bx + c = 0$ |\n",
    );

    let calls = Rc::new(RefCell::new(Calls::default()));
    calls.borrow_mut().files.insert("/fixture/project/DOC.md".into(), rich_md.into());
    calls.borrow_mut().shared.insert("files.treeOpen".into(), json!(false));

    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-files-mermaid-math.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(fixture(true, json!({ "file": "DOC.md" }), updates("current", "current")), calls.clone()))
            .unwrap(),
    );
    let runtime = ShellRuntime::new_isolated().unwrap();
    let win = cx.add_window(|window, cx| RootedView(runtime.try_load(&staged.0.join("mods/files"), window, cx).unwrap()));
    let view: gpui::Entity<gpui_shell::ScriptView> = win
        .read_with(cx, |root, cx| root.0.read(cx).content().clone().downcast().unwrap())
        .unwrap();
    let mut visual = VisualTestContext::from_window(*win.deref(), cx);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
    visual.update(|window, cx| window.draw(cx).clear(cx));

    let tree = visual.update(|_, cx| {
        view.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    for expected in [
        "files-md-view",
        "files-md-mermaid-block-2",
        "files-md-mermaid-body-2",
        "Flowchart",
        "files-md-mermaid-block-3",
        "files-md-mermaid-body-3",
        "Sequence",
        "files-md-math-block-4",
        "files-md-math-body-4",
        "files-md-math-block-5",
        "files-md-table-6",
        "data:image/svg+xml,",
    ] {
        assert!(tree.contains(expected), "Mermaid/Math preview missing {expected}: {tree}");
    }
    assert!(calls.borrow().unexpected.is_empty(), "unexpected host calls: {:?}", calls.borrow().unexpected);

    // Extract every generated `data:image/svg+xml,...` URL from the rendered tree and prove
    // GPUI's real usvg/resvg SvgRenderer decodes and rasterizes each one cleanly.
    let mut svg_count = 0usize;
    let mut rest = tree.as_str();
    while let Some(pos) = rest.find("data:image/svg+xml,") {
        let after = &rest[pos + "data:image/svg+xml,".len()..];
        let end = after.find(|c: char| matches!(c, '"' | '\'' | '\\' | ' ' | ')' | '>')).unwrap_or(after.len());
        let encoded = &after[..end];
        let decoded = percent_decode_test(encoded);
        assert!(decoded.starts_with("<svg "), "expected decoded SVG payload, got: {}", &decoded[..decoded.len().min(80)]);
        let rendered = visual.update(|_, cx| cx.svg_renderer().render_single_frame(decoded.as_bytes(), 1.0));
        let image = rendered.unwrap_or_else(|e| panic!("SvgRenderer failed on SVG #{svg_count}: {e}\n{decoded}"));
        let size = image.size(0);
        assert!(i32::from(size.width) > 0 && i32::from(size.height) > 0, "rendered SVG #{svg_count} has zero size: {size:?}");
        svg_count += 1;
        rest = &after[end..];
    }
    assert!(svg_count >= 6, "expected at least 6 rendered SVG data URLs (2 mermaid + 2 display math + 2+ inline math), got {svg_count}");

    visual.update(|window, _| window.remove_window());
    gpui_shell::policy::set_default(Policy::new());
    cx.run_until_parked();
}

fn percent_decode_test(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(val) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(val);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).expect("valid UTF-8 SVG")
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
    assert!(!tree.contains("acp-mount-tab"), "side-chat must not expose Open as Tab button: {tree}");
    assert!(!tree.contains("acp-split-right"), "side-chat must not expose Split right/Dock button: {tree}");
    assert!(!tree.contains("acp-fork-turn-"), "side-chat must not expose assistant fork button: {tree}");
    assert!(!tree.contains("acp-fork-from-"), "side-chat must not expose user fork button: {tree}");

    // Side-chat agent context menu must never offer mounting into tabs or splits.
    let mut sc_menu_values = values.clone();
    sc_menu_values.insert("props".into(), json!({"overlay":"menu","tab":"agent","target":{"terminal":"wsl/a4h","pane":null}}));
    let sc_menu_tree = describe_with_values(cx, &staged, "mods/menu", sc_menu_values, None).unwrap();
    assert!(sc_menu_tree.contains("Open in side panel"), "side-chat menu should offer Open in side panel: {sc_menu_tree}");
    assert!(!sc_menu_tree.contains("Open as Tab"), "side-chat menu must not offer Open as Tab: {sc_menu_tree}");
    assert!(!sc_menu_tree.contains("Split right"), "side-chat menu must not offer Split right: {sc_menu_tree}");
    assert!(!sc_menu_tree.contains("Split down"), "side-chat menu must not offer Split down: {sc_menu_tree}");

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

    // Render handoff dialog in side-chat, handoff, and fork modes and verify custom turn count UI works across all three.
    for kind in ["handoff", "fork", "side-chat"] {
        let mut ho_values = values.clone();
        ho_values.insert("props".into(), json!({"overlay":"handoff","source":"wsl/t2t","kind":kind}));
        let ho_tree = describe_with_values(cx, &staged, "mods/handoff", ho_values.clone(), None).unwrap();
        assert!(ho_tree.contains("ho-rt-acp"), "expected ACP option in {kind} dialog: {ho_tree}");
        assert!(ho_tree.contains("ho-rt-pty"), "expected CLI Terminal option in {kind} dialog: {ho_tree}");
        assert!(!ho_tree.contains("ho-mode-merge-back"), "merge-back tab must not appear in {kind} dialog: {ho_tree}");
        assert!(ho_tree.contains("ho-turns-custom"), "expected Custom turns chip in {kind} dialog: {ho_tree}");
        assert!(!ho_tree.contains("ho-turns-custom-input"), "custom turns input should stay hidden until Custom is selected: {ho_tree}");

        let custom_tree = describe_with_values(
            cx,
            &staged,
            "mods/handoff",
            ho_values.clone(),
            Some("this.customTurnsMode = true; this.customTurnsInput.set_value('12');"),
        ).unwrap();
        assert!(custom_tree.contains("ho-turns-custom-input"), "expected custom turns input in {kind} dialog: {custom_tree}");
        assert!(!custom_tree.contains("ho-turns-custom-error"), "valid custom turns should not show validation error: {custom_tree}");

        let invalid_tree = describe_with_values(
            cx,
            &staged,
            "mods/handoff",
            ho_values,
            Some("this.customTurnsMode = true; this.customTurnsInput.set_value('0');"),
        ).unwrap();
        assert!(invalid_tree.contains("ho-turns-custom-error"), "invalid custom turns (0) should show validation error in {kind}: {invalid_tree}");
    }
}

#[gpui::test]
fn checkpoints_long_multiline_names_render_single_line_cleanly(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let values = fixture(true, json!({}), updates("current", "current"));
    let tree = describe_with_values(
        cx,
        &staged,
        "mods/checkpoints",
        values,
        Some(r#"
            this.targetAddr = 't3y';
            this.context = this.viewKey();
            this.selectedTurn = 1;
            this.checkpoints = [
                { turn: 0, files: [] },
                { turn: 1, prompt: '<pasted_content id="1b85">\n[来自 t2t 的会话分叉] 请先读取完整的上下文交接文件\n[Worktree 工作区声明] 很长的中文快照标题用于验证单行截断不遮挡\n</pasted_content id="1b85">', files: [{ path: 'calc.py', added: 140, deleted: 16 }] }
            ];
        "#),
    ).unwrap();
    assert!(tree.contains("ckpt-0"), "expected T0 checkpoint row: {tree}");
    assert!(tree.contains("ckpt-1"), "expected T1 checkpoint row: {tree}");
    assert!(!tree.contains("pasted_content"), "pasted_content wrapper tags should be stripped from summary: {tree}");
    assert!(
        tree.contains("[来自 t2t 的会话分叉] 请先读取完整的上下文交接文件 [Worktree 工作区声明]"),
        "multiline prompt should be collapsed into a single line: {tree}"
    );
}

#[gpui::test]
fn changes_and_checkpoints_render_diff_lines_and_inline_review_without_errors(cx: &mut TestAppContext) {
    use gpui::VisualTestContext;
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    // 1) Changes mod: verify description AND real GPUI draw() executing measured `list` layout pass without throwing.
    let changes_setup = r#"
        this.root = '/repo/demo';
        this.on = null;
        this.context = this.viewKey();
        this.files = [{ path: 'src/api.rs', status: 'M', fresh: false, add: 2, del: 1 }];
        this.open = 'src/api.rs';
        this.setLines(['@@ -10,2 +10,3 @@', '-let old_v = 1;', '+let new_v = "a very long line that must wrap cleanly to the viewport width instead of being truncated off-screen in narrow panels";', '+let extra = 3;']);
        this.setDrafts([{ id: 'c1', path: 'src/api.rs', side: 'new', start_line: 10, end_line: 11, body: 'Validate range before assigning' }]);
        this.composer = { file: 'src/api.rs', startRowIdx: 2, endRowIdx: 3, side: 'new', startLine: 10, endLine: 11, editingId: null };
    "#;
    let changes_tree = describe_with_values(
        cx,
        &staged,
        "mods/changes",
        fixture(true, json!({}), updates("current", "current")),
        Some(changes_setup),
    ).unwrap();
    assert!(changes_tree.contains("list \"changes-diff\" ×5 :on_item_click"), "expected measured list with 4 diff rows + 1 comment: {changes_tree}");
    assert!(!changes_tree.contains("changes-diff-x"), "changes diff should wrap to container width without horizontal overflow wrapper: {changes_tree}");
    assert!(changes_tree.contains("chg-inline-composer"), "docked composer must render: {changes_tree}");
    assert!(changes_tree.contains("chg-review-bar"), "review send bar must render: {changes_tree}");

    // Draw real frames so GPUI executes list's layout renderer callback and verify zero runtime errors.
    let calls = Rc::new(RefCell::new(Calls::default()));
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-changes-draw.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(fixture(true, json!({}), updates("current", "current")), calls.clone()))
            .unwrap(),
    );
    let rt_changes = ShellRuntime::new_isolated().unwrap();
    let changes_wrapped = staged.wrapper("mods/changes", changes_setup);
    let win = cx.add_window(|window, cx| RootedView(rt_changes.try_load(&changes_wrapped, window, cx).unwrap()));
    let mut visual = VisualTestContext::from_window(win.into(), cx);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
    let err = win.read_with(cx, |r, cx| {
        r.0.read(cx)
            .content()
            .clone()
            .downcast::<gpui_shell::ScriptView>()
            .ok()
            .and_then(|v| v.read(cx).build_error().map(str::to_owned))
    });
    assert_eq!(err.unwrap(), None, "changes list layout pass must produce zero errors");
    visual.update(|window, _| window.remove_window());
    cx.run_until_parked();

    // 2) Checkpoints mod: real multi-file patch diff lines + saved comment + real GPUI draw().
    let ckpt_setup = r#"
        this.targetAddr = 'a1';
        this.context = this.viewKey();
        this.selectedTurn = 2;
        this.checkpoints = [{ turn: 0, files: [] }, { turn: 2, prompt: 'Refactor parser', files: [{ path: 'src/p.rs', added: 2, deleted: 1 }] }];
        this.setLines(['File: src/deeply/nested/path/that/exceeds/narrow/panel/width/p.rs', '@@ -1,1 +1,2 @@', '-old_fn()', '+new_fn("a very long argument string that should wrap cleanly inside checkpoints diff view")', '+check()']);
        this.setDrafts([{ id: 'k1', path: 'src/deeply/nested/path/that/exceeds/narrow/panel/width/p.rs', side: 'new', start_line: 1, body: 'Add unit test for check()' }]);
    "#;
    let ckpt_tree = describe_with_values(
        cx,
        &staged,
        "mods/checkpoints",
        fixture(true, json!({}), updates("current", "current")),
        Some(ckpt_setup),
    ).unwrap();
    assert!(ckpt_tree.contains("list \"checkpoints-diff\" ×6 :on_item_click"), "expected measured list with 5 diff rows + 1 comment: {ckpt_tree}");
    assert!(!ckpt_tree.contains("checkpoints-diff-x"), "checkpoints diff should wrap to container width without horizontal overflow wrapper: {ckpt_tree}");
    assert!(ckpt_tree.contains("ckpt-review-bar"), "checkpoints review bar must render: {ckpt_tree}");

    let rt_ckpt = ShellRuntime::new_isolated().unwrap();
    let ckpt_wrapped = staged.wrapper("mods/checkpoints", ckpt_setup);
    let win2 = cx.add_window(|window, cx| RootedView(rt_ckpt.try_load(&ckpt_wrapped, window, cx).unwrap()));
    let mut visual2 = VisualTestContext::from_window(win2.into(), cx);
    visual2.update(|window, cx| window.draw(cx).clear(cx));
    visual2.run_until_parked();
    let err2 = win2.read_with(cx, |r, cx| {
        r.0.read(cx)
            .content()
            .clone()
            .downcast::<gpui_shell::ScriptView>()
            .ok()
            .and_then(|v| v.read(cx).build_error().map(str::to_owned))
    });
    assert_eq!(err2.unwrap(), None, "checkpoints list layout pass must produce zero errors");
    visual2.update(|window, _| window.remove_window());
    cx.run_until_parked();

    // 3) Files mod: Markdown preview block annotations + shared review bar render cleanly.
    let files_setup = r#"
        this.repoRoot = this.root;
        this.file = 'docs/plan.md';
        this.selectedFile = 'docs/plan.md';
        this.loadingFile = false;
        this.fileError = null;
        this.mdRenderLimit = 10;
        this.liveText = '# Plan\n\nImplement inline comments.';
        this.diskText = this.liveText;
        this.doc = {
            leading: '',
            defs: '',
            blocks: [
                { raw: '# Plan', gapAfter: '\n\n', kind: 'heading', lang: '', headingLevel: 1, tasks: [] },
                { raw: 'Implement inline comments.', gapAfter: '', kind: 'paragraph', lang: '', headingLevel: 0, tasks: [] },
            ],
        };
        this.setReviewDrafts([{ id: 'f1', path: 'docs/plan.md', side: 'file', start_line: 3, body: 'Add fallback note' }]);
        this.reviewComposer = { path: 'docs/plan.md', startLine: 3, endLine: 3, blockIndex: 1, editingId: null };
    "#;
    let files_tree = describe_with_values(
        cx,
        &staged,
        "mods/files",
        fixture(true, json!({}), updates("current", "current")),
        Some(files_setup),
    ).unwrap();
    assert!(files_tree.contains("files-add-comment"), "files toolbar comment button must render: {files_tree}");
    assert!(files_tree.contains("Add fallback note"), "markdown inline comment card must render: {files_tree}");
    assert!(files_tree.contains("files-review-composer"), "markdown docked composer must render: {files_tree}");
    assert!(files_tree.contains("files-review-bar"), "files shared review bar must render: {files_tree}");

    // 4) Files mod in Line-Comment mode: measured `list` with wrapped lines + inline comment + docked composer + real GPUI draw().
    let files_review_setup = r#"
        this.repoRoot = this.root;
        this.file = 'src/main.rs';
        this.selectedFile = 'src/main.rs';
        this.loadingFile = false;
        this.fileError = null;
        this.reviewMode = true;
        this.liveText = 'fn main() {\n    let x = "a very long string literal that must wrap according to panel width in files review mode";\n    println!("{x}");\n}';
        this.diskText = this.liveText;
        this.setReviewDrafts([{ id: 'fr1', path: 'src/main.rs', side: 'file', start_line: 2, end_line: 3, body: 'Extract helper' }]);
        this.reviewComposer = { path: 'src/main.rs', startRowIdx: 1, endRowIdx: 2, startLine: 2, endLine: 3, blockIndex: null, editingId: null };
    "#;
    let files_review_tree = describe_with_values(
        cx,
        &staged,
        "mods/files",
        fixture(true, json!({}), updates("current", "current")),
        Some(files_review_setup),
    ).unwrap();
    assert!(
        files_review_tree.contains("list \"files-review-lines\" ×5 :on_item_click"),
        "expected measured list with 4 file lines + 1 inline comment: {files_review_tree}"
    );
    assert!(!files_review_tree.contains("files-review-x"), "files review should wrap to container width without horizontal overflow wrapper: {files_review_tree}");
    assert!(files_review_tree.contains("files-edit-mode"), "expected Edit mode toggle in code toolbar: {files_review_tree}");
    assert!(files_review_tree.contains("files-review-composer"), "expected docked composer in files review mode: {files_review_tree}");
    assert!(files_review_tree.contains("files-review-bar"), "expected review bar in files review mode: {files_review_tree}");

    let rt_files_rev = ShellRuntime::new_isolated().unwrap();
    let files_rev_wrapped = staged.wrapper("mods/files", files_review_setup);
    let win3 = cx.add_window(|window, cx| RootedView(rt_files_rev.try_load(&files_rev_wrapped, window, cx).unwrap()));
    let mut visual3 = VisualTestContext::from_window(win3.into(), cx);
    visual3.update(|window, cx| window.draw(cx).clear(cx));
    visual3.run_until_parked();
    let err3 = win3.read_with(cx, |r, cx| {
        r.0.read(cx)
            .content()
            .clone()
            .downcast::<gpui_shell::ScriptView>()
            .ok()
            .and_then(|v| v.read(cx).build_error().map(str::to_owned))
    });
    assert_eq!(err3.unwrap(), None, "files review list layout pass must produce zero errors");
    visual3.update(|window, _| window.remove_window());
    cx.run_until_parked();
}

#[gpui::test]
fn files_changes_and_checkpoints_wrap_long_lines_to_narrow_panel_width(cx: &mut TestAppContext) {
    use gpui::{Modifiers, VisualTestContext, div, point, px};
    use gpui::prelude::*;

    struct NarrowRoot(gpui::Entity<gpui_shell::ShellRoot>);
    impl gpui::Render for NarrowRoot {
        fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            div().w(px(300.)).h(px(520.)).child(self.0.clone())
        }
    }

    struct SplitRoot(gpui::Entity<gpui_shell::ShellRoot>);
    impl gpui::Render for SplitRoot {
        fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            div().w(px(520.)).h(px(520.)).child(self.0.clone())
        }
    }

    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let long_token = "long_identifier_segment_without_breaks_".repeat(12);

    // 1) Changes: side-by-side layout (left files ~220px | right diff ~300px).
    //    A 400+ char diff line at row 1 must wrap past 50px tall in the right ~300px diff pane,
    //    so clicking at x=360, y=150 (56px below start of row 1) still selects line 1 (not line 2).
    let changes_setup = format!(r#"
        this.viewKey = () => 'pinned-wrap-test';
        this.refresh = () => {{}};
        this.loading = false;
        this.root = '/repo/demo';
        this.on = null;
        this.context = 'pinned-wrap-test';
        this.files = [{{ path: 'src/long.rs', status: 'M', fresh: false, add: 2, del: 0 }}];
        this.open = 'src/long.rs';
        this.gitLogOpen = false;
        this.setLines(['@@ -1,0 +1,2 @@', '+let s = "{long_token}";', '+let short_line = 2;']);
    "#);
    let calls = Rc::new(RefCell::new(Calls::default()));
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-wrap-changes.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(fixture(true, json!({}), updates("current", "current")), calls.clone()))
            .unwrap(),
    );
    let rt_chg = ShellRuntime::new_isolated().unwrap();
    let chg_wrapped = staged.wrapper("mods/changes", &changes_setup);
    let win_chg = cx.add_window(|window, cx| SplitRoot(rt_chg.try_load(&chg_wrapped, window, cx).unwrap()));
    let view_chg: gpui::Entity<gpui_shell::ScriptView> = win_chg
        .read_with(cx, |root, cx| root.0.read(cx).content().clone().downcast().unwrap())
        .unwrap();
    let mut vis_chg = VisualTestContext::from_window(win_chg.into(), cx);
    vis_chg.update(|window, cx| window.draw(cx).clear(cx));
    vis_chg.run_until_parked();
    // Header(42) + diff toolbar(32) + hunk(20) = ~94px -> row 1 starts around y=94 in the right pane (x > 220);
    // clicking at (360, 150) is ~56px inside row 1, which ONLY hits L1 if row 1 wrapped to >= 3 visual lines!
    vis_chg.simulate_click(point(px(360.), px(150.)), Modifiers::default());
    vis_chg.run_until_parked();
    vis_chg.update(|window, cx| window.draw(cx).clear(cx));
    let chg_tree = vis_chg.update(|_, cx| {
        view_chg.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(
        chg_tree.contains("h_resizable \"changes-split\""),
        "Changes must use draggable h_resizable split layout: {chg_tree}"
    );
    assert!(
        chg_tree.contains("src/long.rs · L1\""),
        "expected wrapped diff line 1 in Changes right pane to span past y=150 (hitting L1, not L2): {chg_tree}"
    );
    vis_chg.update(|window, _| window.remove_window());
    cx.run_until_parked();

    // 1b) Changes in Split (side-by-side Before / After) diff mode: renders header & wrapped split list with zero errors.
    let calls_split = Rc::new(RefCell::new(Calls::default()));
    calls_split.borrow_mut().shared.insert("diff.viewMode".into(), json!("split"));
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-wrap-changes-split.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(fixture(true, json!({}), updates("current", "current")), calls_split.clone()))
            .unwrap(),
    );
    let changes_split_setup = format!(r#"
        this.viewKey = () => 'pinned-split-test';
        this.refresh = () => {{}};
        this.loading = false;
        this.root = '/repo/demo';
        this.on = null;
        this.context = 'pinned-split-test';
        this.files = [{{ path: 'src/long.rs', status: 'M', fresh: false, add: 2, del: 1 }}];
        this.open = 'src/long.rs';
        this.gitLogOpen = false;
        this.setLines(['@@ -1,1 +1,2 @@', '-let old_s = "{long_token}";', '+let new_s = "{long_token}";', '+let short_line = 2;']);
    "#);
    let rt_split = ShellRuntime::new_isolated().unwrap();
    let split_wrapped = staged.wrapper("mods/changes", &changes_split_setup);
    let win_split = cx.add_window(|window, cx| SplitRoot(rt_split.try_load(&split_wrapped, window, cx).unwrap()));
    let view_split: gpui::Entity<gpui_shell::ScriptView> = win_split
        .read_with(cx, |root, cx| root.0.read(cx).content().clone().downcast().unwrap())
        .unwrap();
    let mut vis_split = VisualTestContext::from_window(win_split.into(), cx);
    vis_split.update(|window, cx| window.draw(cx).clear(cx));
    vis_split.run_until_parked();
    let split_err = vis_split.update(|_, cx| view_split.read(cx).build_error().map(str::to_owned));
    assert_eq!(split_err, None, "changes split diff layout pass must produce zero errors");
    // Header(42) + toolbar(32) + split header(22) + hunk(20) = ~116px; clicking at (360, 175) hits wrapped pair row 1 (L1).
    vis_split.simulate_click(point(px(360.), px(175.)), Modifiers::default());
    vis_split.run_until_parked();
    vis_split.update(|window, cx| window.draw(cx).clear(cx));
    let split_tree = vis_split.update(|_, cx| {
        view_split.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(split_tree.contains("chg-split-header"), "split mode must render Before/After header: {split_tree}");
    assert!(split_tree.contains("list \"changes-diff-split\" ×3 :on_item_click"), "split mode must pair -/+ into 3 rows (hunk + 2 pairs): {split_tree}");
    assert!(split_tree.contains("src/long.rs · L1\""), "clicking wrapped split row 1 must open composer on L1: {split_tree}");
    vis_split.update(|window, _| window.remove_window());
    cx.run_until_parked();

    // Restore unified mode for remaining wrapping checks.
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-wrap-ckpt.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(fixture(true, json!({}), updates("current", "current")), calls.clone()))
            .unwrap(),
    );

    // 2) Checkpoints: same real-layout wrapping proof in a 300px panel.
    let ckpt_setup = format!(r#"
        this.viewKey = () => 'pinned-wrap-test';
        this.refresh = () => {{}};
        this.loading = false;
        this.targetAddr = 'a1';
        this.context = 'pinned-wrap-test';
        this.selectedTurn = 1;
        this.checkpoints = [{{ turn: 0, files: [] }}, {{ turn: 1, prompt: 'Wrap test', files: [{{ path: 'src/long.rs', added: 2, deleted: 0 }}] }}];
        this.setLines(['File: src/long.rs', '@@ -1,0 +1,2 @@', '+let s = "{long_token}";', '+let short_line = 2;']);
    "#);
    let rt_ckpt = ShellRuntime::new_isolated().unwrap();
    let ckpt_wrapped = staged.wrapper("mods/checkpoints", &ckpt_setup);
    let win_ckpt = cx.add_window(|window, cx| NarrowRoot(rt_ckpt.try_load(&ckpt_wrapped, window, cx).unwrap()));
    let view_ckpt: gpui::Entity<gpui_shell::ScriptView> = win_ckpt
        .read_with(cx, |root, cx| root.0.read(cx).content().clone().downcast().unwrap())
        .unwrap();
    let mut vis_ckpt = VisualTestContext::from_window(win_ckpt.into(), cx);
    vis_ckpt.update(|window, cx| window.draw(cx).clear(cx));
    vis_ckpt.run_until_parked();
    // Header(42) + bisect bar(39) + checkpoints list(86) + diff toolbar(30) + file header(24) + hunk(20) = ~241px;
    // clicking at y=300 is ~59px inside row 1, which ONLY hits L1 if row 1 wrapped!
    vis_ckpt.simulate_click(point(px(120.), px(300.)), Modifiers::default());
    vis_ckpt.run_until_parked();
    vis_ckpt.update(|window, cx| window.draw(cx).clear(cx));
    let ckpt_tree = vis_ckpt.update(|_, cx| {
        view_ckpt.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(
        ckpt_tree.contains("src/long.rs · L1\""),
        "expected wrapped diff line 1 in Checkpoints to span past y=270 (hitting L1, not L2): {ckpt_tree}"
    );
    vis_ckpt.update(|window, _| window.remove_window());
    cx.run_until_parked();

    // 3) Files review mode: same real-layout wrapping proof in a 300px panel.
    let files_rev_setup = format!(r#"
        this.refresh = () => {{}};
        this.pollOpenFile = () => {{}};
        this.repoRoot = this.root;
        this.file = 'src/long.rs';
        this.selectedFile = 'src/long.rs';
        this.loadingFile = false;
        this.fileError = null;
        this.reviewMode = true;
        this.liveText = 'let s = "{long_token}";\nlet short_line = 2;';
        this.diskText = this.liveText;
    "#);
    let calls_f = Rc::new(RefCell::new(Calls::default()));
    calls_f.borrow_mut().shared.insert("files.treeOpen".into(), json!(false));
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-wrap-files.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(fixture(true, json!({}), updates("current", "current")), calls_f.clone()))
            .unwrap(),
    );
    let rt_frev = ShellRuntime::new_isolated().unwrap();
    let frev_wrapped = staged.wrapper("mods/files", &files_rev_setup);
    let win_frev = cx.add_window(|window, cx| NarrowRoot(rt_frev.try_load(&frev_wrapped, window, cx).unwrap()));
    let view_frev: gpui::Entity<gpui_shell::ScriptView> = win_frev
        .read_with(cx, |root, cx| root.0.read(cx).content().clone().downcast().unwrap())
        .unwrap();
    let mut vis_frev = VisualTestContext::from_window(win_frev.into(), cx);
    vis_frev.update(|window, cx| window.draw(cx).clear(cx));
    vis_frev.run_until_parked();
    // Toolbar(36) -> row 0 (L1) starts at y=36; clicking at y=96 is 60px below top of L1,
    // which ONLY hits L1 if line 1 wrapped across multiple visual rows!
    vis_frev.simulate_click(point(px(120.), px(96.)), Modifiers::default());
    vis_frev.run_until_parked();
    vis_frev.update(|window, cx| window.draw(cx).clear(cx));
    let frev_tree = vis_frev.update(|_, cx| {
        view_frev.read(cx).snapshot().map(gpui_shell::RenderSnapshot::debug_tree).unwrap_or_default()
    });
    assert!(
        frev_tree.contains("src/long.rs · L1\""),
        "expected wrapped source line 1 in Files review mode to span past y=96 (hitting L1, not L2): {frev_tree}"
    );
    vis_frev.update(|window, _| window.remove_window());
    cx.run_until_parked();

    // 4) Files source code editor: a 400+ char line 1 wraps across multiple visual rows in a 300px panel,
    //    so clicking 60px below the top of line 1 and typing 'Z' edits line 1 (not line 2).
    let long_src = format!("let s = \"{long_token}\";\nlet short_line = 2;\n");
    let calls_code = Rc::new(RefCell::new(Calls::default()));
    calls_code.borrow_mut().files.insert("/fixture/project/src/long.rs".into(), long_src.clone());
    calls_code.borrow_mut().shared.insert("files.treeOpen".into(), json!(false));
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-wrap-files-code.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(fixture(true, json!({ "file": "src/long.rs" }), updates("current", "current")), calls_code.clone()))
            .unwrap(),
    );
    let rt_code = ShellRuntime::new_isolated().unwrap();
    let win_code = cx.add_window(|window, cx| NarrowRoot(rt_code.try_load(&staged.0.join("mods/files"), window, cx).unwrap()));
    let mut vis_code = VisualTestContext::from_window(win_code.into(), cx);
    vis_code.update(|window, cx| window.draw(cx).clear(cx));
    vis_code.run_until_parked();
    vis_code.update(|window, cx| window.draw(cx).clear(cx));
    vis_code.simulate_click(point(px(120.), px(96.)), Modifiers::default());
    vis_code.simulate_keystrokes("Z");
    vis_code.run_until_parked();
    vis_code.simulate_keystrokes("ctrl-s");
    vis_code.run_until_parked();
    vis_code.update(|window, cx| window.draw(cx).clear(cx));
    let written = calls_code.borrow().writes.last().map(|(_, text, _, _)| text.clone()).unwrap_or_default();
    let mut lines_iter = written.lines();
    let first_line = lines_iter.next().unwrap_or_default();
    let second_line = lines_iter.next().unwrap_or_default();
    assert!(
        first_line.contains('Z') && second_line == "let short_line = 2;",
        "expected soft-wrapped source editor line 1 to receive 'Z' at y=96 while line 2 stays untouched, got:\nline1={first_line}\nline2={second_line}"
    );
    vis_code.update(|window, _| window.remove_window());
    gpui_shell::policy::set_default(Policy::new());
    cx.run_until_parked();
}

#[gpui::test]
fn flash_note_button_composer_and_sidebar_list_work_end_to_end(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    // 1) Top-right tabs bar does not render the removed Flash Notes button.
    let tabs_tree = describe(cx, &staged, "mods/tabs", true, props(), updates("current", "current"), None).unwrap();
    assert!(!tabs_tree.contains("btn-flash-note"), "top-right flash note button should be removed: {tabs_tree}");

    // 2) Bottom-left sidebar list renders empty state and saved notes cleanly.
    let list_empty = describe_with_values(
        cx,
        &staged,
        "mods/flash-note",
        fixture(true, json!({"list": true}), updates("current", "current")),
        None,
    ).unwrap();
    assert!(list_empty.contains("fn-sidebar"), "sidebar flash notes container missing: {list_empty}");
    assert!(list_empty.contains("fn-recycle"), "sidebar flash notes recycle bin button missing: {list_empty}");
    assert!(list_empty.contains("mods/workspace-chrome/archive.svg"), "flash notes recycle bin should use the same archive icon as agents: {list_empty}");
    assert!(list_empty.contains("fn-list-new"), "sidebar flash notes + button missing: {list_empty}");

    let list_calls = Rc::new(RefCell::new(Calls::default()));
    list_calls.borrow_mut().shared.insert(
        "flash_notes.items".into(),
        json!([
            { "id": "n1", "title": "Cache refactor", "text": "Refactor cache invalidation before release", "createdAt": 1700000000000i64, "updatedAt": 1700000001000i64, "sentTo": null, "sentAt": null },
            { "id": "n2", "text": "Add unit test for sidebar list", "createdAt": 1700000000000i64, "updatedAt": 1700000000500i64, "sentTo": "Build (t1)", "sentAt": 1700000000600i64 }
        ]),
    );
    let list_policy = Policy::new()
        .with_storage_path(staged.0.join("store-flash-list.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(fixture(true, json!({"list": true}), updates("current", "current")), list_calls.clone()))
        .unwrap();
    let list_rt = ShellRuntime::new_isolated().unwrap();
    let list_win = cx.add_window(|_, _| Empty);
    let list_populated = list_win.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(list_rt, staged.0.join("mods/flash-note"), list_policy, window, cx).unwrap();
        let tree = session.describe(window, cx).unwrap();
        window.remove_window();
        tree
    }).unwrap();
    cx.run_until_parked();
    assert!(list_populated.contains("fn-item-n1"), "first saved note missing: {list_populated}");
    assert!(list_populated.contains("Cache refactor"), "custom note title missing: {list_populated}");
    assert!(list_populated.contains("Refactor cache invalidation"), "first note preview missing: {list_populated}");
    assert!(list_populated.contains("fn-item-n2"), "second saved note missing: {list_populated}");
    assert!(list_populated.contains("Sent → Build (t1)"), "sent badge missing: {list_populated}");
    // Ascending chronological order: earlier note (n2: 1700000000500) must appear above later note (n1: 1700000001000).
    let pos_n2 = list_populated.find("fn-item-n2").unwrap();
    let pos_n1 = list_populated.find("fn-item-n1").unwrap();
    assert!(pos_n2 < pos_n1, "flash notes list must sort earlier notes on top and later notes below: {list_populated}");

    // 2b) Inline rename + moving a note to recycle bin + recycle bin popup rendering.
    let ops_calls = Rc::new(RefCell::new(Calls::default()));
    ops_calls.borrow_mut().shared.insert(
        "flash_notes.items".into(),
        json!([
            { "id": "n1", "text": "Refactor cache invalidation before release", "createdAt": 1700000000000i64, "updatedAt": 1700000001000i64, "sentTo": null, "sentAt": null },
            { "id": "n2", "text": "Add unit test for sidebar list", "createdAt": 1700000000000i64, "updatedAt": 1700000000500i64, "sentTo": null, "sentAt": null }
        ]),
    );
    let ops_policy = Policy::new()
        .with_storage_path(staged.0.join("store-flash-ops.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(fixture(true, json!({"list": true}), updates("current", "current")), ops_calls.clone()))
        .unwrap();
    let ops_rt = ShellRuntime::new_isolated().unwrap();
    let ops_root = staged.wrapper(
        "mods/flash-note",
        r#"
            const target = this.savedNotes().find((n) => n.id === "n1");
            this.startInlineRename(target, cx);
            this.renameInput.set_value("Release cache plan");
            this.commitInlineRename(cx);
            this.trashNote("n2", cx);
        "#,
    );
    let ops_win = cx.add_window(|_, _| Empty);
    let ops_tree = ops_win.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(ops_rt, ops_root, ops_policy, window, cx).unwrap();
        let tree = session.describe(window, cx).unwrap();
        window.remove_window();
        tree
    }).unwrap();
    cx.run_until_parked();
    assert!(ops_tree.contains("Release cache plan"), "renamed note title missing after inline rename: {ops_tree}");
    assert!(!ops_tree.contains("fn-item-n2"), "trashed note should leave active list: {ops_tree}");
    let items_after = ops_calls.borrow().shared.get("flash_notes.items").cloned().unwrap_or(Value::Null);
    let trash_after = ops_calls.borrow().shared.get("flash_notes.trash").cloned().unwrap_or(Value::Null);
    assert_eq!(items_after.as_array().map(|a| a.len()), Some(1));
    assert_eq!(items_after[0]["title"], json!("Release cache plan"));
    assert_eq!(trash_after.as_array().map(|a| a.len()), Some(1));
    assert_eq!(trash_after[0]["id"], json!("n2"));

    let trash_calls = Rc::new(RefCell::new(Calls::default()));
    trash_calls.borrow_mut().shared.insert("flash_notes.items".into(), items_after.clone());
    trash_calls.borrow_mut().shared.insert("flash_notes.trash".into(), trash_after.clone());
    let trash_policy = Policy::new()
        .with_storage_path(staged.0.join("store-flash-trash.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(fixture(true, json!({"overlay": "fn-trash-ov", "trashMenu": true}), updates("current", "current")), trash_calls.clone()))
        .unwrap();
    let trash_rt = ShellRuntime::new_isolated().unwrap();
    let trash_win = cx.add_window(|_, _| Empty);
    let trash_tree = trash_win.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(trash_rt, staged.0.join("mods/flash-note"), trash_policy, window, cx).unwrap();
        let tree = session.describe(window, cx).unwrap();
        window.remove_window();
        tree
    }).unwrap();
    cx.run_until_parked();
    assert!(trash_tree.contains("fn-trash-menu"), "recycle bin menu missing: {trash_tree}");
    assert!(trash_tree.contains("fn-trash-empty"), "recycle bin empty button missing: {trash_tree}");
    assert!(trash_tree.contains("fn-trash-row-n2"), "trashed note row missing: {trash_tree}");
    assert!(trash_tree.contains("fn-trash-restore-n2"), "restore button missing: {trash_tree}");
    assert!(trash_tree.contains("fn-trash-purge-n2"), "purge button missing: {trash_tree}");

    // 3) Floating composer window supports title input, existing agent, new agent (ACP/CLI + space), saving, and sending.
    let compose_existing = describe_with_values(
        cx,
        &staged,
        "mods/flash-note",
        fixture(true, json!({"overlay": "fn-ov", "mode": "compose"}), updates("current", "current")),
        None,
    ).unwrap();
    assert!(compose_existing.contains("flash-note-window"), "floating flash note window missing: {compose_existing}");
    assert!(compose_existing.contains("fn-title-wrap"), "composer optional title input missing: {compose_existing}");
    assert!(compose_existing.contains("fn-mode-existing") && compose_existing.contains("fn-mode-new"), "target mode tabs missing: {compose_existing}");
    assert!(compose_existing.contains("fn-existing-btn"), "existing agent picker missing: {compose_existing}");
    assert!(compose_existing.contains("fn-save") && compose_existing.contains("fn-send"), "save/send footer buttons missing: {compose_existing}");

    let compose_new = describe_with_values(
        cx,
        &staged,
        "mods/flash-note",
        fixture(true, json!({"overlay": "fn-ov", "mode": "compose"}), updates("current", "current")),
        Some("this.targetMode = 'new';"),
    ).unwrap();
    assert!(compose_new.contains("fn-preset-btn"), "new agent preset picker missing: {compose_new}");
    assert!(compose_new.contains("fn-new-rt-acp") && compose_new.contains("fn-new-rt-cli"), "ACP/CLI mode toggles missing: {compose_new}");
    assert!(compose_new.contains("fn-model-btn"), "new agent model picker missing: {compose_new}");
    assert!(compose_new.contains("fn-eff-default") && compose_new.contains("fn-eff-high"), "new agent reasoning effort chips missing: {compose_new}");
    assert!(compose_new.contains("fn-space-btn"), "workspace picker missing: {compose_new}");

    // Verify saving persists title + text into shared("flash_notes.items") and sending to a new ACP/CLI agent dispatches tab.new with model, effort, and task.
    let calls = Rc::new(RefCell::new(Calls::default()));
    let values = fixture(true, json!({"overlay": "fn-ov", "mode": "compose"}), updates("current", "current"));
    let policy = Policy::new()
        .with_storage_path(staged.0.join("store-flash-note.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(values, calls.clone()))
        .unwrap();
    let runtime = ShellRuntime::new_isolated().unwrap();
    let root = staged.wrapper(
        "mods/flash-note",
        r#"
            this.titleInput.set_value("Idle memory check");
            this.input.set_value("Investigate memory spike on idle");
            this.saveIdea(cx);
            this.targetMode = "new";
            this.selectPreset("claude");
            this.selectModel("opus");
            this.selectedEffort = "high";
            this.sendIdea(cx);
            this.newRuntimeMode = "cli";
            this.selectPreset("antigravity");
            this.selectModel("gemini-4-carbon");
            this.selectedEffort = "high";
            this.input.set_value("markdown 的预览页面能否增加行数和批注功能");
            this.sendIdea(cx);
        "#,
    );
    let window = cx.add_window(|_, _| Empty);
    window.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(runtime, root, policy, window, cx).unwrap();
        let _ = session.describe(window, cx).unwrap();
        window.remove_window();
    }).unwrap();
    cx.run_until_parked();

    let saved = calls.borrow().shared.get("flash_notes.items").cloned().unwrap_or(Value::Null);
    let saved_arr = saved.as_array().expect("flash_notes.items must be persisted as array");
    assert_eq!(saved_arr.len(), 1);
    assert_eq!(saved_arr[0]["title"], json!("Idle memory check"));
    let tab_new_calls: Vec<Value> = calls
        .borrow()
        .effects
        .iter()
        .filter(|(name, args)| name == "run" && args.first().and_then(Value::as_str) == Some("tab.new"))
        .map(|(_, args)| args[1].clone())
        .collect();
    assert_eq!(tab_new_calls.len(), 2, "expected both ACP and CLI tab.new dispatches: {tab_new_calls:?}");
    assert_eq!(tab_new_calls[0]["acp"], json!(true));
    assert_eq!(tab_new_calls[0]["agent"], json!("claude"));
    assert_eq!(tab_new_calls[0]["model"], json!("opus"));
    assert_eq!(tab_new_calls[0]["effort"], json!("high"));
    assert_eq!(tab_new_calls[0]["task"], json!("Investigate memory spike on idle"));

    assert_eq!(tab_new_calls[1]["argv"], json!(["agy", "--dangerously-skip-permissions"]));
    assert_eq!(tab_new_calls[1]["model"], json!("gemini-4-carbon-high"));
    assert_eq!(tab_new_calls[1]["effort"], json!("high"));
    assert_eq!(tab_new_calls[1]["task"], json!("markdown 的预览页面能否增加行数和批注功能"));
    assert!(
        !calls.borrow().effects.iter().any(|(name, _)| name == "closeOverlay"),
        "saving or sending flash notes must keep the composer window open",
    );
}

#[gpui::test]
fn flash_note_group_supports_multiple_inputs_selective_send_and_group_send(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    // 1) Compose window with 3 entries: save group, send selected subset (keeps window open),
    //    send a single entry directly, and send the whole group to a new agent.
    let calls = Rc::new(RefCell::new(Calls::default()));
    let values = fixture(true, json!({"overlay": "fn-group-ov", "mode": "compose"}), updates("current", "current"));
    let policy = Policy::new()
        .with_storage_path(staged.0.join("store-flash-group.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(values, calls.clone()))
        .unwrap();
    let runtime = ShellRuntime::new_isolated().unwrap();
    let root = staged.wrapper(
        "mods/flash-note",
        r#"
            this.titleInput.set_value("Sprint Ideas");
            this.entryStates[0].input.set_value("Optimize terminal scroll");
            this.addEntry(cx, "Render mermaid diagrams");
            this.addEntry(cx, "Support LaTeX formulas");
            this.targetMode = "new";
            this.selectPreset("claude");
            // Uncheck entry #1 (second item) -> auto-switches to sendScope="selected" (items #1 and #3).
            this.toggleEntrySelected(1, cx);
            this.sendIdea(cx);
        "#,
    );
    let win = cx.add_window(|_, _| Empty);
    let partial_tree = win
        .update(cx, |_, window, cx| {
            let mut session = DescriptionSession::load(runtime, root, policy, window, cx).unwrap();
            let tree = session.describe(window, cx).unwrap();
            window.remove_window();
            tree
        })
        .unwrap();
    cx.run_until_parked();

    for expected in [
        "fn-group-bar",
        "fn-add-entry",
        "fn-entries-list",
        "fn-entry-check-0",
        "fn-entry-check-1",
        "fn-entry-check-2",
        "fn-entry-send-1",
        "fn-send-scope",
        "fn-scope-all",
        "fn-scope-selected",
    ] {
        assert!(partial_tree.contains(expected), "multi-entry flash note composer missing {expected}: {partial_tree}");
    }

    // Verify partial send dispatched ONLY the 2 selected items, saved the 3-entry group, and kept overlay open.
    let tab_new_partial: Vec<Value> = calls
        .borrow()
        .effects
        .iter()
        .filter(|(name, args)| name == "run" && args.first().and_then(Value::as_str) == Some("tab.new"))
        .map(|(_, args)| args[1].clone())
        .collect();
    assert_eq!(tab_new_partial.len(), 1, "expected 1 selective send tab.new call: {tab_new_partial:?}");
    assert_eq!(
        tab_new_partial[0]["task"],
        json!("[Sprint Ideas]\n\n1. Optimize terminal scroll\n\n2. Support LaTeX formulas"),
    );
    let closed_after_partial = calls
        .borrow()
        .effects
        .iter()
        .any(|(name, _)| name == "closeOverlay");
    assert!(!closed_after_partial, "partial group send must keep composer open for remaining unsent items");

    let saved_group = calls.borrow().shared.get("flash_notes.items").cloned().unwrap_or(Value::Null);
    let saved_arr = saved_group.as_array().expect("flash_notes.items must be array");
    assert_eq!(saved_arr.len(), 1);
    let saved_entries = saved_arr[0]["entries"].as_array().expect("entries array missing");
    assert_eq!(saved_entries.len(), 3);
    assert!(saved_entries[0]["sentTo"].is_string(), "entry 0 must be marked sent");
    assert!(saved_entries[1]["sentTo"].is_null(), "entry 1 must remain unsent");
    assert!(saved_entries[2]["sentTo"].is_string(), "entry 2 must be marked sent");

    // 2) Sidebar list renders the saved group badge (3 ideas) and partial sent progress (2/3 sent).
    let list_calls = Rc::new(RefCell::new(Calls::default()));
    list_calls.borrow_mut().shared.insert("flash_notes.items".into(), saved_group.clone());
    let list_policy = Policy::new()
        .with_storage_path(staged.0.join("store-flash-group-list.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(fixture(true, json!({"list": true}), updates("current", "current")), list_calls.clone()))
        .unwrap();
    let list_rt = ShellRuntime::new_isolated().unwrap();
    let list_win = cx.add_window(|_, _| Empty);
    let list_tree = list_win
        .update(cx, |_, window, cx| {
            let mut session = DescriptionSession::load(list_rt, staged.0.join("mods/flash-note"), list_policy, window, cx).unwrap();
            let tree = session.describe(window, cx).unwrap();
            window.remove_window();
            tree
        })
        .unwrap();
    cx.run_until_parked();
    assert!(list_tree.contains("Sprint Ideas"), "group title missing in sidebar: {list_tree}");
    assert!(list_tree.contains("3 ideas"), "group count badge missing in sidebar: {list_tree}");
    assert!(list_tree.contains("2/3 sent"), "partial sent progress missing in sidebar: {list_tree}");

    // 3) Re-opening the saved group restores all 3 entries; sending the whole group dispatches all 3 and closes overlay.
    let group_id = saved_arr[0]["id"].as_str().unwrap().to_owned();
    let full_calls = Rc::new(RefCell::new(Calls::default()));
    full_calls.borrow_mut().shared.insert("flash_notes.items".into(), saved_group);
    let full_policy = Policy::new()
        .with_storage_path(staged.0.join("store-flash-group-full.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(
            fixture(true, json!({"overlay": "fn-group-ov2", "mode": "compose", "noteId": group_id}), updates("current", "current")),
            full_calls.clone(),
        ))
        .unwrap();
    let full_rt = ShellRuntime::new_isolated().unwrap();
    let full_root = staged.wrapper(
        "mods/flash-note",
        r#"
            this.targetMode = "new";
            this.selectPreset("codex");
            this.sendScope = "all";
            this.sendIdea(cx);
        "#,
    );
    let full_win = cx.add_window(|_, _| Empty);
    full_win
        .update(cx, |_, window, cx| {
            let mut session = DescriptionSession::load(full_rt, full_root, full_policy, window, cx).unwrap();
            let _ = session.describe(window, cx).unwrap();
            window.remove_window();
        })
        .unwrap();
    cx.run_until_parked();

    let tab_new_full: Vec<Value> = full_calls
        .borrow()
        .effects
        .iter()
        .filter(|(name, args)| name == "run" && args.first().and_then(Value::as_str) == Some("tab.new"))
        .map(|(_, args)| args[1].clone())
        .collect();
    assert_eq!(tab_new_full.len(), 1, "expected 1 whole-group tab.new dispatch: {tab_new_full:?}");
    assert_eq!(
        tab_new_full[0]["task"],
        json!("[Sprint Ideas]\n\n1. Optimize terminal scroll\n\n2. Render mermaid diagrams\n\n3. Support LaTeX formulas"),
    );
    assert!(
        !full_calls.borrow().effects.iter().any(|(name, _)| name == "closeOverlay"),
        "sending the whole group must keep the composer overlay open until closed via the top-right X button",
    );
}

#[gpui::test]
fn files_open_request_loads_external_path_and_peek_ctrl_clicks_files(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    // 1. Files mod consumes shared("files.open_request") for an external absolute path outside workspace root.
    let calls_files = Rc::new(RefCell::new(Calls::default()));
    {
        let mut c = calls_files.borrow_mut();
        c.files.insert(
            "/tmp/external-notes/sample.rs".into(),
            "fn one() {}\nfn two() {}\nfn three() {}\n".into(),
        );
        c.shared.insert(
            "files.open_request".into(),
            json!({ "seq": 1, "path": "/tmp/external-notes/sample.rs", "line": 2, "end_line": 2 }),
        );
    }
    let files_values = fixture(false, json!({}), updates("current", "current"));
    let files_policy = Policy::new()
        .with_storage_path(staged.0.join("store-files-ext.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(files_values, calls_files.clone()))
        .unwrap();
    let files_rt = ShellRuntime::new_isolated().unwrap();
    let files_root = staged.wrapper("mods/files", "");
    let w1 = cx.add_window(|_, _| Empty);
    let mut tree = String::new();
    w1.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(files_rt, files_root, files_policy, window, cx).unwrap();
        let _ = session.describe(window, cx).unwrap();
        tree = session.describe(window, cx).unwrap();
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();
    assert!(tree.contains("sample.rs"), "external filename missing in files toolbar: {tree}");
    assert!(tree.contains("/tmp/external-notes"), "external dir badge missing in files toolbar: {tree}");

    // 2. Peek mod strictly requires Ctrl+left-click for file links and dispatches `files.open`.
    let calls_peek = Rc::new(RefCell::new(Calls::default()));
    let mut peek_values = fixture(true, json!({ "terminal": "a1", "acp": true }), updates("current", "current"));
    peek_values.insert(
        "terminals".into(),
        json!([{ "address": "a1", "acp": true, "state": "live", "cwd": "/fixture/project", "status": "idle" }]),
    );
    let peek_policy = Policy::new()
        .with_storage_path(staged.0.join("store-peek-link.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(peek_values, calls_peek.clone()))
        .unwrap();
    let peek_rt = ShellRuntime::new_isolated().unwrap();
    let peek_root = staged.wrapper(
        "mods/peek",
        r#"
            this.handleMarkdownLinkClick("herdr-file://src/window.rs:42", { button: "left", ctrl: false, meta: false, shift: false, alt: false }, cx);
            this.handleMarkdownLinkClick("herdr-file://src/window.rs:42-48", { button: "left", ctrl: true, meta: false, shift: false, alt: false }, cx);
        "#,
    );
    let w2 = cx.add_window(|_, _| Empty);
    w2.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(peek_rt, peek_root, peek_policy, window, cx).unwrap();
        let _ = session.describe(window, cx).unwrap();
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();

    let open_calls: Vec<_> = calls_peek
        .borrow()
        .effects
        .iter()
        .filter(|(name, args)| name == "run" && args.first().and_then(Value::as_str) == Some("files.open"))
        .cloned()
        .collect();
    assert_eq!(open_calls.len(), 1, "only Ctrl+click should trigger files.open: {open_calls:?}");
    let args = &open_calls[0].1[1];
    assert_eq!(args["path"], json!("src/window.rs"));
    assert_eq!(args["line"], json!(42));
    assert_eq!(args["end_line"], json!(48));
    assert_eq!(args["cwd"], json!("/fixture/project"));
}

#[gpui::test]
fn files_switching_between_cached_code_and_markdown_via_open_request_never_errors_in_render(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    let calls = Rc::new(RefCell::new(Calls::default()));
    {
        let mut c = calls.borrow_mut();
        c.files.insert(
            "/tmp/switch-a.rs".into(),
            "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n".into(),
        );
        c.files.insert(
            "/tmp/switch-b.md".into(),
            "# Title\n\nBody paragraph.\n".into(),
        );
        c.files.insert(
            "/tmp/switch-c.js".into(),
            "export const ok = 42;\n".into(),
        );
        c.shared.insert(
            "files.open_request".into(),
            json!({ "seq": 1, "path": "/tmp/switch-a.rs", "line": 2, "end_line": 2 }),
        );
    }

    let values = fixture(false, json!({}), updates("current", "current"));
    let policy = Policy::new()
        .with_storage_path(staged.0.join("store-files-switch.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(values, calls.clone()))
        .unwrap();
    let rt = ShellRuntime::new_isolated().unwrap();
    let root = staged.wrapper("mods/files", "");
    let w = cx.add_window(|_, _| Empty);

    let mut session_slot = None;
    w.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(rt, root, policy, window, cx).unwrap();
        let _ = session.describe(window, cx).unwrap();
        session_slot = Some(session);
    })
    .unwrap();
    cx.run_until_parked();

    let mut session = session_slot.unwrap();
    let mut tree_a = String::new();
    w.update(cx, |_, window, cx| {
        tree_a = session.describe(window, cx).unwrap();
    })
    .unwrap();
    assert!(tree_a.contains("switch-a.rs"), "initial file A missing: {tree_a}");

    // Switch to Markdown file B (releases code EditorState).
    calls.borrow_mut().shared.insert(
        "files.open_request".into(),
        json!({ "seq": 2, "path": "/tmp/switch-b.md" }),
    );
    w.update(cx, |_, window, cx| {
        let _ = session.describe(window, cx).unwrap();
    })
    .unwrap();
    cx.run_until_parked();
    let mut tree_b = String::new();
    w.update(cx, |_, window, cx| {
        tree_b = session.describe(window, cx).unwrap();
    })
    .unwrap();
    assert!(tree_b.contains("switch-b.md"), "file B missing after switch: {tree_b}");

    // Switch BACK to cached Rust file A with a line jump: previously threw
    // `EditorState.new(...) cannot run during render` inside `render(cx)`.
    calls.borrow_mut().shared.insert(
        "files.open_request".into(),
        json!({ "seq": 3, "path": "/tmp/switch-a.rs", "line": 3, "end_line": 3 }),
    );
    w.update(cx, |_, window, cx| {
        let _ = session.describe(window, cx).unwrap();
    })
    .unwrap();
    cx.run_until_parked();
    let mut tree_a2 = String::new();
    w.update(cx, |_, window, cx| {
        tree_a2 = session.describe(window, cx).unwrap();
    })
    .unwrap();
    assert!(tree_a2.contains("switch-a.rs"), "cached file A missing after switch back: {tree_a2}");
    assert!(!tree_a2.contains("Loading file"), "cached file A stuck loading: {tree_a2}");
    assert!(!tree_a2.contains("Preparing editor"), "cached file A editor failed to build: {tree_a2}");

    // Switch to JS file C (different language signature) and back to cached A.
    calls.borrow_mut().shared.insert(
        "files.open_request".into(),
        json!({ "seq": 4, "path": "/tmp/switch-c.js" }),
    );
    w.update(cx, |_, window, cx| {
        let _ = session.describe(window, cx).unwrap();
    })
    .unwrap();
    cx.run_until_parked();
    let mut tree_c = String::new();
    w.update(cx, |_, window, cx| {
        tree_c = session.describe(window, cx).unwrap();
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();
    assert!(tree_c.contains("switch-c.js"), "file C missing after switch: {tree_c}");
    assert!(!tree_c.contains("Loading file"), "file C stuck loading: {tree_c}");
    assert!(!tree_c.contains("Preparing editor"), "file C editor failed to build: {tree_c}");
}

#[gpui::test]
fn files_wakes_via_next_command_open_request_without_refresh(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    let calls = Rc::new(RefCell::new(Calls::default()));
    {
        let mut c = calls.borrow_mut();
        c.files.insert("/tmp/wake-a.rs".into(), "fn wake_a() {}\n".into());
        c.files.insert("/tmp/wake-b.py".into(), "def wake_b():\n    return 1\n".into());
    }

    let (cmd_tx, cmd_rx) = async_channel::unbounded::<String>();
    let values = fixture(false, json!({}), updates("current", "current"));
    let host_mod = module(values, calls.clone()).async_function("nextCommand", move |_| {
        let rx = cmd_rx.clone();
        Ok(async move {
            rx.recv()
                .await
                .map(HostValue::Str)
                .map_err(|_| gpui_shell::HostError::new("closed"))
        })
    });
    let policy = Policy::new()
        .with_storage_path(staged.0.join("store-files-wake.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(host_mod)
        .unwrap();
    let rt = ShellRuntime::new_isolated().unwrap();
    let root = staged.wrapper("mods/files", "");
    let w = cx.add_window(|_, _| Empty);

    let mut session_slot = None;
    w.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(rt, root, policy, window, cx).unwrap();
        let initial = session.describe(window, cx).unwrap();
        assert!(initial.contains("Select a file"), "expected empty detail pane initially: {initial}");
        session_slot = Some(session);
    })
    .unwrap();
    cx.run_until_parked();

    // Deliver open_request ONLY via nextCommand() — no runtime.refresh() before run_until_parked().
    calls.borrow_mut().shared.insert(
        "files.open_request".into(),
        json!({ "seq": 1, "path": "/tmp/wake-a.rs", "line": 1, "end_line": 1 }),
    );
    cmd_tx.try_send("open_request".into()).unwrap();
    cx.run_until_parked();

    let mut session = session_slot.unwrap();
    let mut tree_a = String::new();
    w.update(cx, |_, window, cx| {
        tree_a = session.describe(window, cx).unwrap();
    })
    .unwrap();
    assert!(tree_a.contains("wake-a.rs"), "file A did not open via nextCommand: {tree_a}");
    assert!(!tree_a.contains("Select a file"), "detail pane stayed empty: {tree_a}");

    // Switch to file B via nextCommand() alone.
    calls.borrow_mut().shared.insert(
        "files.open_request".into(),
        json!({ "seq": 2, "path": "/tmp/wake-b.py", "line": 2, "end_line": 2 }),
    );
    cmd_tx.try_send("open_request".into()).unwrap();
    cx.run_until_parked();

    let mut tree_b = String::new();
    w.update(cx, |_, window, cx| {
        tree_b = session.describe(window, cx).unwrap();
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();
    assert!(tree_b.contains("wake-b.py"), "file B did not open via nextCommand: {tree_b}");
    assert!(!tree_b.contains("Loading file"), "file B stuck loading: {tree_b}");
}

#[gpui::test]
fn files_switching_agent_a_to_b_and_back_to_a_restores_open_file_without_hanging_on_loading(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    let calls = Rc::new(RefCell::new(Calls::default()));
    {
        let mut c = calls.borrow_mut();
        c.files.insert("/fixture/agent-a/src/app.rs".into(), "fn from_agent_a() {}\n".into());
        c.files.insert("/fixture/agent-b/README.md".into(), "# Agent B\n".into());
    }

    // Dynamic host module so we can switch active space/focused agent between renders.
    let active_agent = Rc::new(RefCell::new("a"));
    let mut base = fixture(false, json!({}), updates("current", "current"));
    let base_mod = module(base.clone(), calls.clone());
    let _ = base_mod;
    let a_flag = active_agent.clone();
    let a_flag2 = active_agent.clone();
    let a_flag3 = active_agent.clone();
    let values_ref = Rc::new(RefCell::new(std::mem::take(&mut base)));
    let make_dyn_module = {
        let calls = calls.clone();
        let values_ref = values_ref.clone();
        move || {
            let mut m = module(values_ref.borrow().clone(), calls.clone());
            let f1 = a_flag.clone();
            m = m.function("spaces", move |_| {
                let is_a = *f1.borrow() == "a";
                Ok(to_host(&json!([
                    { "id": "sa", "name": "Agent A", "folder": "/fixture/agent-a", "machine": null, "active": is_a, "index": 0 },
                    { "id": "sb", "name": "Agent B", "folder": "/fixture/agent-b", "machine": null, "active": !is_a, "index": 1 }
                ])))
            });
            let f2 = a_flag2.clone();
            m = m.function("panes", move |_| {
                let is_a = *f2.borrow() == "a";
                Ok(to_host(&json!([
                    { "pane": "pa", "terminal": "ta", "cwd": "/fixture/agent-a", "focused": is_a },
                    { "pane": "pb", "terminal": "tb", "cwd": "/fixture/agent-b", "focused": !is_a }
                ])))
            });
            let f3 = a_flag3.clone();
            m = m.function("focused", move |_| {
                let is_a = *f3.borrow() == "a";
                Ok(to_host(&if is_a {
                    json!({ "pane": "pa", "terminal": "ta", "cwd": "/fixture/agent-a", "agent": "ta" })
                } else {
                    json!({ "pane": "pb", "terminal": "tb", "cwd": "/fixture/agent-b", "agent": "tb" })
                }))
            });
            m
        }
    };

    let policy = Policy::new()
        .with_storage_path(staged.0.join("store-files-agent-switch.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(make_dyn_module())
        .unwrap();
    let rt = ShellRuntime::new_isolated().unwrap();
    // Open `src/app.rs` in Agent A during init.
    let root = staged.wrapper(
        "mods/files",
        r#"
            this.show("src/app.rs", cx);
        "#,
    );
    let w = cx.add_window(|_, _| Empty);

    let mut session_slot = None;
    w.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(rt, root, policy, window, cx).unwrap();
        let _ = session.describe(window, cx).unwrap();
        session_slot = Some(session);
    })
    .unwrap();
    cx.run_until_parked();

    let mut session = session_slot.unwrap();
    let mut tree_a1 = String::new();
    w.update(cx, |_, window, cx| {
        tree_a1 = session.describe(window, cx).unwrap();
    })
    .unwrap();
    assert!(tree_a1.contains("app.rs"), "Agent A initial file missing: {tree_a1}");
    assert!(!tree_a1.contains("Loading file"), "Agent A initial file stuck loading: {tree_a1}");

    // Switch to Agent B.
    *active_agent.borrow_mut() = "b";
    w.update(cx, |_, window, cx| {
        let _ = session.describe(window, cx).unwrap();
    })
    .unwrap();
    cx.run_until_parked();

    // Switch back to Agent A: previously `show(saved.file, cx)` ran inside render() and threw
    // on `cx.notify()` after setting `loadingFile = true`, leaving "Loading file…" forever.
    *active_agent.borrow_mut() = "a";
    w.update(cx, |_, window, cx| {
        let _ = session.describe(window, cx).unwrap();
    })
    .unwrap();
    cx.run_until_parked();
    let mut tree_a2 = String::new();
    w.update(cx, |_, window, cx| {
        tree_a2 = session.describe(window, cx).unwrap();
        window.remove_window();
    })
    .unwrap();
    cx.run_until_parked();

    assert!(tree_a2.contains("app.rs"), "Agent A restored file missing: {tree_a2}");
    assert!(!tree_a2.contains("Loading file"), "Agent A restored file stuck on Loading file: {tree_a2}");
    assert!(!tree_a2.contains("Preparing editor"), "Agent A restored editor failed to build: {tree_a2}");
}

#[gpui::test]
fn catchup_briefing_semantic_naming_and_ai_switch_render_cleanly(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let claude = json!({"key":"claude","name":"Claude Code","icon":null,"tint":false,"tile":"#1c1d21","color":"#ffffff","shell":false,"agent":true});

    let agents_list = json!([
        {"address":"a1","acp":true,"pane":null,"space":"s1","space_name":"API","tab":null,"tab_index":0,"tab_name":"headless","status":"blocked","program":claude,"title":"Claude Code","semantic_title":"修复登录超时","micro_summary":"等待审批 cargo test","seq":10,"pinned":false,"focused":false},
        {"address":"wsl/a2","acp":true,"pane":null,"space":"s1","space_name":"API","tab":null,"tab_index":0,"tab_name":"headless","status":"blocked","program":claude,"title":"Claude Code","semantic_title":"迁移存储层","micro_summary":"等待审批 cargo test","seq":11,"pinned":false,"focused":false},
        {"address":"t1","acp":false,"pane":"p1","space":"s1","space_name":"API","tab":"tab1","tab_index":1,"tab_name":"1","status":"done","program":claude,"title":"Claude Code","semantic_title":"重构缓存键","micro_summary":"已完成 3/3 个文件迁移并通过单测","seq":12,"pinned":false,"focused":false}
    ]);
    let terminals_list = json!([
        {"address":"a1","acp":true,"state":"live","status":"blocked","semantic_title":"修复登录超时","micro_summary":"等待审批 cargo test","pending_permission":{"request_id":"r1","title":"Run cargo test auth","kind":"execute","options":[]},"labels":{},"cwd":"/repo/api","program":claude},
        {"address":"wsl/a2","acp":true,"state":"live","status":"blocked","semantic_title":"迁移存储层","micro_summary":"等待审批 cargo test","pending_permission":{"request_id":"r2","title":"Run cargo test storage","kind":"execute","options":[]},"labels":{},"cwd":"/repo/api","program":claude},
        {"address":"t1","acp":false,"state":"live","status":"done","semantic_title":"重构缓存键","micro_summary":"已完成 3/3 个文件迁移并通过单测","labels":{},"cwd":"/repo/api","program":claude}
    ]);
    let spaces_list = json!([
        {"id":"s1","index":1,"name":"API","folder":"/repo/api","machine":null,"branch":"feat/cache","active":true,"status":"blocked","tabs":[{"id":"tab1","index":1,"name":"1","named":false,"active":false,"status":"done","panes":1,"zoomed":false,"settled":false,"settled_at":null}]}
    ]);
    let panes_list = json!([
        {"pane":"p1","terminal":"t1","tab":"tab1","title":"Claude Code","semantic_title":"重构缓存键","micro_summary":"已完成 3/3 个文件迁移并通过单测","cwd":"/repo/api","branch":"feat/cache","status":"done","program":claude,"focused":false,"zoomed":false,"movable":false}
    ]);

    // 1. Agents sidebar displays semantic titles and Catch-up Briefing trigger button.
    let mut sidebar_vals = fixture(true, json!({}), updates("current", "current"));
    sidebar_vals.get_mut("settings").unwrap()["language"] = json!("zh-CN");
    sidebar_vals.insert("agents".into(), agents_list.clone());
    sidebar_vals.insert("terminals".into(), terminals_list.clone());
    sidebar_vals.insert("spaces".into(), spaces_list.clone());
    sidebar_vals.insert("panes".into(), panes_list.clone());
    let sidebar_tree = describe_with_values(cx, &staged, "mods/agents", sidebar_vals, None).unwrap();
    for expected in ["ag-briefing", "速览", "修复登录超时", "迁移存储层", "重构缓存键"] {
        assert!(sidebar_tree.contains(expected), "Agents sidebar missing {expected}: {sidebar_tree}");
    }

    // 2. Catch-up Briefing dialog renders clustered batch approvals, Drove downstream impact, and ready-to-review diff actions.
    let mut brief_vals = fixture(true, json!({ "briefing": true, "awayMinutes": 15, "overlay": "ov-br" }), updates("current", "current"));
    brief_vals.insert("checking".into(), json!(true));
    brief_vals.get_mut("settings").unwrap()["language"] = json!("zh-CN");
    brief_vals.insert("agents".into(), agents_list);
    brief_vals.insert("terminals".into(), terminals_list.clone());
    brief_vals.insert("spaces".into(), spaces_list.clone());
    brief_vals.insert("panes".into(), panes_list.clone());
    let brief_tree = describe_with_values(
        cx,
        &staged,
        "mods/agents",
        brief_vals,
        Some("this.coreBriefing = { generatedMs: 2000, headline: '核心 schema 失败正阻塞 3 个下游任务；2 个测试审批可一键全批。', headlineFromAi: true, criticalBlockers: [{ id: 'drove_task:t1', kind: 'drove_blocker', title: 'schema: Update OpenRPC', summary: '正阻塞 3 个下游任务 · compile error', status: 'error', blockedDownstreamCount: 3, runId: 'run_1', taskKey: 'schema' }], permissionClusters: [], pendingDecisions: [], readyToReview: [], workingItems: [] };"),
    ).unwrap();
    for expected in ["ag-briefing-dialog", "全局进展速览", "离开 15 分钟", "阻塞 +3 下游", "一键全批 (2)", "检视 Diff", "重构缓存键"] {
        assert!(brief_tree.contains(expected), "Briefing dialog missing {expected}: {brief_tree}");
    }

    // 3. Unnamed tab adopts semantic_title in tabs bar.
    let mut tabs_vals = fixture(true, json!({}), updates("current", "current"));
    tabs_vals.get_mut("settings").unwrap()["language"] = json!("zh-CN");
    tabs_vals.insert("spaces".into(), spaces_list);
    tabs_vals.insert("panes".into(), panes_list);
    tabs_vals.insert("terminals".into(), terminals_list);
    let tabs_tree = describe_with_values(cx, &staged, "mods/tabs", tabs_vals, None).unwrap();
    assert!(tabs_tree.contains("重构缓存键"), "Unnamed tab should adopt semantic_title: {tabs_tree}");

    // 4. Settings -> AI Assistant exposes the new Semantic Naming & Hover Micro-Summary toggle.
    let mut ai_vals = fixture(true, json!({ "section": "ai" }), updates("current", "current"));
    ai_vals.get_mut("settings").unwrap()["language"] = json!("zh-CN");
    let ai_tree = describe_with_values(
        cx,
        &staged,
        "frame/settings",
        ai_vals,
        Some("this.aiStatus = { enabled: true, configured: true, baseUrl: 'https://api.example.com/v1', model: 'gpt-4o-mini', effort: 'low', switches: { ptyStatusArbitration: true, notesDigest: true, promptCacheEnabled: true, semanticSummary: true }, usage: { totalCalls: 1, localCacheHits: 0, sessionsReused: 0, activeSessions: 0, inputTokens: 1000, cachedInputTokens: 0, outputTokens: 50, cacheHitRatio: 0 } };"),
    )
    .unwrap();
    assert!(ai_tree.contains("st-ai-sw-semantic"), "AI settings missing st-ai-sw-semantic: {ai_tree}");
    assert!(ai_tree.contains("语义命名与悬停微摘要"), "AI settings missing zh label: {ai_tree}");
}

#[gpui::test]
fn working_cli_peek_renders_large_transcript_within_quickjs_budget(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let program = json!({"key":"antigravity", "name":"Antigravity", "icon":null, "tint":false, "tile":"#1c1d21", "color":"#ffffff", "shell":false, "agent":true});
    let mut pty_values = fixture(true, json!({"terminal":"t_large_working","acp":true,"pty":true,"tab":true,"pane":"p1"}), updates("current", "current"));
    pty_values.insert("terminals".into(), json!([
        {"address":"t_large_working","name":"Antigravity","title":"Antigravity","argv":["agy"],"parent":null,"labels":{"herdr.view":"peek"},"state":"live","status":"working","turn":26,"program":program,"cwd":"/home/zzh0838/Project/herdr2","working_since":1795267674185i64}
    ]));
    pty_values.insert("agents".into(), json!([
        {"address":"t_large_working","pane":"p1","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Antigravity","status":"working","app":"antigravity","program":program,"title":"Antigravity","seq":1,"focused":true}
    ]));
    pty_values.insert("panes".into(), json!([
        {"pane":"p1","terminal":"t_large_working","tab":"tab1","title":"Antigravity","cwd":"/home/zzh0838/Project/herdr2","status":"working","program":program,"structured":true,"focused":true,"zoomed":false,"movable":false}
    ]));

    let calls = Rc::new(RefCell::new(Calls::default()));
    let policy = Policy::new()
        .with_storage_path(staged.0.join("store-large-working-cli.json"))
        .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
        .with_host_module(module(pty_values, calls.clone()))
        .unwrap();
    let runtime = ShellRuntime::new_isolated().unwrap();
    let root = staged.0.join("mods/peek");
    let window = cx.add_window(|_, _| Empty);
    let mut session = window.update(cx, |_, window, cx| {
        let mut session = DescriptionSession::load(runtime, root, policy, window, cx).unwrap();
        let _ = session.describe(window, cx).unwrap();
        session
    }).unwrap();
    cx.run_until_parked();
    let tree = window.update(cx, |_, window, cx| session.describe(window, cx)).unwrap().unwrap();
    assert!(!tree.contains("acp-error-banner"), "working CLI peek should render without error banner: {tree}");
    assert!(tree.contains("acp-load-earlier"), "long transcript should window earlier items: {tree}");
    assert!(tree.contains("acp-grp-more-"), "expanded working activity group should cap visible steps: {tree}");
    assert!(tree.contains("acp-working-tail"), "working CLI peek should show active working tail: {tree}");
    assert!(tree.contains("acp-stop"), "working CLI peek should render stop button: {tree}");
}

#[gpui::test]
fn handoff_model_dropdown_scrolls_long_model_lists(cx: &mut TestAppContext) {
    use gpui::VisualTestContext;
    use std::ops::Deref;

    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let mut vals = fixture(true, json!({"source":"t1","kind":"fork","overlay":"ov-ho"}), updates("current", "current"));
    vals.insert("checking".into(), json!(true));
    let setup = "this.modelPickerOpen = true; this.preset = 'codex'; this.agentCatalog = { codex: { rawModels: Array.from({length: 12}, (_, i) => `gpt-5.${i}`), defaultModel: 'gpt-5.0', efforts: ['low', 'medium', 'high'] } };";
    let tree = describe_with_values(cx, &staged, "mods/handoff", vals.clone(), Some(setup)).unwrap();
    let dropdown = tree.lines().find(|line| line.contains("ho-model-dropdown")).unwrap();
    assert!(dropdown.contains("on_scroll_wheel"), "dropdown wrapper must stop scroll chaining: {tree}");
    let scroll = tree.lines().find(|line| line.contains("ho-model-list")).unwrap();
    assert!(scroll.contains("overflow_y_scrollbar"), "model list must use native vertical scrollbar: {tree}");
    assert!(scroll.contains(".h[Number(180.0)]"), "model list needs explicit viewport height to scroll inside fit-content dialog: {tree}");
    assert!(!scroll.contains("on_scroll_wheel"), "scroller itself must not intercept wheel before native scroll: {tree}");
    let opt = tree.lines().find(|line| line.contains("ho-model-opt-11")).unwrap();
    assert!(opt.contains(".flex_none"), "model option rows must not shrink into viewport: {tree}");

    // End-to-end visual scroll verification under `fit_content = true` (matching `open_overlay`
    // for Dialog): wheel-scrolling over `ho-model-list` must actually move the viewport so a
    // bottom model (gpt-5.8..gpt-5.11) becomes clickable and triggers `selectModel`.
    let calls = Rc::new(RefCell::new(Calls::default()));
    std::fs::write(
        staged.0.join("main.js"),
        format!(
            "import Real from './mods/handoff/main.js'; import {{ setShared }} from 'herdr'; export default class S extends Real {{ init(p, cx) {{ super.init(p, cx); {setup} }} selectModel(m) {{ super.selectModel(m); setShared('test.picked', m); }} }}"
        ),
    )
    .unwrap();
    let wrapper_dir = staged.0.clone();
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-ho-scroll.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(vals, calls.clone()))
            .unwrap(),
    );
    let runtime = ShellRuntime::new_isolated().unwrap();
    struct FitDialog(gpui::Entity<gpui_shell::ShellRoot>);
    impl gpui::Render for FitDialog {
        fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
            use gpui::{ParentElement as _, Styled as _};
            gpui::div()
                .w(gpui::px(580.))
                .max_h(gpui::px(760.))
                .flex()
                .flex_col()
                .child(self.0.clone())
        }
    }
    let window = cx.add_window(|window, cx| {
        let root = runtime.try_load(&wrapper_dir, window, cx).unwrap();
        root.update(cx, |r, cx| r.set_fit_content(true, cx));
        FitDialog(root)
    });
    let mut visual = VisualTestContext::from_window(*window.deref(), cx);
    for _ in 0..3 {
        visual.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
    }
    for y in [260.0_f32, 300.0, 340.0, 380.0, 420.0] {
        let pt = gpui::point(gpui::px(220.), gpui::px(y));
        visual.simulate_mouse_move(pt, None, Default::default());
        visual.simulate_event(gpui::ScrollWheelEvent {
            position: pt,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-500.))),
            ..Default::default()
        });
    }
    for _ in 0..3 {
        visual.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
    }
    // Click near the bottom of the scrolled list to pick one of the later models (gpt-5.8..gpt-5.11).
    let mut picked = None;
    for y in [440.0_f32, 420.0, 400.0, 380.0, 360.0, 340.0, 320.0, 300.0, 280.0] {
        visual.simulate_click(gpui::point(gpui::px(220.), gpui::px(y)), Default::default());
        visual.run_until_parked();
        if let Some(found) = calls.borrow().shared.get("test.picked").cloned() {
            picked = Some(found.to_string());
            break;
        }
    }
    let picked_str = picked.unwrap_or_default();
    assert!(
        picked_str.contains("gpt-5.8") || picked_str.contains("gpt-5.9") || picked_str.contains("gpt-5.10") || picked_str.contains("gpt-5.11"),
        "scrolling ho-model-list must bring later models (gpt-5.8..gpt-5.11) into clickable view, got: {picked_str:?}"
    );
    visual.update(|window, _| window.remove_window());
    gpui_shell::policy::set_default(Policy::new());
    cx.run_until_parked();
}

#[gpui::test]
fn exited_cli_agent_hides_task_summary_timer_and_badges_in_tabs_and_pane_header(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let shell_prog = json!({
        "key": "shell",
        "name": "zsh",
        "icon": null,
        "tint": true,
        "tile": "#334455",
        "color": "#aabbcc",
        "shell": true,
        "agent": false
    });
    let mut values = fixture(true, json!({"pane":"p1","terminal":"t1"}), updates("current", "current"));
    // Simulate a terminal whose CLI agent exited back to zsh even if stale agent metadata is still present.
    values.insert("spaces".into(), json!([
        {
            "id": "s1",
            "index": 1,
            "name": "Local",
            "folder": "/fixture/project",
            "machine": null,
            "branch": "main",
            "active": true,
            "status": "done",
            "tabs": [{
                "id": "tab1",
                "index": 1,
                "name": "1",
                "named": false,
                "active": true,
                "status": "done",
                "panes": 1,
                "zoomed": false,
                "settled": false,
                "settled_at": null,
                "changed_at": 1700000000000i64,
                "working_since": null,
                "work_ms": 42000,
                "total_work_ms": 42000
            }]
        }
    ]));
    values.insert("panes".into(), json!([{
        "pane": "p1",
        "terminal": "t1",
        "tab": "tab1",
        "title": "zsh",
        "semantic_title": "Stale CLI task title",
        "micro_summary": "Stale CLI task progress",
        "ai_status_summary": "Stale AI status",
        "cwd": "/fixture/project",
        "branch": "main",
        "status": null,
        "app": null,
        "program": shell_prog,
        "structured": false,
        "working_since": null,
        "work_ms": 42000,
        "total_work_ms": 42000,
        "focused": true,
        "zoomed": false
    }]));
    values.insert("terminals".into(), json!([{
        "address": "t1",
        "name": null,
        "title": "zsh",
        "argv": ["zsh"],
        "parent": null,
        "labels": {},
        "state": "live",
        "status": null,
        "turn": 3,
        "semantic_title": "Stale CLI task title",
        "micro_summary": "Stale CLI task progress",
        "ai_status_summary": "Stale AI status",
        "program": shell_prog,
        "cwd": "/fixture/project"
    }]));

    let tabs_tree = describe_with_values(cx, &staged, "mods/tabs", values.clone(), None).unwrap();
    assert!(!tabs_tree.contains("Stale CLI task title"), "tab bar must not show stale semantic title after CLI exit: {tabs_tree}");
    assert!(!tabs_tree.contains("Stale CLI task progress"), "tab bar must not show stale micro summary after CLI exit: {tabs_tree}");
    assert!(!tabs_tree.contains("tab-time-tab1"), "tab bar must not show execution timer after CLI exit: {tabs_tree}");
    assert!(!tabs_tree.contains("\"CLI\"") && !tabs_tree.contains("\"终端\""), "tab bar must not show CLI badge after CLI exit: {tabs_tree}");
    assert!(tabs_tree.contains("zsh"), "tab bar should revert to shell program name: {tabs_tree}");

    let pane_tree = describe_with_values(cx, &staged, "mods/pane", values, None).unwrap();
    assert!(!pane_tree.contains("Stale CLI task title"), "pane header must not show stale semantic title after CLI exit: {pane_tree}");
    assert!(!pane_tree.contains("Stale CLI task progress"), "pane header must not show stale micro summary after CLI exit: {pane_tree}");
    assert!(!pane_tree.contains("Stale AI status"), "pane header must not show stale AI status after CLI exit: {pane_tree}");
    assert!(!pane_tree.contains("ph-type-p1"), "pane header must not show CLI type badge after CLI exit: {pane_tree}");
    assert!(!pane_tree.contains("ph-time-p1"), "pane header must not show execution timer after CLI exit: {pane_tree}");
    assert!(!pane_tree.contains("ph-addr-p1"), "pane header must not show agent id badge after CLI exit: {pane_tree}");
    assert!(!pane_tree.contains("ph-ho-p1"), "pane header must not show handoff button after CLI exit: {pane_tree}");
    assert!(pane_tree.contains("Interactive terminal"), "pane header summary should revert to Interactive terminal: {pane_tree}");
}

#[gpui::test]
fn fresh_cli_agent_tab_ignores_launch_command_osc_title(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let agy_prog = json!({
        "key": "antigravity",
        "name": "Antigravity",
        "icon": "icons/antigravity.svg",
        "tint": false,
        "shell": false,
        "agent": true
    });
    let mut values = fixture(true, json!({"pane":"p1","terminal":"t1"}), updates("current", "current"));
    values.insert("spaces".into(), json!([{
        "id": "s1",
        "name": "Local",
        "folder": "/fixture/project",
        "active": true,
        "tabs": [{
            "id": "tab1",
            "name": "Antigravity",
            "named": true,
            "active": true,
            "status": "idle",
            "zoomed": false
        }]
    }]));
    values.insert("panes".into(), json!([{
        "pane": "p1",
        "terminal": "t1",
        "tab": "tab1",
        "title": "agy --dangerously-skip-permissions",
        "cwd": "/fixture/project",
        "branch": "main",
        "status": "idle",
        "program": agy_prog,
        "focused": true,
        "zoomed": false
    }]));
    values.insert("terminals".into(), json!([{
        "address": "t1",
        "name": null,
        "title": "agy --dangerously-skip-permissions",
        "argv": ["agy", "--dangerously-skip-permissions"],
        "parent": null,
        "labels": {},
        "state": "live",
        "status": "idle",
        "turn": 0,
        "program": agy_prog,
        "cwd": "/fixture/project"
    }]));

    let tabs_tree = describe_with_values(cx, &staged, "mods/tabs", values, None).unwrap();
    assert!(tabs_tree.contains("Antigravity"), "fresh CLI tab should display program name: {tabs_tree}");
    assert!(
        !tabs_tree.contains("agy --dangerously-skip-permissions"),
        "fresh CLI tab must not display raw shell preexec launch command: {tabs_tree}"
    );
}

#[gpui::test]
fn changes_renders_git_branch_status_and_recent_log_without_errors(cx: &mut TestAppContext) {
    use gpui::VisualTestContext;
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    // 1) Branch info + git status summary + staged file pill + recent git log.
    let setup = r#"
        this.root = '/repo/demo';
        this.on = null;
        this.context = this.viewKey();
        this.hasHead = true;
        this.gitBranch = {
            name: 'feat/git-panel',
            detached: false,
            hasHead: true,
            headOid: '8962c5907e763c13a7f1a885e20dbc7a2bc2ffca',
            headShort: '8962c59',
            upstream: 'origin/feat/git-panel',
            upstreamGone: false,
            ahead: 2,
            behind: 1,
        };
        this.gitStatus = {
            staged: 1,
            unstaged: 1,
            untracked: 1,
            conflicts: 0,
            total: 3,
            clean: false,
        };
        this.stashCount = 1;
        this.files = [
            { path: 'src/api.rs', status: 'M', staged: true, unstaged: false, fresh: false, add: 4, del: 1 },
            { path: 'src/new.rs', status: 'A', staged: false, unstaged: false, fresh: true, add: 12, del: 0 },
        ];
        this.gitLog = [
            { hash: '8962c5907e763c13a7f1a885e20dbc7a2bc2ffca', shortHash: '8962c59', subject: 'feat(pty): track CLI foreground', author: 'Alice', relTime: '3m ago', refs: ['HEAD -> feat/git-panel'] },
            { hash: '0206eaeede86ac76fd67e79065cf054f0af33300', shortHash: '0206eae', subject: 'fix(ui): polish changes panel', author: 'Bob', relTime: '2h ago', refs: ['tag: v0.4.1'] },
        ];
        this.open = 'src/api.rs';
        this.setLines(['@@ -1,2 +1,3 @@', '-old', '+new', '+more']);
    "#;
    let tree_collapsed = describe_with_values(
        cx,
        &staged,
        "mods/changes",
        fixture(true, json!({}), updates("current", "current")),
        Some(setup),
    ).unwrap();
    assert!(tree_collapsed.contains("chg-git-log"), "collapsed recent git log bar must render: {tree_collapsed}");
    assert!(tree_collapsed.contains("feat(pty): track CLI foreground"), "collapsed recent git log must preview latest commit subject: {tree_collapsed}");
    assert!(!tree_collapsed.contains("chg-commit-0206eae"), "recent git log should default to collapsed: {tree_collapsed}");

    let expanded_setup = format!("{setup}\nthis.gitLogOpen = true;");
    let tree = describe_with_values(
        cx,
        &staged,
        "mods/changes",
        fixture(true, json!({}), updates("current", "current")),
        Some(&expanded_setup),
    ).unwrap();
    assert!(tree.contains("chg-git-info"), "git info bar must render: {tree}");
    assert!(tree.contains("feat/git-panel"), "branch name must render: {tree}");
    assert!(tree.contains("8962c59"), "short HEAD sha must render: {tree}");
    assert!(tree.contains("→ origin/feat/git-panel"), "upstream branch must render: {tree}");
    assert!(tree.contains("↑2"), "ahead badge must render: {tree}");
    assert!(tree.contains("↓1"), "behind badge must render: {tree}");
    assert!(tree.contains("stash 1"), "stash count badge must render: {tree}");
    assert!(tree.contains("chg-git-status-bar"), "git status bar must render: {tree}");
    assert!(tree.contains("1 staged"), "staged count pill must render: {tree}");
    assert!(tree.contains("1 modified"), "modified count pill must render: {tree}");
    assert!(tree.contains("1 untracked"), "untracked count pill must render: {tree}");
    assert!(tree.contains("chg-git-log"), "recent git log section must render: {tree}");
    assert!(tree.contains("chg-commit-8962c59"), "first commit row must render: {tree}");
    assert!(tree.contains("chg-commit-0206eae"), "second commit row must render: {tree}");
    assert!(tree.contains("feat(pty): track CLI foreground"), "commit subject must render: {tree}");
    // Verify chg-git-log renders full-width below changes-split, not inside narrow changes-sidebar.
    let split_pos = tree.find("changes-split").expect("changes-split");
    let detail_pos = tree.find("changes-detail").expect("changes-detail");
    let log_pos = tree.find("chg-git-log").expect("chg-git-log");
    assert!(log_pos > detail_pos && detail_pos > split_pos, "chg-git-log must render full-width below changes-split: {tree}");

    // 2) Commit preview mode + clean working tree + real GPUI draw().
    let preview_setup = r#"
        this.root = '/repo/demo';
        this.on = null;
        this.context = this.viewKey();
        this.hasHead = true;
        this.gitBranch = {
            name: 'master',
            detached: false,
            hasHead: true,
            headOid: '8962c5907e763c13a7f1a885e20dbc7a2bc2ffca',
            headShort: '8962c59',
            upstream: '',
            upstreamGone: false,
            ahead: 0,
            behind: 0,
        };
        this.gitStatus = { staged: 0, unstaged: 0, untracked: 0, conflicts: 0, total: 0, clean: true };
        this.files = [];
        this.gitLog = [
            { hash: '8962c5907e763c13a7f1a885e20dbc7a2bc2ffca', shortHash: '8962c59', subject: 'feat(pty): track CLI foreground', author: 'Alice', relTime: '3m ago', refs: ['HEAD -> master'] },
        ];
        this.selectedCommit = this.gitLog[0];
        this.setLines(['commit 8962c59', '@@ -1,1 +1,2 @@', '-old', '+new']);
    "#;
    let preview_tree = describe_with_values(
        cx,
        &staged,
        "mods/changes",
        fixture(true, json!({}), updates("current", "current")),
        Some(preview_setup),
    ).unwrap();
    assert!(preview_tree.contains("Clean working tree"), "clean working tree badge must render: {preview_tree}");
    assert!(preview_tree.contains("chg-commit-banner"), "commit preview banner must render: {preview_tree}");
    assert!(preview_tree.contains("chg-commit-close"), "commit preview back button must render: {preview_tree}");

    let calls = Rc::new(RefCell::new(Calls::default()));
    gpui_shell::policy::set_default(
        Policy::new()
            .with_storage_path(staged.0.join("store-changes-git-info.json"))
            .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
            .with_host_module(module(fixture(true, json!({}), updates("current", "current")), calls.clone()))
            .unwrap(),
    );
    let rt = ShellRuntime::new_isolated().unwrap();
    let wrapped = staged.wrapper("mods/changes", preview_setup);
    let win = cx.add_window(|window, cx| RootedView(rt.try_load(&wrapped, window, cx).unwrap()));
    let mut visual = VisualTestContext::from_window(win.into(), cx);
    visual.update(|window, cx| window.draw(cx).clear(cx));
    visual.run_until_parked();
    let err = win.read_with(cx, |r, cx| {
        r.0.read(cx)
            .content()
            .clone()
            .downcast::<gpui_shell::ScriptView>()
            .ok()
            .and_then(|v| v.read(cx).build_error().map(str::to_owned))
    });
    assert_eq!(err.unwrap(), None, "changes git info + log + commit preview must draw with zero errors");
    visual.update(|window, _| window.remove_window());
    cx.run_until_parked();

    // Verify external (out-of-project) ACP changed files render cleanly in sidebar + detail pane + split mode.
    let ext_setup = r#"
        this.root = '/fixture/repo';
        this.context = this.viewKey();
        this.files = [];
        this.externalFiles = [
            { path: '/tmp/buildlink-audit-claude/harness.py', displayPath: '/tmp/buildlink-audit-claude/harness.py', on: null, add: 3, del: 1, status: 'M', sourceKind: 'agent-diff', terminal: 'a65', turn: 1 },
            { path: '/tmp/buildlink-audit-claude/x0_smoke.py', displayPath: '/tmp/buildlink-audit-claude/x0_smoke.py', on: null, add: 15, del: 0, status: 'A', sourceKind: 'file-content', terminal: 'a65', turn: 1 },
        ];
        this.selectedExternal = this.externalFiles[0];
        this.setLines(['@@ -1,1 +1,3 @@', '-old_line()', '+new_line()', '+extra_line()']);
    "#;
    let ext_tree = describe_with_values(
        cx,
        &staged,
        "mods/changes",
        fixture(true, json!({}), updates("current", "current")),
        Some(ext_setup),
    ).unwrap();
    assert!(ext_tree.contains("changes-external-section"), "external files sidebar section must render: {ext_tree}");
    assert!(ext_tree.contains("chg-ext-banner"), "external file preview banner must render: {ext_tree}");
    assert!(ext_tree.contains("chg-ext-open-file"), "open in files button must render: {ext_tree}");
    assert!(ext_tree.contains("chg-ext-close"), "external preview back button must render: {ext_tree}");
    assert!(ext_tree.contains("/tmp/buildlink-audit-claude/harness.py"), "external path must be visible: {ext_tree}");

    let rt_ext = ShellRuntime::new_isolated().unwrap();
    let ext_wrapped = staged.wrapper("mods/changes", ext_setup);
    let win_ext = cx.add_window(|window, cx| RootedView(rt_ext.try_load(&ext_wrapped, window, cx).unwrap()));
    let mut visual_ext = VisualTestContext::from_window(win_ext.into(), cx);
    visual_ext.update(|window, cx| window.draw(cx).clear(cx));
    visual_ext.run_until_parked();
    let ext_err = win_ext.read_with(cx, |r, cx| {
        r.0.read(cx)
            .content()
            .clone()
            .downcast::<gpui_shell::ScriptView>()
            .ok()
            .and_then(|v| v.read(cx).build_error().map(str::to_owned))
    });
    assert_eq!(ext_err.unwrap(), None, "changes external file diff must draw with zero errors");
    visual_ext.update(|window, _| window.remove_window());
    cx.run_until_parked();

    // Verify in-project preview (e.g. already-committed or gitignored file inside repo) never renders as External.
    let in_proj_setup = r#"
        this.root = '/fixture/repo';
        this.context = this.viewKey();
        this.files = [];
        this.externalFiles = [];
        this.selectedExternal = {
            path: '/fixture/repo/src/committed.rs',
            displayPath: 'src/committed.rs',
            relPath: 'src/committed.rs',
            inProject: true,
            on: null,
            add: 2,
            del: 1,
            status: 'M',
            sourceKind: 'agent-diff',
            terminal: 'a65',
            turn: 1,
        };
        this.setLines(['@@ -1,1 +1,2 @@', '-old_line()', '+new_line()', '+extra_line()']);
    "#;
    let in_proj_tree = describe_with_values(
        cx,
        &staged,
        "mods/changes",
        fixture(true, json!({}), updates("current", "current")),
        Some(in_proj_setup),
    ).unwrap();
    assert!(!in_proj_tree.contains("changes-external-section"), "in-project preview must not show external section: {in_proj_tree}");
    assert!(in_proj_tree.contains("chg-ext-banner"), "in-project preview banner must render: {in_proj_tree}");
    assert!(in_proj_tree.contains("In project"), "in-project preview must be labeled In project: {in_proj_tree}");
    assert!(in_proj_tree.contains("src/committed.rs"), "in-project preview must show relative path: {in_proj_tree}");
}

#[gpui::test]
fn prompt_polish_renders_across_acp_cli_pane_prompt_and_flash_note(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    // 1) Peek composer (ACP & CLI chat view) renders Polish button + polished undo banner with shortcut hint.
    let peek_setup = r#"
        this.input.set_value('请修复登录超时问题并补充单元测试');
        this.polishUndoText = '修下登录超时';
        this.polishSummary = '明确修复范围与测试要求';
        this.polishApplied = true;
    "#;
    let mut peek_values = fixture(true, json!({ "terminal": "a1", "acp": true, "tab": true }), updates("current", "current"));
    peek_values.insert("keys".into(), json!([
        { "section": "global", "keys": "ctrl-alt-p", "action": "prompt.polish" },
        { "section": "global", "keys": "ctrl-alt-u", "action": "prompt.polish_undo" }
    ]));
    let acp_tree = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        peek_values.clone(),
        Some(peek_setup),
    )
    .unwrap();
    assert!(acp_tree.contains("acp-polish"), "peek composer must render Polish button: {acp_tree}");
    assert!(acp_tree.contains("acp-polish-banner"), "peek composer must render polished summary banner: {acp_tree}");
    assert!(acp_tree.contains("acp-polish-undo"), "peek composer must render undo button: {acp_tree}");
    assert!(acp_tree.contains("Ctrl+Alt+U"), "peek undo button tooltip must include undo shortcut hint: {acp_tree}");

    // Verify revertPolishedPrompt restores original text and hides the undo banner.
    let peek_reverted_tree = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        peek_values,
        Some(r#"
            this.input.set_value('请修复登录超时问题并补充单元测试');
            this.polishUndoText = '修下登录超时';
            this.polishSummary = '明确修复范围与测试要求';
            this.polishApplied = true;
            this.revertPolishedPrompt({ notify() {} });
            if (this.input.value() !== '修下登录超时') throw new Error('expected restored text');
        "#),
    )
    .unwrap();
    assert!(!peek_reverted_tree.contains("acp-polish-banner"), "peek undo banner must disappear after revert: {peek_reverted_tree}");

    // 2) Prompt modal in mode="polish" (used by CLI raw terminal panes) renders full polisher, undo button & shortcut.
    let prompt_setup = r#"
        this.textarea.set_value('Refactor auth middleware and add retry backoff tests');
        this.polishUndoText = 'refactor auth add tests';
        this.polishSummary = 'Clarified scope and test target';
    "#;
    let mut prompt_values = fixture(
        true,
        json!({
            "overlay": "ov-polish",
            "mode": "polish",
            "terminal": "t1",
            "agentName": "Codex",
            "agentKind": "cli",
            "cwd": "/fixture/project"
        }),
        updates("current", "current"),
    );
    prompt_values.insert("keys".into(), json!([
        { "section": "global", "keys": "ctrl-alt-p", "action": "prompt.polish" },
        { "section": "global", "keys": "ctrl-alt-u", "action": "prompt.polish_undo" }
    ]));
    let prompt_tree = describe_with_values(
        cx,
        &staged,
        "mods/prompt",
        prompt_values.clone(),
        Some(prompt_setup),
    )
    .unwrap();
    assert!(prompt_tree.contains("prompt-polish-modal"), "prompt modal must render polish view: {prompt_tree}");
    assert!(prompt_tree.contains("pm-polish-run"), "prompt modal must render AI Polish button: {prompt_tree}");
    assert!(prompt_tree.contains("pm-polish-send"), "prompt modal must render Send button: {prompt_tree}");
    assert!(prompt_tree.contains("pm-polish-undo"), "prompt modal must render Undo button: {prompt_tree}");
    assert!(prompt_tree.contains("Ctrl+Alt+U"), "prompt modal undo button tooltip must include shortcut hint: {prompt_tree}");
    assert!(prompt_tree.contains("CLI · Codex"), "prompt modal must show CLI agent badge: {prompt_tree}");

    // Verify revertPolish in Prompt modal restores original text.
    let prompt_reverted_tree = describe_with_values(
        cx,
        &staged,
        "mods/prompt",
        prompt_values,
        Some(r#"
            this.textarea.set_value('Refactor auth middleware and add retry backoff tests');
            this.polishUndoText = 'refactor auth add tests';
            this.polishSummary = 'Clarified scope and test target';
            this.revertPolish({ notify() {} });
            if (this.textarea.value() !== 'refactor auth add tests') throw new Error('expected prompt restored');
        "#),
    )
    .unwrap();
    assert!(!prompt_reverted_tree.contains("pm-polish-undo"), "prompt undo button must disappear after revert: {prompt_reverted_tree}");

    // 3) Pane header & terminals-pane header render Polish + post-polish Undo button with live shortcuts.
    let mut values = fixture(true, json!({ "pane": "p1" }), updates("current", "current"));
    values.insert("keys".into(), json!([
        { "section": "global", "keys": "ctrl-alt-p", "action": "prompt.polish" },
        { "section": "global", "keys": "ctrl-alt-u", "action": "prompt.polish_undo" },
        { "section": "global", "keys": "ctrl-shift-h", "action": "agent.handoff" }
    ]));
    values.insert("panes".into(), json!([{
        "pane": "p1",
        "terminal": "t1",
        "acp": false,
        "structured": false,
        "title": "Codex",
        "status": "idle",
        "focused": true,
        "cwd": "/fixture/project",
        "program": { "name": "Codex", "agent": true, "tint": true }
    }]));
    values.insert("terminals".into(), json!([{
        "address": "t1",
        "title": "Codex",
        "argv": ["codex"],
        "state": "live",
        "status": "idle",
        "program": { "name": "Codex", "agent": true, "tint": true },
        "cwd": "/fixture/project"
    }]));
    let pane_tree_no_undo = describe_with_values(cx, &staged, "mods/pane", values.clone(), None).unwrap();
    let polish_pos = pane_tree_no_undo.find("ph-polish-p1").expect("CLI terminal pane header must render Polish button");
    assert!(pane_tree_no_undo.contains("Ctrl+Alt+P"), "Polish tooltip must include shortcut hint: {pane_tree_no_undo}");
    assert!(!pane_tree_no_undo.contains("ph-polish-undo-p1"), "Undo button must stay hidden before polish completes: {pane_tree_no_undo}");
    if let Some(side_pos) = pane_tree_no_undo.find("ph-side-p1") {
        assert!(polish_pos < side_pos, "Polish button must appear before Side/Fork/Handoff buttons: {pane_tree_no_undo}");
    }

    let mut values_with_undo = values.clone();
    values_with_undo.insert("shared".into(), json!({
        "polish.undo": {
            "t1": { "summary": "Clarified prompt", "createdAtMs": 1_700_000_000_000u64 }
        }
    }));
    let pane_tree_undo = describe_with_values(cx, &staged, "mods/pane", values_with_undo.clone(), None).unwrap();
    assert!(pane_tree_undo.contains("ph-polish-undo-p1"), "CLI pane header must render post-polish undo button: {pane_tree_undo}");
    assert!(pane_tree_undo.contains("Ctrl+Alt+U"), "CLI pane undo tooltip must include shortcut hint: {pane_tree_undo}");

    let term_pane_tree = describe_with_values(cx, &staged, "mods/terminals-pane", values_with_undo, None).unwrap();
    assert!(term_pane_tree.contains("tm-polish-p1"), "terminals-pane header must render Polish button: {term_pane_tree}");
    assert!(term_pane_tree.contains("tm-polish-undo-p1"), "terminals-pane header must render post-polish undo button: {term_pane_tree}");
    assert!(term_pane_tree.contains("Ctrl+Alt+U"), "terminals-pane undo tooltip must include shortcut hint: {term_pane_tree}");

    // 4) Flash Note compose window renders Polish + Undo button (single-entry & multi-entry) and restores on revertPolish.
    let mut fn_values = fixture(true, json!({}), updates("current", "current"));
    fn_values.insert("keys".into(), json!([
        { "section": "global", "keys": "ctrl-alt-p", "action": "prompt.polish" },
        { "section": "global", "keys": "ctrl-alt-u", "action": "prompt.polish_undo" }
    ]));
    let fn_tree = describe_with_values(
        cx,
        &staged,
        "mods/flash-note",
        fn_values.clone(),
        Some(r#"
            this.entryStates[0].input.set_value('优化后的闪念提示词');
            this.entryStates[0].polishUndoText = '原始闪念';
            this.entryStates[0].polishSummary = '表达更清晰';
            this.polishUndoText = '原始闪念';
            this.polishSummary = '表达更清晰';
            this.lastPolishedEntryId = this.entryStates[0].id;
        "#),
    )
    .unwrap();
    assert!(fn_tree.contains("fn-polish"), "flash-note composer must render Polish button: {fn_tree}");
    assert!(fn_tree.contains("fn-polish-undo"), "flash-note single-entry must render Undo button: {fn_tree}");
    assert!(fn_tree.contains("Ctrl+Alt+U"), "flash-note undo tooltip must include shortcut hint: {fn_tree}");

    // Verify multi-entry shortcut undo restores the last polished entry even if focus moved to another entry.
    let fn_multi_tree = describe_with_values(
        cx,
        &staged,
        "mods/flash-note",
        fn_values,
        Some(r#"
            const e2 = this.addEntry({ notify() {} }, '润色后的第二条');
            e2.polishUndoText = '第二条原文';
            e2.polishSummary = '补充细节';
            this.lastPolishedEntryId = e2.id;
            this.activeEntryIndex = 0;
            this.revertPolish({ notify() {} });
            if (e2.input.value() !== '第二条原文') throw new Error('expected multi-entry undo to restore last polished entry');
            if (e2.polishUndoText !== null) throw new Error('expected polishUndoText cleared');
        "#),
    )
    .unwrap();
    assert!(!fn_multi_tree.contains("fn-polish-undo-1"), "multi-entry undo banner must clear after revert: {fn_multi_tree}");

    // 4b) Toast renders explicit post-polish undo button with shortcut tooltip when buttonAction + actionLabel are set.
    let mut toast_values = fixture(
        true,
        json!({
            "overlay": "ov-toast-1",
            "title": "✨ Input polished",
            "body": "Clarified prompt",
            "level": "info",
            "actionLabel": "↩ Undo",
            "actionTooltip": "Undo last polish and restore original input",
            "buttonAction": { "name": "prompt.polish_undo", "args": { "terminal": "t1" } }
        }),
        updates("current", "current"),
    );
    toast_values.insert("keys".into(), json!([
        { "section": "global", "keys": "ctrl-alt-u", "action": "prompt.polish_undo" }
    ]));
    let toast_tree = describe_with_values(cx, &staged, "mods/toast", toast_values, None).unwrap();
    assert!(toast_tree.contains("toast-action-btn"), "post-polish toast must render undo action button: {toast_tree}");
    assert!(toast_tree.contains("Ctrl+Alt+U"), "post-polish toast button tooltip must include shortcut hint: {toast_tree}");

    // 5) Floating hot-reloadable polish-progress mod renders polishing & applying states cleanly.
    let mut pp_values = fixture(true, json!({}), updates("current", "current"));
    pp_values.insert(
        "shared".into(),
        json!({
            "polish.state": {
                "id": 7,
                "addr": "t1",
                "preview": "第一行输入 第二行约束",
                "lineCount": 3,
                "stage": "polishing",
                "startedAtMs": 1_700_000_000_000u64,
                "stageChangedAtMs": 1_700_000_000_000u64
            }
        }),
    );
    let pp_tree = describe_with_values(cx, &staged, "mods/polish-progress", pp_values, None).unwrap();
    assert!(pp_tree.contains("polish-progress-card"), "polish-progress mod must render card: {pp_tree}");
    assert!(pp_tree.contains("polish-progress-cancel"), "polish-progress mod must render cancel button while polishing: {pp_tree}");
    assert!(pp_tree.contains("3 lines"), "polish-progress mod must show multi-line badge: {pp_tree}");
    assert!(pp_tree.contains("第一行输入 第二行约束"), "polish-progress mod must show prompt preview: {pp_tree}");
}

#[gpui::test]
fn send_context_button_and_dialog_support_cli_and_acp_targets(cx: &mut TestAppContext) {
    let staged = Staged::new();
    let mut values = fixture(true, json!({ "pane": "p1" }), updates("current", "current"));
    values.insert(
        "terminals".into(),
        json!([
            {
                "address": "wsl/t1",
                "title": "Claude CLI",
                "argv": ["claude"],
                "state": "live",
                "status": "idle",
                "program": { "name": "Claude", "agent": true, "tint": true },
                "cwd": "/fixture/project"
            },
            {
                "address": "wsl/a2",
                "title": "Codex ACP",
                "acp": true,
                "agent": "codex",
                "state": "live",
                "status": "working",
                "program": { "name": "Codex", "agent": true, "tint": true },
                "cwd": "/fixture/project"
            },
            {
                "address": "t3",
                "title": "ZCode CLI",
                "argv": ["zcode"],
                "state": "live",
                "status": "idle",
                "program": { "name": "ZCode", "agent": true, "tint": true },
                "cwd": "D:/work/project"
            }
        ]),
    );
    values.insert(
        "agents".into(),
        json!([
            { "address": "wsl/t1", "title": "Claude CLI", "status": "idle", "space_name": "Main" },
            { "address": "wsl/a2", "title": "Codex ACP", "acp": true, "status": "working", "space_name": "Main" },
            { "address": "t3", "title": "ZCode CLI", "status": "idle", "space_name": "Windows" }
        ]),
    );

    // 1) CLI agent pane header renders Send Context button.
    values.insert(
        "panes".into(),
        json!([{
            "pane": "p1",
            "tab": "tab1",
            "terminal": "wsl/t1",
            "acp": false,
            "structured": false,
            "status": "idle",
            "program": { "name": "Claude", "agent": true, "tint": true },
            "cwd": "/fixture/project"
        }]),
    );
    let cli_pane = describe_with_values(cx, &staged, "mods/pane", values.clone(), None).unwrap();
    assert!(cli_pane.contains("ph-send-ctx-p1"), "CLI pane header must render Send Context button: {cli_pane}");
    let cli_term_pane = describe_with_values(cx, &staged, "mods/terminals-pane", values.clone(), None).unwrap();
    assert!(cli_term_pane.contains("tm-send-ctx-p1"), "terminals-pane header must render Send Context button: {cli_term_pane}");

    // 2) ACP agent pane header also renders Send Context button.
    let mut acp_values = values.clone();
    acp_values.insert(
        "panes".into(),
        json!([{
            "pane": "p1",
            "tab": "tab1",
            "terminal": "wsl/a2",
            "acp": true,
            "structured": true,
            "status": "working",
            "program": { "name": "Codex", "agent": true, "tint": true },
            "cwd": "/fixture/project"
        }]),
    );
    let acp_pane = describe_with_values(cx, &staged, "mods/pane", acp_values, None).unwrap();
    assert!(acp_pane.contains("ph-send-ctx-p1"), "ACP pane header must render Send Context button: {acp_pane}");

    // 3) Handoff dialog in `send-context` mode lists both ACP and CLI live targets (excluding source), hides creation tabs, and renders note input.
    let mut dlg_values = values.clone();
    dlg_values.insert("props".into(), json!({ "overlay": "handoff", "source": "wsl/t1", "kind": "send-context" }));
    let dlg_tree = describe_with_values(cx, &staged, "mods/handoff", dlg_values, None).unwrap();
    assert!(dlg_tree.contains("ho-send-target-wsl/a2"), "send-context dialog must list live ACP target: {dlg_tree}");
    assert!(dlg_tree.contains("ho-send-target-t3"), "send-context dialog must list live CLI target: {dlg_tree}");
    assert!(!dlg_tree.contains("ho-send-target-wsl/t1"), "send-context dialog must exclude source session: {dlg_tree}");
    assert!(!dlg_tree.contains("ho-mode-handoff"), "send-context dialog must hide creation mode tabs: {dlg_tree}");
    assert!(dlg_tree.contains("ho-prompt-input"), "send-context dialog must render optional note textarea: {dlg_tree}");
    assert!(dlg_tree.contains("ho-focus-target-toggle"), "send-context dialog must render focus toggle: {dlg_tree}");
    assert!(dlg_tree.contains("ho-turns-1"), "send-context dialog must render 1-turn preset: {dlg_tree}");
    assert!(dlg_tree.contains("ho-turns-2"), "send-context dialog must render 2-turn preset: {dlg_tree}");
    assert!(dlg_tree.contains("ho-turns-4"), "send-context dialog must render 4-turn preset: {dlg_tree}");
    assert!(dlg_tree.contains("ho-turns-0"), "send-context dialog must render All preset: {dlg_tree}");
    assert!(dlg_tree.contains("ho-turns-custom"), "send-context dialog must render Custom turn option: {dlg_tree}");
    assert!(!dlg_tree.contains("ho-turns-8"), "send-context dialog must not render 8-turn preset: {dlg_tree}");
    assert!(!dlg_tree.contains("ho-turns-15"), "send-context dialog must not render 15-turn preset: {dlg_tree}");
}

#[gpui::test]
fn polish_shortcut_conflict_dialog_and_settings_sync(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    // 1) Confirm dialog with hideCancel omits the Cancel button and shows primary + secondary actions.
    let mut values = fixture(
        true,
        json!({
            "overlay": "polish-shortcut-conflict",
            "title": "润色快捷键已被其他软件占用",
            "detail": "检测到全局热键 Ctrl+Alt+P 已被系统中的其他软件占用。\n已自动切换为 Ctrl+Alt+R。",
            "hideCancel": true,
            "confirmLabel": "知道了",
            "secondary": {
                "label": "打开快捷键设置",
                "command": { "name": "app.settings", "args": { "section": "keyboard" } }
            }
        }),
        updates("current", "current"),
    );
    let dlg = describe_with_values(cx, &staged, "mods/confirm", values.clone(), None).unwrap();
    assert!(dlg.contains("yes"), "conflict dialog must show confirm button: {dlg}");
    assert!(dlg.contains("secondary"), "conflict dialog must show settings button: {dlg}");
    assert!(!dlg.contains("\"id\":\"no\"") && !dlg.contains("#no"), "hideCancel must omit cancel button: {dlg}");

    // 2) Settings keyboard section reflects updated keys() binding for prompt.polish.
    let mut st_props = props();
    st_props["section"] = json!("keyboard");
    values = fixture(true, st_props, updates("current", "current"));
    values.insert("window".into(), json!({ "platform": "windows" }));
    values.insert(
        "keys".into(),
        json!([
            { "section": "global", "keys": "ctrl-alt-r", "action": "prompt.polish" },
            { "section": "global", "keys": "ctrl-alt-u", "action": "prompt.polish_undo" }
        ]),
    );
    let st = describe_with_values(cx, &staged, "frame/settings", values.clone(), None).unwrap();
    assert!(st.contains("st-key-global-prompt.polish"), "settings must show polish row: {st}");
    assert!(st.contains("Ctrl + Alt + R"), "settings must show reassigned polish chord: {st}");

    // 3) Peek & pane tooltips also display the reassigned shortcut from keys().
    let mut peek_values = fixture(true, json!({ "terminal": "a1", "acp": true, "tab": true }), updates("current", "current"));
    peek_values.insert("keys".into(), json!([
        { "section": "global", "keys": "ctrl-alt-r", "action": "prompt.polish" },
        { "section": "global", "keys": "ctrl-alt-u", "action": "prompt.polish_undo" }
    ]));
    let peek_tree = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        peek_values,
        Some("this.input.set_value('测试提示词');"),
    )
    .unwrap();
    assert!(peek_tree.contains("Ctrl+Alt+R"), "peek polish tooltip must show reassigned shortcut: {peek_tree}");
}

#[gpui::test]
fn agents_sidebar_scrolls_when_vertically_narrow(cx: &mut TestAppContext) {
    use gpui::VisualTestContext;
    use std::ops::Deref;

    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let program = json!({
        "key": "claude",
        "name": "Claude",
        "icon": null,
        "tint": true,
        "tile": "#d97757",
        "color": "#ffffff",
        "shell": false,
        "agent": true
    });
    let many_agents: Vec<Value> = (1..=12)
        .map(|i| {
            json!({
                "address": format!("a{i}"),
                "agent": "claude",
                "acp": true,
                "pane": format!("p{i}"),
                "space": "s1",
                "space_name": "Local",
                "tab": format!("tab{i}"),
                "tab_index": i,
                "tab_name": format!("Agent {i}"),
                "status": "idle",
                "app": "claude-code",
                "program": program,
                "title": format!("Agent {i}"),
                "seq": i,
                "focused": i == 1
            })
        })
        .collect();
    let many_terms: Vec<Value> = (1..=12)
        .map(|i| {
            json!({
                "address": format!("a{i}"),
                "acp": true,
                "agent": "claude",
                "name": format!("Agent {i}"),
                "title": format!("Agent {i}"),
                "argv": ["claude-agent-acp"],
                "parent": null,
                "labels": {},
                "state": "live",
                "status": "idle",
                "program": program,
                "cwd": "/fixture/project"
            })
        })
        .collect();
    let many_panes: Vec<Value> = (1..=12)
        .map(|i| {
            json!({
                "pane": format!("p{i}"),
                "terminal": format!("a{i}"),
                "tab": format!("tab{i}"),
                "space": "s1",
                "focused": i == 1
            })
        })
        .collect();

    for (height, label) in [(90.0_f32, "min-90"), (120.0_f32, "narrow-120"), (70.0_f32, "very-narrow-70")] {
        let mut vals = fixture(true, json!({}), updates("current", "current"));
        vals.insert("agents".into(), Value::Array(many_agents.clone()));
        vals.insert("terminals".into(), Value::Array(many_terms.clone()));
        vals.insert("panes".into(), Value::Array(many_panes.clone()));
        vals.insert("checking".into(), json!(false));

        let calls = Rc::new(RefCell::new(Calls::default()));
        gpui_shell::policy::set_default(
            Policy::new()
                .with_storage_path(staged.0.join(format!("store-ag-scroll-{label}.json")))
                .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
                .with_host_module(module(vals, calls.clone()))
                .unwrap(),
        );
        let runtime = ShellRuntime::new_isolated().unwrap();
        let mod_dir = staged.0.join("mods/agents");

        struct NarrowAgentsBox {
            root: gpui::Entity<gpui_shell::ShellRoot>,
            height: f32,
        }
        impl gpui::Render for NarrowAgentsBox {
            fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
                use gpui::{ParentElement as _, Styled as _};
                gpui::div()
                    .w(gpui::px(268.))
                    .h(gpui::px(self.height))
                    .relative()
                    .overflow_hidden()
                    .flex()
                    .child(gpui::div().size_full().child(self.root.clone()))
            }
        }

        let window = cx.add_window(|window, cx| {
            let root = runtime.try_load(&mod_dir, window, cx).unwrap();
            NarrowAgentsBox { root, height }
        });
        let mut visual = VisualTestContext::from_window(*window.deref(), cx);
        for _ in 0..3 {
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }

        // 1) Mouse wheel scrolling down inside ag-list (header is 41px, border 1px -> list starts at y=42).
        let wheel_pt = gpui::point(gpui::px(120.), gpui::px(height - 14.0));
        visual.simulate_mouse_move(wheel_pt, None, Default::default());
        for _ in 0..5 {
            visual.simulate_event(gpui::ScrollWheelEvent {
                position: wheel_pt,
                delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-200.))),
                ..Default::default()
            });
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
        visual.simulate_click(wheel_pt, Default::default());
        visual.run_until_parked();
        let after_wheel = calls.borrow().shared.get("agents.selected").and_then(Value::as_str).unwrap_or("").to_owned();
        assert_eq!(after_wheel, "a12", "[{label}] wheel-scrolling down must reach the bottom agent a12");

        // Reset scroll to top via positive wheel, then nudge past the 30px group header
        // so we can confirm top items (a1/a2) are reachable without clicking the fold header.
        for _ in 0..5 {
            visual.simulate_event(gpui::ScrollWheelEvent {
                position: wheel_pt,
                delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(200.))),
                ..Default::default()
            });
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
        visual.simulate_event(gpui::ScrollWheelEvent {
            position: wheel_pt,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(-40.))),
            ..Default::default()
        });
        for _ in 0..2 {
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
        calls.borrow_mut().shared.remove("agents.selected");
        visual.simulate_click(wheel_pt, Default::default());
        visual.run_until_parked();
        let after_reset = calls.borrow().shared.get("agents.selected").and_then(Value::as_str).unwrap_or("").to_owned();
        assert!(
            matches!(after_reset.as_str(), "a1" | "a2" | "a3"),
            "[{label}] wheel-scrolling back up must return to top agents, got {after_reset:?}"
        );
        // Return all the way to offset 0 before testing scrollbar thumb drag.
        visual.simulate_event(gpui::ScrollWheelEvent {
            position: wheel_pt,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(200.))),
            ..Default::default()
        });
        visual.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
        calls.borrow_mut().shared.remove("agents.selected");

        // 2) Scrollbar thumb drag from top of ag-list scrollbar (x=260, y=47) to bottom (x=260, y=height-2).
        let thumb_start = gpui::point(gpui::px(260.), gpui::px(47.));
        let thumb_end = gpui::point(gpui::px(260.), gpui::px(height - 2.0));
        visual.simulate_mouse_move(thumb_start, None, Default::default());
        visual.simulate_mouse_down(thumb_start, gpui::MouseButton::Left, Default::default());
        visual.simulate_mouse_move(thumb_end, gpui::MouseButton::Left, Default::default());
        visual.simulate_mouse_up(thumb_end, gpui::MouseButton::Left, Default::default());
        for _ in 0..3 {
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
        // Moving pointer off the scrollbar and onto a row (which triggers hover re-renders)
        // must keep the scrolled position instead of snapping back to top.
        visual.simulate_mouse_move(wheel_pt, None, Default::default());
        visual.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
        visual.simulate_click(wheel_pt, Default::default());
        visual.run_until_parked();
        let after_thumb = calls.borrow().shared.get("agents.selected").and_then(Value::as_str).unwrap_or("").to_owned();
        assert!(
            matches!(after_thumb.as_str(), "a11" | "a12"),
            "[{label}] dragging scrollbar thumb down on narrow agents panel must scroll to bottom (a11/a12), got {after_thumb:?}"
        );

        // 3) Return to top and verify clicking the lower scrollbar track jumps downward too.
        for _ in 0..5 {
            visual.simulate_event(gpui::ScrollWheelEvent {
                position: wheel_pt,
                delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(200.))),
                ..Default::default()
            });
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
        calls.borrow_mut().shared.remove("agents.selected");
        visual.simulate_mouse_move(thumb_end, None, Default::default());
        visual.simulate_click(thumb_end, Default::default());
        for _ in 0..3 {
            visual.update(|window, cx| {
                window.simulate_next_frame(cx);
                window.draw(cx).clear(cx);
            });
            visual.run_until_parked();
        }
        visual.simulate_mouse_move(wheel_pt, None, Default::default());
        visual.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
        visual.simulate_click(wheel_pt, Default::default());
        visual.run_until_parked();
        let after_track = calls.borrow().shared.get("agents.selected").and_then(Value::as_str).unwrap_or("").to_owned();
        assert!(
            matches!(after_track.as_str(), "a10" | "a11" | "a12"),
            "[{label}] clicking bottom of scrollbar track on narrow agents panel must jump near bottom, got {after_track:?}"
        );

        visual.update(|window, _| window.remove_window());
        gpui_shell::policy::set_default(Policy::new());
        cx.run_until_parked();
    }
}

#[gpui::test]
fn peek_composer_up_down_recalls_prompt_history_in_acp_and_pty_chat(cx: &mut TestAppContext) {
    use gpui::VisualTestContext;
    use std::ops::Deref;

    cx.update(gpui_shell::init);
    let staged = Staged::new();
    let program = json!({
        "key": "claude",
        "name": "Claude Code",
        "icon": null,
        "tint": false,
        "tile": "#1c1d21",
        "color": "#ffffff",
        "shell": false,
        "agent": true
    });

    for (label, addr, is_pty) in [("acp", "a_hist", false), ("pty_chat", "t_hist", true)] {
        let mut values = fixture(
            true,
            json!({"terminal": addr, "acp": true, "pty": is_pty, "tab": true, "pane": "p1"}),
            updates("current", "current"),
        );
        values.insert("terminals".into(), json!([{
            "address": addr,
            "acp": !is_pty,
            "agent": "claude",
            "name": "Claude",
            "title": "Claude",
            "argv": if is_pty { json!(["claude"]) } else { json!(["claude-agent-acp"]) },
            "parent": null,
            "labels": if is_pty { json!({"herdr.view": "peek"}) } else { json!({}) },
            "state": "live",
            "status": "idle",
            "turn": 3,
            "transcript_seq": 3,
            "program": program,
            "cwd": "/fixture/project"
        }]));
        values.insert("agents".into(), json!([{
            "address": addr,
            "acp": !is_pty,
            "pane": "p1",
            "space": "s1",
            "space_name": "Local",
            "tab": "tab1",
            "tab_index": 1,
            "tab_name": "Claude",
            "status": "idle",
            "app": "claude-code",
            "program": program,
            "title": "Claude",
            "seq": 3,
            "focused": true
        }]));
        values.insert("panes".into(), json!([{
            "pane": "p1",
            "acp": !is_pty,
            "structured": is_pty,
            "mod": "peek",
            "terminal": addr,
            "tab": "tab1",
            "title": "Claude",
            "cwd": "/fixture/project",
            "status": "idle",
            "program": program,
            "focused": true,
            "zoomed": false,
            "movable": false
        }]));

        let calls = Rc::new(RefCell::new(Calls::default()));
        gpui_shell::policy::set_default(
            Policy::new()
                .with_storage_path(staged.0.join(format!("store-hist-{label}.json")))
                .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
                .with_host_module(module(values, calls.clone()))
                .unwrap(),
        );
        let wrapper = staged.wrapper("mods/peek", "this.input?.focus?.();");
        let runtime = ShellRuntime::new_isolated().unwrap();
        let window = cx.add_window(|window, cx| RootedView(runtime.try_load(&wrapper, window, cx).unwrap()));
        let mut visual = VisualTestContext::from_window(*window.deref(), cx);
        let pump = |v: &mut VisualTestContext| {
            v.update(|window, cx| window.draw(cx).clear(cx));
            v.run_until_parked();
        };
        pump(&mut visual);
        pump(&mut visual);

        // 1) Type an unsent draft, press Up 3x across single-line & multi-line entries,
        //    then Down 3x to restore the exact draft and submit it.
        visual.simulate_input("draft msg");
        pump(&mut visual);
        visual.simulate_keystrokes("up");
        pump(&mut visual);
        visual.simulate_keystrokes("up");
        pump(&mut visual);
        visual.simulate_keystrokes("up");
        pump(&mut visual);
        visual.simulate_keystrokes("down");
        pump(&mut visual);
        visual.simulate_keystrokes("down");
        pump(&mut visual);
        visual.simulate_keystrokes("down");
        pump(&mut visual);
        visual.simulate_keystrokes("enter");
        pump(&mut visual);

        // 2) Immediately press Up to recall the just-sent prompt ("draft msg"), then Up again
        //    to recall "latest prompt", and submit it.
        visual.simulate_keystrokes("up");
        pump(&mut visual);
        visual.simulate_keystrokes("up");
        pump(&mut visual);
        visual.simulate_keystrokes("enter");
        pump(&mut visual);

        // 3) History is now ["first prompt", "multi line one\nmulti line two", "latest prompt", "draft msg", "latest prompt"].
        //    Press Up 5x from empty input to reach "first prompt" (skipping past the multi-line entry
        //    with one Up press per history item), then submit.
        for _ in 0..5 {
            visual.simulate_keystrokes("up");
            pump(&mut visual);
        }
        visual.simulate_keystrokes("enter");
        pump(&mut visual);

        // 4) Inside a multi-line draft, pressing Up from line 2 moves the caret to line 1
        //    instead of replacing the draft with history.
        visual.simulate_input("aaa");
        visual.simulate_keystrokes("shift-enter");
        visual.simulate_input("bbb");
        pump(&mut visual);
        visual.simulate_keystrokes("up");
        pump(&mut visual);
        visual.simulate_input("X");
        pump(&mut visual);
        visual.simulate_keystrokes("down");
        pump(&mut visual);
        visual.simulate_keystrokes("enter");
        pump(&mut visual);

        let sent: Vec<String> = calls
            .borrow()
            .effects
            .iter()
            .filter(|(k, _)| k == "h2.send")
            .filter_map(|(_, args)| {
                args.first()
                    .and_then(Value::as_array)
                    .and_then(|arr| arr.last())
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .collect();
        assert_eq!(
            sent,
            vec![
                "draft msg".to_owned(),
                "latest prompt".to_owned(),
                "first prompt".to_owned(),
                "aaaX\nbbb".to_owned(),
            ],
            "[{label}] unexpected sent prompts sequence"
        );

        visual.update(|window, _| window.remove_window());
        gpui_shell::policy::set_default(Policy::new());
        cx.run_until_parked();
    }
}

#[gpui::test]
fn acp_and_pty_peek_render_slash_and_codex_at_command_completion_popups(cx: &mut TestAppContext) {
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    // 1. ACP Claude agent typing `/` shows live available_commands popup.
    let claude_prog = json!({"key":"claude", "name":"Claude Code", "icon":null, "tint":false, "tile":"#1c1d21", "color":"#ffffff", "shell":false, "agent":true});
    let mut claude_values = fixture(true, json!({"terminal":"a1","acp":true,"tab":true,"pane":"p2"}), updates("current", "current"));
    claude_values.insert("terminals".into(), json!([
        {
            "address":"a1","acp":true,"agent":"claude","name":"Claude","title":"Claude",
            "argv":["claude-agent-acp"],"parent":null,"labels":{},"state":"live","status":"idle",
            "turn":1,"transcript_seq":1,"config_options":[],
            "available_commands":[
                {"name":"compact","description":"Compact conversation","input_hint":"[instructions]","source":"command"},
                {"name":"drove","description":"Multi-agent DAG planning","source":"skill"}
            ],
            "report":{"app":"claude-code"},"program":claude_prog,"cwd":"/fixture/project"
        }
    ]));
    claude_values.insert("agents".into(), json!([
        {"address":"a1","agent":"claude","acp":true,"pane":"p2","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Claude","status":"idle","app":"claude-code","program":claude_prog,"title":"Claude","seq":1,"focused":true}
    ]));
    let claude_tree = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        claude_values,
        Some("this.inputFocused = true; this.input.set_value('/'); this.input.set_selected_range({start:1,end:1});"),
    ).unwrap();
    assert!(claude_tree.contains("acp-completion-popup"), "slash completion popup missing: {claude_tree}");
    assert!(claude_tree.contains("/compact"), "slash command /compact missing: {claude_tree}");
    assert!(claude_tree.contains("/drove"), "skill /drove missing: {claude_tree}");

    // 2. ACP Codex agent typing `@` surfaces Codex commands, $skills, and workspace files.
    let codex_prog = json!({"key":"codex", "name":"Codex", "icon":null, "tint":false, "tile":"#1c1d21", "color":"#ffffff", "shell":false, "agent":true});
    let mut codex_values = fixture(true, json!({"terminal":"a1","acp":true,"tab":true,"pane":"p2"}), updates("current", "current"));
    codex_values.insert("terminals".into(), json!([
        {
            "address":"a1","acp":true,"agent":"codex","name":"Codex","title":"Codex",
            "argv":["codex-acp"],"parent":null,"labels":{},"state":"live","status":"idle",
            "turn":1,"transcript_seq":1,"config_options":[],
            "available_commands":[
                {"name":"plan","description":"Turn plan mode on.","source":"command"},
                {"name":"$drove","description":"Multi-agent DAG planning","source":"skill"}
            ],
            "report":{"app":"codex"},"program":codex_prog,"cwd":"/fixture/project"
        }
    ]));
    codex_values.insert("agents".into(), json!([
        {"address":"a1","agent":"codex","acp":true,"pane":"p2","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Codex","status":"idle","app":"codex","program":codex_prog,"title":"Codex","seq":1,"focused":true}
    ]));
    let codex_tree = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        codex_values,
        Some("this.workspaceFilesCache.set('local:/fixture/project', {files:['README.md','src/main.rs'], fetchedAt: Date.now(), loading: false}); this.inputFocused = true; this.input.set_value('@'); this.input.set_selected_range({start:1,end:1});"),
    ).unwrap();
    assert!(codex_tree.contains("acp-completion-popup"), "codex @ completion popup missing: {codex_tree}");
    assert!(codex_tree.contains("/plan"), "codex /plan missing on @ trigger: {codex_tree}");
    assert!(codex_tree.contains("$drove"), "codex $drove skill missing on @ trigger: {codex_tree}");
    assert!(codex_tree.contains("@README.md"), "codex @README.md file missing on @ trigger: {codex_tree}");

    // 3. PTY CLI agent in structured chat view (`pty: true`) typing `/co` renders discovered commands and accepts cleanly.
    let mut pty_values = fixture(true, json!({"terminal":"t1","acp":true,"pty":true,"tab":true,"pane":"p2"}), updates("current", "current"));
    pty_values.insert("terminals".into(), json!([
        {
            "address":"t1","acp":false,"name":"Claude CLI","title":"Claude CLI",
            "argv":["claude"],"parent":null,"labels":{},"state":"live","status":"idle",
            "turn":1,"report":{"app":"claude-code"},"program":claude_prog,"cwd":"/fixture/project"
        }
    ]));
    pty_values.insert("agents".into(), json!([
        {"address":"t1","acp":false,"pane":"p2","space":"s1","space_name":"Local","tab":"tab1","tab_index":1,"tab_name":"Claude CLI","status":"idle","app":"claude-code","program":claude_prog,"title":"Claude CLI","seq":1,"focused":true}
    ]));
    let pty_tree = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        pty_values.clone(),
        Some("this.readAvailableCommands = [{name:'compact',description:'Compact context',source:'command'},{name:'review',description:'Review diff',source:'command'}]; this.inputFocused = true; this.input.set_value('/co'); this.input.set_selected_range({start:3,end:3});"),
    ).unwrap();
    assert!(pty_tree.contains("acp-completion-popup"), "PTY chat view slash popup missing: {pty_tree}");
    assert!(pty_tree.contains("/compact"), "PTY chat view /compact missing: {pty_tree}");
    assert!(!pty_tree.contains("/review"), "non-matching /review should be filtered out: {pty_tree}");

    // Accepting the completion replaces `/co` with `/compact ` and closes the popup.
    let accepted_tree = describe_with_values(
        cx,
        &staged,
        "mods/peek",
        pty_values,
        Some("this.readAvailableCommands = [{name:'compact',description:'Compact context',source:'command'}]; this.inputFocused = true; this.input.set_value('/co'); this.input.set_selected_range({start:3,end:3}); this.handleComposerKeyDown({key:'tab',modifiers:{}}, cx);"),
    ).unwrap();
    assert!(!accepted_tree.contains("acp-completion-popup"), "popup should close after accepting completion: {accepted_tree}");
}

#[gpui::test]
fn ai_polish_button_stops_spinning_after_result_arrives(cx: &mut TestAppContext) {
    use gpui::VisualTestContext;
    use std::ops::Deref as _;
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    // Helper: mounts a mod in a real VisualTestContext where `startPolish*` runs from a
    // call-scoped `cx` (same `ContextBinding::Call` lifetime as `on_click`), awaits async `h2`,
    // and verifies the view re-renders with the polished text and WITHOUT a lingering spinner.
    let assert_polish_finishes = |cx: &mut TestAppContext,
                                  entry: &str,
                                  values: BTreeMap<String, Value>,
                                  setup: &str,
                                  expected_banner_id: &str| {
        let calls = Rc::new(RefCell::new(Calls::default()));
        static NEXT_POLISH_STORE: AtomicU64 = AtomicU64::new(0);
        gpui_shell::policy::set_default(
            Policy::new()
                .with_storage_path(staged.0.join(format!(
                    "store-polish-spin-{}.json",
                    NEXT_POLISH_STORE.fetch_add(1, Ordering::Relaxed)
                )))
                .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
                .with_host_module(module(values, calls.clone()))
                .unwrap(),
        );
        let runtime = ShellRuntime::new_isolated().unwrap();
        let root = staged.wrapper(entry, setup);
        let win = cx.add_window(|window, cx| RootedView(runtime.try_load(&root, window, cx).unwrap()));
        let view: gpui::Entity<gpui_shell::ScriptView> = win
            .read_with(cx, |root, cx| root.0.read(cx).content().clone().downcast().unwrap())
            .unwrap();
        let mut visual = VisualTestContext::from_window(*win.deref(), cx);
        visual.update(|window, cx| {
            window.activate_window();
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));
        let tree = visual.update(|_, cx| {
            view.read(cx)
                .snapshot()
                .map(gpui_shell::RenderSnapshot::debug_tree)
                .unwrap_or_default()
        });
        assert!(
            calls.borrow().effects.iter().any(|(name, _)| name == "h2.polish"),
            "{entry}: expected h2 ai polish-prompt call, got {:?}",
            calls.borrow().effects
        );
        assert!(
            tree.contains(expected_banner_id),
            "{entry}: expected post-polish banner `{expected_banner_id}` after async polish completed: {tree}"
        );
        assert!(
            !tree.contains("spinner.svg") && !tree.contains("Polishing…") && !tree.contains("润色中…"),
            "{entry}: AI polish button stayed stuck spinning after result arrived: {tree}"
        );
        visual.update(|window, _| window.remove_window());
        cx.run_until_parked();
    };

    // 1) Flash Note compose window (`#fn-polish` -> `startPolishIdea(cx)`)
    let fn_values = fixture(true, json!({ "mode": "compose" }), updates("current", "current"));
    assert_polish_finishes(
        cx,
        "mods/flash-note",
        fn_values,
        "this.entryStates[0].input.set_value('fix login bug'); this.startPolishIdea(cx);",
        "fn-polish-banner",
    );

    // 2) Prompt polish modal (`#pm-polish-run` -> `startPolish(cx)`)
    let pm_values = fixture(
        true,
        json!({ "mode": "polish", "value": "fix login bug", "terminal": "t1", "agentKind": "cli", "overlay": "ov-pm" }),
        updates("current", "current"),
    );
    assert_polish_finishes(
        cx,
        "mods/prompt",
        pm_values,
        "this.startPolish(cx);",
        "pm-polish-banner",
    );

    // 3) Peek ACP composer (`#acp-polish` -> `startPolishPrompt(cx)`)
    let mut peek_values = fixture(true, json!({ "terminal": "a1", "acp": true }), updates("current", "current"));
    peek_values.insert(
        "terminals".into(),
        json!([{ "address": "a1", "acp": true, "state": "live", "cwd": "/fixture/project", "status": "idle" }]),
    );
    assert_polish_finishes(
        cx,
        "mods/peek",
        peek_values,
        "this.input.set_value('fix login bug'); this.startPolishPrompt(cx);",
        "acp-polish-banner",
    );
}

#[gpui::test]
fn acp_completion_tab_and_enter_complete_without_sending(cx: &mut TestAppContext) {
    use gpui::VisualTestContext;
    use std::ops::Deref as _;
    cx.update(gpui_shell::init);
    let staged = Staged::new();

    let codex_prog = json!({
        "key":"codex","name":"Codex","icon":null,"tint":true,"tile":"#112233",
        "color":"#10a37f","shell":false,"agent":true
    });

    let make_values = || {
        let mut values = fixture(
            true,
            json!({ "terminal": "a1", "acp": true, "tab": true, "pane": "p2" }),
            updates("current", "current"),
        );
        values.insert("terminals".into(), json!([
            {
                "address": "a1", "acp": true, "name": "Codex", "title": "Codex",
                "argv": ["codex-acp"], "parent": null, "labels": {}, "state": "live", "status": "idle",
                "turn": 1,
                "available_commands": [
                    { "name": "compact", "description": "Compact context", "source": "command" },
                    { "name": "plan", "description": "Plan mode", "source": "command" },
                    { "name": "$drove", "description": "Multi-agent orchestration", "source": "skill" }
                ],
                "report": { "app": "codex" }, "program": codex_prog, "cwd": "/fixture/project"
            }
        ]));
        values.insert("agents".into(), json!([
            {
                "address": "a1", "agent": "codex", "acp": true, "pane": "p2", "space": "s1",
                "space_name": "Local", "tab": "tab1", "tab_index": 1, "tab_name": "Codex",
                "status": "idle", "app": "codex", "program": codex_prog, "title": "Codex",
                "seq": 1, "focused": true
            }
        ]));
        values
    };

    let run_scenario = |cx: &mut TestAppContext, setup: &str, keystrokes: &str| {
        static NEXT_STORE: AtomicU64 = AtomicU64::new(0);
        let calls = Rc::new(RefCell::new(Calls::default()));
        gpui_shell::policy::set_default(
            Policy::new()
                .with_storage_path(staged.0.join(format!(
                    "store-acp-comp-{}.json",
                    NEXT_STORE.fetch_add(1, Ordering::Relaxed)
                )))
                .with_capabilities(Capabilities::new().storage(true).read_roots([staged.0.clone()]))
                .with_host_module(module(make_values(), calls.clone()))
                .unwrap(),
        );
        let runtime = ShellRuntime::new_isolated().unwrap();
        let dir = staged.0.clone();
        // Wrap Peek so we also render the exact live textarea value and focus flag
        // into a probe node for assertions without bypassing real GPUI keystroke dispatch.
        std::fs::write(
            dir.join("main.js"),
            format!(
                "import {{ div }} from 'gpui-kit'; \
                 import {{ v_flex }} from 'gpui-base'; \
                 import Real from './mods/peek/main.js'; \
                 export default class Scenario extends Real {{ \
                   init(props, cx) {{ super.init(props, cx); {setup} cx.spawn(async () => {{ this.input?.focus?.(); }}); }} \
                   render(cx) {{ \
                     const el = super.render(cx); \
                     return v_flex().size_full().child(el).child(div().id('probe').child(`VALUE=[[${{this.input?.value?.() ?? ''}}]] FOCUSED=${{this.inputFocused}}`)); \
                   }} \
                 }}"
            ),
        )
        .unwrap();
        let win = cx.add_window(|window, cx| RootedView(runtime.try_load(&dir, window, cx).unwrap()));
        let view: gpui::Entity<gpui_shell::ScriptView> = win
            .read_with(cx, |root, cx| root.0.read(cx).content().clone().downcast().unwrap())
            .unwrap();
        let mut visual = VisualTestContext::from_window(*win.deref(), cx);
        visual.update(|window, cx| {
            window.activate_window();
            window.draw(cx).clear(cx);
        });
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));

        let before_tree = visual.update(|_, cx| {
            view.read(cx)
                .snapshot()
                .map(gpui_shell::RenderSnapshot::debug_tree)
                .unwrap_or_default()
        });
        assert!(
            before_tree.contains("acp-completion-popup"),
            "expected completion popup before keystrokes `{keystrokes}`: {before_tree}"
        );

        visual.simulate_keystrokes(keystrokes);
        visual.run_until_parked();
        visual.update(|window, cx| window.draw(cx).clear(cx));

        let after_tree = visual.update(|_, cx| {
            view.read(cx)
                .snapshot()
                .map(gpui_shell::RenderSnapshot::debug_tree)
                .unwrap_or_default()
        });
        let sends: Vec<Value> = calls
            .borrow()
            .effects
            .iter()
            .filter(|(k, _)| k == "h2.send")
            .flat_map(|(_, args)| args.clone())
            .collect();
        visual.update(|window, _| window.remove_window());
        cx.run_until_parked();
        (after_tree, sends)
    };

    // 1) Slash trigger + real Tab keystroke: completes `/compact `, keeps focus, does NOT indent or send.
    let (tree, sends) = run_scenario(
        cx,
        "this.input.set_value('/co'); this.input.set_selected_range({start:3,end:3}); this.input.focus();",
        "tab",
    );
    assert!(tree.contains("VALUE=[[/compact ]]"), "Tab should complete `/co` to `/compact `: {tree}");
    assert!(tree.contains("FOCUSED=true"), "Tab completion must keep focus in textarea: {tree}");
    assert!(!tree.contains("acp-completion-popup"), "popup should close after Tab completion: {tree}");
    assert!(sends.is_empty(), "Tab completion must not send prompt: {sends:?}");

    // 2) Slash trigger + real Enter keystroke: completes `/compact ` without inserting newline or sending.
    let (tree, sends) = run_scenario(
        cx,
        "this.input.set_value('/co'); this.input.set_selected_range({start:3,end:3}); this.input.focus();",
        "enter",
    );
    assert!(tree.contains("VALUE=[[/compact ]]"), "Enter should complete `/co` to `/compact ` without newline: {tree}");
    assert!(!tree.contains("acp-completion-popup"), "popup should close after Enter completion: {tree}");
    assert!(sends.is_empty(), "Enter while completion popup is open must NOT send prompt: {sends:?}");

    // 3) Slash trigger + Enter (completes) + second Enter (sends completed command cleanly).
    let (_tree, sends) = run_scenario(
        cx,
        "this.input.set_value('/co'); this.input.set_selected_range({start:3,end:3}); this.input.focus();",
        "enter enter",
    );
    assert_eq!(sends.len(), 1, "second Enter after completion closes should send once: {sends:?}");
    assert!(
        sends[0].to_string().contains("/compact"),
        "second Enter should send completed `/compact`: {sends:?}"
    );

    // 4) Dollar skill trigger + real Tab keystroke.
    let (tree, sends) = run_scenario(
        cx,
        "this.input.set_value('use $dr'); this.input.set_selected_range({start:7,end:7}); this.input.focus();",
        "tab",
    );
    assert!(tree.contains("VALUE=[[use $drove ]]"), "Tab should complete `$dr` to `$drove `: {tree}");
    assert!(sends.is_empty(), "Tab on $ trigger must not send: {sends:?}");

    // 5) Multiline @ trigger + real Down + Enter: Down navigates popup (not cursor line) and Enter accepts 2nd item.
    let (tree, sends) = run_scenario(
        cx,
        "this.workspaceFilesCache.set('local:/fixture/project', {files:['README.md','src/main.rs'], fetchedAt: Date.now(), loading: false}); \
         this.input.set_value('check\\n@\\nplease'); \
         this.input.set_selected_range({start:7,end:7}); \
         this.input.focus();",
        "down enter",
    );
    // With Codex @ not at prompt start, skills ($drove) come first, then files (README.md is index 1!).
    // Suffix starts with `\n`, so `applyComposerCompletion` cleanly avoids adding a trailing space before `\n`.
    assert!(
        tree.contains(r"VALUE=[[check\n@README.md\nplease]]"),
        "Down should select 2nd item (@README.md after $drove) and Enter should insert it without moving cursor off line: {tree}"
    );
    assert!(sends.is_empty(), "Down + Enter on @ trigger must not send: {sends:?}");

    // 6) Escape dismisses completion popup without altering text or sending.
    let (tree, sends) = run_scenario(
        cx,
        "this.input.set_value('/co'); this.input.set_selected_range({start:3,end:3}); this.input.focus();",
        "escape",
    );
    assert!(tree.contains("VALUE=[[/co]]"), "Escape should keep text unchanged: {tree}");
    assert!(!tree.contains("acp-completion-popup"), "Escape should dismiss popup: {tree}");
    assert!(sends.is_empty(), "Escape must not send: {sends:?}");

    // 7) Shift+Enter still inserts a newline without accepting completion or sending.
    let (tree, sends) = run_scenario(
        cx,
        "this.input.set_value('/co'); this.input.set_selected_range({start:3,end:3}); this.input.focus();",
        "shift-enter",
    );
    assert!(tree.contains(r"VALUE=[[/co\n]]"), "Shift+Enter should insert a newline: {tree}");
    assert!(sends.is_empty(), "Shift+Enter must not send: {sends:?}");
}
