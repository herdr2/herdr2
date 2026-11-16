//! Native GPUI ACP agent session pane. Renders structured transcript entries,
//! inline permission approval prompts, search bar, and a prompt composer with zero
//! terminal emulation or Ghostty dependencies.

mod drag_paths;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use base64::Engine as _;
use gpui::{
    AnyElement, App, ClipboardEntry, ClipboardItem, Context, ExternalPaths, FocusHandle,
    Focusable, Hsla, ImageFormat, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, Pixels, Point, Render, Rgba, ScrollHandle, SharedString, Size,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, canvas, div,
    prelude::FluentBuilder as _, point, px, size,
};
use gpui_base::input::{
    Copy as InputCopy, Input, InputEditorStyle, InputEvent, InputState, Paste as InputPaste,
};
use gpui_shell::DraggedPaths;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::hc::{self, Core};
use crate::links;
use crate::settings;
use crate::vt::{self, CellPosition, Rgb, SelectionPositions};

thread_local! {
    static FRAME: Cell<u64> = const { Cell::new(1) };
}

fn pad() -> (f32, f32) {
    let padding = settings::get().terminal_padding;
    let zoom = settings::zoom();
    (padding[0] * zoom, padding[1] * zoom)
}

/// Logical character cell grid dimensions derived from pane bounds, used to keep
/// layout sizing calculations consistent across hidden and split panes.
pub(crate) fn grid_size(bounds: Size<Pixels>, cell: Size<Pixels>) -> (u16, u16) {
    let (px_, py_) = pad();
    let cols = ((f32::from(bounds.width) - 2.0 * px_) / f32::from(cell.width).max(1.0))
        .floor()
        .clamp(2.0, u16::MAX as f32) as u16;
    let rows = ((f32::from(bounds.height) - 2.0 * py_) / f32::from(cell.height).max(1.0))
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    (cols, rows)
}

/// Measures a single monospace cell at the current font settings.
pub fn cell_size(window: &Window) -> Size<Pixels> {
    let s = settings::get();
    let font_size = px(settings::font_size());
    let mut text_font = gpui::font(settings::terminal_font());
    text_font.weight = gpui::FontWeight(s.font_weight as f32);
    let ts = window.text_system();
    let primary = ts.resolve_font(&text_font);
    let width = ts
        .advance(primary, font_size, 'M')
        .map(|a| a.width)
        .unwrap_or_else(|_| font_size * 0.6)
        * s.terminal_cell_width;
    let height = font_size * s.terminal_line_height;
    size(width.max(px(4.0)), height.max(px(6.0)))
}

