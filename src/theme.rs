//! Omarchy theme integration.
//!
//! Omarchy keeps the active theme behind a symlink at
//! `~/.local/state/omarchy/current/theme`, with the palette in `colors.toml`
//! and the theme's name in a sibling `theme.name`. We read that palette and
//! compile it into the GTK stylesheet, so omarepos repaints itself whenever the
//! user switches themes.

use std::path::PathBuf;

use serde::Deserialize;

use crate::config::home;

pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("omarchy/current")
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RawPalette {
    mode: Option<String>,
    accent: Option<String>,
    selection: Option<String>,
    muted: Option<String>,
    background: Option<String>,
    dark_background: Option<String>,
    darker_background: Option<String>,
    lighter_background: Option<String>,
    foreground: Option<String>,
    dark_foreground: Option<String>,
    light_foreground: Option<String>,
    bright_foreground: Option<String>,
    red: Option<String>,
    yellow: Option<String>,
    orange: Option<String>,
    green: Option<String>,
    cyan: Option<String>,
    blue: Option<String>,
    magenta: Option<String>,
}

/// A fully resolved palette: no optionals left for the stylesheet to worry about.
#[derive(Debug, Clone)]
pub struct Palette {
    pub name: String,
    pub dark: bool,
    pub bg: String,
    pub bg_raised: String,
    pub bg_sunken: String,
    pub fg: String,
    pub fg_dim: String,
    pub border: String,
    pub accent: String,
    pub red: String,
    pub yellow: String,
    pub green: String,
    pub blue: String,
    pub cyan: String,
    pub magenta: String,
}

impl Palette {
    /// The built-in palette, used when Omarchy's state directory isn't there.
    pub fn fallback() -> Self {
        Self {
            name: "builtin".into(),
            dark: true,
            bg: "#14161b".into(),
            bg_raised: "#1c1f26".into(),
            bg_sunken: "#0f1115".into(),
            fg: "#dcd7ba".into(),
            fg_dim: "#8f939c".into(),
            border: "#2a2e37".into(),
            accent: "#7e9cd8".into(),
            red: "#e46876".into(),
            yellow: "#dca561".into(),
            green: "#98bb6c".into(),
            blue: "#7e9cd8".into(),
            cyan: "#7aa89f".into(),
            magenta: "#957fb8".into(),
        }
    }

    /// Read the active Omarchy palette, or fall back to the built-in one.
    pub fn load() -> Self {
        let dir = state_dir();
        let name = std::fs::read_to_string(dir.join("theme.name"))
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|s| !s.is_empty());
        let Ok(text) = std::fs::read_to_string(dir.join("theme/colors.toml")) else {
            let mut fb = Self::fallback();
            if let Some(name) = name {
                fb.name = name;
            }
            return fb;
        };
        let raw: RawPalette = toml::from_str(&text).unwrap_or_default();
        let fb = Self::fallback();
        let dark = raw.mode.as_deref().map(|m| m != "light").unwrap_or(true);

        let pick = |value: &Option<String>, default: &str| -> String {
            value
                .as_deref()
                .map(str::trim)
                .filter(|v| v.starts_with('#') && (v.len() == 7 || v.len() == 4))
                .unwrap_or(default)
                .to_string()
        };

