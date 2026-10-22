//! Omarchy theme integration.
//!
//! Omarchy keeps the active theme behind a symlink at
//! `~/.local/state/omarchy/current/theme`, with the palette in `colors.toml`
//! and the theme's name in a sibling `theme.name`. We read that palette and
//! compile it into the GTK stylesheet, so omarepos repaints itself whenever the
//! user switches themes.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::home;

/// Whether the desktop asks for dark, which is what the built-in palette
/// goes by where there is no Omarchy theme. The window says so before it
/// loads a palette (`set_desktop_dark`); until then, and for whatever reads a
/// palette without a window, dark.
static DESKTOP_DARK: AtomicBool = AtomicBool::new(true);

pub fn set_desktop_dark(dark: bool) {
    DESKTOP_DARK.store(dark, Ordering::Relaxed);
}

/// Whether there is an Omarchy theme to follow: on another desktop there is
/// none, and the window follows the desktop's light or dark instead.
pub fn has_omarchy_theme() -> bool {
    state_dir().join("theme/colors.toml").is_file()
}

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

/// What a card's colour is shaded by under the pointer and under the
/// keyboard, and how much of the accent is mixed into a ticked card, at rest
/// and then under either. They are named for the reason the rows' are: what
/// a card writes in a hue is fitted to these backgrounds.
const CARD_LIT: f64 = 1.08;
const CARD_FOCUSED: f64 = 1.30;
const CARD_TICKED: f64 = 0.10;
const CARD_TICKED_LIT: f64 = 0.12;

/// How much of its own hue the fill of a coloured label carries.
const WASH: f64 = 0.16;

/// The contrast coloured text is fitted to. WCAG asks 4.5:1 of text this
/// small. The little over it is for what happens between here and the
/// screen, where GTK blends a see-through row or a card at 90% and rounds
/// the result to whole bytes.
const LEGIBLE: f64 = 4.6;
/// How much of the foreground small print keeps; the rest is the page.
const MUTED: f64 = 0.70;

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