pub fn hsla(c: Rgb) -> Hsla {
    Rgba {
        r: c.0 as f32 / 255.0,
        g: c.1 as f32 / 255.0,
        b: c.2 as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

pub fn next_frame() {
    FRAME.set(FRAME.get().wrapping_add(1).max(1));
}

pub fn flush_output_clock(_cx: &mut gpui::App) {}

pub fn initial_display(window: &Window) -> Value {
    let cell = cell_size(window);
    let scale = window.scale_factor();
    let cw = (f32::from(cell.width) * scale).round().max(1.0) as u32;
    let ch = (f32::from(cell.height) * scale).round().max(1.0) as u32;
    let theme = vt::theme();
    json!({
        "cell": [cw, ch],
        "bg": format!("#{:02x}{:02x}{:02x}", theme.bg.0, theme.bg.1, theme.bg.2),
        "fg": format!("#{:02x}{:02x}{:02x}", theme.fg.0, theme.fg.1, theme.fg.2),
    })
}

/// Strips leading agent status indicators (braille spinners, checks) for clean tab labels.
pub(crate) fn shown_title(title: &str) -> std::borrow::Cow<'_, str> {
    let mut rest = title;
    loop {
        let trimmed = rest.trim_start();
        let Some(first) = trimmed.chars().next() else {
            return if trimmed.len() == title.len() {
                std::borrow::Cow::Borrowed(title)
            } else {
                std::borrow::Cow::Borrowed(trimmed)
            };
        };
        let is_prefix = matches!(
            first,
            '\u{2800}'..='\u{28ff}' | '✳' | '✓' | '✔' | '✗' | '✘' | '•' | '·' | '▸' | '⏳' | '⟳'
        );
        if is_prefix {
            rest = trimmed[first.len_utf8()..].trim_start();
        } else {
            return if rest.len() == title.len() {
                std::borrow::Cow::Borrowed(title)
            } else {
                std::borrow::Cow::Borrowed(rest)
            };
        }
    }
}

pub enum TerminalEvent {
    Peek(String),
    TitleChanged,
    SelectionChanged,
    ScrollChanged,
    Ended,
    ContextMenu(Point<Pixels>),
    Notice { title: String, body: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollMetrics {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
    pub from_bottom: u64,
}

impl ScrollMetrics {
    pub fn json(self) -> Value {
        json!({
            "total": self.total,
            "offset": self.offset,
            "len": self.len,
            "from_bottom": self.from_bottom,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollTarget {
    Lines(isize),
    Pages(f64),
    Top,
    Bottom,
}

pub fn scroll_target(args: &Value) -> Result<ScrollTarget, String> {
    if let Some(to) = args.get("to").and_then(Value::as_str) {
        return match to {
            "top" => Ok(ScrollTarget::Top),
            "bottom" => Ok(ScrollTarget::Bottom),
            other => Err(format!("unknown scroll target `{other}` (expected `top` or `bottom`)")),
        };
    }
    if let Some(lines) = args.get("lines").and_then(Value::as_i64) {
        return isize::try_from(lines)
            .map(ScrollTarget::Lines)
            .map_err(|_| format!("lines out of range: {lines}"));
    }
    if let Some(pages) = args.get("pages").and_then(Value::as_f64) {
        if !pages.is_finite() {
            return Err("pages must be finite".into());
        }
        return Ok(ScrollTarget::Pages(pages));
    }
    Err("expected `to` (`top`/`bottom`), `lines`, or `pages`".into())
}

/// Structured ACP transcript entry matching `herdr2::transcript::Entry`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TranscriptEntry {
    User {
        #[serde(default)]
        turn: u64,
        #[serde(default)]
        from: Option<String>,
        text: String,
        #[serde(default)]
        ts_ms: u64,
    },
    Thought {
        #[serde(default)]
        turn: u64,
        text: String,
        #[serde(default)]
        ts_ms: u64,
    },
    Assistant {
        #[serde(default)]
        turn: u64,
        text: String,
        #[serde(default)]
        ts_ms: u64,
    },
    ToolCall {
        #[serde(default)]
        turn: u64,
        id: String,
        title: String,
        #[serde(default)]
        tool_kind: Option<String>,
        status: String,
        #[serde(default)]
        ts_ms: u64,
    },
    Plan {
        #[serde(default)]
        turn: u64,
        #[serde(default)]
        items: Vec<String>,
        #[serde(default)]
        ts_ms: u64,
    },
    Permission {
        #[serde(default)]
        turn: u64,
        title: String,
        outcome: String,
        #[serde(default)]
        ts_ms: u64,
    },
    Error {
        #[serde(default)]
        turn: u64,
        message: String,
        #[serde(default)]
        ts_ms: u64,
    },
    Stderr {
        text: String,
        #[serde(default)]
        ts_ms: u64,
    },
}

impl TranscriptEntry {
    fn plain_text(&self) -> String {
        match self {
            TranscriptEntry::User { from, text, .. } => {
                let (env_from, body) = unwrap_h2_envelope(text);
                match from.as_ref().or(env_from.as_ref()) {
                    Some(sender) => format!("User (from {sender}): {body}"),
                    None => format!("User: {body}"),
                }
            }
            TranscriptEntry::Thought { text, .. } => format!("Thought: {text}"),
            TranscriptEntry::Assistant { text, .. } => text.clone(),
            TranscriptEntry::ToolCall { title, tool_kind, status, .. } => match tool_kind {
                Some(kind) => format!("[{status}] {kind}: {title}"),
                None => format!("[{status}] {title}"),
            },
            TranscriptEntry::Plan { items, .. } => {
                if items.is_empty() {
                    "Plan: (empty)".to_owned()
                } else {
                    format!("Plan:\n{}", items.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n"))
                }
            }
            TranscriptEntry::Permission { title, outcome, .. } => format!("Permission ({outcome}): {title}"),
            TranscriptEntry::Error { message, .. } => format!("Error: {message}"),
            TranscriptEntry::Stderr { text, .. } => format!("stderr: {text}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
struct PermissionOption {
    option_id: String,
    name: String,
    kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
struct PendingPermission {
    request_id: String,
    #[serde(default)]
    session_id: String,
    title: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    options: Vec<PermissionOption>,
}

/// Unwraps `<h2 from="..." ...>body</h2>` or `<hc ...>body</hc>` envelopes injected
/// during agent-to-agent delivery so the UI renders clean message text + sender chip.
pub(crate) fn unwrap_h2_envelope(raw: &str) -> (Option<String>, String) {
    let trimmed = raw.trim();
    for tag in ["h2", "hc"] {
        let open_prefix = format!("<{tag}");
        let close_tag = format!("</{tag}>");
        if let Some(after_open) = trimmed.strip_prefix(&open_prefix)
            && matches!(after_open.chars().next(), Some(' ' | '\t' | '>'))
            && let Some(gt) = after_open.find('>')
            && let Some(inner_with_open) = trimmed.strip_suffix(&close_tag)
        {
            let header = &after_open[..gt];
            let open_len = open_prefix.len() + gt + 1;
            if open_len <= inner_with_open.len() {
                let body = inner_with_open[open_len..].trim().to_owned();
                let from = extract_attr(header, "from");
                return (from, body);
            }
        }
    }
    (None, raw.to_owned())
}

fn extract_attr(header: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = header.find(&needle)? + needle.len();
    let end = header[start..].find('"')?;
    let val = header[start..start + end].trim();
    (!val.is_empty()).then(|| val.to_owned())
}

/// Merges consecutive streamed `Assistant`, `Thought`, `Stderr` chunks and updates
/// in-flight `ToolCall` entries with matching `(turn, id)` into single readable rows.
pub(crate) fn coalesce_entries(entries: &mut Vec<TranscriptEntry>, next: TranscriptEntry) {
    match next {
        TranscriptEntry::Assistant { turn, text, ts_ms } => {
            if let Some(TranscriptEntry::Assistant { turn: prev_turn, text: prev_text, ts_ms: prev_ts }) = entries.last_mut()
                && *prev_turn == turn
            {
                prev_text.push_str(&text);
                *prev_ts = ts_ms;
                return;
            }
            entries.push(TranscriptEntry::Assistant { turn, text, ts_ms });
        }
        TranscriptEntry::Thought { turn, text, ts_ms } => {
            if let Some(TranscriptEntry::Thought { turn: prev_turn, text: prev_text, ts_ms: prev_ts }) = entries.last_mut()
                && *prev_turn == turn
            {
                prev_text.push_str(&text);
                *prev_ts = ts_ms;
                return;
            }
            entries.push(TranscriptEntry::Thought { turn, text, ts_ms });
        }
        TranscriptEntry::Stderr { text, ts_ms } => {
            if let Some(TranscriptEntry::Stderr { text: prev_text, ts_ms: prev_ts }) = entries.last_mut() {
                if !prev_text.is_empty() && !prev_text.ends_with('\n') && !text.starts_with('\n') {
                    prev_text.push('\n');
                }
                prev_text.push_str(&text);
                *prev_ts = ts_ms;
                return;
            }
            entries.push(TranscriptEntry::Stderr { text, ts_ms });
        }
        TranscriptEntry::ToolCall { turn, id, title, tool_kind, status, ts_ms } => {
            if let Some(existing) = entries.iter_mut().rev().take(24).find(|e| {
                matches!(e, TranscriptEntry::ToolCall { turn: t, id: eid, .. } if *t == turn && *eid == id)
            }) {
                *existing = TranscriptEntry::ToolCall { turn, id, title, tool_kind, status, ts_ms };
                return;
            }
            entries.push(TranscriptEntry::ToolCall { turn, id, title, tool_kind, status, ts_ms });
        }
        other => entries.push(other),
    }
}

struct FitState {
    cols: u16,
    rows: u16,
    cell: Size<Pixels>,
    scale: f32,
    attach_ready: bool,
    fit_ready: Option<async_channel::Sender<()>>,
}

impl FitState {
    fn ready_to_attach(&mut self) {
        self.attach_ready = true;
        if let Some(ready) = self.fit_ready.take() {
            let _ = ready.try_send(());
        }
    }

    fn fit(&mut self, cols: u16, rows: u16, cell: Size<Pixels>, scale: f32) {
        self.cols = cols.max(2);
        self.rows = rows.max(1);
        self.cell = cell;
        self.scale = scale;
        self.ready_to_attach();
    }
}

pub struct TerminalView {
    pub address: String,
    pub title: String,
    pub ended: Option<String>,
    pub lost: bool,
    pub cwd: Option<PathBuf>,
    pub right_click_to_program: bool,

    core: Arc<Core>,
    preset: Option<String>,
    state: String,
    turn: u64,
    inbox_len: u64,
    pending_permission: Option<PendingPermission>,
    entries: Vec<TranscriptEntry>,

    selected_entry: Option<usize>,
    all_selected: bool,
    published_selection: Option<SelectionPositions>,

    hovered_now: bool,
    focus: FocusHandle,
    composer: Option<gpui::Entity<InputState>>,
    composer_sub: Option<Subscription>,
    composer_focused: bool,
    clear_composer: bool,
    pending_insert_text: Option<String>,
    pending_prompt_error: Option<String>,

    scroll_handle: ScrollHandle,
    stick_to_bottom: bool,
    last_scroll_metrics: Option<ScrollMetrics>,

    search_open: bool,
    search_keys: bool,
    search_query: String,
    search_input: Option<gpui::Entity<InputState>>,
    search_sub: Option<Subscription>,
    match_indices: Vec<usize>,
    selected_match: Option<usize>,

    shown_in: Cell<u64>,
    fit_state: Rc<RefCell<FitState>>,
    attach_deadline: std::time::Instant,
    _loader: Option<Task<()>>,
}

impl TerminalView {
    /// Waits briefly for first pane layout before querying core so `fit()` dimensions settle cleanly.
    pub fn attach(core: Arc<Core>, address: String, size_hint: (u16, u16), cx: &mut Context<Self>) -> Self {
        let attach_deadline = cx.background_executor().now() + Duration::from_millis(100);
        let mut view = Self {
            address: address.clone(),
            title: address.clone(),
            ended: None,
            lost: false,
            cwd: None,
            right_click_to_program: false,
            core: core.clone(),
            preset: None,
            state: "starting".to_owned(),
            turn: 0,
            inbox_len: 0,
            pending_permission: None,
            entries: Vec::new(),
            selected_entry: None,
            all_selected: false,
            published_selection: None,
            hovered_now: false,
            focus: cx.focus_handle(),
            composer: None,
            composer_sub: None,
            composer_focused: false,
            clear_composer: false,
            pending_insert_text: None,
            pending_prompt_error: None,
            scroll_handle: ScrollHandle::new(),
            stick_to_bottom: true,
            last_scroll_metrics: None,
            search_open: false,
            search_keys: false,
            search_query: String::new(),
            search_input: None,
            search_sub: None,
            match_indices: Vec::new(),
            selected_match: None,
            shown_in: Cell::new(0),
            fit_state: Rc::new(RefCell::new(FitState {
                cols: size_hint.0.max(2),
                rows: size_hint.1.max(1),
                cell: size(px(9.0), px(18.0)),
                scale: 1.0,
                attach_ready: false,
                fit_ready: None,
            })),
            attach_deadline,
            _loader: None,
        };
        view.connect(core, cx);
        view
    }

    pub fn reattach(&mut self, core: Arc<Core>, cx: &mut Context<Self>) {
        if matches!(self.ended.as_deref(), Some("exited" | "killed" | "finished")) {
            return;
        }
        self.ended = None;
        self.lost = false;
        self.core = core.clone();
        self.connect(core, cx);
        cx.notify();
    }

    pub fn reattach_if_needed(&mut self, core: Arc<Core>, cx: &mut Context<Self>) {
        if self.lost || self.ended.is_some() {
            self.reattach(core, cx);
        }
    }

    fn connect(&mut self, core: Arc<Core>, cx: &mut Context<Self>) {
        self.core = core.clone();
        let fit_wait = {
            let mut fs = self.fit_state.borrow_mut();
            if fs.attach_ready {
                None
            } else {
                let (ready, wait) = async_channel::bounded(1);
                fs.fit_ready = Some(ready);
                Some(wait)
            }
        };
        let address = self.address.clone();
        let deadline = cx
            .background_executor()
            .timer(self.attach_deadline.saturating_duration_since(cx.background_executor().now()));

        self._loader = Some(cx.spawn(async move |this, cx| {
            if let Some(ready) = fit_wait {
                let mut ready = std::pin::pin!(ready.recv());
                let mut deadline = std::pin::pin!(deadline);
                std::future::poll_fn(|cx| {
                    if ready.as_mut().poll(cx).is_ready() || deadline.as_mut().poll(cx).is_ready() {
                        Poll::Ready(())
                    } else {
                        Poll::Pending
                    }
                })
                .await;
            }
            let _ = this.update(cx, |this, _| {
                this.fit_state.borrow_mut().ready_to_attach();
            });
            let addr_get = address.clone();
            let core_get = core.clone();
            let fetched = cx
                .background_executor()
                .spawn(async move {
                    let info = core_get.get_agent(&addr_get)?;
                    let transcript = core_get.read_agent(&addr_get)?;
                    Ok::<_, anyhow::Error>((info, transcript))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.apply_initial_fetch(fetched, cx);
            });
        }));
    }

    fn apply_initial_fetch(
        &mut self,
        result: anyhow::Result<(Value, Value)>,
        cx: &mut Context<Self>,
    ) {
        cx.notify();
        match result {
            Ok((info, transcript)) => {
                self.lost = false;
                self.ended = None;
                self.sync_record(&info, cx);
                self.entries.clear();
                if let Some(arr) = transcript.get("entries").and_then(Value::as_array) {
                    for raw in arr {
                        if let Ok(entry) = serde_json::from_value::<TranscriptEntry>(raw.clone()) {
                            coalesce_entries(&mut self.entries, entry);
                        }
                    }
                }
                self.recompute_matches();
                if self.stick_to_bottom {
                    self.scroll_to_bottom_internal();
                }
                self.publish_scroll_if_changed(cx);
            }
            Err(e) => {
                let msg = format!("{e:#}");
                let kind = hc::error_kind(&e);
                if kind == Some("unavailable")
                    || msg.contains("lost the core")
                    || msg.contains("no reply")
                    || msg.contains("offline")
                    || msg.contains("disconnected")
                {
                    self.lost = true;
                } else if kind == Some("finished") {
                    self.ended = Some("finished".to_owned());
                    cx.emit(TerminalEvent::Ended);
                } else {
                    self.ended = Some(msg);
                }
            }
        }
    }

    /// Updates agent metadata, status, inbox count, and pending permission from a core record.
    pub fn sync_record(&mut self, rec: &Value, cx: &mut Context<Self>) {
        let report = rec.get("report").filter(|v| !v.is_null()).or_else(|| rec.get("status"));
        if let Some(preset) = rec.get("preset").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            self.preset = Some(preset.to_owned());
        } else if let Some(preset) = rec.pointer("/agent_info/preset").and_then(Value::as_str) {
            self.preset = Some(preset.to_owned());
        }
        if let Some(title) = rec.get("title").and_then(Value::as_str).filter(|t| !t.is_empty()) {
            self.set_title(title, cx);
        }
        if let Some(state) = rec
            .pointer("/status/state")
            .and_then(Value::as_str)
            .or_else(|| rec.get("status").and_then(Value::as_str))
            .or_else(|| report.and_then(|r| r.get("state")).and_then(Value::as_str))
        {
            self.state = state.to_owned();
            if matches!(state, "exited" | "killed" | "finished" | "stopped" | "ended") {
                if self.ended.is_none() {
                    self.ended = Some(state.to_owned());
                }
            } else {
                self.ended = None;
            }
        }
        if let Some(report) = report {
            if let Some(turn) = report.get("turn").and_then(Value::as_u64) {
                self.turn = turn;
            }
            if let Some(inbox) = report.get("inbox_len").and_then(Value::as_u64) {
                self.inbox_len = inbox;
            }
            self.pending_permission = report
                .get("pending_permission")
                .filter(|v| !v.is_null())
                .and_then(|v| serde_json::from_value::<PendingPermission>(v.clone()).ok());
        }
        if self.cwd.is_none()
            && let Some(dir) = rec.get("cwd").and_then(Value::as_str).filter(|d| !d.is_empty())
        {
            self.cwd = Some(PathBuf::from(dir));
        }
        cx.notify();
    }

    /// Appends or coalesces a single pushed JSON transcript entry emitted by `EventLines`.
    pub fn push_transcript_entry(&mut self, entry: Value, cx: &mut Context<Self>) {
        if let Ok(parsed) = serde_json::from_value::<TranscriptEntry>(entry) {
            coalesce_entries(&mut self.entries, parsed);
            self.recompute_matches();
            if self.stick_to_bottom {
                self.scroll_to_bottom_internal();
            }
            self.publish_scroll_if_changed(cx);
            cx.notify();
        }
    }

    pub fn refresh_theme(&self) {}

    fn set_title(&mut self, title: &str, cx: &mut Context<Self>) {
        let shown = shown_title(title);
        if shown != self.title {
            self.title = shown.into_owned();
            cx.emit(TerminalEvent::TitleChanged);
        }
    }

    pub(crate) fn dimensions(&self) -> (u16, u16) {
        let fs = self.fit_state.borrow();
        (fs.cols, fs.rows)
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn owns_keyboard_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.is_focused(window)
            || self.find_field_focused(window, cx)
            || self.composer_field_focused(window, cx)
    }

    pub fn focus_target(&self, cx: &App) -> FocusHandle {
        if self.search_open
            && self.search_keys
            && let Some(input) = &self.search_input
        {
            return input.read(cx).focus_handle(cx);
        }
        if let Some(composer) = &self.composer {
            return composer.read(cx).focus_handle(cx);
        }
        self.focus.clone()
    }

    fn find_field_focused(&self, window: &Window, cx: &App) -> bool {
        self.search_open
            && self
                .search_input
                .as_ref()
                .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window))
    }

    fn composer_field_focused(&self, window: &Window, cx: &App) -> bool {
        self.composer
            .as_ref()
            .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window))
    }

    pub fn detach(&self) {}

    pub fn mark_shown(&self) {
        self.shown_in.set(FRAME.get());
    }

    pub fn blur(&mut self) {
        self.composer_focused = false;
    }

    pub fn fit(&self, cols: u16, rows: u16, cell: Size<Pixels>, scale: f32) {
        self.fit_state.borrow_mut().fit(cols, rows, cell, scale);
    }

    pub fn on_screen_this_frame(&self) -> bool {
        self.shown_in.get() == FRAME.get()
    }

    pub fn hovered(&self) -> bool {
        self.hovered_now
    }

    fn submit_prompt(&mut self, text: String, steer: bool, cx: &mut Context<Self>) {
        self.pending_prompt_error = None;
        let core = self.core.clone();
        let addr = self.address.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { core.send_prompt(&addr, &text, steer) })
                .await;
            if let Err(err) = res {
                let _ = this.update(cx, |this, cx| {
                    this.pending_prompt_error = Some(format!("{err:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn interrupt(&mut self, cx: &mut Context<Self>) {
        let core = self.core.clone();
        let addr = self.address.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { core.interrupt_agent(&addr) })
                .await;
            if let Err(err) = res {
                let _ = this.update(cx, |this, cx| {
                    this.pending_prompt_error = Some(format!("{err:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn resolve_permission(&mut self, option_id: Option<String>, cx: &mut Context<Self>) {
        let core = self.core.clone();
        let addr = self.address.clone();
        self.pending_permission = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    match option_id.as_deref() {
                        Some(id) => core.approve_agent(&addr, Some(id)),
                        None => core.deny_agent(&addr),
                    }
                })
                .await;
            if let Err(err) = res {
                let _ = this.update(cx, |this, cx| {
                    this.pending_prompt_error = Some(format!("{err:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn dropped_from_mod(&mut self, paths: &DraggedPaths, _window: &mut Window, cx: &mut Context<Self>) {
        let machine = self.machine_name().unwrap_or("");
        match drag_paths::same_machine(paths, machine) {
            Ok(true) => {
                if let Ok(quoted) = drag_paths::quote_paths(paths.paths.iter().cloned(), machine, cfg!(target_os = "windows") && machine.is_empty()) {
                    self.queue_composer_insert(quoted, cx);
                }
            }
            Ok(false) => {
                let local: Vec<PathBuf> = paths.paths.iter().map(PathBuf::from).collect();
                self.dropped(local, cx);
            }
            Err(why) => drag_paths::unavailable(why),
        }
    }

    fn dropped(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let machine = self.machine_name().unwrap_or("").to_owned();
        if !machine.is_empty() {
            let mut valid = Vec::new();
            let mut refused = Vec::new();
            for path in paths {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| path.display().to_string());
                if let Some(why) = drag_paths::upload_refusal(&path) {
                    refused.push((name, why.to_owned()));
                } else {
                    valid.push(path);
                }
            }
            if let Some((title, body)) = refusal_notice(&refused) {
                cx.emit(TerminalEvent::Notice { title, body });
            }
            if let Err(why) = drag_paths::validate_upload(&valid) {
                if refused.is_empty() {
                    cx.emit(TerminalEvent::Notice {
                        title: "Couldn't upload dropped files".into(),
                        body: why.to_owned(),
                    });
                }
                return;
            }
            self.upload_paths(valid, cx);
            return;
        }
        let quoted = drag_paths::quote_paths(
            paths.into_iter().map(|p| p.to_string_lossy().into_owned()),
            "",
            cfg!(target_os = "windows"),
        );
        match quoted {
            Ok(text) => self.queue_composer_insert(text, cx),
            Err(why) => drag_paths::unavailable(why),
        }
    }

    fn upload_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let core = self.core.clone();
        let addr = self.address.clone();
        cx.spawn(async move |this, cx| {
            let (uploaded, refused) = cx
                .background_executor()
                .spawn(async move {
                    let mut uploaded = Vec::new();
                    let mut refused = Vec::new();
                    for path in paths {
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("attachment")
                            .to_owned();
                        match std::fs::read(&path) {
                            Ok(bytes) => {
                                let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                                match core.put_file(&addr, &name, &encoded) {
                                    Ok(remote) => uploaded.push(remote),
                                    Err(e) => refused.push((name, format!("{e:#}"))),
                                }
                            }
                            Err(e) => refused.push((name, e.to_string())),
                        }
                    }
                    (uploaded, refused)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some((title, body)) = refusal_notice(&refused) {
                    cx.emit(TerminalEvent::Notice { title, body });
                }
                if !uploaded.is_empty() {
                    let machine = this.machine_name().unwrap_or("");
                    if let Ok(quoted) = drag_paths::quote_paths(uploaded, machine, false) {
                        this.queue_composer_insert(quoted, cx);
                    }
                }
            });
        })
        .detach();
    }

    fn machine_name(&self) -> Option<&str> {
        let (machine, rest) = self.address.split_once(':')?;
        (!machine.is_empty() && !rest.is_empty()).then_some(machine)
    }

    fn queue_composer_insert(&mut self, text: String, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        let insertion = match &mut self.pending_insert_text {
            Some(buf) => {
                if !buf.ends_with(' ') {
                    buf.push(' ');
                }
                buf.push_str(&text);
                return;
            }
            slot => slot,
        };
        *insertion = Some(text);
        cx.notify();
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(paths) = copied_files(&item) {
            self.dropped(paths, cx);
            return;
        }
        if let Some(text) = item.text()
            && !text.is_empty()
        {
            self.queue_composer_insert(text, cx);
            return;
        }
        if let Some(path) = image_file(&item) {
            self.upload_paths(vec![path], cx);
        }
    }

    pub fn paste_image(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(paths) = copied_files(&item) {
            self.dropped(paths, cx);
            return;
        }
        if let Some(path) = image_file(&item) {
            self.upload_paths(vec![path], cx);
        } else if let Some(text) = item.text()
            && !text.is_empty()
        {
            self.queue_composer_insert(text, cx);
        }
    }

    pub fn copy(&mut self, cx: &mut Context<Self>) -> bool {
        let text = if self.all_selected {
            self.all_transcript_text()
        } else if let Some(idx) = self.selected_entry {
            self.entries.get(idx).map(TranscriptEntry::plain_text).unwrap_or_default()
        } else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    pub fn selection(&self) -> Option<SelectionPositions> {
        self.published_selection.clone()
    }

    pub fn selection_command(&mut self, name: &str, _args: &Value, cx: &mut Context<Self>) -> Result<(), String> {
        match name {
            "terminal.selection.clear" | "clear" => {
                self.clear_selection(cx);
                Ok(())
            }
            "terminal.select_all" | "all" => {
                self.select_all(cx);
                Ok(())
            }
            other => Err(format!("unsupported selection command in agent view: {other}")),
        }
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        if self.entries.is_empty() {
            return;
        }
        self.all_selected = true;
        self.selected_entry = None;
        self.published_selection = Some(SelectionPositions {
            start: CellPosition { row: 0, col: 0 },
            end: CellPosition {
                row: self.entries.len().saturating_sub(1) as u32,
                col: 1,
            },
        });
        cx.emit(TerminalEvent::SelectionChanged);
        cx.notify();
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        let had = self.all_selected || self.selected_entry.is_some() || self.published_selection.is_some();
        self.all_selected = false;
        self.selected_entry = None;
        self.published_selection = None;
        if had {
            cx.emit(TerminalEvent::SelectionChanged);
            cx.notify();
        }
    }

    fn select_entry(&mut self, idx: usize, cx: &mut Context<Self>) {
        self.all_selected = false;
        self.selected_entry = Some(idx);
        self.published_selection = Some(SelectionPositions {
            start: CellPosition { row: idx as u32, col: 0 },
            end: CellPosition { row: idx as u32, col: 1 },
        });
        if settings::get().copy_on_select
            && let Some(entry) = self.entries.get(idx)
        {
            cx.write_to_clipboard(ClipboardItem::new_string(entry.plain_text()));
        }
        cx.emit(TerminalEvent::SelectionChanged);
        cx.notify();
    }

    fn all_transcript_text(&self) -> String {
        self.entries
            .iter()
            .map(TranscriptEntry::plain_text)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = true;
        self.search_keys = true;
        if self.search_input.is_none() {
            let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in transcript"));
            let sub = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.search_query = input.read(cx).value().to_string();
                    this.recompute_matches();
                    cx.notify();
                } else if let InputEvent::PressEnter { shift, .. } = event {
                    this.step_search(*shift, cx);
                }
            });
            self.search_sub = Some(sub);
            self.search_input = Some(input);
        }
        if let Some(input) = &self.search_input {
            input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
        self.search_keys = false;
        self.search_query.clear();
        self.match_indices.clear();
        self.selected_match = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn recompute_matches(&mut self) {
        self.match_indices.clear();
        self.selected_match = None;
        let q = self.search_query.trim().to_lowercase();
        if q.is_empty() {
            return;
        }
        for (idx, entry) in self.entries.iter().enumerate() {
            if entry.plain_text().to_lowercase().contains(&q) {
                self.match_indices.push(idx);
            }
        }
        if !self.match_indices.is_empty() {
            self.selected_match = Some(self.match_indices.len() - 1);
            if let Some(&entry_idx) = self.match_indices.last() {
                self.scroll_handle.scroll_to_item(entry_idx);
            }
        }
    }

    fn step_search(&mut self, backwards: bool, cx: &mut Context<Self>) {
        if self.match_indices.is_empty() {
            return;
        }
        let len = self.match_indices.len();
        let next = match self.selected_match {
            Some(cur) if backwards => (cur + len - 1) % len,
            Some(cur) => (cur + 1) % len,
            None => 0,
        };
        self.selected_match = Some(next);
        self.stick_to_bottom = false;
        self.scroll_handle.scroll_to_item(self.match_indices[next]);
        cx.notify();
    }

    pub fn scroll_history(&mut self, target: ScrollTarget, cx: &mut Context<Self>) {
        let max_y = f32::from(self.scroll_handle.max_offset().y).max(0.0);
        let cur_y = -f32::from(self.scroll_handle.offset().y);
        let viewport_h = f32::from(self.scroll_handle.bounds().size.height).max(200.0);
        let line_h = f32::from(self.fit_state.borrow().cell.height).max(16.0);
        let target_y = match target {
            ScrollTarget::Top => 0.0,
            ScrollTarget::Bottom => {
                self.stick_to_bottom = true;
                max_y
            }
            ScrollTarget::Lines(n) => {
                let next = (cur_y + (n as f32) * line_h).clamp(0.0, max_y);
                self.stick_to_bottom = (max_y - next) <= 8.0;
                next
            }
            ScrollTarget::Pages(p) => {
                let next = (cur_y + (p as f32) * viewport_h * 0.9).clamp(0.0, max_y);
                self.stick_to_bottom = (max_y - next) <= 8.0;
                next
            }
        };
        self.scroll_handle.set_offset(point(px(0.0), px(-target_y)));
        self.publish_scroll_if_changed(cx);
        cx.notify();
    }

    pub fn scroll_metrics(&self) -> Option<ScrollMetrics> {
        let bounds = self.scroll_handle.bounds();
        let view_h = f32::from(bounds.size.height);
        if view_h <= 0.0 {
            return None;
        }
        let max_y = f32::from(self.scroll_handle.max_offset().y).max(0.0);
        let top_y = (-f32::from(self.scroll_handle.offset().y)).clamp(0.0, max_y);
        let total = (view_h + max_y).round() as u64;
        let len = view_h.round() as u64;
        let offset = top_y.round() as u64;
        let from_bottom = (max_y - top_y).round().max(0.0) as u64;
        Some(ScrollMetrics {
            total,
            offset,
            len,
            from_bottom,
        })
    }

    fn scroll_to_bottom_internal(&self) {
        let max_y = f32::from(self.scroll_handle.max_offset().y).max(0.0);
        self.scroll_handle.set_offset(point(px(0.0), px(-max_y)));
    }

    fn publish_scroll_if_changed(&mut self, cx: &mut Context<Self>) {
        let current = self.scroll_metrics();
        if current != self.last_scroll_metrics {
            self.last_scroll_metrics = current;
            cx.emit(TerminalEvent::ScrollChanged);
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let border = settings::color("border");
        let panel = settings::color("panel");
        let fg = settings::color("fg");
        let muted = settings::color("muted");
        let accent = settings::color("accent");
        let addr = self.address.clone();
        let preset = self.preset.as_deref().unwrap_or("agent").to_owned();
        let is_running = matches!(self.state.as_str(), "running" | "starting" | "thinking" | "busy");
        let state_label = self.ended.clone().unwrap_or_else(|| self.state.clone());

        div()
            .w_full()
            .px_3()
            .py_2()
            .bg(panel)
            .border_b_1()
            .border_color(border)
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .overflow_hidden()
                    .child(
                        div()
                            .px_2()
                            .py(px(2.0))
                            .rounded_sm()
                            .bg(accent.opacity(0.18))
                            .text_color(accent)
                            .text_xs()
                            .child(preset),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(fg)
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("@{}", addr)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .px_2()
                            .py(px(2.0))
                            .rounded_sm()
                            .bg(settings::color("raised"))
                            .text_color(muted)
                            .text_xs()
                            .child(format!("Turn {}", self.turn)),
                    )
                    .when(self.inbox_len > 0, |d| {
                        d.child(
                            div()
                                .px_2()
                                .py(px(2.0))
                                .rounded_sm()
                                .bg(accent.opacity(0.16))
                                .text_color(accent)
                                .text_xs()
                                .child(format!("Queued: {}", self.inbox_len)),
                        )
                    })
                    .child(
                        div()
                            .px_2()
                            .py(px(2.0))
                            .rounded_sm()
                            .bg(settings::color("raised"))
                            .text_color(if is_running { accent } else { muted })
                            .text_xs()
                            .child(state_label),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("hdr-find-{}", addr)))
                            .px_2()
                            .py(px(2.0))
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|d| d.bg(settings::color("hover")))
                            .text_xs()
                            .text_color(muted)
                            .on_click(cx.listener(|this, _, window, cx| this.open_search(window, cx)))
                            .child("Find"),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("hdr-copy-{}", addr)))
                            .px_2()
                            .py(px(2.0))
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|d| d.bg(settings::color("hover")))
                            .text_xs()
                            .text_color(muted)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.select_all(cx);
                                this.copy(cx);
                                this.clear_selection(cx);
                            }))
                            .child("Copy All"),
                    )
                    .when(is_running && self.ended.is_none(), |d| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("hdr-stop-{}", addr)))
                                .px_2()
                                .py(px(2.0))
                                .rounded_sm()
                                .bg(hsla(Rgb(190, 65, 65)).opacity(0.2))
                                .text_color(hsla(Rgb(240, 120, 120)))
                                .cursor_pointer()
                                .hover(|d| d.bg(hsla(Rgb(190, 65, 65)).opacity(0.35)))
                                .text_xs()
                                .on_click(cx.listener(|this, _, _, cx| this.interrupt(cx)))
                                .child("Stop"),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_entry(&self, idx: usize, entry: &TranscriptEntry, cx: &mut Context<Self>) -> AnyElement {
        let fg = settings::color("fg");
        let muted = settings::color("muted");
        let accent = settings::color("accent");
        let panel = settings::color("panel");
        let raised = settings::color("raised");
        let border = settings::color("border");
        let selected = self.all_selected || self.selected_entry == Some(idx);
        let is_active_match = self
            .selected_match
            .and_then(|m| self.match_indices.get(m))
            .is_some_and(|&matched_idx| matched_idx == idx);
        let is_match = !self.search_query.trim().is_empty() && self.match_indices.contains(&idx);
        let row_bg = if selected {
            accent.opacity(0.16)
        } else if is_active_match {
            accent.opacity(0.22)
        } else if is_match {
            accent.opacity(0.09)
        } else {
            gpui::transparent_black()
        };

        let inner: AnyElement = match entry {
            TranscriptEntry::User { from, text, .. } => {
                let (env_from, body) = unwrap_h2_envelope(text);
                let sender = from.clone().or(env_from);
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap_1()
                    .child(
                        div()
                            .max_w_full()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(raised)
                            .border_1()
                            .border_color(border)
                            .flex()
                            .flex_col()
                            .gap_1()
                            .when_some(sender, |d, sender| {
                                let peek_addr = sender.clone();
                                d.child(
                                    div()
                                        .id(SharedString::from(format!("peek-{}-{idx}", self.address)))
                                        .text_xs()
                                        .text_color(accent)
                                        .cursor_pointer()
                                        .hover(|d| d.underline())
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.emit(TerminalEvent::Peek(peek_addr.clone()));
                                        }))
                                        .child(format!("from {sender}")),
                                )
                            })
                            .child(div().text_sm().text_color(fg).child(body)),
                    )
                    .into_any_element()
            }
            TranscriptEntry::Thought { text, .. } => div()
                .w_full()
                .px_3()
                .py_1()
                .rounded_sm()
                .border_l_2()
                .border_color(muted.opacity(0.5))
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().text_color(muted).child("Thinking"))
                .child(div().text_xs().text_color(muted).child(text.clone()))
                .into_any_element(),
            TranscriptEntry::Assistant { text, .. } => div()
                .w_full()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(panel.opacity(0.6))
                .border_l_2()
                .border_color(accent)
                .text_sm()
                .text_color(fg)
                .child(text.clone())
                .into_any_element(),
            TranscriptEntry::ToolCall { title, tool_kind, status, .. } => {
                let status_color = match status.as_str() {
                    "completed" | "done" => hsla(Rgb(110, 200, 130)),
                    "failed" | "error" => hsla(Rgb(235, 110, 110)),
                    _ => accent,
                };
                div()
                    .w_full()
                    .px_3()
                    .py_1()
                    .rounded_sm()
                    .bg(panel)
                    .border_1()
                    .border_color(border)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .px_1()
                            .rounded_xs()
                            .bg(status_color.opacity(0.18))
                            .text_color(status_color)
                            .text_xs()
                            .child(status.clone()),
                    )
                    .when_some(tool_kind.clone(), |d, kind| {
                        d.child(div().text_xs().text_color(muted).child(kind))
                    })
                    .child(div().text_xs().text_color(fg).child(title.clone()))
                    .into_any_element()
            }
            TranscriptEntry::Plan { items, .. } => {
                let mut col = div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(panel)
                    .border_1()
                    .border_color(border)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().text_color(accent).child("Plan"));
                for (i, item) in items.iter().enumerate() {
                    col = col.child(div().text_xs().text_color(fg).child(format!("{}. {item}", i + 1)));
                }
                col.into_any_element()
            }
            TranscriptEntry::Permission { title, outcome, .. } => div()
                .w_full()
                .px_3()
                .py_1()
                .rounded_sm()
                .bg(panel)
                .border_1()
                .border_color(border)
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(div().text_xs().text_color(fg).child(format!("Permission: {title}")))
                .child(div().text_xs().text_color(accent).child(outcome.clone()))
                .into_any_element(),
            TranscriptEntry::Error { message, .. } => div()
                .w_full()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(hsla(Rgb(180, 50, 50)).opacity(0.18))
                .border_1()
                .border_color(hsla(Rgb(220, 80, 80)).opacity(0.5))
                .text_xs()
                .text_color(hsla(Rgb(245, 160, 160)))
                .child(format!("Error: {message}"))
                .into_any_element(),
            TranscriptEntry::Stderr { text, .. } => div()
                .w_full()
                .px_3()
                .py_1()
                .rounded_sm()
                .bg(panel)
                .border_l_2()
                .border_color(hsla(Rgb(210, 140, 80)))
                .font_family(settings::terminal_font())
                .text_xs()
                .text_color(muted)
                .child(text.clone())
                .into_any_element(),
        };

        div()
            .id(SharedString::from(format!("entry-{}-{idx}", self.address)))
            .w_full()
            .rounded_md()
            .bg(row_bg)
            .px_1()
            .py(px(2.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.select_entry(idx, cx)),
            )
            .child(inner)
            .into_any_element()
    }

    fn render_permission_bar(&self, perm: &PendingPermission, cx: &mut Context<Self>) -> AnyElement {
        let accent = settings::color("accent");
        let border = settings::color("border");
        let fg = settings::color("fg");
        let muted = settings::color("muted");
        let mut buttons = div().flex().items_center().flex_wrap().gap_2();
        for (i, opt) in perm.options.iter().enumerate() {
            let opt_id = opt.option_id.clone();
            let label = opt.name.clone();
            buttons = buttons.child(
                div()
                    .id(SharedString::from(format!("perm-opt-{}-{i}", self.address)))
                    .px_3()
                    .py_1()
                    .rounded_sm()
                    .bg(accent.opacity(0.2))
                    .border_1()
                    .border_color(accent.opacity(0.5))
                    .cursor_pointer()
                    .hover(|d| d.bg(accent.opacity(0.32)))
                    .text_xs()
                    .text_color(fg)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.resolve_permission(Some(opt_id.clone()), cx);
                    }))
                    .child(label),
            );
        }
        buttons = buttons.child(
            div()
                .id(SharedString::from(format!("perm-deny-{}", self.address)))
                .px_3()
                .py_1()
                .rounded_sm()
                .bg(hsla(Rgb(190, 65, 65)).opacity(0.2))
                .border_1()
                .border_color(hsla(Rgb(220, 90, 90)).opacity(0.45))
                .cursor_pointer()
                .hover(|d| d.bg(hsla(Rgb(190, 65, 65)).opacity(0.32)))
                .text_xs()
                .text_color(hsla(Rgb(245, 165, 165)))
                .on_click(cx.listener(|this, _, _, cx| this.resolve_permission(None, cx)))
                .child("Deny"),
        );

        div()
            .w_full()
            .px_3()
            .py_2()
            .bg(settings::color("panel"))
            .border_t_1()
            .border_color(border)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .px_2()
                            .py(px(1.0))
                            .rounded_xs()
                            .bg(accent.opacity(0.18))
                            .text_color(accent)
                            .text_xs()
                            .child("Approval Needed"),
                    )
                    .when_some(perm.kind.clone(), |d, kind| {
                        d.child(div().text_xs().text_color(muted).child(kind))
                    })
                    .child(div().text_xs().text_color(fg).child(perm.title.clone())),
            )
            .child(buttons)
            .into_any_element()
    }

    fn ensure_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Entity<InputState> {
        if let Some(existing) = &self.composer {
            return existing.clone();
        }
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .placeholder("Message agent… (Enter to send, Cmd/Ctrl+Enter to steer)");
            state.set_editor_style(InputEditorStyle::default());
            state
        });
        let sub = cx.subscribe(&input, |this, input, event: &InputEvent, cx| match event {
            InputEvent::PressEnter { secondary, shift } if !shift => {
                let text = input.read(cx).value().to_string();
                if !text.trim().is_empty() {
                    this.submit_prompt(text, *secondary, cx);
                    this.clear_composer = true;
                    cx.notify();
                }
            }
            InputEvent::Focus => {
                this.composer_focused = true;
            }
            InputEvent::Blur => {
                this.composer_focused = false;
            }
            InputEvent::PasteWithoutText => {
                this.paste_image(cx);
            }
            _ => {}
        });
        self.composer_sub = Some(sub);
        self.composer = Some(input.clone());
        input
    }
}

