//! Omarchy theme integration.
//!
//! Omarchy keeps the active theme behind a symlink at
//! `~/.local/state/omarchy/current/theme`, with the palette in `colors.toml`
//! and the theme's name in a sibling `theme.name`. We read that palette and
//! compile it into the GTK stylesheet, so omarepos repaints itself whenever the
//! user switches themes.

use std::path::PathBuf;

use crate::config::home;

pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("omarchy/current")
}

/// Whether `value` is a colour this module can use: `#` and three or six hex
/// digits. The test used to be the length alone, and a length counts bytes:
/// `#fff` and a zero-width space pasted in from a web page is seven of them,
/// and so are `#zzzzzz` and `#;}*{x;`. Each went into the stylesheet as
/// written, and the first went on to `Palette::rgb`.
fn is_hex(value: &str) -> bool {
    matches!(value.len(), 4 | 7)
        && value.starts_with('#')
        && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Omarchy's test for a theme that does not say whether it is light: it is
/// when the background's red, green and blue add up to more than 382 of a
/// possible 765.
fn is_light(background: &str) -> bool {
    let (r, g, b) = Palette::rgb(background);
    (r + g + b) * 255.0 > 382.5
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
        let mut palette = Self::resolve(&text, dir.join("theme/light.mode").is_file());
        if let Some(name) = name {
            palette.name = name;
        }
        palette
    }

    /// The palette a `colors.toml` describes, read by the rules of Omarchy's
    /// own resolver, `omarchy-theme-color`. `marked_light` is whether a
    /// `light.mode` file lies beside it.
    ///
    /// The stock themes spell everything out, and this used to read only
    /// what they spell: the long names, and `mode`. Omarchy asks less of a
    /// theme. One that predates colors.toml has the file generated from its
    /// terminal colours, which gives `background`, `foreground`, `accent`,
    /// `selection` and `color0` to `color15` and no `mode`; older ones use
    /// the short `bg` and `fg`; and a theme may say it is light with
    /// `theme_type`, with a `light.mode` file, or by having a light
    /// background and saying nothing. Such a theme got its own background and
    /// foreground, the built-in dark palette's status colours, and, if it was
    /// light, Adwaita's dark dialogs and the dark syntax colours on top: diff
    /// text at 1.53:1 on a generated Catppuccin Latte.
    ///
    /// Each name is tried in the order the resolver tries it, the canonical
    /// one first. A file may give both spellings, which is why these are
    /// lookups and not serde aliases: an alias turns the second spelling into
    /// a duplicate-field error, and the error into no palette at all.
    fn resolve(text: &str, marked_light: bool) -> Self {
        let table: toml::Table = text.parse().unwrap_or_default();
        let word = |name: &str| {
            table.get(name).and_then(|value| value.as_str()).map(str::trim)
        };
        let colour = |names: &[&str]| -> Option<String> {
            names
                .iter()
                .filter_map(|name| word(name))
                .find(|value| is_hex(value))
                .map(str::to_string)
        };
        let fb = Self::fallback();

        let background = colour(&["background", "bg", "color0"]);
        let dark = match ["mode", "theme_type"]
            .into_iter()
            .filter_map(word)
            .find(|mode| !mode.is_empty())
        {
            Some(mode) => mode != "light",
            None if marked_light => false,
            None => !background.as_deref().is_some_and(is_light),
        };

        let bg = background.unwrap_or_else(|| fb.bg.clone());
        let fg = colour(&["foreground", "fg", "color7"]).unwrap_or_else(|| fb.fg.clone());
        Self {
            name: "omarchy".into(),
            dark,
            // Where a theme names no shade of its own, the resolver makes one
            // up: the background mixed half-way to black, `color8` for the
            // quieter foreground. Those serve a terminal. Behind this window's
            // header, or as its small print, either puts a generated light
            // theme's text under 2:1, so here a shade the theme does not name
            // is the colour it is a shade of.
            bg_raised: colour(&["lighter_background", "lighter_bg"]).unwrap_or_else(|| bg.clone()),
            bg_sunken: colour(&["darker_background", "darker_bg", "dark_background", "dark_bg"])
                .unwrap_or_else(|| bg.clone()),
            fg_dim: colour(&["light_foreground", "light_fg", "dark_foreground", "dark_fg"])
                .unwrap_or_else(|| fg.clone()),
            bg,
            fg,
            border: colour(&["muted", "color8", "selection", "selection_background"])
                .unwrap_or(fb.border),
            accent: colour(&["accent"]).unwrap_or(fb.accent),
            red: colour(&["red", "color1"]).unwrap_or(fb.red),
            yellow: colour(&["yellow", "color3", "orange"]).unwrap_or(fb.yellow),
            green: colour(&["green", "color2"]).unwrap_or(fb.green),
            blue: colour(&["blue", "color4"]).unwrap_or(fb.blue),
            cyan: colour(&["cyan", "color6"]).unwrap_or(fb.cyan),
            magenta: colour(&["magenta", "color5", "purple"]).unwrap_or(fb.magenta),
        }
    }

    /// Parsed `(r, g, b)` in 0.0..=1.0, for cairo drawing. A digit that is not
    /// a hex digit counts as zero, and anything that is not three or six of
    /// them is black.
    ///
    /// It reads bytes and never slices the `str`. A slice that lands inside a
    /// character panics, the three-digit branch counted characters where the
    /// length had counted bytes, and this is called from GTK callbacks, which
    /// a panic cannot unwind through: one non-ASCII character in a theme's
    /// colour closed the application when a commit graph was opened.
    pub fn rgb(hex: &str) -> (f64, f64, f64) {
        let h = hex.trim_start_matches('#').as_bytes();
        let digit = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
        let [r, g, b] = match h.len() {
            3 => [h[0], h[1], h[2]].map(|c| digit(c) * 17),
            6 => [0, 2, 4].map(|i| (digit(h[i]) << 4) | digit(h[i + 1])),
            _ => [0, 0, 0],
        };
        (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0)
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

/* The commit you followed a search result to. It has to be findable at a
   glance in a list of three hundred near-identical rows. */
.om-detail {{
  background-color: @om_sunken;
  border-top: 1px solid @om_border;
}}

textview.om-diff,
textview.om-diff text {{
  background-color: @om_bg;
  color: @om_fg;
  font-size: 12px;
}}

/* A group whose changes do not block an update steps back from the ones that
   do — the view is usually being read to answer "what must I clean up". */
listbox.om-results > row.om-aside {{
  opacity: 0.70;
}}

.om-commit {{
  padding: 8px 10px;
  border-top: 1px solid @om_border;
  background-color: @om_bg;
}}

.om-status {{
  font-family: monospace;
  font-size: 11px;
  font-weight: 700;
  min-width: 14px;
  color: @om_dim;
}}

.om-status.added {{
  color: {green};
}}

.om-status.deleted {{
  color: {red};
}}

.om-status.renamed {{
  color: {cyan};
}}

.om-status.untracked {{
  color: @om_dim;
}}

.om-status.modified {{
  color: {yellow};
}}

paned > separator {{
  background-color: @om_border;
  min-height: 2px;
}}

listbox.om-results > row:selected {{
  background-color: alpha(@om_fg, 0.10);
}}

listbox.om-results > row.om-target {{
  background-color: mix(@om_raised, @om_accent, 0.38);
  box-shadow: inset 3px 0 0 0 @om_accent;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

    /// Put a theme where `Palette::load` looks for the active one. Only under
    /// `isolate()`: the directory is emptied first, and without the fake state
    /// home that would be the real one.
    fn install(colors: &str) -> PathBuf {
        let dir = state_dir();
        assert!(dir.starts_with(base()), "isolate() comes first");
        let _ = std::fs::remove_dir_all(&dir);
        write(&dir.join("theme/colors.toml"), colors);
        dir
    }

    /// A colour was accepted on its length in bytes and a leading `#`. A
    /// zero-width space after `#fff` makes seven bytes, an accented letter
    /// and a digit make four, and neither is a colour; nor is a value that
    /// closes the declaration it is written into.
    #[test]
    fn a_colour_that_is_not_hex_digits_is_not_taken_from_a_theme() {
        let _guard = isolate();
        install(
            "background = \"#fff\u{200b}\"\n\
             foreground = \"#é1\"\n\
             accent = \"#;}*{x;\"\n\
             red = \"#zzzzzz\"\n\
             green = \" #40A02B \"\n\
             cyan = \"#1ab\"\n",
        );

        let palette = Palette::load();
        let builtin = Palette::fallback();
        assert_eq!(palette.bg, builtin.bg);
        assert_eq!(palette.fg, builtin.fg);
        assert_eq!(palette.accent, builtin.accent);
        assert_eq!(palette.red, builtin.red);
        assert_eq!(palette.green, "#40A02B", "a colour with spaces round it is still one");
        assert_eq!(palette.cyan, "#1ab");
        assert!(!palette.stylesheet().contains("}*{"));
    }

    /// The first three of these panicked: two on a slice that ended inside a
    /// character, one on an index past the two channels it had parsed.
    #[test]
    fn rgb_takes_any_string_without_panicking() {
        for odd in ["#fff\u{200b}", "#é1", "#€", "é", "#", "", "#12345", "#zzzzzz"] {
            let (r, g, b) = Palette::rgb(odd);
            assert!([r, g, b].iter().all(|v| (0.0..=1.0).contains(v)), "{odd:?}");
        }

        assert_eq!(Palette::rgb("#0a2540"), (10.0 / 255.0, 37.0 / 255.0, 64.0 / 255.0));
        assert_eq!(Palette::rgb("0A2540"), Palette::rgb("#0a2540"));
        assert_eq!(Palette::rgb("#fa0"), Palette::rgb("#ffaa00"));
    }

    /// What `omarchy-theme-colors-from-alacritty` writes for a theme that
    /// brings only terminal colours, here Catppuccin Latte's: no `mode`, and
    /// none of the names the stock themes use beyond the first four.
    const GENERATED_LATTE: &str = r##"accent = "#1e66f5"
selection = "#dc8a78"

background = "#eff1f5"
foreground = "#4c4f69"

color0 = "#eff1f5"
color1 = "#d20f39"
color2 = "#40a02b"
color3 = "#df8e1d"
color4 = "#1e66f5"
color5 = "#ea76cb"
color6 = "#179299"
color7 = "#4c4f69"
color8 = "#acb0be"
color9 = "#d20f39"
color10 = "#40a02b"
color11 = "#df8e1d"
color12 = "#1e66f5"
color13 = "#ea76cb"
color14 = "#179299"
color15 = "#bcc0cc"
"##;

    /// Omarchy resolves this file as a light theme with Latte's own red,
    /// green and yellow. It was read here as a dark one, with the built-in
    /// palette's status colours and the selection colour for its borders.
    #[test]
    fn a_theme_generated_from_terminal_colours_is_read_as_omarchy_reads_it() {
        let _guard = isolate();
        install(GENERATED_LATTE);

        let palette = Palette::load();
        assert!(!palette.dark, "a background of #eff1f5 is a light theme");
        assert_eq!(
            [&palette.red, &palette.green, &palette.yellow],
            ["#d20f39", "#40a02b", "#df8e1d"]
        );
        assert_eq!(
            [&palette.blue, &palette.magenta, &palette.cyan],
            ["#1e66f5", "#ea76cb", "#179299"]
        );
        assert_eq!(palette.border, "#acb0be", "color8, not the selection's salmon");
        assert_eq!((palette.bg.as_str(), palette.fg.as_str()), ("#eff1f5", "#4c4f69"));

        // The shades it does not name are not invented.
        assert_eq!([&palette.bg_raised, &palette.bg_sunken], ["#eff1f5", "#eff1f5"]);
        assert_eq!(palette.fg_dim, "#4c4f69");
    }

    /// The order is Omarchy's: `mode`, `theme_type`, a `light.mode` file, the
    /// background, and dark when there is nothing to go on. Only the first
    /// was read.
    #[test]
    fn light_or_dark_is_settled_in_omarchys_order() {
        let night = "background = \"#1a1b26\"\n";
        let paper = "background = \"#eff1f5\"\n";

        assert!(Palette::resolve("", false).dark);
        assert!(Palette::resolve(night, false).dark);
        assert!(!Palette::resolve(paper, false).dark);
        // 382 of 765 is the last dark background, as it is for Omarchy.
        assert!(Palette::resolve("background = \"#7f7f80\"\n", false).dark);
        assert!(!Palette::resolve("background = \"#7f8080\"\n", false).dark);
        // The short name and the terminal's are the same background.
        assert!(!Palette::resolve("bg = \"#eff1f5\"\n", false).dark);
        assert!(!Palette::resolve("color0 = \"#eff1f5\"\n", false).dark);

        assert!(!Palette::resolve(night, true).dark, "the file outranks the background");
        assert!(
            Palette::resolve(&format!("theme_type = \"dark\"\n{paper}"), true).dark,
            "theme_type outranks the file"
        );
        assert!(
            !Palette::resolve(&format!("mode = \"light\"\ntheme_type = \"dark\"\n{night}"), false)
                .dark,
            "mode outranks theme_type"
        );
        assert!(
            !Palette::resolve("mode = \"\"\ntheme_type = \"light\"\n", false).dark,
            "an empty mode is no answer"
        );
    }

    #[test]
    fn a_light_mode_file_beside_the_colours_is_found() {
        let _guard = isolate();
        let dir = install("background = \"#1a1b26\"\nforeground = \"#a9b1d6\"\n");
        assert!(Palette::load().dark);

        write(&dir.join("theme/light.mode"), "");
        write(&dir.join("theme.name"), "paper\n");
        let palette = Palette::load();
        assert!(!palette.dark);
        assert_eq!(palette.name, "paper");
    }

    /// Omarchy lets a theme give a colour under both its names and takes the
    /// canonical one. Read through serde aliases, the second spelling would
    /// have been a duplicate field and the whole palette lost.
    #[test]
    fn the_canonical_name_comes_before_an_older_spelling() {
        let palette = Palette::resolve(
            "background = \"#111111\"\nbg = \"#222222\"\ncolor0 = \"#333333\"\n\
             fg = \"#eeeeee\"\ncolor7 = \"#dddddd\"\n\
             red = \"#aa0000\"\ncolor1 = \"#bb0000\"\n\
             lighter_bg = \"#444444\"\ndarker_bg = \"#000000\"\ndark_bg = \"#0a0a0a\"\n\
             light_fg = \"#cccccc\"\npurple = \"#800080\"\norange = \"#ff8800\"\n",
            false,
        );
        assert_eq!((palette.bg.as_str(), palette.fg.as_str()), ("#111111", "#eeeeee"));
        assert_eq!(palette.red, "#aa0000");
        assert_eq!([&palette.bg_raised, &palette.bg_sunken], ["#444444", "#000000"]);
        assert_eq!(palette.fg_dim, "#cccccc");
        assert_eq!([&palette.magenta, &palette.yellow], ["#800080", "#ff8800"]);

        // A value that is not a colour passes the turn to the next name, and
        // one that is not text does not cost the theme the rest of its file.
        let palette =
            Palette::resolve("red = \"crimson\"\ncolor1 = \"#bb0000\"\ncolor2 = 2\n", false);
        assert_eq!(palette.red, "#bb0000");
        assert_eq!(palette.green, Palette::fallback().green);
    }

    /// A stock theme's file, which names everything, resolves to what it
    /// always did.
    #[test]
    fn a_stock_theme_resolves_as_before() {
        let retro = r##"mode = "dark"

accent = "#faa968"
selection = "#134e5a"
muted = "#2a6b78"

background = "#05182e"
dark_background = "#031222"
darker_background = "#020c17"
lighter_background = "#0a2540"

foreground = "#f6dcac"
dark_foreground = "#3f8f8a"
light_foreground = "#a7c9c6"
bright_foreground = "#f6dcac"

red = "#f85525"
yellow = "#e97b3c"
orange = "#faa968"
green = "#028391"
cyan = "#8cbfb8"
blue = "#3f8f8a"
magenta = "#3f8f8a"
brown = "#743d1e"
"##;
        let palette = Palette::resolve(retro, false);
        assert!(palette.dark);
        assert_eq!(
            [&palette.bg, &palette.bg_raised, &palette.bg_sunken],
            ["#05182e", "#0a2540", "#020c17"]
        );
        assert_eq!([&palette.fg, &palette.fg_dim], ["#f6dcac", "#a7c9c6"]);
        assert_eq!([&palette.border, &palette.accent], ["#2a6b78", "#faa968"]);
        assert_eq!(
            [&palette.red, &palette.yellow, &palette.green],
            ["#f85525", "#e97b3c", "#028391"]
        );
        assert_eq!(
            [&palette.blue, &palette.cyan, &palette.magenta],
            ["#3f8f8a", "#8cbfb8", "#3f8f8a"]
        );
    }
}
