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

/// The opacity of whatever steps back to let something else stand out. It
/// fades text along with the surface under it, so it cannot go far: on
/// catppuccin-latte a card's name drops under 4.5:1 below 0.88.
const STEPPED_BACK: f64 = 0.90;

/// How much of the foreground is laid over a row under the pointer, and over
/// the selected one. They are named because the status letters in a row are
/// fitted to these backgrounds.
const ROW_LIT: f64 = 0.07;
const ROW_SELECTED: f64 = 0.10;

/// How much of its own hue the fill of a coloured label carries.
const WASH: f64 = 0.16;

/// The contrast a coloured label's text is fitted to. WCAG asks 4.5:1 of text
/// this small. The little over it is for what happens between here and the
/// screen, where GTK blends a see-through row or a card at 90% and rounds
/// the result to whole bytes.
const LEGIBLE: f64 = 4.6;

/// An opaque colour as `Palette::rgb` gives it, each channel in 0.0..=1.0.
type Rgb = (f64, f64, f64);

/// `from`, moved `by` of the way to `to`. This is GTK's `mix()`, and what
/// drawing `to` at an alpha or an opacity of `by` over `from` comes to: GTK
/// blends the encoded values, not linear light.
fn mix(from: Rgb, to: Rgb, by: f64) -> Rgb {
    (
        from.0 + (to.0 - from.0) * by,
        from.1 + (to.1 - from.1) * by,
        from.2 + (to.2 - from.2) * by,
    )
}