        let bg = pick(&raw.background, &fb.bg);
        let fg = pick(&raw.foreground, &fb.fg);
        Self {
            name: name.unwrap_or_else(|| "omarchy".into()),
            dark,
            bg_raised: pick(&raw.lighter_background, &bg),
            bg_sunken: pick(&raw.darker_background, &pick(&raw.dark_background, &bg)),
            bg,
            fg_dim: pick(&raw.light_foreground, &pick(&raw.dark_foreground, &fg)),
            fg,
            border: pick(&raw.muted, &pick(&raw.selection, &fb.border)),
            accent: pick(&raw.accent, &fb.accent),
            red: pick(&raw.red, &fb.red),
            yellow: pick(&raw.yellow, &pick(&raw.orange, &fb.yellow)),
            green: pick(&raw.green, &fb.green),
            blue: pick(&raw.blue, &fb.blue),
            cyan: pick(&raw.cyan, &fb.cyan),
            magenta: pick(&raw.magenta, &fb.magenta),
        }
    }

    /// Parsed `(r, g, b)` in 0.0..=1.0, for cairo drawing.
    pub fn rgb(hex: &str) -> (f64, f64, f64) {
        let h = hex.trim_start_matches('#');
        let expand = |c: u8| -> u8 { (c << 4) | c };
        let bytes: Vec<u8> = match h.len() {
            3 => h
                .chars()
                .map(|c| expand(c.to_digit(16).unwrap_or(0) as u8))
                .collect(),
            6 => (0..3)
                .map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap_or(0))
                .collect(),
            _ => vec![0, 0, 0],
        };
        (
            bytes[0] as f64 / 255.0,
            bytes[1] as f64 / 255.0,
            bytes[2] as f64 / 255.0,
        )
    }

    /// The GTK stylesheet for this palette.
    pub fn stylesheet(&self) -> String {
        let p = self;
        format!(
            r#"
@define-color om_bg {bg};
@define-color om_raised {raised};
@define-color om_sunken {sunken};
@define-color om_fg {fg};
@define-color om_dim {dim};
@define-color om_border {border};
@define-color om_accent {accent};

window.omarepos,
window.omarepos > contents {{
  background-color: @om_bg;
  color: @om_fg;
}}

window.omarepos headerbar {{
  background-color: @om_sunken;
  color: @om_fg;
  box-shadow: none;
  border-bottom: 1px solid @om_border;
  min-height: 42px;
}}

window.omarepos headerbar button {{
  color: @om_fg;
  background: transparent;
  border: none;
  box-shadow: none;
}}

window.omarepos headerbar button:hover {{
  background-color: alpha(@om_accent, 0.18);
}}

.om-title {{
  font-weight: 700;
  letter-spacing: 1px;
}}

.om-subtitle {{
  font-size: 11px;
  color: @om_dim;
}}

entry.om-search {{
  background-color: @om_bg;
  color: @om_fg;
  border: 1px solid @om_border;
  border-radius: 8px;
  min-height: 28px;
  caret-color: @om_accent;
}}

entry.om-search:focus-within {{
  border-color: @om_accent;
}}

dropdown.om-sort > button {{
  background-color: @om_bg;
  color: @om_fg;
  border: 1px solid @om_border;
  border-radius: 8px;
  min-height: 28px;
}}

.om-card {{
  background-color: @om_raised;
  border: 1px solid @om_border;
  border-radius: 10px;
  padding: 12px 14px;
  transition: border-color 120ms ease, background-color 120ms ease;
}}

.om-card:hover {{
  border-color: @om_accent;
  background-color: shade(@om_raised, 1.08);
}}

.om-card.om-selected {{
  border-color: @om_accent;
  background-color: mix(@om_raised, @om_accent, 0.22);
  box-shadow: inset 0 0 0 2px @om_accent;
}}

.om-card.om-selected:hover {{
  background-color: mix(@om_raised, @om_accent, 0.30);
}}

/* A muted theme accent cannot carry the highlight on its own, so once
   anything is selected the rest of the grid steps back. */
flowbox.om-flow.om-has-selection .om-card:not(.om-selected) {{
  opacity: 0.42;
}}

flowbox.om-flow.om-has-selection .om-card:not(.om-selected):hover {{
  opacity: 0.85;
}}

.om-card.om-selected checkbutton check {{
  background-color: @om_accent;
  border-color: @om_accent;
}}

.om-tag {{
  font-size: 11px;
  font-weight: 600;
  padding: 1px 8px 1px 6px;
  border-radius: 999px;
  background-color: alpha(@om_fg, 0.10);
  color: @om_fg;
}}

.om-quick-tag {{
  border-radius: 999px;
  padding: 2px 12px;
  font-size: 12px;
  font-weight: 600;
  background-color: alpha(@om_fg, 0.08);
  color: @om_fg;
  border: 1px solid transparent;
  box-shadow: none;
}}

.om-quick-tag:hover {{
  background-color: alpha(@om_fg, 0.16);
}}

.om-quick-tag:checked {{
  background-color: mix(@om_raised, @om_accent, 0.40);
  border-color: @om_accent;
  color: @om_fg;
}}

.om-tag-chip {{
  font-size: 11px;
  border-radius: 999px;
  min-height: 24px;
  padding: 0 10px;
}}

.om-card.om-dirty {{
  border-left: 3px solid {yellow};
  padding-left: 12px;
}}

.om-card.om-error {{
  border-left: 3px solid {red};
  padding-left: 12px;
}}

.om-name {{
  font-weight: 700;
  font-size: 15px;
  color: @om_fg;
}}

.om-path {{
  font-size: 11px;
  color: @om_dim;
}}

.om-meta {{
  font-size: 11px;
  color: @om_dim;
}}

.om-summary {{
  font-size: 12px;
  color: @om_fg;
}}

.om-pill {{
  font-size: 11px;
  font-weight: 700;
  padding: 1px 8px;
  border-radius: 999px;
  background-color: alpha(@om_fg, 0.10);
  color: @om_fg;
}}

.om-pill.branch {{
  background-color: alpha(@om_accent, 0.20);
  color: {accent};
}}

.om-pill.clean {{
  background-color: alpha({green}, 0.18);
  color: {green};
}}

.om-pill.dirty {{
  background-color: alpha({yellow}, 0.18);
  color: {yellow};
}}

.om-pill.ahead {{
  background-color: alpha({cyan}, 0.18);
  color: {cyan};
}}

.om-pill.behind {{
  background-color: alpha({magenta}, 0.18);
  color: {magenta};
}}

.om-pill.danger {{
  background-color: alpha({red}, 0.20);
  color: {red};
}}

.om-dot {{
  border-radius: 999px;
  min-width: 8px;
  min-height: 8px;
}}

scrolledwindow.om-grid {{
  background-color: @om_bg;
}}

flowbox.om-flow {{
  background-color: @om_bg;
}}

flowbox.om-flow > flowboxchild {{
  padding: 0;
  background: transparent;
}}

flowbox.om-flow > flowboxchild:selected {{
  background: transparent;
  outline: 0;
}}

flowbox.om-flow > flowboxchild:focus-visible {{
  outline: 0;
}}

/* Keyboard focus is drawn in the foreground colour rather than the accent.
   The accent is whatever the theme says it is — in some themes it sits a
   shade away from the card background and the ring all but disappears — while
   the foreground contrasts with the background by definition. */
flowbox.om-flow > flowboxchild:focus-visible .om-card {{
  outline: 2px solid @om_fg;
  outline-offset: 2px;
  border-color: @om_fg;
  background-color: shade(@om_raised, 1.30);
}}

/* While the keyboard is driving, hover says nothing useful — the pointer is
   wherever it was left — and a lit card beside the focused one reads as a
   second, weaker selection. */
flowbox.om-flow.om-keyboard .om-card:hover {{
  background-color: @om_raised;
  border-color: @om_border;
}}

flowbox.om-flow.om-keyboard .om-card.om-selected:hover {{
  background-color: mix(@om_raised, @om_accent, 0.22);
  border-color: @om_accent;
}}

flowbox.om-flow.om-keyboard.om-has-selection .om-card:not(.om-selected):hover {{
  opacity: 0.42;
}}

/* A focused card is the one being looked at, so it is never the dimmed one. */
flowbox.om-flow.om-has-selection > flowboxchild:focus-visible .om-card:not(.om-selected) {{
  opacity: 1;
}}

flowbox.om-flow > flowboxchild:focus-visible .om-card.om-selected {{
  outline-color: @om_accent;
  background-color: mix(@om_raised, @om_accent, 0.38);
}}

.om-statusbar {{
  font-size: 11px;
  color: @om_dim;
  border-top: 1px solid @om_border;
  background-color: @om_sunken;
  padding: 4px 12px;
}}

listbox.om-results,
listbox.om-results > row {{
  background-color: transparent;
}}

listbox.om-results > row {{
  padding: 0;
  min-height: 0;
  border-radius: 6px;
}}

listbox.om-results > row:focus-visible {{
  outline: 2px solid @om_fg;
  outline-offset: -2px;
}}

.om-row {{
  border-radius: 6px;
  padding: 0 6px;
}}

.om-row:hover {{
  background-color: alpha(@om_fg, 0.07);
}}

.om-sha {{
  font-family: monospace;
  font-size: 11px;
  color: @om_dim;
}}

.om-ref {{
  font-size: 10px;
  font-weight: 700;
  padding: 0 6px;
  border-radius: 4px;
  background-color: alpha(@om_fg, 0.12);
  color: @om_fg;
}}

.om-ref.local {{
  background-color: alpha({green}, 0.20);
  color: {green};
}}

.om-ref.current {{
  background-color: alpha(@om_accent, 0.30);
  color: {accent};
}}

.om-ref.remote {{
  background-color: alpha({cyan}, 0.18);
  color: {cyan};
}}

.om-ref.tag {{
  background-color: alpha({yellow}, 0.20);
  color: {yellow};
}}

.om-ref.head {{
  background-color: alpha({red}, 0.24);
  color: {red};
}}

.om-big {{
  font-size: 22px;
  font-weight: 700;
  color: @om_fg;
}}

/* The author row's header is a button only so it can be opened from the
   keyboard; it should not look like one. */
button.om-stat-head {{
  background: none;
  background-color: transparent;
  border: none;
  box-shadow: none;
  padding: 0;
  margin: 0;
  min-height: 0;
  outline: 0;
}}

button.om-stat-head:hover {{
  background-color: transparent;
}}

button.om-stat-head:focus-visible {{
  outline: 2px solid @om_fg;
  outline-offset: 3px;
}}

.om-stat-row {{
  padding: 10px 14px;
}}

.om-stat-row:hover {{
  border-color: @om_border;
  background-color: @om_raised;
}}

popover.menu contents, popover contents {{
  background-color: @om_raised;
  color: @om_fg;
  border: 1px solid @om_border;
  border-radius: 10px;
}}
"#,
            bg = p.bg,
            raised = p.bg_raised,
            sunken = p.bg_sunken,
            fg = p.fg,
            dim = p.fg_dim,
            border = p.border,
            accent = p.accent,
            green = p.green,
            yellow = p.yellow,
            red = p.red,
            cyan = p.cyan,
            magenta = p.magenta,
        )
    }
}
