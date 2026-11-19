//! Windows: overlays a mod opens with `kind: "window"`. They stay until
//! closed, move where they're dropped (a press anywhere the mod doesn't keep
//! for itself drags), and the one pressed last is on top. With a `key` a
//! window remembers its spot, and opening that key again replaces it; with
//! `keep` it also reopens on the next start, in the layout that opened it.
//! A layout switch closes them (they belong to their layout, like its mods).

use super::*;

/// How far the pointer moves before a press becomes a drag.
const DRAG_START: f32 = 4.0;
/// How much of a window stays on screen however far it's dragged, px.
const KEEP_SHOWN: f32 = 48.0;
/// Edge hit-zone thickness for resizable floating windows, px.
const RESIZE_EDGE_PX: f32 = 6.0;
/// Bottom-right corner grip size for resizable floating windows, px.
const RESIZE_CORNER_PX: f32 = 14.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WinOp {
    Move,
    Resize { left: bool, right: bool, bottom: bool },
}

pub struct WinDrag {
    pub id: String,
    op: WinOp,
    /// The pointer's offset from the window's top left (for Move).
    grab: Point<Pixels>,
    down: Point<Pixels>,
    pointer: Point<Pixels>,
    moving: bool,
    start_origin: Point<Pixels>,
    start_size: Size<Pixels>,
    viewport: Size<Pixels>,
}

// ---- windows.json: spots and sizes by key, and the kept windows of each layout

fn path() -> PathBuf {
    file("windows.json")
}

fn read() -> Value {
    std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str(&s).ok()).filter(Value::is_object).unwrap_or_else(|| json!({}))
}

fn edit(f: impl FnOnce(&mut Value)) {
    let mut v = read();
    f(&mut v);
    if let Ok(s) = serde_json::to_string_pretty(&v) {
        let _ = crate::persist::write_atomic(&path(), s + "\n");
    }
}

pub fn place_of(key: &str) -> Option<Point<Pixels>> {
    let p = read().get("places")?.get(key)?.as_array()?.clone();
    Some(point(px(p.first()?.as_f64()? as f32), px(p.get(1)?.as_f64()? as f32)))
}

pub fn size_of(key: &str) -> Option<Size<f32>> {
    let p = read().get("sizes")?.get(key)?.as_array()?.clone();
    let w = p.first()?.as_f64()? as f32;
    let h = p.get(1)?.as_f64()? as f32;
    (w > 0.0 && h > 0.0).then_some(size(w, h))
}

fn remember(key: &str, at: Point<Pixels>) {
    edit(|v| v["places"][key] = json!([f32::from(at.x).round(), f32::from(at.y).round()]));
}

fn remember_size(key: &str, sz: Size<f32>) {
    edit(|v| v["sizes"][key] = json!([sz.width.round(), sz.height.round()]));
}

pub fn keep(layout: &str, key: &str, spec: &Value) {
    edit(|v| v["kept"][layout][key] = spec.clone());
}

pub fn forget(layout: &str, key: &str) {
    if read().get("kept").and_then(|k| k.get(layout)).and_then(|l| l.get(key)).is_none() {
        return;
    }
    edit(|v| {
        if let Some(l) = v["kept"].get_mut(layout).and_then(Value::as_object_mut) {
            l.remove(key);
        }
    });
}

fn kept(layout: &str) -> Vec<Value> {
    read().get("kept").and_then(|k| k.get(layout)).and_then(Value::as_object).map(|l| l.values().cloned().collect()).unwrap_or_default()
}