impl gpui::EventEmitter<TerminalEvent> for TerminalView {}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.mark_shown();
        self.publish_scroll_if_changed(cx);
        crate::bench::count(&crate::bench::TERMINAL);

        let composer = self.ensure_composer(window, cx);
        if std::mem::take(&mut self.clear_composer) {
            composer.update(cx, |state, cx| state.set_value("", window, cx));
        }
        if let Some(insert) = self.pending_insert_text.take() {
            composer.update(cx, |state, cx| state.insert(insert, window, cx));
        }

        let cell = cell_size(window);
        let fit_state = self.fit_state.clone();
        let address = self.address.clone();
        let search_input = self.search_open.then(|| self.search_input.clone()).flatten();
        let search_label = if self.search_query.trim().is_empty() {
            String::new()
        } else if self.match_indices.is_empty() {
            "No matches".to_owned()
        } else if let Some(sel) = self.selected_match {
            format!("{} / {}", sel + 1, self.match_indices.len())
        } else {
            format!("{} matches", self.match_indices.len())
        };
        let ended = self
            .ended
            .clone()
            .or_else(|| self.lost.then(|| "reconnecting to core…".to_owned()));
        let permission = self.pending_permission.clone();

        let (pad_x, pad_y) = pad();
        let mut feed = div()
            .id(SharedString::from(format!("transcript-scroll-{}", address)))
            .track_scroll(&self.scroll_handle)
            .overflow_y_scroll()
            .flex_1()
            .w_full()
            .px(px(pad_x.max(10.0)))
            .py(px(pad_y.max(10.0)))
            .flex()
            .flex_col()
            .gap_2();

        if self.entries.is_empty() {
            feed = feed.child(
                div()
                    .w_full()
                    .py_6()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .text_color(settings::color("muted"))
                    .child(if ended.is_some() {
                        "Session ended"
                    } else {
                        "Ready — send a prompt below to start this ACP session."
                    }),
            );
        } else {
            for (idx, entry) in self.entries.iter().enumerate() {
                feed = feed.child(self.render_entry(idx, entry, cx));
            }
        }

        let header = self.render_header(cx);
        let bg = hsla(vt::theme().bg).opacity(settings::get().terminal_opacity);
        let border = settings::color("border");
        let panel = settings::color("panel");
        let accent = settings::color("accent");
        let fg = settings::color("fg");
        let composer_for_send = composer.clone();
        let prompt_error = self.pending_prompt_error.clone();

        div()
            .id(SharedString::from(format!("term-{}", address)))
            .track_focus(&self.focus)
            .key_context("Terminal")
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(bg)
            .on_action(cx.listener(|this, a: &gpui_shell::action::ShellAction, window, cx| {
                if this.find_field_focused(window, cx) || this.composer_field_focused(window, cx) {
                    let action: Option<Box<dyn gpui::Action>> = match a.id() {
                        "terminal.copy" => Some(Box::new(InputCopy)),
                        "terminal.paste" => Some(Box::new(InputPaste)),
                        _ => None,
                    };
                    if let Some(action) = action {
                        window.dispatch_action(action, cx);
                        cx.stop_propagation();
                        return;
                    }
                }
                cx.propagate();
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if this.search_open && ev.keystroke.key == "escape" {
                    this.close_search(window, cx);
                    cx.stop_propagation();
                }
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.dropped(paths.paths().to_vec(), cx);
            }))
            .on_drop(cx.listener(Self::dropped_from_mod))
            .on_hover(cx.listener(|this, hovered: &bool, _, _| {
                this.hovered_now = *hovered;
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.focus.focus(window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|_, ev: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    cx.emit(TerminalEvent::ContextMenu(ev.position));
                }),
            )
            // Zero-paint full-pane canvas records rendered pane geometry for pane_size checks.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .child(
                        canvas(
                            move |bounds, window, _| {
                                let (cols, rows) = grid_size(bounds.size, cell);
                                fit_state
                                    .borrow_mut()
                                    .fit(cols, rows, cell, window.scale_factor());
                            },
                            |_, _, _, _| {},
                        )
                        .size_full(),
                    ),
            )
            .child(header)
            .child(feed)
            .when_some(prompt_error, |d, err| {
                d.child(
                    div()
                        .w_full()
                        .px_3()
                        .py_1()
                        .bg(hsla(Rgb(180, 50, 50)).opacity(0.2))
                        .text_xs()
                        .text_color(hsla(Rgb(245, 160, 160)))
                        .child(err),
                )
            })
            .when_some(permission, |d, perm| d.child(self.render_permission_bar(&perm, cx)))
            .child(
                div()
                    .w_full()
                    .px_3()
                    .py_2()
                    .bg(panel)
                    .border_t_1()
                    .border_color(border)
                    .flex()
                    .items_center()
                    .gap_2()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(80.0))
                            .min_h(px(34.0))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(border)
                            .bg(bg)
                            .child(Input::new(&composer)),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("send-btn-{}", address)))
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .bg(accent.opacity(0.22))
                            .border_1()
                            .border_color(accent.opacity(0.45))
                            .cursor_pointer()
                            .hover(|d| d.bg(accent.opacity(0.35)))
                            .text_xs()
                            .text_color(fg)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let text = composer_for_send.read(cx).value().to_string();
                                if !text.trim().is_empty() {
                                    this.submit_prompt(text, false, cx);
                                    composer_for_send.update(cx, |state, cx| state.set_value("", window, cx));
                                }
                            }))
                            .child("Send"),
                    ),
            )
            .when_some(search_input, |d, input| {
                d.child(
                    div()
                        .flex()
                        .id(SharedString::from(format!("find-bar-{address}")))
                        .debug_selector(|| "find-bar".to_owned())
                        .absolute()
                        .top(px(44.0))
                        .right_2()
                        .w_full()
                        .max_w(px(460.0))
                        .h(px(38.0))
                        .gap_1()
                        .items_center()
                        .px_1()
                        .rounded_md()
                        .border_1()
                        .border_color(border)
                        .bg(settings::color("popover"))
                        .shadow_md()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(div().flex_1().min_w(px(48.0)).h_full().child(Input::new(&input)))
                        .child(div().w(px(58.0)).text_xs().text_color(settings::color("muted")).child(search_label))
                        .child(
                            div()
                                .id(SharedString::from(format!("find-prev-{address}")))
                                .w(px(26.0))
                                .h(px(26.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_sm()
                                .hover(|d| d.bg(settings::color("hover")))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| this.step_search(true, cx)))
                                .child("↑"),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("find-next-{address}")))
                                .w(px(26.0))
                                .h(px(26.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_sm()
                                .hover(|d| d.bg(settings::color("hover")))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, _, cx| this.step_search(false, cx)))
                                .child("↓"),
                        )
                        .child(
                            div()
                                .id(SharedString::from(format!("find-close-{address}")))
                                .w(px(26.0))
                                .h(px(26.0))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_sm()
                                .hover(|d| d.bg(settings::color("hover")))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx)))
                                .child("×"),
                        ),
                )
            })
            .when_some(ended, |d, why| {
                d.child(
                    div()
                        .absolute()
                        .bottom(px(54.0))
                        .right_3()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(settings::color("raised"))
                        .text_color(fg)
                        .text_xs()
                        .child(format!("[{why}]")),
                )
            })
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn copied_files(item: &ClipboardItem) -> Option<Vec<PathBuf>> {
    item.entries().iter().find_map(|e| match e {
        ClipboardEntry::ExternalPaths(p) if !p.paths().is_empty() => Some(p.paths().to_vec()),
        _ => None,
    })
}