/// GTK's `shade()`: the colour taken to hue, lightness and saturation, the
/// last two multiplied, and brought back.
fn shade((r, g, b): Rgb, factor: f64) -> Rgb {
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
    (channel(hue + 1.0 / 3.0), channel(hue), channel(hue - 1.0 / 3.0))
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
    /// The built-in dark palette: for a theme that leaves colours out, and,
    /// with `fallback_light`, for a desktop that is not Omarchy.
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

    /// The built-in light palette, for a light desktop with no Omarchy theme.
    /// It was the dark one whatever the desktop said, and omarepos the one
    /// dark window on a light GNOME or KDE. Coloured text on it is fitted to
    /// its backgrounds like any theme's (`ink`).
    pub fn fallback_light() -> Self {
        Self {
            name: "builtin".into(),
            dark: false,
            bg: "#fafafa".into(),
            bg_raised: "#ffffff".into(),
            bg_sunken: "#ebebed".into(),
            fg: "#1e2128".into(),
            fg_dim: "#5e636e".into(),
            border: "#d8dadf".into(),
            accent: "#3b6fd4".into(),
            red: "#b8303f".into(),
            yellow: "#b07a10".into(),
            green: "#3f8a2f".into(),
            blue: "#3b6fd4".into(),
            cyan: "#1d8189".into(),
            magenta: "#8656c9".into(),
        }
    }

    /// The built-in palette for a desktop that asks for dark, or not.
    pub fn builtin(dark: bool) -> Self {
        if dark { Self::fallback() } else { Self::fallback_light() }
    }

    /// Read the active Omarchy palette, or fall back to the built-in one for
    /// the desktop's light or dark (`set_desktop_dark`).
    pub fn load() -> Self {
        Self::load_for(DESKTOP_DARK.load(Ordering::Relaxed))
    }

    /// `load`, for a desktop that asks for dark or not.
    pub fn load_for(desktop_dark: bool) -> Self {
        let dir = state_dir();
        let name = std::fs::read_to_string(dir.join("theme.name"))
            // The name goes into the header's label, which takes no NUL.
            .map(|s| s.trim().replace('\0', ""))
            .ok()
            .filter(|s| !s.is_empty());
        let Ok(text) = std::fs::read_to_string(dir.join("theme/colors.toml")) else {
            let mut fb = Self::builtin(desktop_dark);
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
        let declared =
            ["mode", "theme_type"].into_iter().filter_map(word).find(|mode| !mode.is_empty());
        let dark = match (background.as_deref(), declared) {
            // Nothing of the theme's is behind the text, then: the window
            // gets the built-in background, which is dark whatever the theme
            // says of itself. A colors.toml that cannot be parsed ends up
            // here too, and may well have a `light.mode` beside it.
            (None, _) => true,
            (Some(_), Some(mode)) => mode != "light",
            (Some(_), None) if marked_light => false,
            (Some(background), None) => !is_light(background),
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
            // Small print is the foreground at seven tenths, which is how
            // Omarchy's own templates write muted text (`color-mix(in srgb,
            // foreground 70%, transparent)`), and not a colour of the
            // theme's. It was `light_foreground`, the nearest name a theme
            // has, and that is not a dimmer foreground in every theme: in
            // Tokyo Night it is the brighter one, and the lines meant to
            // recede stood out from the ones they were to recede behind.
            fg_dim: hex(mix(Self::rgb(&bg), Self::rgb(&fg), MUTED)),
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

        // The names of branches and tags in the graph. Each was a theme
        // colour written on an 18 to 30% wash of that same colour. A colour
        // is closer to a wash of itself than to the bare surface, and these
        // colours come from a terminal palette, which nobody chose for 10px
        // text on a tint: of the name and palette pairs tested below, 34 in
        // 45 were under 4.5:1 and the worst was 1.78.
        //
        // The fill still carries the hue, and the text as much of it as can
        // be read (`ink`). The fill is also opaque now, mixed here into the
        // page the name lies on, where it was a see-through `alpha()`. A row
        // changes colour when it is selected or under the pointer, and a
        // see-through fill changed with it, under text that did not.
        //
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
        .map(|(class, hue)| {
            let fill = whole(mix(page, Self::rgb(hue), WASH));
            let text = p.ink(hue, |ink| contrast(ink, fill) >= LEGIBLE);
            format!(".om-ref.{class} {{\n  background-color: {};\n  color: {text};\n}}", hex(fill))
        })
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
        // What a card writes in a hue has no fill either: yellow for work
        // that is not committed, the accent for commits that are nowhere
        // else, red for trouble. It has to be read on each colour a card
        // takes, and on a card that has stepped back, where it fades with
        // the card.
        let cards = [
            card,
            shade(card, CARD_LIT),
            shade(card, CARD_FOCUSED),
            mix(card, Self::rgb(&p.accent), CARD_TICKED),
            mix(card, Self::rgb(&p.accent), CARD_TICKED_LIT),
        ];
        let behind = |colour: Rgb| mix(page, colour, STEPPED_BACK);
        let on_a_card = |ink: Rgb| {
            cards.iter().all(|&lit| contrast(ink, lit) >= LEGIBLE)
                && contrast(behind(ink), behind(card)) >= LEGIBLE
        };
        let inks = [
            (".om-tree.work", &p.yellow),
            (".om-out,\n.om-arrow.out", &p.accent),
            (".om-bad,\n.om-tree.bad,\n.om-arrow.bad", &p.red),
        ]
        .map(|(written, hue)| format!("{written} {{\n  color: {};\n}}", p.ink(hue, on_a_card)))
        .join("\n\n");
        // The flag is the one fill on a card, and it is the theme's red as it
        // stands, which is what Omarchy's bar gives whatever calls attention
        // to itself. Its text is the card's own colour where that can be read
        // on the red, then another of the theme's plain colours, and black or
        // white where none of them can: nothing rose-pine has gets past
        // 3.84:1 there. Where nothing reaches the figure it is whichever
        // comes nearest.
        let red = Self::rgb(&p.red);
        let on_red = |text: Rgb| contrast(text, red).min(contrast(behind(text), behind(red)));
        let plain = [card, page, Self::rgb(&p.bg_sunken), fg, (0.0, 0.0, 0.0), (1.0, 1.0, 1.0)];
        let flag = plain.into_iter().find(|&text| on_red(text) >= LEGIBLE).unwrap_or_else(|| {
            let nearer = |a: Rgb, b: Rgb| if on_red(b) > on_red(a) { b } else { a };
            plain.into_iter().fold(card, nearer)
        });
        // What a page puts forward as the thing to do (the commit button,
        // the way to Settings on a window with nothing to list) is filled
        // with the theme's accent, which is what Omarchy gives the selected
        // and the primary. It was libadwaita's own blue in every theme. Its
        // text is found the way the flag's is.
        let accent = Self::rgb(&p.accent);
        let on_accent = |text: Rgb| contrast(text, accent);
        let primary = plain.into_iter().find(|&text| on_accent(text) >= LEGIBLE).unwrap_or_else(|| {
            let nearer = |a: Rgb, b: Rgb| if on_accent(b) > on_accent(a) { b } else { a };
            plain.into_iter().fold(page, nearer)
        });
        // The arcade (`ui::arcade`), a page of its own on the sunken colour.
        // Its table is written on that and on the row that is chosen, rank
        // by rank in five of the theme's colours, the way an arcade's
        // high-score table is.
        let sunken = Self::rgb(&p.bg_sunken);
        let chosen = whole(mix(sunken, fg, ROW_SELECTED));
        let on_the_table = |ink: Rgb| contrast(ink, sunken) >= LEGIBLE && contrast(ink, chosen) >= LEGIBLE;
        let ranks = [&p.red, &p.yellow, &p.green, &p.cyan, &p.magenta]
            .iter()
            .enumerate()
            .map(|(rank, hue)| {
                format!(".om-arcade .om-rank-{rank} {{\n  color: {};\n}}", p.ink(hue, on_the_table))
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let arcade = format!(
            r#"
.om-arcade {{
  background-color: @om_sunken;
  color: @om_fg;
  font-family: "Press Start 2P", monospace;
  font-size: 1rem;
}}

.om-arcade-small {{
  font-size: 0.75rem;
}}

.om-arcade-title {{
  font-size: 1.6rem;
  color: {title};
}}

.om-arcade-hi {{
  color: {gold};
}}

.om-arcade-dim,
.om-arcade-badge .om-arcade-badge-name {{
  color: {dim};
}}

{ranks}

.om-arcade-row {{
  padding: 6px 12px;
}}

.om-arcade-row.om-chosen {{
  background-color: {chosen};
}}

.om-arcade-tab {{
  padding: 8px 10px;
  border: 2px solid @om_border;
  color: {dim};
}}

.om-arcade-tab.om-on {{
  background-color: @om_accent;
  border-color: @om_accent;
  color: {primary};
}}

.om-arcade-bar {{
  min-height: 16px;
  background-image: repeating-linear-gradient(to right, {bar} 0px, {bar} 12px, transparent 12px, transparent 16px);
}}

.om-arcade-badge {{
  padding: 10px 12px;
  border: 2px solid @om_border;
}}

.om-arcade-badge.om-won {{
  border-color: {gold};
}}

.om-arcade-badge.om-won .om-arcade-badge-name {{
  color: {gold};
}}

.om-arcade-screen:focus,
.om-arcade-screen:focus-visible {{
  outline: none;
}}

/* Over the whole screen, and drawn on top of it. */
.om-arcade-scanlines {{
  background-image: repeating-linear-gradient(to bottom, alpha(black, 0.16) 0px, alpha(black, 0.16) 1px, transparent 1px, transparent 3px);
}}

/* GTK leaves animations out where the desktop asks for less motion. */
.om-arcade-blink {{
  animation: om-blink 1s linear infinite;
}}

@keyframes om-blink {{
  0% {{ opacity: 1; }}
  49% {{ opacity: 1; }}
  50% {{ opacity: 0; }}
  100% {{ opacity: 0; }}
}}
"#,
            title = p.ink(&p.magenta, on_the_table),
            gold = p.ink(&p.yellow, on_the_table),
            dim = hex(whole(mix(sunken, fg, MUTED))),
            chosen = hex(chosen),
            primary = hex(primary),
            bar = p.green,
        );
        // A card's left edge is the state of its repository: red for
        // trouble, yellow for work that is not committed, the accent for
        // commits that are nowhere else. It is 4px where the border is 1px,
        // and the padding gives the 3px back so that nothing on the card
        // moves.
        //
        // `border-color` is all four sides, and the rules that set it for the
        // keyboard, on the card it is on and on a card the pointer was left
        // on, outweigh a card's own two classes. An edge written with those
        // alone is recoloured with the rest of the border on the one card
        // that is being read. So it is written again at each of their
        // weights, with its own class on top.
        let edges = [("trouble", &p.red), ("work", &p.yellow), ("out", &p.accent)]
            .map(|(state, hue)| {
                let edge = format!(".om-card.om-edge-{state}");
                format!(
                    "{edge},\n\
                     flowbox.om-flow > flowboxchild:focus-visible {edge},\n\
                     flowbox.om-flow.om-keyboard {edge}:hover,\n\
                     flowbox.om-flow.om-keyboard {edge}.om-selected:hover {{\n  \
                     border-left: 4px solid {hue};\n  padding-left: 11px;\n}}"
                )
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
  background-color: shade(@om_raised, {card_lit});
}}

/* The ring and the check say a card is ticked. The fill only backs them up,
   because it goes under the card's text and the accent is whatever the theme
   says it is. At 0.22, and 0.38 on a ticked card with the focus, the name of
   the repository fell under 4.5:1 on half the stock themes; on kanagawa,
   whose accent is its foreground, the card was mixed toward the colour of
   its own text. */
.om-card.om-selected {{
  border-color: @om_accent;
  background-color: mix(@om_raised, @om_accent, {card_ticked});
  box-shadow: inset 0 0 0 2px @om_accent;
}}

.om-card.om-selected:hover {{
  background-color: mix(@om_raised, @om_accent, {card_ticked_lit});
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

/* A card that still shows what the last run read, until the scan has read
   its repository again: a second or two at launch, and it is not to be taken
   for the present. Faded further than the cards a selection steps back,
   which are current. */
.om-card.om-earlier {{
  opacity: 0.5;
}}

.om-card.om-selected checkbutton check {{
  background-color: @om_accent;
  border-color: @om_accent;
}}

.om-tag {{
  font-size: 0.75rem;
  font-weight: 700;
  padding: 1px 8px 1px 6px;
  border-radius: 999px;
  background-color: alpha(@om_fg, 0.10);
  color: @om_fg;
}}

/* The one chip among the tags that is not a tag: "new", on a repository
   found since the last look. It is an outline with nothing in it, so that it
   is not read as one of them, and the pixel of border comes off the padding
   to leave it the size of the others. */
.om-tag.new {{
  padding: 0 6px;
  background-color: transparent;
  border: 1px solid @om_dim;
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

{edges}

.om-name {{
  font-weight: 700;
  font-size: 1.02rem;
  color: @om_fg;
}}

.om-meta {{
  font-size: 0.75rem;
  color: @om_dim;
}}

.om-summary {{
  font-size: 0.82rem;
  color: @om_fg;
}}

/* A card is grey for as long as its repository wants nothing. Colour is
   kept for what is unfinished there and for trouble, and it is on the words
   themselves, with nothing behind them: the rules that give a hue come
   after these, fitted to the card. */
.om-where,
.om-who,
.om-caption {{
  font-size: 0.75rem;
  color: @om_dim;
}}

/* A detached HEAD is written as a branch is, for now. `.om-branch.detached`
   has no rule of its own, and is the name one would go by. */
.om-branch {{
  font-family: monospace;
  font-size: 0.82rem;
  font-weight: 600;
  color: @om_fg;
}}

.om-tree {{
  font-family: monospace;
  font-size: 0.82rem;
  font-weight: 700;
  color: @om_fg;
}}

/* A tree with nothing to count says so in a word, and a word goes back to
   the font of the text around it. */
.om-tree.word {{
  font-family: inherit;
  font-weight: 400;
  color: @om_dim;
}}

.om-out,
.om-in,
.om-bad {{
  font-size: 0.82rem;
  font-weight: 700;
}}

.om-in {{
  color: @om_fg;
}}

.om-arrow {{
  font-size: 0.82rem;
  color: @om_dim;
}}

{inks}

.om-age {{
  font-family: monospace;
  font-size: 0.75rem;
  color: @om_dim;
}}

.om-subject {{
  font-size: 0.82rem;
  color: @om_fg;
}}

.om-flag {{
  font-size: 0.75rem;
  font-weight: 700;
  padding: 0 6px;
  border-radius: 4px;
  background-color: {red};
  color: {flag};
}}

/* The one thing a page puts forward to be done. */
window.omarepos button.suggested-action {{
  background-color: {accent};
  color: {primary};
}}

/* What the card draws for itself it draws in the colour its widget has
   here, so a change of theme repaints it as it does a label, with nothing
   in the code to hear of it. */
.om-glyph {{
  color: @om_dim;
}}

.om-pulse {{
  color: @om_fg;
}}

separator.om-rule {{
  background-color: @om_border;
  min-height: 1px;
}}

/* A key that acts on the card, drawn as the cap of one. The letter is
   centred by its label: there is no property for it here. */
.om-key {{
  font-family: monospace;
  font-size: 0.75rem;
  color: @om_dim;
  border: 1px solid @om_border;
  border-radius: 3px;
  min-width: 14px;
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
  background-color: shade(@om_raised, {card_focused});
}}

/* While the keyboard is driving, hover says nothing useful — the pointer is
   wherever it was left — and a lit card beside the focused one reads as a
   second, weaker selection. */
flowbox.om-flow.om-keyboard .om-card:hover {{
  background-color: @om_raised;
  border-color: @om_border;
}}

flowbox.om-flow.om-keyboard .om-card.om-selected:hover {{
  background-color: mix(@om_raised, @om_accent, {card_ticked});
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
  background-color: mix(@om_raised, @om_accent, {card_ticked_lit});
}}

/* A card has two layers in the one place and shows one of them. At rest it
   is the short one, `om-less`. The long one, `om-more`, is shown where the
   card is being acted on: with `om-keys` on the card the keyboard is on,
   with `om-enter` on the filter's top hit while the filter is typed in, and
   by itself on the card under the pointer. They change places by opacity,
   which leaves the layout alone, so nothing in the grid moves when the
   keyboard or the pointer does.

   In this order. The keyboard is in the filter or on a card and never both,
   and should the rules meet all the same, they agree on the layer. */
.om-card .om-more,
.om-card .om-keys,
.om-card .om-enter {{
  opacity: 0;
}}

.om-card .om-less {{
  opacity: 1;
}}

flowbox.om-flow > flowboxchild:focus-visible .om-card .om-more,
flowbox.om-flow > flowboxchild:focus-visible .om-card .om-keys {{
  opacity: 1;
}}

flowbox.om-flow > flowboxchild:focus-visible .om-card .om-less {{
  opacity: 0;
}}

flowbox.om-flow.om-typing .om-card.om-top-hit .om-more,
flowbox.om-flow.om-typing .om-card.om-top-hit .om-enter {{
  opacity: 1;
}}

flowbox.om-flow.om-typing .om-card.om-top-hit .om-less {{
  opacity: 0;
}}

/* Under the pointer, once it is the pointer that is choosing a card: it has
   been moved, or the wheel turned, since the keyboard was last used, which is
   what `om-pointer` says. A pointer left where it was lies over some card
   every time the window opens, and that card stays as it is. No keys here:
   they go to the card the keyboard is on, wherever the pointer is. */
flowbox.om-flow.om-pointer .om-card:hover .om-more {{
  opacity: 1;
}}

flowbox.om-flow.om-pointer .om-card:hover .om-less {{
  opacity: 0;
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

/* The untracked group steps back from the ones above it: those are changes
   to files git tracks, which is what the view is usually being read for. It
   steps back as far as a card does and no further. The 0.70 written here
   was never drawn; it would have put the heading at 3.34:1 on rose-pine and
   the note beside it at 2.72. */
list.om-results > row.om-aside {{
  opacity: {stepped_back};
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

/* Setting an author aside. Shown on the row the pointer is on and to the
   keyboard, and on every row already set aside, which is where it is
   looked for to bring one back; a picture of the list shows none. */
button.om-stat-aside {{
  background: none;
  background-color: transparent;
  border: none;
  box-shadow: none;
  min-height: 0;
  min-width: 0;
  padding: 6px;
  color: @om_dim;
  opacity: 0;
}}

.om-stat-row:hover button.om-stat-aside,
.om-set-aside button.om-stat-aside,
button.om-stat-aside:focus-visible {{
  opacity: 1;
}}

button.om-stat-aside:hover {{
  color: @om_fg;
}}

/* The race (`ui::race`): names, and totals in figures of one width so that
   they do not shimmer as they count up, and the date large and quiet. */
.om-race-name {{
  color: @om_fg;
}}

.om-race-total {{
  color: @om_fg;
  font-feature-settings: "tnum";
}}

.om-race-date {{
  font-size: 1.6rem;
  font-weight: 700;
  color: @om_dim;
  font-feature-settings: "tnum";
}}

{arcade}

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
            red = p.red,
            flag = hex(flag),
            primary = hex(primary),
            stepped_back = STEPPED_BACK,
            card_lit = CARD_LIT,
            card_focused = CARD_FOCUSED,
            card_ticked = CARD_TICKED,
            card_ticked_lit = CARD_TICKED_LIT,
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
        all.push(Palette::fallback_light());
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
                // An animation's steps are rules inside a rule. They name no
                // element and set no colour, and are passed over whole.
                if let Some(after) = rest.strip_prefix("@keyframes") {
                    let mut depth = 0;
                    let end = after
                        .char_indices()
                        .find(|&(_, c)| {
                            depth += match c {
                                '{' => 1,
                                '}' => -1,
                                _ => 0,
                            };
                            c == '}' && depth == 0
                        })
                        .map(|(at, _)| at + 1)
                        .expect("an animation is closed");
                    rest = &after[end..];
                    continue;
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

        /// Where the first rule for exactly this selector stands in the
        /// sheet. Of two rules that weigh the same, the later one wins.
        fn place(&self, selector: &str) -> usize {
            self.rules
                .iter()
                .position(|(written, _, _)| written == selector)
                .unwrap_or_else(|| panic!("no rule for `{selector}`"))
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
                ("shade", [colour, by]) => {
                    let (r, g, b, alpha) = self.eval(colour);
                    let (r, g, b) = shade((r, g, b), factor(by));
                    (r, g, b, alpha)
                }
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

    /// What a card writes or draws in the foreground or in a hue.
    const WRITTEN: [&str; 11] = [
        ".om-branch", ".om-tree", ".om-tree.work", ".om-tree.bad", ".om-out", ".om-in",
        ".om-bad", ".om-arrow.out", ".om-arrow.bad", ".om-subject", ".om-pulse",
    ];
    /// And in the theme's dim.
    const MUTED: [&str; 8] = [
        ".om-where", ".om-tree.word", ".om-arrow", ".om-age", ".om-who", ".om-caption",
        ".om-key", ".om-glyph",
    ];
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

    /// Not on Omarchy there is no theme to follow, and the built-in palette
    /// is the desktop's light or dark one. Where there is a theme, the theme
    /// says, whatever the desktop asks.
    #[test]
    fn with_no_omarchy_theme_the_desktop_says_light_or_dark() {
        let _guard = isolate();
        let dir = install("background = \"#1a1b26\"\nforeground = \"#a9b1d6\"\n");
        assert!(has_omarchy_theme());
        assert!(Palette::load_for(false).dark, "the theme is dark, the desktop light");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!has_omarchy_theme());

        let light = Palette::load_for(false);
        assert!(!light.dark);
        assert_eq!(light.bg, Palette::fallback_light().bg);
        let dark = Palette::load_for(true);
        assert!(dark.dark);
        assert_eq!(dark.bg, Palette::fallback().bg);
        assert!(!light.stylesheet().contains(&Palette::fallback().bg));
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
        // Small print is the foreground at seven tenths on the page, here as
        // in every theme.
        assert_eq!(palette.fg_dim, "#7d8093");
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
            !Palette::resolve(&format!("mode = \"\"\ntheme_type = \"light\"\n{night}"), false)
                .dark,
            "an empty mode is no answer"
        );
    }

    /// The flag picks the colour scheme and the syntax colours that go on the
    /// window's background. When the theme supplies no background, because
    /// its colors.toml names none or cannot be parsed at all, that
    /// background is the built-in dark one, and a `light.mode` file or a
    /// `mode` saying otherwise would put light-theme text on it.
    #[test]
    fn a_theme_with_no_background_of_its_own_is_dark() {
        let _guard = isolate();
        let dir = install("background = #eff1f5 and this is not TOML\n");
        write(&dir.join("theme/light.mode"), "");

        let palette = Palette::load();
        assert_eq!(palette.bg, Palette::fallback().bg);
        assert!(palette.dark);

        assert!(Palette::resolve("mode = \"light\"\nforeground = \"#4c4f69\"\n", true).dark);
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
        // Not `light_fg`: small print is made from the foreground and the
        // page, whatever lighter foreground a theme names.
        assert_eq!(palette.fg_dim, "#acacac");
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
        assert_eq!([&palette.fg, &palette.fg_dim], ["#f6dcac", "#aea186"]);
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

    /// What a card has to say it writes straight on itself, in the foreground,
    /// in the theme's dim or in a hue. Nothing is behind the words but the
    /// card, and the card changes colour under them, so everything on it is
    /// measured on each colour it takes, and on a card that has stepped back.
    /// The hues are fitted to all of those: the theme's yellow as it stands
    /// would be 1.58:1 on a ticked flexoki-light card under the pointer.
    ///
    /// The one fill is the flag's, the theme's red as it stands, and what
    /// gives way there is the text.
    #[test]
    fn what_a_card_says_reads_whatever_state_the_card_is_in() {
        let mut written = Measured::new("what a card writes in the foreground or a hue", AA);
        let mut muted = Measured::new("what a card writes in the theme's dim", FLOOR);
        let mut flags = Measured::new("a flag's text on its red", AA);
        for palette in palettes() {
            let sheet = Sheet::of(&palette);
            let page = sheet.solid("flowbox.om-flow", "background-color");
            let back = sheet.number(STEPS_BACK, "opacity");
            // A card that has stepped back is a card at rest: the pointer and
            // the focus bring it forward again.
            let mut states: Vec<(&str, Solid, f64)> = CARD_STATES
                .iter()
                .map(|&(state, rule)| (state, sheet.solid(rule, "background-color"), 1.0))
                .collect();
            states.push(("stepped back", sheet.solid(".om-card", "background-color"), back));
            for (state, card, opacity) in states {
                let on_card = |rule: &str| {
                    let text = sheet.solid(rule, "color");
                    ratio(faded(text, opacity, page), faded(card, opacity, page))
                };
                for rule in WRITTEN {
                    written.add(&palette, &format!("{rule}, {state}"), on_card(rule));
                }
                for rule in MUTED {
                    muted.add(&palette, &format!("{rule}, {state}"), on_card(rule));
                }
            }

            let text = sheet.solid(".om-flag", "color");
            let red = sheet.solid(".om-flag", "background-color");
            flags.add(&palette, "", ratio(text, red));
            let seen = ratio(faded(text, back, page), faded(red, back, page));
            flags.add(&palette, "stepped back", seen);
        }
        written.check();
        muted.check();
        flags.check();
    }

    /// The button a page puts forward is the theme's accent, and what is
    /// written on it can be read there, in every stock theme.
    #[test]
    fn the_button_a_page_puts_forward_is_the_accent_and_can_be_read() {
        for palette in palettes() {
            let sheet = Sheet::of(&palette);
            let rule = "window.omarepos button.suggested-action";
            assert_eq!(sheet.value(rule, "background-color"), palette.accent, "{}", palette.name);
            let read = ratio(sheet.solid(rule, "color"), sheet.solid(rule, "background-color"));
            assert!(read >= 4.5, "{}: the button's text is {read:.2}:1 on its fill", palette.name);
        }
    }

    /// A card has two layers in one place and shows one of them: the short
    /// one at rest, and the long one where the keyboard is, or on the filter's
    /// top hit while the filter is typed in. And its left edge is its state,
    /// on the card the keyboard is on as on any other. Both come down to which
    /// rule wins, which is a matter of weight and then of order.
    #[test]
    fn the_card_under_the_keyboard_shows_its_other_layer_and_keeps_its_edge() {
        let palette = Palette::fallback();
        let sheet = Sheet::of(&palette);
        let focused = "flowbox.om-flow > flowboxchild:focus-visible .om-card";
        let top_hit = "flowbox.om-flow.om-typing .om-card.om-top-hit";
        let rank = |selector: &str| (weight(selector), sheet.place(selector));
        let shown = |card: &str, layer: &str| sheet.number(&format!("{card} {layer}"), "opacity");

        let at_rest = [".om-less", ".om-more", ".om-keys", ".om-enter"];
        assert_eq!(at_rest.map(|layer| shown(".om-card", layer)), [1.0, 0.0, 0.0, 0.0]);
        let with_the_focus = [".om-less", ".om-more", ".om-keys"];
        assert_eq!(with_the_focus.map(|layer| shown(focused, layer)), [0.0, 1.0, 1.0]);
        let as_top_hit = [".om-less", ".om-more", ".om-enter"];
        assert_eq!(as_top_hit.map(|layer| shown(top_hit, layer)), [0.0, 1.0, 1.0]);

        // Rest, the focus, the top hit, in that order. And of a layer the
        // focus rule sets, a rule that says otherwise has to lose to it, as
        // rest does. The top hit's would win, and agrees.
        let more = |card: &str| sheet.place(&format!("{card} .om-more"));
        assert!(more(".om-card") < more(focused) && more(focused) < more(top_hit));
        for layer in with_the_focus {
            let lit = format!("{focused} {layer}");
            for (selector, property, value) in &sheet.rules {
                if property == "opacity" && selector.ends_with(layer) {
                    let agrees = value == sheet.value(&lit, "opacity");
                    assert!(agrees || rank(selector) < rank(&lit), "`{selector}` undoes the focus");
                }
            }
        }

        // The card under a pointer that is choosing shows the long layer as
        // well, and none of the keys: those go to the card the keyboard is on.
        let pointed = "flowbox.om-flow.om-pointer .om-card:hover";
        let under_the_pointer = [".om-less", ".om-more"];
        assert_eq!(under_the_pointer.map(|layer| shown(pointed, layer)), [0.0, 1.0]);
        assert!(more(top_hit) < more(pointed));
        // And only such a pointer. One that was left over a card when the
        // window opened is over a card all the same, and `:hover` is all a
        // stylesheet knows of either: no rule may swap the layers for that.
        for (selector, property, _) in &sheet.rules {
            let layer = [".om-less", ".om-more", ".om-keys", ".om-enter"]
                .into_iter()
                .find(|layer| selector.ends_with(layer));
            if property == "opacity" && selector.contains(":hover") {
                if let Some(layer) = layer {
                    let asked = selector.contains(".om-pointer");
                    assert!(asked, "`{selector}` changes a card that is only lying under the pointer");
                    assert!(under_the_pointer.contains(&layer), "`{selector}` shows keys under the pointer");
                }
            }
        }

        // `border-color` is all four sides. A rule that sets it on a card has
        // to lose to the card's edge, or be written again with the edge's
        // class on the card, which then outweighs it: the focus rule is.
        let edges =
            [("trouble", &palette.red), ("work", &palette.yellow), ("out", &palette.accent)];
        for (state, hue) in edges {
            let edge = format!(".om-card.om-edge-{state}");
            assert_eq!(sheet.value(&edge, "border-left"), format!("4px solid {hue}"));
            let lit = format!("{focused}.om-edge-{state}");
            assert_eq!(sheet.value(&lit, "border-left"), sheet.value(&edge, "border-left"));
            for (rule, property, _) in &sheet.rules {
                let card = rule.rsplit(' ').next().unwrap_or_default();
                if property == "border-color" && card.contains(".om-card") {
                    let again = rule.replacen(".om-card", &edge, 1);
                    let kept =
                        sheet.rules.iter().any(|(s, p, _)| *s == again && p == "border-left");
                    assert!(kept || rank(rule) < rank(&edge), "`{rule}` recolours `{edge}`");
                }
            }
        }
    }

    /// The card that wore pills is gone, and the rules written for it go with
    /// it. A rule that nothing wears is still worked out and loaded at every
    /// change of theme, and reads as a class there is to use.
    #[test]
    fn no_rule_is_left_for_what_a_card_no_longer_wears() {
        let sheet = Sheet::of(&Palette::fallback());
        for gone in [".om-pill", ".om-path", ".om-dirty", ".om-error"] {
            let left = sheet.rules.iter().any(|(selector, _, _)| selector.contains(gone));
            assert!(!left, "a rule is left for `{gone}`");
        }
    }

    /// The names of branches and tags in the graph were each drawn in a theme
    /// colour on a wash of that colour, up to 30% of it: 1.78:1 for a tag on
    /// rose-pine.
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
    /// The colours are how work that is not committed is told from commits
    /// that are not pushed, and both from trouble, without reading a word, so
    /// a hue that can be read is left exactly as the theme has it, and one
    /// that cannot is moved no further than it must be.
    #[test]
    fn a_hue_that_can_be_read_is_left_alone() {
        let retro = &palettes()[0];
        assert_eq!(retro.ink("#faa968", |_| true), "#faa968");
        assert_eq!(retro.ink("#028391", |_| false), retro.fg);

        let sheet = Sheet::of(retro);
        let colour = |rule: &str| sheet.value(rule, "color").to_string();
        assert_eq!(colour(".om-out"), retro.accent);
        assert_eq!(colour(".om-ref.tag"), retro.yellow);
        assert_eq!(colour(".om-status.modified"), retro.yellow);
        // A flag's red is never moved. Its text is what is fitted.
        assert_eq!(sheet.value(".om-flag", "background-color"), retro.red);

        // The red is #f85525 there, which is 3.77:1 on a ticked card under
        // the pointer. It is moved, and stops short of the foreground.
        let bad = colour(".om-bad");
        assert!(bad != retro.red && bad != retro.fg, "{bad}");
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
        let place = |selector: &str| sheet.place(selector);
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
