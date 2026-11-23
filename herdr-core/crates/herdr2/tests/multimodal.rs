#![cfg(unix)]

mod common;

use common::*;
use serde_json::Value;

const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x11, 0x49,
    0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x14, 0x32, 0x09, 0xfb, 0xcf, 0xc0, 0xc0, 0x00, 0x00, 0x09, 0x0d, 0x01, 0x9d,
    0xf7, 0x66, 0xcd, 0x15, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

fn parse_blocks_reply(s: &Session, addr: &str) -> (Vec<Value>, String) {
    let text = s.json(&["read", "--last", addr])["text"].as_str().unwrap_or_default().to_string();
    let json_part = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("BLOCKS:"))
        .unwrap_or_else(|| panic!("expected BLOCKS reply, got: {text:?}"));
    let blocks: Vec<Value> =
        serde_json::from_str(json_part).unwrap_or_else(|e| panic!("bad BLOCKS json ({e}): {text:?}"));
    (blocks, text)
}

#[test]
fn acp_multimodal_input_and_output_round_trip() {
    let s = Session::new();
    let img_path = s.root.join("bug-shot.png");
    let res_path = s.root.join("notes.md");
    std::fs::write(&img_path, TINY_PNG).unwrap();
    std::fs::write(&res_path, "# Layout Bug\nButton overlaps sidebar.\n").unwrap();

    let addr = run_fake(&s, &["--name", "vis"]);
    let agent = s.json(&["get", &addr]);
    assert_eq!(agent["prompt_capabilities"]["image"], true);
    assert_eq!(agent["prompt_capabilities"]["embeddedContext"], true);

    // 1. Send image + embedded resource + resource_link together with text.
    let img_s = img_path.display().to_string();
    let res_s = res_path.display().to_string();
    s.json(&[
        "send",
        "--wait",
        &addr,
        "--image",
        &img_s,
        "--resource",
        &res_s,
        "--resource-link",
        "https://example.com/arch.svg",
        "BLOCKS",
    ]);
    let (blocks, _) = parse_blocks_reply(&s, &addr);
    assert_eq!(blocks.len(), 4, "{blocks:?}");
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[1]["type"], "image");
    assert_eq!(blocks[1]["mimeType"], "image/png");
    assert_eq!(blocks[1]["hasData"], true);
    assert_eq!(blocks[2]["type"], "resource");
    assert_eq!(blocks[2]["mimeType"], "text/markdown");
    assert_eq!(blocks[2]["hasData"], true);
    assert_eq!(blocks[3]["type"], "resource_link");
    assert_eq!(blocks[3]["uri"], "https://example.com/arch.svg");

    // Transcript stores compact staged attachment metadata (no raw base64 bloat).
    let read_json = s.json(&["read", &addr]);
    let entries = read_json["entries"].as_array().unwrap();
    let user_entry = entries.iter().find(|e| e["kind"] == "user" && e["turn"] == 1).unwrap();
    let user_atts = user_entry["attachments"].as_array().unwrap();
    assert_eq!(user_atts.len(), 3);
    assert_eq!(user_atts[0]["kind"], "image");
    let staged_img = user_atts[0]["path"].as_str().unwrap();
    assert!(std::path::Path::new(staged_img).is_file(), "staged user image missing: {staged_img}");
    assert_eq!(user_atts[1]["kind"], "resource");
    assert_eq!(user_atts[2]["kind"], "resource_link");

    // 2. Image-only send (no text) works cleanly.
    s.json(&["send", "--wait", &addr, "--image", &img_s]);
    let (blocks2, _) = parse_blocks_reply(&s, &addr);
    assert_eq!(blocks2.len(), 1);
    assert_eq!(blocks2[0]["type"], "image");

    // 3. Agent multimodal output (assistant image + embedded resource + tool_call image).
    s.json(&["send", "--wait", &addr, "EMIT_IMAGE"]);
    let read_out = s.json(&["read", &addr]);
    let out_entries = read_out["entries"].as_array().unwrap();
    let tool_entry = out_entries.iter().find(|e| e["kind"] == "tool_call" && e["id"] == "tc-shot").unwrap();
    let tool_atts = tool_entry["attachments"].as_array().unwrap();
    assert_eq!(tool_atts.len(), 1);
    assert_eq!(tool_atts[0]["kind"], "image");
    assert!(std::path::Path::new(tool_atts[0]["path"].as_str().unwrap()).is_file());

    let asst_entry = out_entries.iter().find(|e| e["kind"] == "assistant" && e["turn"] == 3).unwrap();
    assert_eq!(asst_entry["text"], "Here is the rendered screenshot:");
    let asst_atts = asst_entry["attachments"].as_array().unwrap();
    assert_eq!(asst_atts.len(), 2);
    assert_eq!(asst_atts[0]["kind"], "image");
    assert!(std::path::Path::new(asst_atts[0]["path"].as_str().unwrap()).is_file());
    assert_eq!(asst_atts[1]["kind"], "resource");
    assert_eq!(asst_atts[1]["name"], "diag.md");
    assert!(asst_atts[1]["text"].as_str().unwrap().contains("All checks ok"));
}

#[test]
fn acp_multimodal_downgrades_gracefully_when_adapter_lacks_or_rejects_image_blocks() {
    let s = Session::new();
    let img_path = s.root.join("ui.png");
    std::fs::write(&img_path, TINY_PNG).unwrap();
    let img_s = img_path.display().to_string();

    // Case 1: Adapter explicitly advertises promptCapabilities.image = false.
    let spec_no_cap = format!("{} --no-image-cap", fake_acp_spec());
    let _a_nocap = s.json(&["run", "--bg", "--name", "nocap", "--agent", &spec_no_cap]);
    let nocap_info = s.json(&["get", "nocap"]);
    assert_eq!(nocap_info["prompt_capabilities"]["image"], false);
    s.json(&["send", "--wait", "nocap", "--image", &img_s, "BLOCKS"]);
    let (blocks1, reply1) = parse_blocks_reply(&s, "nocap");
    assert_eq!(blocks1[0]["type"], "text");
    assert_eq!(blocks1[1]["type"], "resource_link");
    assert_eq!(blocks1[1]["mimeType"], "image/png");
    assert!(reply1.contains("[Attached image:"), "expected path hint in downgraded prompt: {reply1:?}");

    // Case 2: Adapter advertises image=true but returns -32602 at prompt time -> auto-retries with resource_link.
    let spec_reject = format!("{} --reject-image", fake_acp_spec());
    let _a_rej = s.json(&["run", "--bg", "--name", "rej", "--agent", &spec_reject]);
    s.json(&["send", "--wait", "rej", "--image", &img_s, "BLOCKS"]);
    let (blocks2, reply2) = parse_blocks_reply(&s, "rej");
    assert_eq!(blocks2[0]["type"], "text");
    assert_eq!(blocks2[1]["type"], "resource_link");
    assert!(reply2.contains("[Attached image:"), "expected path hint on -32602 retry: {reply2:?}");
}