impl AppWindow {
    /// The windows a layout keeps, reopened (at start, or on switching to it).
    pub(super) fn open_kept_windows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for spec in kept(&self.layout_name) {
            self.open_overlay(mods::new_overlay_id(), spec, window, cx);
        }
    }

    /// Leaving a layout: its windows close (the kept ones come back with it).
    pub(super) fn close_windows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The layout's windows go with it; the app's own (What's new) stay.
        let ids: Vec<String> = self.overlays.iter().filter(|o| o.kind == OverlayKind::Window && !o.frame).map(|o| o.id.clone()).collect();
        for id in ids {
            self.remove_overlay(&id, window, cx);
        }
    }

    /// A window at its spot: where it was dropped, else centered once its size
    /// is known (it stays invisible until then), always partly on screen.
    pub(super) fn render_window(&self, o: &Overlay, body: Stateful<Div>, viewport: Size<Pixels>, cx: &mut Context<Self>) -> AnyElement {
        let measured = self.bounds.borrow().get(&o.slot).map(|b| b.size);
        let size = measured.unwrap_or_default();
        let at = match (&self.win_drag, o.place) {
            (Some(d), _) if d.id == o.id && d.moving && d.op == WinOp::Move => d.pointer - d.grab,
            (_, Some(p)) => p,
            _ => point((viewport.width - size.width) / 2.0, (viewport.height - size.height) / 2.0),
        };
        let keep = px(KEEP_SHOWN);
        let x = at.x.min(viewport.width - keep).max(keep - size.width);
        let y = at.y.min(viewport.height - keep).max(px(0.0));
        let measure = {
            let (bounds, slot, known) = (self.bounds.clone(), o.slot.clone(), measured.is_some());
            canvas(
                move |b, window, _| {
                    if !known {
                        window.request_animation_frame();
                    }
                    bounds.borrow_mut().insert(slot, b);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full()
        };
        let (raise, press) = (o.id.clone(), o.id.clone());
        let mut body = body
            .relative()
            .child(measure)
            // Pressed anywhere, even on the mod's own controls: to the front.
            .capture_any_mouse_down(cx.listener(move |this, _, _, cx| this.raise_window(&raise, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.window_press(&press, ev.position, viewport, cx);
                }),
            );
        if o.resizable {
            let edge = px(RESIZE_EDGE_PX);
            let corner = px(RESIZE_CORNER_PX);
            let make_handle = |suffix: &'static str, op: WinOp, cursor: CursorStyle| {
                let id = o.id.clone();
                div()
                    .id(SharedString::from(format!("win-{suffix}-{}", o.id)))
                    .absolute()
                    .cursor(cursor)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.window_resize_press(&id, op, ev.position, viewport, cx);
                        }),
                    )
            };
            body = body
                .child(make_handle("rz-r", WinOp::Resize { left: false, right: true, bottom: false }, CursorStyle::ResizeLeftRight).right_0().top_0().bottom(corner).w(edge))
                .child(make_handle("rz-l", WinOp::Resize { left: true, right: false, bottom: false }, CursorStyle::ResizeLeftRight).left_0().top_0().bottom(corner).w(edge))
                .child(make_handle("rz-b", WinOp::Resize { left: false, right: false, bottom: true }, CursorStyle::ResizeUpDown).left(edge).right(corner).bottom_0().h(edge))
                .child(
                    make_handle("rz-br", WinOp::Resize { left: false, right: true, bottom: true }, CursorStyle::ResizeUpLeftDownRight)
                        .right_0()
                        .bottom_0()
                        .w(corner)
                        .h(corner)
                        .flex()
                        .items_end()
                        .justify_end()
                        .p(px(2.0))
                        .child(
                            div()
                                .w(px(7.0))
                                .h(px(7.0))
                                .border_r(px(1.5))
                                .border_b(px(1.5))
                                .border_color(c("dim"))
                                .rounded_br(px(2.0)),
                        ),
                );
        }
        div().absolute().left(x).top(y).when(measured.is_none(), |d| d.opacity(0.0)).child(body).into_any_element()
    }

    fn raise_window(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(i) = self.overlays.iter().position(|o| o.id == id) else { return };
        if i + 1 < self.overlays.len() {
            let o = self.overlays.remove(i);
            self.overlays.push(o);
            cx.notify();
        }
    }

    fn window_press(&mut self, id: &str, at: Point<Pixels>, viewport: Size<Pixels>, cx: &mut Context<Self>) {
        // A terminal in the window keeps its presses (selecting text).
        let on_terminal = self.overlays.iter().find(|o| o.id == id).and_then(|o| o.layout.as_ref()).is_some_and(|l| {
            let bounds = self.bounds.borrow();
            l.visible_blocks().iter().any(|n| matches!(n.content(), Some(Content::Terminal { .. })) && bounds.get(&n.id).is_some_and(|b| b.contains(&at)))
        });
        if on_terminal {
            return;
        }
        let Some(b) = self.bounds.borrow().get(&format!("overlay:{id}")).copied() else { return };
        self.win_drag = Some(WinDrag {
            id: id.to_owned(),
            op: WinOp::Move,
            grab: at - b.origin,
            down: at,
            pointer: at,
            moving: false,
            start_origin: b.origin,
            start_size: b.size,
            viewport,
        });
        cx.notify();
    }

    fn window_resize_press(&mut self, id: &str, op: WinOp, at: Point<Pixels>, viewport: Size<Pixels>, cx: &mut Context<Self>) {
        let Some(b) = self.bounds.borrow().get(&format!("overlay:{id}")).copied() else { return };
        self.raise_window(id, cx);
        self.win_drag = Some(WinDrag {
            id: id.to_owned(),
            op,
            grab: point(px(0.0), px(0.0)),
            down: at,
            pointer: at,
            moving: true,
            start_origin: b.origin,
            start_size: b.size,
            viewport,
        });
        cx.notify();
    }

    fn window_move(&mut self, at: Point<Pixels>, pressed: bool, cx: &mut Context<Self>) {
        let Some(d) = &mut self.win_drag else { return };
        if !pressed {
            self.win_drag = None;
            cx.notify();
            return;
        }
        d.pointer = at;
        let dist = f32::from((at.x - d.down.x).abs()).max(f32::from((at.y - d.down.y).abs()));
        if !d.moving && dist < DRAG_START {
            return;
        }
        d.moving = true;
        if let WinOp::Resize { left, right, bottom } = d.op {
            let id = d.id.clone();
            let dx = f32::from(at.x - d.down.x);
            let dy = f32::from(at.y - d.down.y);
            let start_w = f32::from(d.start_size.width);
            let start_h = f32::from(d.start_size.height);
            let start_x = f32::from(d.start_origin.x);
            let start_y = f32::from(d.start_origin.y);
            let max_w = (f32::from(d.viewport.width) - 24.0).max(320.0);
            let max_h = (f32::from(d.viewport.height) - 24.0).max(240.0);
            let zoom = f32::from(settings::z(1.0)).max(0.25);
            if let Some(o) = self.overlays.iter_mut().find(|o| o.id == id) {
                let min_w = f32::from(settings::z(o.min_size.width)).max(240.0);
                let min_h = f32::from(settings::z(o.min_size.height)).max(180.0);
                let mut new_w = start_w;
                let mut new_h = start_h;
                let mut new_x = start_x;
                if right {
                    new_w = (start_w + dx).clamp(min_w, max_w);
                }
                if left {
                    let clamped_w = (start_w - dx).clamp(min_w, max_w);
                    new_x = start_x + (start_w - clamped_w);
                    new_w = clamped_w;
                }
                if bottom {
                    new_h = (start_h + dy).clamp(min_h, max_h);
                }
                o.size = size((new_w / zoom).max(o.min_size.width), (new_h / zoom).max(o.min_size.height));
                o.fit = false;
                if left || o.place.is_none() {
                    o.place = Some(point(px(new_x), px(start_y)));
                }
            }
        }
        cx.notify();
    }

    /// Ends a press: a drag leaves the window there (and remembers it under its
    /// key); returns true so the release doesn't also click what's under it.
    fn window_release(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(d) = self.win_drag.take() else { return false };
        cx.notify();
        if !d.moving {
            return false;
        }
        match d.op {
            WinOp::Move => {
                let at = d.pointer - d.grab;
                if let Some(o) = self.overlays.iter_mut().find(|o| o.id == d.id) {
                    o.place = Some(at);
                    if let Some(k) = &o.key {
                        remember(k, at);
                    }
                }
            }
            WinOp::Resize { .. } => {
                if let Some(o) = self.overlays.iter().find(|o| o.id == d.id)
                    && let Some(k) = &o.key
                {
                    remember_size(k, o.size);
                    if let Some(at) = o.place {
                        remember(k, at);
                    }
                }
            }
        }
        true
    }

    /// While a window is pressed: follows the pointer anywhere in the app.
    pub(super) fn window_tracker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.win_drag.as_ref()?;
        let this = cx.entity().downgrade();
        Some(
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let t = this.clone();
                    window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            let _ = t.update(cx, |this, cx| this.window_move(ev.position, ev.pressed_button == Some(MouseButton::Left), cx));
                        }
                    });
                    let t = this.clone();
                    window.on_mouse_event(move |ev: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture && ev.button == MouseButton::Left && t.update(cx, |this, cx| this.window_release(cx)).unwrap_or(false) {
                            cx.stop_propagation();
                        }
                    });
                },
            )
            .absolute()
            .size_full()
            .into_any_element(),
        )
    }
}