/// The WCAG 2 contrast ratio of two colours: 1 for the same colour twice, 21
/// for black on white.
fn contrast(a: Rgb, b: Rgb) -> f64 {
    let luminance = |(r, g, b): Rgb| {
        let linear =
            |v: f64| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    };
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The nearest colour that can be written as `#rrggbb`, so that what is
/// measured is what goes into the stylesheet.
fn whole(colour: Rgb) -> Rgb {
    let byte = |v: f64| (v * 255.0).round() / 255.0;
    (byte(colour.0), byte(colour.1), byte(colour.2))
}

/// A colour as the stylesheet writes one.
fn hex(colour: Rgb) -> String {
    let byte = |v: f64| (v * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(colour.0), byte(colour.1), byte(colour.2))
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

    /// `hue` as a text colour: the hue itself where `reads` accepts it, and
    /// otherwise moved toward the foreground, a twentieth at a time, as far
    /// as it has to go. Where not even the foreground is accepted it is the
    /// foreground, which is the most a palette has to give.
    fn ink(&self, hue: &str, reads: impl Fn(Rgb) -> bool) -> String {
        let (hue, fg) = (Self::rgb(hue), Self::rgb(&self.fg));
        let ink = (0..=20)
            .map(|step| whole(mix(hue, fg, f64::from(step) / 20.0)))
            .find(|&ink| reads(ink))
            .unwrap_or(fg);
        hex(ink)
    }

    /// The GTK stylesheet for this palette.
    pub fn stylesheet(&self) -> String {
        let p = self;
        let (page, card, fg) = (Self::rgb(&p.bg), Self::rgb(&p.bg_raised), Self::rgb(&p.fg));

        // The coloured labels: the pills on a card, the ref names in the
        // graph, the status letters in the file list. Each was a theme colour
        // written on an 18 to 30% wash of that same colour. A colour is
        // closer to a wash of itself than to the bare surface, and these
        // colours come from a terminal palette, which nobody chose for 11px
        // text on a tint: of the pill and palette pairs tested below, 46 in
        // 54 were under 4.5:1 and the worst was 1.66.
        //
        // The fill still carries the hue, and the text as much of it as can
        // be read (`ink`). The fill is also opaque now, mixed here into the
        // surface the label rests on, where it was a see-through `alpha()`.
        // A card changes colour when it is ticked, focused or under the
        // pointer, and a see-through fill changed with it, under text that
        // did not: "held back" on a ticked, focused kanagawa card was 1.04:1.
        let label = |family: &str, surface: Rgb, steps_back: bool, class: &str, hue: &str| {
            let fill = whole(mix(surface, Self::rgb(hue), WASH));
            let behind = |colour: Rgb| mix(page, colour, STEPPED_BACK);
            let text = p.ink(hue, |ink| {
                contrast(ink, fill) >= LEGIBLE
                    && (!steps_back || contrast(behind(ink), behind(fill)) >= LEGIBLE)
            });
            format!(
                ".{family}.{class} {{\n  background-color: {};\n  color: {text};\n}}",
                hex(fill)
            )
        };
        // A pill is read on a card that has stepped back as well as on one
        // that has not, so its text is fitted to both.
        let pills = [
            ("branch", &p.accent),
            ("clean", &p.green),
            ("dirty", &p.yellow),
            ("ahead", &p.cyan),
            ("behind", &p.magenta),
            ("danger", &p.red),
        ]
        .map(|(class, hue)| label("om-pill", card, true, class, hue))
        .join("\n\n");
        // In this order: the branch HEAD is on is `local` and `current`, and
        // takes the later rule's accent; a detached HEAD is `current` and
        // `head`, and stays red.
        let refs = [
            ("local", &p.green),
            ("current", &p.accent),
            ("remote", &p.cyan),
            ("tag", &p.yellow),
            ("head", &p.red),
        ]
        .map(|(class, hue)| label("om-ref", page, false, class, hue))
        .join("\n\n");
        // A status letter has no fill. It is drawn straight on the row, and
        // has to be read on each background a row can have.
        let rows = [page, mix(page, fg, ROW_LIT), mix(page, fg, ROW_SELECTED)];
        let letters = [
            ("added", &p.green),
            ("deleted", &p.red),
            ("renamed", &p.cyan),
            ("modified", &p.yellow),
        ]
        .map(|(class, hue)| {
            let text = p.ink(hue, |ink| rows.iter().all(|&row| contrast(ink, row) >= LEGIBLE));
            format!(".om-status.{class} {{\n  color: {text};\n}}")
        })
        .join("\n\n");

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

/* The reset above also took away the one thing that shows a toggle is on.
   Adwaita marks a pressed flat button with a background and nothing else,
   and this stylesheet is loaded at application priority, where it outranks
   the theme's whatever the selectors say. The needs-attention filter looked
   the same on and off, and "6 shown" in the status bar was the only sign of
   why the grid was short. This comes after `:hover`, which weighs the same,
   so a toggle shows as on while the pointer that pressed it is still there. */
window.omarepos headerbar button:checked {{
  background-color: alpha(@om_accent, 0.30);
}}

.om-title {{
  font-weight: 700;
  letter-spacing: 1px;
}}

/* Text is sized in rem, which is the desktop's interface font: 0.75rem is
   11px at the stock 11pt, 0.68 is 10px, 0.82 is 12px, 1.02 is 15px and 1.5 is
   22px. Every size here used to be one of those pixel counts, and a length
   in px is used as written. A larger font or a text-scaling factor, which
   someone sets because they need it, made the buttons and dialogs grow and
   left the names, paths, pills and diff, most of what the window shows, at
   10 to 15px. Not em or a percentage, which compound from a widget to the
   one inside it, and not pt, which follows the scaling factor and ignores
   the font. */
.om-subtitle {{
  font-size: 0.75rem;
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

/* The ring and the check say a card is ticked. The fill only backs them up,
   because it goes under the card's text and the accent is whatever the theme
   says it is. At 0.22, and 0.38 on a ticked card with the focus, the name of
   the repository fell under 4.5:1 on half the stock themes; on kanagawa,
   whose accent is its foreground, the card was mixed toward the colour of
   its own text. */
.om-card.om-selected {{
  border-color: @om_accent;
  background-color: mix(@om_raised, @om_accent, 0.10);
  box-shadow: inset 0 0 0 2px @om_accent;
}}

.om-card.om-selected:hover {{
  background-color: mix(@om_raised, @om_accent, 0.12);
}}

/* A muted theme accent cannot carry the highlight on its own, so once
   anything is selected the rest of the grid steps back. Only a little,
   because opacity fades the text along with the card, and these are the
   cards read next, to choose the second one. At 0.42 no stock theme kept a
   name at 4.5:1. */
flowbox.om-flow.om-has-selection .om-card:not(.om-selected) {{
  opacity: {stepped_back};
}}

flowbox.om-flow.om-has-selection .om-card:not(.om-selected):hover {{
  opacity: 1;
}}

.om-card.om-selected checkbutton check {{
  background-color: @om_accent;
  border-color: @om_accent;
}}

.om-tag {{
  font-size: 0.75rem;
  font-weight: 600;
  padding: 1px 8px 1px 6px;
  border-radius: 999px;
  background-color: alpha(@om_fg, 0.10);
  color: @om_fg;
}}

/* The quick-tag toggles live in the header, so each rule is also written at
   the weight of the header's rules plus the class. As a bare class they lost
   to the reset there: a chip had no fill until it was on and never a border,
   and took the header's hover wash for its own.

   The fills are under the tag's name and over the header's colour, and are
   held to what leaves the name at 4.5:1 there. Two of the three had never
   been drawn as written, and 16% of the foreground under the pointer would
   have been 4.28 on rose-pine. The one that was drawn, 40% accent in the
   card colour on a chip that is on, left the name at 3.16 on everforest;
   the border says on, now that there is one. */
.om-quick-tag,
window.omarepos headerbar button.om-quick-tag {{
  border-radius: 999px;
  padding: 2px 12px;
  font-size: 0.82rem;
  font-weight: 600;
  background-color: alpha(@om_fg, 0.08);
  color: @om_fg;
  border: 1px solid transparent;
  box-shadow: none;
}}

.om-quick-tag:hover,
window.omarepos headerbar button.om-quick-tag:hover {{
  background-color: alpha(@om_fg, 0.12);
}}

.om-quick-tag:checked,
window.omarepos headerbar button.om-quick-tag:checked {{
  background-color: alpha(@om_accent, 0.18);
  border-color: @om_accent;
  color: @om_fg;
}}

.om-tag-chip {{
  font-size: 0.75rem;
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
  font-size: 1.02rem;
  color: @om_fg;
}}

.om-path {{
  font-size: 0.75rem;
  color: @om_dim;
}}

.om-meta {{
  font-size: 0.75rem;
  color: @om_dim;
}}

.om-summary {{
  font-size: 0.82rem;
  color: @om_fg;
}}

.om-pill {{
  font-size: 0.75rem;
  font-weight: 700;
  padding: 1px 8px;
  border-radius: 999px;
  background-color: alpha(@om_fg, 0.10);
  color: @om_fg;
}}

{pills}

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
  background-color: mix(@om_raised, @om_accent, 0.10);
  border-color: @om_accent;
}}

flowbox.om-flow.om-keyboard.om-has-selection .om-card:not(.om-selected):hover {{
  opacity: {stepped_back};
}}

/* A focused card is the one being looked at, so it is never the dimmed one. */
flowbox.om-flow.om-has-selection > flowboxchild:focus-visible .om-card:not(.om-selected) {{
  opacity: 1;
}}

flowbox.om-flow > flowboxchild:focus-visible .om-card.om-selected {{
  outline-color: @om_accent;
  background-color: mix(@om_raised, @om_accent, 0.12);
}}

.om-statusbar {{
  font-size: 0.75rem;
  color: @om_dim;
  border-top: 1px solid @om_border;
  background-color: @om_sunken;
  padding: 4px 12px;
}}

/* The CSS node of a GtkListBox is called `list`. These rules were written for
   `listbox`, a name GTK accepts without a word and matches to nothing, so the
   commit graph, the file list and the search results were never themed:
   Adwaita painted them its own grey, or white, over the theme's background,
   and kept its own colour for the text and icons inside them. */
list.om-results,
list.om-results > row {{
  background-color: transparent;
  color: @om_fg;
}}

/* No padding here. A row in the graph or the search results carries `om-row`
   itself and takes its side padding from that rule, further down, which a
   padding set on this selector would outrank. */
list.om-results > row {{
  min-height: 0;
  border-radius: 6px;
}}

.om-detail {{
  background-color: @om_sunken;
  border-top: 1px solid @om_border;
}}

textview.om-diff,
textview.om-diff text {{
  background-color: @om_bg;
  color: @om_fg;
  font-size: 0.82rem;
}}

/* A group whose changes do not block an update steps back from the ones that
   do — the view is usually being read to answer "what must I clean up". It
   steps back as far as a card does and no further. The 0.70 written here
   was never drawn; it would have put the heading at 3.34:1 on rose-pine and
   the note beside it at 2.72. */
list.om-results > row.om-aside {{
  opacity: {stepped_back};
}}

.om-commit {{
  padding: 8px 10px;
  border-top: 1px solid @om_border;
  background-color: @om_bg;
}}

.om-status {{
  font-family: monospace;
  font-size: 0.75rem;
  font-weight: 700;
  min-width: 14px;
  color: @om_dim;
}}

.om-status.untracked {{
  color: @om_dim;
}}

{letters}

paned > separator {{
  background-color: @om_border;
  min-height: 2px;
}}

list.om-results > row:selected {{
  background-color: alpha(@om_fg, {row_selected});
}}

/* The commit you followed a search result to. It has to be findable at a
   glance in a list of three hundred near-identical rows, and then it has to
   be read. The bar down its edge is what finds it; the tint goes under its
   text, so it is a light one, mixed into the background the row lies on. At
   0.38 of the accent, as first written and never drawn, the summary of that
   one commit would have been at 3.26:1 on everforest and its sha at 2.26. */
list.om-results > row.om-target {{
  background-color: mix(@om_bg, @om_accent, 0.14);
  box-shadow: inset 3px 0 0 0 @om_accent;
}}

list.om-results > row:focus-visible {{
  outline: 2px solid @om_fg;
  outline-offset: -2px;
}}

.om-row {{
  border-radius: 6px;
  padding: 0 6px;
}}

.om-row:hover {{
  background-color: alpha(@om_fg, {row_lit});
}}

/* In the file list `om-row` is on the line inside the row and not on the row,
   so there the pointer would light a selected row a second time, on top of
   the selection. A selected row in the graph does not change under it. */
list.om-results > row:selected .om-row:hover {{
  background-color: transparent;
}}

.om-sha {{
  font-family: monospace;
  font-size: 0.75rem;
  color: @om_dim;
}}

.om-ref {{
  font-size: 0.68rem;
  font-weight: 700;
  padding: 0 6px;
  border-radius: 4px;
  background-color: alpha(@om_fg, 0.12);
  color: @om_fg;
}}

{refs}

.om-big {{
  font-size: 1.5rem;
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
            yellow = p.yellow,
            red = p.red,
            stepped_back = STEPPED_BACK,
            row_lit = ROW_LIT,
            row_selected = ROW_SELECTED,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

    // Nothing here opens a window, so nothing here can look at the result.
    // What can be done without one is to read the stylesheet back, work out
    // the colour of each piece of text and of what it is drawn on, and measure
    // the two against each other: that is what most of these tests do, for
    // real palettes.

    /// What WCAG asks between text and its background at the sizes used
    /// here, all of them under 18.66px bold.
    const AA: f64 = 4.5;

    /// What the muted lines are held to instead: path, remote, sha, author,
    /// age. On miasma, rose-pine and everforest they do not reach 4.5:1 on a
    /// surface nothing has touched. That is the theme's choice of colours;
    /// what is tested is that no state of the interface takes them under 3:1,
    /// which is where several used to leave them.
    const FLOOR: f64 = 3.0;

    /// What WCAG asks between an icon and what it is drawn on.
    const ICON: f64 = 3.0;

    /// Stock Omarchy palettes as `Palette::load` resolves them, copied here
    /// so that the tests do not depend on what is installed: the one this was
    /// written under, Omarchy's default, the three dark ones with the least
    /// contrast to spare or the oddest accent, and three light ones. In the
    /// order of the fields of `Palette`.
    const STOCK: [(&str, bool, [&str; 13]); 8] = [
        (
            "retro-82",
            true,
            [
                "#05182e", "#0a2540", "#020c17", "#f6dcac", "#a7c9c6", "#2a6b78", "#faa968",
                "#f85525", "#e97b3c", "#028391", "#3f8f8a", "#8cbfb8", "#3f8f8a",
            ],
        ),
        (
            "tokyo-night",
            true,
            [
                "#1a1b26", "#24283b", "#0e0e14", "#a9b1d6", "#b4bee6", "#414868", "#7aa2f7",
                "#f7768e", "#e0af68", "#9ece6a", "#7aa2f7", "#449dab", "#ad8ee6",
            ],
        ),
        (
            "everforest",
            true,
            [
                "#2d353b", "#343f44", "#181d20", "#d3c6aa", "#9da9a0", "#475258", "#7fbbb3",
                "#e67e80", "#dbbc7f", "#a7c080", "#7fbbb3", "#83c092", "#d699b6",
            ],
        ),
        (
            "miasma",
            true,
            [
                "#222222", "#2c2c2c", "#121212", "#c2c2b0", "#8a8a7e", "#666666", "#78824b",
                "#685742", "#b36d43", "#5f875f", "#78824b", "#c9a554", "#bb7744",
            ],
        ),
        (
            "kanagawa",
            true,
            [
                "#1f1f28", "#223249", "#111116", "#dcd7ba", "#c8c093", "#54546d", "#dcd7ba",
                "#c34043", "#c0a36e", "#76946a", "#7e9cd8", "#6a9589", "#957fb8",
            ],
        ),
        (
            "catppuccin-latte",
            false,
            [
                "#eff1f5", "#dce0e8", "#d7d8dc", "#4c4f69", "#5c5f77", "#acb0be", "#1e66f5",
                "#d20f39", "#df8e1d", "#40a02b", "#1e66f5", "#179299", "#ea76cb",
            ],
        ),
        (
            "rose-pine",
            false,
            [
                "#faf4ed", "#f2e9e1", "#e1dbd5", "#575279", "#6e6a86", "#cecacd", "#56949f",
                "#b4637a", "#ea9d34", "#286983", "#56949f", "#d7827e", "#907aa9",
            ],
        ),
        (
            "flexoki-light",
            false,
            [
                "#fffcf0", "#e6e4d9", "#e5e2d8", "#100f0f", "#403e3c", "#b7b5ac", "#205ea6",
                "#d14d41", "#d0a215", "#879a39", "#205ea6", "#3aa99f", "#ce5d97",
            ],
        ),
    ];

    /// The stock palettes above, and the built-in one.
    fn palettes() -> Vec<Palette> {
        let mut all: Vec<Palette> = STOCK
            .iter()
            .map(|(name, dark, c)| Palette {
                name: name.to_string(),
                dark: *dark,
                bg: c[0].into(),
                bg_raised: c[1].into(),
                bg_sunken: c[2].into(),
                fg: c[3].into(),
                fg_dim: c[4].into(),
                border: c[5].into(),
                accent: c[6].into(),
                red: c[7].into(),
                yellow: c[8].into(),
                green: c[9].into(),
                blue: c[10].into(),
                cyan: c[11].into(),
                magenta: c[12].into(),
            })
            .collect();
        all.push(Palette::fallback());
        all
    }

    /// A colour that is what it is whatever lies under it.
    type Solid = (f64, f64, f64);
    /// A colour as the stylesheet states it, which may let what is under it
    /// show through.
    type Rgba = (f64, f64, f64, f64);

    /// The generated stylesheet, read back: its colour definitions, and every
    /// declaration with the selector it was written for.
    struct Sheet {
        colours: Vec<(String, String)>,
        rules: Vec<(String, String, String)>,
    }

    impl Sheet {
        fn of(palette: &Palette) -> Self {
            let mut css = palette.stylesheet();
            while let Some(start) = css.find("/*") {
                let end = start + css[start..].find("*/").expect("a comment is closed") + 2;
                css.replace_range(start..end, "");
            }
            let mut sheet = Sheet { colours: Vec::new(), rules: Vec::new() };
            let mut rest = css.as_str();
            loop {
                rest = rest.trim_start();
                if rest.is_empty() {
                    break;
                }
                if let Some(define) = rest.strip_prefix("@define-color") {
                    let (statement, after) = define.split_once(';').expect("a definition ends");
                    let (name, value) = statement
                        .trim()
                        .split_once(char::is_whitespace)
                        .expect("a definition has a name and a value");
                    sheet.colours.push((name.to_string(), value.trim().to_string()));
                    rest = after;
                    continue;
                }
                let (selectors, after) = rest.split_once('{').expect("a rule opens");
                let (body, after) = after.split_once('}').expect("a rule closes");
                for selector in selectors.split(',') {
                    let selector = selector.split_whitespace().collect::<Vec<_>>().join(" ");
                    for declaration in body.split(';') {
                        if let Some((property, value)) = declaration.split_once(':') {
                            sheet.rules.push((
                                selector.clone(),
                                property.trim().to_string(),
                                value.trim().to_string(),
                            ));
                        }
                    }
                }
                rest = after;
            }
            sheet
        }

        /// What the stylesheet declares for exactly this selector. This is
        /// not the cascade: a test names the rule it knows to win for the
        /// state it is asking about.
        fn value(&self, selector: &str, property: &str) -> &str {
            self.rules
                .iter()
                .rev()
                .find(|(s, p, _)| s == selector && p == property)
                .map(|(_, _, value)| value.as_str())
                .unwrap_or_else(|| panic!("the stylesheet sets no `{property}` on `{selector}`"))
        }

        fn colour(&self, selector: &str, property: &str) -> Rgba {
            self.eval(self.value(selector, property))
        }

        fn solid(&self, selector: &str, property: &str) -> Solid {
            let (r, g, b, alpha) = self.colour(selector, property);
            assert_eq!(alpha, 1.0, "`{property}` on `{selector}` is not opaque");
            (r, g, b)
        }

        fn number(&self, selector: &str, property: &str) -> f64 {
            self.value(selector, property).parse().expect("a number")
        }

        /// A colour expression, as GTK resolves it.
        fn eval(&self, expr: &str) -> Rgba {
            let expr = expr.trim();
            if let Some(name) = expr.strip_prefix('@') {
                let (_, value) = self
                    .colours
                    .iter()
                    .find(|(defined, _)| defined == name)
                    .unwrap_or_else(|| panic!("`@{name}` is used and never defined"));
                return self.eval(value);
            }
            if expr.starts_with('#') {
                let (r, g, b) = Palette::rgb(expr);
                return (r, g, b, 1.0);
            }
            if expr == "transparent" {
                return (0.0, 0.0, 0.0, 0.0);
            }
            let (function, list) = expr
                .strip_suffix(')')
                .and_then(|open| open.split_once('('))
                .unwrap_or_else(|| panic!("`{expr}` is not a colour this test can read"));
            let factor = |text: &str| -> f64 { text.parse().expect("a factor") };
            match (function, arguments(list).as_slice()) {
                ("alpha", [colour, by]) => {
                    let (r, g, b, alpha) = self.eval(colour);
                    (r, g, b, alpha * factor(by))
                }
                ("mix", [from, to, by]) => {
                    let (from, to, by) = (self.eval(from), self.eval(to), factor(by));
                    let lerp = |a: f64, b: f64| a + (b - a) * by;
                    (lerp(from.0, to.0), lerp(from.1, to.1), lerp(from.2, to.2), lerp(from.3, to.3))
                }
                ("shade", [colour, by]) => shade(self.eval(colour), factor(by)),
                _ => panic!("`{expr}` is not a colour this test can read"),
            }
        }
    }

    /// The arguments of a call, split at the commas that are its own.
    fn arguments(list: &str) -> Vec<&str> {
        let (mut depth, mut start, mut out) = (0, 0, Vec::new());
        for (at, c) in list.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    out.push(list[start..at].trim());
                    start = at + 1;
                }
                _ => {}
            }
        }
        out.push(list[start..].trim());
        out
    }

    /// GTK's `shade()`: the colour taken to hue, lightness and saturation,
    /// the last two multiplied, and brought back.
    fn shade((r, g, b, alpha): Rgba, factor: f64) -> Rgba {
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let lightness = (max + min) / 2.0;
        let (hue, saturation) = if max == min {
            (0.0, 0.0)
        } else {
            let spread = max - min;
            let saturation =
                if lightness > 0.5 { spread / (2.0 - max - min) } else { spread / (max + min) };
            let sixth = if max == r {
                (g - b) / spread + if g < b { 6.0 } else { 0.0 }
            } else if max == g {
                (b - r) / spread + 2.0
            } else {
                (r - g) / spread + 4.0
            };
            (sixth / 6.0, saturation)
        };
        let lightness = (lightness * factor).clamp(0.0, 1.0);
        let saturation = (saturation * factor).clamp(0.0, 1.0);
        let upper = if lightness < 0.5 {
            lightness * (1.0 + saturation)
        } else {
            lightness + saturation - lightness * saturation
        };
        let lower = 2.0 * lightness - upper;
        let channel = |turn: f64| {
            let turn = turn.rem_euclid(1.0);
            if turn < 1.0 / 6.0 {
                lower + (upper - lower) * 6.0 * turn
            } else if turn < 0.5 {
                upper
            } else if turn < 2.0 / 3.0 {
                lower + (upper - lower) * (2.0 / 3.0 - turn) * 6.0
            } else {
                lower
            }
        };
        (channel(hue + 1.0 / 3.0), channel(hue), channel(hue - 1.0 / 3.0), alpha)
    }

    /// One colour drawn over another. GTK blends the encoded values, not
    /// linear light, unless it is asked to with a debug flag.
    fn over((r, g, b, alpha): Rgba, under: Solid) -> Solid {
        (
            under.0 + (r - under.0) * alpha,
            under.1 + (g - under.1) * alpha,
            under.2 + (b - under.2) * alpha,
        )
    }

    /// A colour inside a widget drawn at `opacity`, with `backdrop` behind
    /// the widget. Opacity takes the whole widget, text and all.
    fn faded(colour: Solid, opacity: f64, backdrop: Solid) -> Solid {
        over((colour.0, colour.1, colour.2, opacity), backdrop)
    }

    /// The WCAG 2 contrast ratio.
    fn ratio(a: Solid, b: Solid) -> f64 {
        let luminance = |(r, g, b): Solid| {
            let linear =
                |v: f64| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
            0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// One kind of text measured across the palettes. It is asserted on once
    /// everything has been measured, so that a failure lists every case that
    /// fell short and not the first; and it prints the worst case when it
    /// passes, to be seen with `--nocapture`.
    struct Measured {
        what: &'static str,
        need: f64,
        worst: (f64, String),
        short: Vec<String>,
    }

    impl Measured {
        fn new(what: &'static str, need: f64) -> Self {
            Self { what, need, worst: (f64::INFINITY, String::new()), short: Vec::new() }
        }

        fn add(&mut self, palette: &Palette, case: &str, ratio: f64) {
            let label = match case {
                "" => format!("{} at {ratio:.2}", palette.name),
                _ => format!("{}, {case}, at {ratio:.2}", palette.name),
            };
            if ratio < self.worst.0 {
                self.worst = (ratio, label.clone());
            }
            if ratio < self.need {
                self.short.push(label);
            }
        }

        fn check(self) {
            println!("{}: lowest is {}", self.what, self.worst.1);
            assert!(
                self.short.is_empty(),
                "{} is under {}:1 in {} cases, lowest {}:\n{:#?}",
                self.what,
                self.need,
                self.short.len(),
                self.worst.1,
                self.short
            );
        }
    }

    /// Every background a card can have, by the rule that wins for it.
    const CARD_STATES: [(&str, &str); 7] = [
        ("at rest", ".om-card"),
        ("under the pointer", ".om-card:hover"),
        ("with the focus", "flowbox.om-flow > flowboxchild:focus-visible .om-card"),
        ("ticked", ".om-card.om-selected"),
        ("ticked, under the pointer", ".om-card.om-selected:hover"),
        (
            "ticked, under an idle pointer",
            "flowbox.om-flow.om-keyboard .om-card.om-selected:hover",
        ),
        (
            "ticked, with the focus",
            "flowbox.om-flow > flowboxchild:focus-visible .om-card.om-selected",
        ),
    ];

    /// The rule that steps a card back once another is ticked.
    const STEPS_BACK: &str = "flowbox.om-flow.om-has-selection .om-card:not(.om-selected)";

    /// Every background a row of the file list or the search results can
    /// have, over the page the list lies on.
    fn rows(sheet: &Sheet) -> Vec<(&'static str, Solid)> {
        let page = sheet.solid("scrolledwindow.om-grid", "background-color");
        let of = |rule: &str| sheet.colour(rule, "background-color");
        let selected = over(of("list.om-results > row:selected"), page);
        vec![
            ("at rest", over(of("list.om-results > row"), over(of("list.om-results"), page))),
            ("under the pointer", over(of(".om-row:hover"), page)),
            ("selected", selected),
            // In the file list the pointer is over the line inside the row,
            // which has a background of its own to lay on the selection.
            (
                "selected, under the pointer",
                over(of("list.om-results > row:selected .om-row:hover"), selected),
            ),
        ]
    }

    /// The same for the commit graph, where one row more can be the commit a
    /// search led to.
    fn graph_rows(sheet: &Sheet) -> Vec<(&'static str, Solid)> {
        let page = sheet.solid("scrolledwindow.om-grid", "background-color");
        let followed = sheet.colour("list.om-results > row.om-target", "background-color");
        let mut all = rows(sheet);
        all.push(("followed from a search", over(followed, page)));
        all
    }

    /// How much a selector weighs when two rules set the same property: its
    /// classes and pseudo-classes first, then its element names. There are no
    /// ids in this stylesheet. `:not()` weighs what is inside it.
    fn weight(selector: &str) -> (usize, usize) {
        let classes = selector.matches(['.', ':']).count() - selector.matches(":not(").count();
        (classes, elements(selector).len())
    }

    const PILLS: [&str; 6] = ["branch", "clean", "dirty", "ahead", "behind", "danger"];
    const REFS: [&str; 5] = ["local", "current", "remote", "tag", "head"];
    const LETTERS: [&str; 4] = ["added", "deleted", "renamed", "modified"];

    /// The element names in a selector: `list` and `row` in
    /// `list.om-results > row:selected`.
    fn elements(selector: &str) -> Vec<&str> {
        selector
            .split([' ', '>'])
            .filter_map(|compound| compound.split(['.', ':']).next())
            .filter(|name| !name.is_empty())
            .collect()
    }

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

    /// A ticked card had the accent mixed in under its text: 22% of it, and
    /// 38% on the card that also had the focus, which is the card under the
    /// keyboard straight after Space. There the name was at 3.26:1 on
    /// everforest and the muted lines at 2.26:1.
    #[test]
    fn the_text_on_a_card_reads_whatever_state_the_card_is_in() {
        let mut names = Measured::new("a repository's name on its card", AA);
        let mut muted = Measured::new("the muted lines on a card", FLOOR);
        for palette in palettes() {
            let sheet = Sheet::of(&palette);
            let name = sheet.solid(".om-name", "color");
            let meta = sheet.solid(".om-meta", "color");
            for (state, rule) in CARD_STATES {
                let card = sheet.solid(rule, "background-color");
                names.add(&palette, state, ratio(name, card));
                muted.add(&palette, state, ratio(meta, card));
            }
        }
        names.check();
        muted.check();
    }

    /// Ticking one card drew every other card at 42% opacity, and opacity
    /// takes the text with it. The names on them, which is what is read to
    /// find the second card to tick, were at 1.85:1 on catppuccin-latte and
    /// no better than 3.98:1 on any stock theme.
    #[test]
    fn a_card_that_has_stepped_back_can_still_be_read() {
        let mut names = Measured::new("a repository's name on a card that has stepped back", AA);
        let mut muted = Measured::new("the muted lines on a card that has stepped back", FLOOR);
        for palette in palettes() {
            let sheet = Sheet::of(&palette);
            let page = sheet.solid("flowbox.om-flow", "background-color");
            let opacity = sheet.number(STEPS_BACK, "opacity");
            let card = faded(sheet.solid(".om-card", "background-color"), opacity, page);
            let on_it = |rule: &str| ratio(faded(sheet.solid(rule, "color"), opacity, page), card);
            names.add(&palette, "", on_it(".om-name"));
            muted.add(&palette, "", on_it(".om-meta"));

            // The pointer and the focus bring a card forward. An idle pointer
            // in keyboard mode leaves it where it was. Nothing sends it
            // further back, which the old hover rule would now do.
            let forward = [
                format!("{STEPS_BACK}:hover"),
                "flowbox.om-flow.om-has-selection > flowboxchild:focus-visible \
                 .om-card:not(.om-selected)"
                    .to_string(),
            ];
            for rule in &forward {
                assert_eq!(sheet.number(rule, "opacity"), 1.0, "{rule}");
            }
            let idle =
                "flowbox.om-flow.om-keyboard.om-has-selection .om-card:not(.om-selected):hover";
            assert_eq!(sheet.number(idle, "opacity"), opacity);
        }
        names.check();
        muted.check();
    }

    /// Seven selectors began with `listbox`. A GtkListBox's node is `list`,
    /// GTK takes an element name it has never heard of without complaint,
    /// and so the rules for three of the four main views applied to nothing
    /// for as long as they existed. The names below are the ones GTK's
    /// documentation gives the widgets this stylesheet addresses.
    #[test]
    fn every_element_a_selector_names_is_one_gtk_has() {
        const NODES: [&str; 18] = [
            "window", "headerbar", "button", "entry", "dropdown", "popover", "contents",
            "flowbox", "flowboxchild", "scrolledwindow", "checkbutton", "check", "list", "row",
            "textview", "text", "paned", "separator",
        ];
        let sheet = Sheet::of(&Palette::fallback());
        for (selector, _, _) in &sheet.rules {
            for element in elements(selector) {
                assert!(NODES.contains(&element), "`{element}` in `{selector}`");
            }
        }

        // The lists show the page under them, and take the theme's colour
        // for whatever in them has no colour of its own: Adwaita's rule for
        // `list` sets both.
        assert_eq!(sheet.colour("list.om-results", "background-color").3, 0.0);
        assert_eq!(
            sheet.solid("list.om-results", "color"),
            sheet.solid("window.omarepos", "color")
        );

        // The rules for a row of these lists outrank `.om-row`, which is on
        // the row itself in the graph and the search results and gives it
        // its 6px at either end. They must leave padding to it.
        assert_eq!(sheet.value(".om-row", "padding"), "0 6px");
        for (selector, property, _) in &sheet.rules {
            assert!(
                !(selector.starts_with("list.om-results > row") && property == "padding"),
                "`{selector}` sets a padding"
            );
        }
    }

    /// With the list rules matching at last, a row gets the backgrounds they
    /// describe. The one written for the commit a search led to mixed 38% of
    /// the accent under that commit's text.
    #[test]
    fn the_text_in_a_row_reads_whatever_state_the_row_is_in() {
        let mut summaries = Measured::new("a commit's summary in its row", AA);
        let mut muted = Measured::new("the sha, author and date in a row", FLOOR);
        for palette in palettes() {
            let sheet = Sheet::of(&palette);
            for (state, row) in graph_rows(&sheet) {
                summaries.add(&palette, state, ratio(sheet.solid(".om-summary", "color"), row));
                for rule in [".om-sha", ".om-meta"] {
                    muted.add(&palette, state, ratio(sheet.solid(rule, "color"), row));
                }
            }
        }
        summaries.check();
        muted.check();
    }

    #[test]
    fn the_heading_of_a_group_set_aside_can_still_be_read() {
        let mut headings = Measured::new("the heading of a group set aside", AA);
        let mut notes = Measured::new("the note beside it", FLOOR);
        for palette in palettes() {
            let sheet = Sheet::of(&palette);
            let page = sheet.solid("scrolledwindow.om-grid", "background-color");
            let opacity = sheet.number("list.om-results > row.om-aside", "opacity");
            let on_it = |rule: &str| ratio(faded(sheet.solid(rule, "color"), opacity, page), page);
            headings.add(&palette, "", on_it(".om-name"));
            notes.add(&palette, "", on_it(".om-meta"));
        }
        headings.check();
        notes.check();
    }

    /// The pills are what the grid is looked at for, and each drew its hue
    /// on a wash of that hue laid over whatever the card was doing: 1.66:1
    /// for "dirty" on a flexoki-light card at rest, and 1.04 for "held back"
    /// on a kanagawa card that was ticked and had the focus.
    #[test]
    fn a_pill_reads_on_its_fill_whatever_state_its_card_is_in() {
        let mut pills = Measured::new("a pill on its card", AA);
        for palette in palettes() {
            let sheet = Sheet::of(&palette);
            for (state, rule) in CARD_STATES {
                let card = sheet.solid(rule, "background-color");
                for class in PILLS {
                    let pill = format!(".om-pill.{class}");
                    let fill = over(sheet.colour(&pill, "background-color"), card);
                    let text = sheet.solid(&pill, "color");
                    pills.add(&palette, &format!("{class}, {state}"), ratio(text, fill));
                }
            }
        }
        pills.check();
    }

    /// On a card that has stepped back a pill is faded with the rest of it.
    /// Its text is fitted to that as well, and where a palette cannot manage
    /// it the text is the plain foreground: there is nothing more to give,
    /// and such a pill is held to the floor instead.
    #[test]
    fn a_pill_reads_on_a_card_that_has_stepped_back_or_is_in_the_plain_foreground() {
        let mut pills = Measured::new("a pill on a card that has stepped back", AA);
        let mut plain = Measured::new("a pill in the plain foreground, stepped back", FLOOR);
        for palette in palettes() {
            let sheet = Sheet::of(&palette);
            let page = sheet.solid("flowbox.om-flow", "background-color");
            let card = sheet.solid(".om-card", "background-color");
            let opacity = sheet.number(STEPS_BACK, "opacity");
            for class in PILLS {
                let pill = format!(".om-pill.{class}");
                let fill = over(sheet.colour(&pill, "background-color"), card);
                let text = sheet.solid(&pill, "color");
                let seen = ratio(faded(text, opacity, page), faded(fill, opacity, page));
                if text == sheet.solid(".om-pill", "color") {
                    plain.add(&palette, class, seen);
                } else {
                    pills.add(&palette, class, seen);
                }
            }
        }
        pills.check();
        plain.check();
    }

    /// The names of branches and tags in the graph were drawn the way the
    /// pills were, on washes of up to 30%: 1.78:1 for a tag on rose-pine.
    #[test]
    fn a_ref_label_reads_on_its_fill_whatever_state_its_row_is_in() {
        let mut labels = Measured::new("a ref label in its row", AA);
        for palette in palettes() {
            let sheet = Sheet::of(&palette);
            for (state, row) in graph_rows(&sheet) {
                for class in REFS {
                    let label = format!(".om-ref.{class}");
                    let fill = over(sheet.colour(&label, "background-color"), row);
                    let text = sheet.solid(&label, "color");
                    labels.add(&palette, &format!("{class}, {state}"), ratio(text, fill));
                }
            }
        }
        labels.check();
    }

    /// A status letter is a theme colour with nothing behind it but the row:
    /// the M of a modified file was 2.05:1 on rose-pine.
    #[test]
    fn a_status_letter_reads_on_every_background_a_row_can_have() {
        let mut letters = Measured::new("a status letter in its row", AA);
        for palette in palettes() {
            let sheet = Sheet::of(&palette);
            for (state, row) in rows(&sheet) {
                for class in LETTERS {
                    let text = sheet.solid(&format!(".om-status.{class}"), "color");
                    letters.add(&palette, &format!("{class}, {state}"), ratio(text, row));
                }
            }
        }
        letters.check();
    }

    /// Making everything the foreground colour would pass every test above.
    /// The colours are how a pill is told from the one beside it without
    /// reading it, so a hue that can be read is left exactly as the theme
    /// has it, and one that cannot is moved no further than it must be.
    #[test]
    fn a_hue_that_can_be_read_is_left_alone() {
        let retro = &palettes()[0];
        assert_eq!(retro.ink("#faa968", |_| true), "#faa968");
        assert_eq!(retro.ink("#028391", |_| false), retro.fg);

        let sheet = Sheet::of(retro);
        let colour = |rule: &str| sheet.value(rule, "color").to_string();
        assert_eq!(colour(".om-pill.branch"), retro.accent);
        assert_eq!(colour(".om-pill.ahead"), retro.cyan);
        assert_eq!(colour(".om-ref.tag"), retro.yellow);
        assert_eq!(colour(".om-status.modified"), retro.yellow);

        // "clean" is #028391 there, which was 2.82:1 on its fill. It is
        // moved, and stops short of the foreground.
        let clean = colour(".om-pill.clean");
        assert!(clean != retro.green && clean != retro.fg, "{clean}");
    }

    /// The figure the labels are fitted to, held against values that are
    /// common knowledge and against these tests' own arithmetic.
    #[test]
    fn contrast_is_the_wcag_ratio() {
        let (black, white) = ((0.0, 0.0, 0.0), (1.0, 1.0, 1.0));
        assert!((contrast(black, white) - 21.0).abs() < 1e-9);
        assert_eq!(contrast(white, white), 1.0);
        // #767676 is the lightest grey that passes on white, and the usual
        // example of it.
        assert!((contrast(Palette::rgb("#767676"), white) - 4.54).abs() < 0.005);
        assert!(contrast(Palette::rgb("#777777"), white) < 4.5);

        let (a, b) = (Palette::rgb("#e97b3c"), Palette::rgb("#0a2540"));
        assert_eq!(contrast(a, b), contrast(b, a));
        assert_eq!(contrast(a, b), ratio(a, b));
    }

    /// All fifteen sizes were in px, which GTK uses as written whatever the
    /// desktop's font size and text-scaling factor are. rem is the unit that
    /// follows both.
    #[test]
    fn text_sizes_follow_the_desktops_font() {
        let sheet = Sheet::of(&Palette::fallback());
        let sizes: Vec<_> =
            sheet.rules.iter().filter(|(_, property, _)| property == "font-size").collect();
        assert!(!sizes.is_empty());
        for (selector, _, size) in sizes {
            let rems = size.strip_suffix("rem").and_then(|number| number.parse::<f64>().ok());
            assert!(
                rems.is_some_and(|rems| (0.6..=1.6).contains(&rems)),
                "`{selector}` has a font-size of {size}"
            );
        }
    }

    /// The header's reset for buttons cleared the background of every one of
    /// them in every state, and a background is all that marks a flat toggle
    /// as on. The quick-tag chips, which are in the header too, were written
    /// as bare classes and lost most of their own rules to the same reset.
    /// Which rule wins is a matter of weight and then of order, so that is
    /// what is checked.
    #[test]
    fn a_toggle_in_the_header_shows_that_it_is_on() {
        let sheet = Sheet::of(&Palette::fallback());
        let place = |selector: &str| {
            sheet
                .rules
                .iter()
                .position(|(written, _, _)| written == selector)
                .unwrap_or_else(|| panic!("no rule for `{selector}`"))
        };
        let button = |state: &str| format!("window.omarepos headerbar button{state}");
        let chip = |state: &str| format!("window.omarepos headerbar button.om-quick-tag{state}");

        assert_eq!(sheet.colour(&button(""), "background").3, 0.0);
        assert!(sheet.colour(&button(":checked"), "background-color").3 > 0.0);
        assert!(weight(&button(":checked")) > weight(&button("")));
        // On weighs what under-the-pointer weighs, and the pointer is on a
        // toggle that has just been pressed: the later rule has to be on.
        assert_eq!(weight(&button(":checked")), weight(&button(":hover")));
        assert!(place(&button(":checked")) > place(&button(":hover")));

        for state in ["", ":hover", ":checked"] {
            assert!(weight(&chip(state)) > weight(&button(state)), "a chip{state}");
            // The class alone says the same, for a chip that is anywhere else.
            assert_eq!(
                sheet.value(&format!(".om-quick-tag{state}"), "background-color"),
                sheet.value(&chip(state), "background-color")
            );
        }
        assert_eq!(weight(&chip(":checked")), weight(&chip(":hover")));
        assert!(place(&chip(":checked")) > place(&chip(":hover")));
    }

    /// A chip's fill is under a tag's name, and a header button's is under
    /// its icon or its label. A chip that was on had 40% accent there, 3.16:1
    /// for the name on everforest.
    #[test]
    fn what_is_on_a_header_button_can_be_read_in_every_state() {
        let mut names = Measured::new("a tag's name on its chip", AA);
        let mut labels = Measured::new("a header button's label under the pointer", AA);
        // The toggles are icons. The one labelled button that is ever "on"
        // is the Tags menu, for as long as its menu is open in front of it.
        let mut icons = Measured::new("a header toggle's icon when it is on", ICON);
        for palette in palettes() {
            let sheet = Sheet::of(&palette);
            let bar = sheet.solid("window.omarepos headerbar", "background-color");
            let on = |rule: &str| over(sheet.colour(rule, "background-color"), bar);

            let chip = "window.omarepos headerbar button.om-quick-tag";
            let name = sheet.solid(chip, "color");
            let states = [("off", ""), ("under the pointer", ":hover"), ("on", ":checked")];
            for (state, pseudo) in states {
                names.add(&palette, state, ratio(name, on(&format!("{chip}{pseudo}"))));
            }

            let button = "window.omarepos headerbar button";
            let ink = sheet.solid(button, "color");
            labels.add(&palette, "", ratio(ink, on(&format!("{button}:hover"))));
            icons.add(&palette, "", ratio(ink, on(&format!("{button}:checked"))));
        }
        names.check();
        labels.check();
        icons.check();
    }
}