fn refusal_notice(refused: &[(String, String)]) -> Option<(String, String)> {
    match refused {
        [] => None,
        [(name, why)] => Some((format!("Couldn't send {name}"), why.clone())),
        many => {
            let mut why: Vec<&str> = many.iter().map(|(_, w)| w.as_str()).collect();
            why.dedup();
            Some((format!("Couldn't send {} files", many.len()), why.join("; ")))
        }
    }
}

fn image_file(item: &ClipboardItem) -> Option<PathBuf> {
    let image = item.entries().iter().find_map(|e| match e {
        ClipboardEntry::Image(i) => Some(i),
        _ => None,
    })?;
    let (format, bytes) = paste_image_bytes(image.format, &image.bytes)?;
    let ext = match format {
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Gif => "gif",
        ImageFormat::Webp => "webp",
        ImageFormat::Svg => "svg",
        _ => "png",
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let path = std::env::temp_dir().join(format!("herdr2-paste-{stamp}.{ext}"));
    std::fs::write(&path, bytes).ok()?;
    Some(path)
}

fn paste_image_bytes(
    format: ImageFormat,
    bytes: &[u8],
) -> Option<(ImageFormat, std::borrow::Cow<'_, [u8]>)> {
    let source = match format {
        ImageFormat::Tiff => image::ImageFormat::Tiff,
        ImageFormat::Bmp => image::ImageFormat::Bmp,
        _ => return Some((format, std::borrow::Cow::Borrowed(bytes))),
    };
    let image = image::load_from_memory_with_format(bytes, source).ok()?;
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some((ImageFormat::Png, std::borrow::Cow::Owned(png.into_inner())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn unwrap_h2_envelope_extracts_sender_and_body() {
        let raw = r#"<h2 from="1" to="2">  please inspect src/main.rs  </h2>"#;
        let (from, body) = unwrap_h2_envelope(raw);
        assert_eq!(from.as_deref(), Some("1"));
        assert_eq!(body, "please inspect src/main.rs");

        let plain = "regular user prompt";
        assert_eq!(unwrap_h2_envelope(plain), (None, "regular user prompt".to_owned()));
    }

    #[test]
    fn coalesce_entries_merges_streams_and_updates_tool_calls() {
        let mut entries = Vec::new();
        coalesce_entries(
            &mut entries,
            TranscriptEntry::Assistant { turn: 1, text: "Hello ".into(), ts_ms: 1 },
        );
        coalesce_entries(
            &mut entries,
            TranscriptEntry::Assistant { turn: 1, text: "world!".into(), ts_ms: 2 },
        );
        coalesce_entries(
            &mut entries,
            TranscriptEntry::ToolCall {
                turn: 1,
                id: "t1".into(),
                title: "read_file".into(),
                tool_kind: Some("fs".into()),
                status: "in_progress".into(),
                ts_ms: 3,
            },
        );
        coalesce_entries(
            &mut entries,
            TranscriptEntry::ToolCall {
                turn: 1,
                id: "t1".into(),
                title: "read_file".into(),
                tool_kind: Some("fs".into()),
                status: "completed".into(),
                ts_ms: 4,
            },
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0],
            TranscriptEntry::Assistant { turn: 1, text: "Hello world!".into(), ts_ms: 2 }
        );
        assert_eq!(
            entries[1],
            TranscriptEntry::ToolCall {
                turn: 1,
                id: "t1".into(),
                title: "read_file".into(),
                tool_kind: Some("fs".into()),
                status: "completed".into(),
                ts_ms: 4,
            }
        );
    }

    #[test]
    fn shown_title_strips_spinner_prefixes() {
        assert_eq!(shown_title("✳ Claude Code"), "Claude Code");
        assert_eq!(shown_title("⠋ Thinking…"), "Thinking…");
        assert_eq!(shown_title("Plain title"), "Plain title");
    }

    #[test]
    fn scroll_target_parses_all_forms() {
        assert_eq!(scroll_target(&json!({"to": "top"})).unwrap(), ScrollTarget::Top);
        assert_eq!(scroll_target(&json!({"to": "bottom"})).unwrap(), ScrollTarget::Bottom);
        assert_eq!(scroll_target(&json!({"lines": -5})).unwrap(), ScrollTarget::Lines(-5));
        assert_eq!(scroll_target(&json!({"pages": 1.5})).unwrap(), ScrollTarget::Pages(1.5));
        assert!(scroll_target(&json!({"to": "middle"})).is_err());
        assert!(scroll_target(&json!({})).is_err());
    }

    #[gpui::test]
    fn offline_terminal_view_marks_lost_after_deadline(cx: &mut gpui::TestAppContext) {
        let view = cx.new(|cx| TerminalView::attach(hc::offline(), "agent-1".into(), (80, 24), cx));
        assert!(!view.read_with(cx, |v, _| v.lost));
        cx.executor().advance_clock(Duration::from_millis(120));
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.lost));
    }
}
