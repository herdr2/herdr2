//! Pure-Rust theme colors and position metadata for ACP session panes.

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CellPosition {
    pub row: u32,
    pub col: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SelectionPositions {
    pub start: CellPosition,
    pub end: CellPosition,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// WCAG relative luminance.
    pub fn luminance(self) -> f32 {
        let ch = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(self.0) + 0.7152 * ch(self.1) + 0.0722 * ch(self.2)
    }
}

pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (x, y) = (a.luminance(), b.luminance());
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[derive(Clone, PartialEq)]
pub struct Theme {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub selection_bg: Rgb,
    pub palette16: [Rgb; 16],
    pub minimum_contrast: f32,
}

/// The pane colors from settings.json's theme.
pub fn theme() -> Theme {
    use crate::settings::rgb;
    let t = crate::settings::theme();
    let get = |k: &str| rgb(t.get(k).map(String::as_str).unwrap_or("#ff00ff"));
    let mut palette16 = [Rgb::default(); 16];
    for (i, c) in palette16.iter_mut().enumerate() {
        *c = get(&format!("ansi{i}"));
    }
    Theme {
        fg: get("terminal_foreground"),
        bg: get("background"),
        cursor: get("cursor"),
        selection_bg: get("selection"),
        palette16,
        minimum_contrast: crate::settings::get().minimum_contrast,
    }
}
