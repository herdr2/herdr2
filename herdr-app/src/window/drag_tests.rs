#[test]
fn dragging_squeezed_layout_blocks_never_crashes() {
    in_own_process("squeezed_layout_drag");
}

#[gpui::test]
fn squeezed_layout_drag(cx: &mut gpui::TestAppContext) {
    use gpui::{point, px, size};
    use serde_json::json;
    if !running_alone("squeezed_layout_drag") { return; }
    let block = |id: &str, height: f32| json!({
        "id": id, "type": "block", "size": {"px": height}, "content": {"mod": "drag-probe"}
    });
    let layout = json!({"id": "root", "type": "row", "children": [
        {"id": "sidebar", "type": "column", "size": {"px": 268}, "resizable": false, "children": [
            block("brand", 46.0), block("button", 52.0),
            {"id": "lists", "type": "column", "divider": {"width": 1, "color": "border"}, "children": [
                block("heading", 34.0),
                {"id": "stack", "type": "stack", "children": [
                    {"id": "list", "type": "block", "content": {"mod": "drag-probe"}}
                ]},
                block("zero1", 0.0), block("zero2", 0.0), block("zero3", 0.0), block("zero4", 0.0), block("zero5", 0.0)
            ]},
            block("pendientes", 200.0), block("vistas", 46.0)
        ]}
    ]});
    let (view, cx) = open_window(cx, "drag-probe", "export default () => null;", layout.clone());
    for height in [800.0, 380.0, 344.0, 300.0] {
        cx.simulate_resize(size(px(900.0), px(height)));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        view.update(cx, |this, _| {
            for (container, count) in [("sidebar", 4), ("lists", 6)] {
                for index in 0..count {
                    for y in [-2000.0, 0.0, 2000.0] {
                        this.chrome = serde_json::from_value(layout.clone()).unwrap();
                        let parent = this.chrome.find(container).unwrap();
                        let a = &parent.children()[index];
                        let b = &parent.children()[index + 1];
                        let bounds = this.bounds.borrow();
                        let total = f32::from(bounds[&a.id].size.height + bounds[&b.id].size.height) / super::settings::zoom();
                        drop(bounds);
                        let before = serde_json::to_value(parent).unwrap();
                        this.drag = Some(super::Drag { container: container.into(), index, axis: super::Axis::Column });
                        this.drag_to(point(px(100.0), px(y)));
                        let after = this.chrome.find(container).unwrap();
                        if container == "sidebar" || total <= 48.0 {
                            assert_eq!(serde_json::to_value(after).unwrap(), before, "{container}[{index}], height {height}");
                        } else {
                            for child in &after.children()[index..=index + 1] {
                                if let Some(super::Span::Px { px }) = child.size {
                                    assert!(px >= 0.0 && px <= total, "{}: {px} out of {total}", child.id);
                                }
                            }
                        }
                    }
                }
            }
            this.drag = None;
        });
    }
}

#[test]
fn divider_drag_respects_trailing_sibling_min_height() {
    in_own_process("trailing_sibling_min_height_drag");
}

#[gpui::test]
fn trailing_sibling_min_height_drag(cx: &mut gpui::TestAppContext) {
    use gpui::{point, px, size};
    use serde_json::json;
    if !running_alone("trailing_sibling_min_height_drag") { return; }
    let layout = json!({"id": "root", "type": "row", "children": [
        {"id": "sidebar", "type": "column", "size": {"px": 268}, "resizable": false, "children": [
            {"id": "sidebar-lists", "type": "column", "children": [
                {"id": "spaces", "type": "block", "size": {"weight": 1.0}, "min": 90.0, "content": {"mod": "drag-probe"}},
                {"id": "agents", "type": "block", "size": {"weight": 1.0}, "min": 90.0, "content": {"mod": "drag-probe"}},
                {"id": "flash-notes", "type": "block", "size": {"weight": 0.75}, "min": 84.0, "content": {"mod": "drag-probe"}}
            ]}
        ]}
    ]});
    let (view, cx) = open_window(cx, "drag-probe", "export default () => null;", layout);
    cx.simulate_resize(size(px(900.0), px(550.0)));
    cx.update(|window, cx| window.draw(cx).clear(cx));
    view.update(cx, |this, _| {
        this.drag = Some(super::Drag {
            container: "sidebar-lists".into(),
            index: 0,
            axis: super::Axis::Column,
        });
        // Dragging the divider between spaces and agents far down must preserve agents.min (90px)
        // in the resulting weights rather than collapsing agents down to 24px.
        this.drag_to(point(px(100.0), px(2000.0)));
        this.drag = None;
        let lists = this.chrome.find("sidebar-lists").unwrap();
        let wa = match lists.children()[0].size { Some(super::Span::Weight { weight }) => weight, _ => panic!("expected weight for spaces") };
        let wb = match lists.children()[1].size { Some(super::Span::Weight { weight }) => weight, _ => panic!("expected weight for agents") };
        let pair_total = f32::from(
            this.bounds.borrow()["spaces"].size.height + this.bounds.borrow()["agents"].size.height,
        ) / super::settings::zoom();
        let expected_agents_px = pair_total * wb / (wa + wb);
        assert!(
            expected_agents_px >= 89.5,
            "dragging spaces divider down must preserve agents.min (90px), got {expected_agents_px:.1}px (wa={wa}, wb={wb})"
        );
    });
}
