//! Palette loading.
//!
//! Colors come from the r2-d2 template (`{{colors.<role>.default.hex}}`),
//! from a rendered stylesheet, and from the live `theme.json` document.
//! The standalone fallback is the dark half of the White accent, which is
//! what r2-d2 uses when a wallpaper cannot be read.

use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const TEMPLATE_CSS: &str = include_str!("../contrib/r2-d2/den-den-mushi.css");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    pub bg: String,
    pub surface: String,
    pub line: String,
    pub accent: String,
    pub on_accent: String,
    pub text: String,
    pub muted: String,
    pub danger: String,
    pub on_danger: String,
    pub warning: String,
    pub on_warning: String,
}

impl Palette {
    /// Dark roles for the White accent in `palette.toml`.
    pub fn fallback_dark() -> Self {
        Self {
            bg: "#0c0c0c".into(),
            surface: "#181818".into(),
            line: "#4a4a4a".into(),
            accent: "#ffffff".into(),
            on_accent: "#0a0a0a".into(),
            text: "#f7f7f7".into(),
            muted: "#a3a3a3".into(),
            danger: "#ff2a4a".into(),
            on_danger: "#2a0008".into(),
            warning: "#ffb000".into(),
            on_warning: "#2a2100".into(),
        }
    }

    pub fn get(&self, role: &str) -> Option<&str> {
        Some(match role {
            "bg" => self.bg.as_str(),
            "surface" => self.surface.as_str(),
            "line" => self.line.as_str(),
            "accent" => self.accent.as_str(),
            "on_accent" => self.on_accent.as_str(),
            "text" => self.text.as_str(),
            "muted" => self.muted.as_str(),
            "danger" => self.danger.as_str(),
            "on_danger" => self.on_danger.as_str(),
            "warning" => self.warning.as_str(),
            "on_warning" => self.on_warning.as_str(),
            _ => return None,
        })
    }

    fn set(&mut self, role: &str, hex: String) {
        let slot = match role {
            "bg" => &mut self.bg,
            "surface" => &mut self.surface,
            "line" => &mut self.line,
            "accent" => &mut self.accent,
            "on_accent" => &mut self.on_accent,
            "text" => &mut self.text,
            "muted" => &mut self.muted,
            "danger" => &mut self.danger,
            "on_danger" => &mut self.on_danger,
            "warning" => &mut self.warning,
            "on_warning" => &mut self.on_warning,
            _ => return,
        };
        *slot = hex;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub css: String,
    pub source: String,
}

pub fn config_dir() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", ".config").join("den-den-mushi")
}

pub fn state_dir() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("den-den-mushi")
}

pub fn dnd_path() -> PathBuf {
    state_dir().join("dnd")
}

pub fn history_path() -> PathBuf {
    state_dir().join("history.json")
}

/// Default number of history entries. `history_max` in config.toml overrides it.
pub const DEFAULT_HISTORY_MAX: usize = 100;
/// Largest `history_max` that is honored. A larger config value is clamped.
pub const HISTORY_MAX_LIMIT: usize = 10_000;

pub fn style_paths() -> Vec<PathBuf> {
    let mut paths = vec![config_dir().join("style.css")];
    if let Some(root) = std::env::var_os("R2D2_PATH") {
        paths.push(PathBuf::from(root).join("config/den-den-mushi/style.css"));
    }
    paths
}

pub fn theme_json_path() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", ".local/state").join("r2-d2/theme.json")
}

fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    if let Some(value) = std::env::var_os(var) {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    home_dir().join(fallback)
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn read_dnd() -> bool {
    fs::read_to_string(dnd_path())
        .map(|text| {
            let text = text.trim();
            text == "1" || text.eq_ignore_ascii_case("on") || text.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

pub fn write_dnd(on: bool) -> std::io::Result<()> {
    let path = dnd_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, if on { "1\n" } else { "0\n" })
}

#[derive(Debug, Default, Deserialize)]
pub struct FileConfig {
    pub margin: Option<i32>,
    pub namespace: Option<String>,
    pub timeout_low_ms: Option<u64>,
    pub timeout_normal_ms: Option<u64>,
    pub history_max: Option<u64>,
    pub history_persist: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub margin: Option<i32>,
    pub namespace: String,
    pub timeout_low_ms: u64,
    pub timeout_normal_ms: u64,
    pub history_max: usize,
    /// When false, history stays in memory and the state file is not touched.
    pub history_persist: bool,
}

impl Settings {
    pub fn load() -> Self {
        let file = config_dir().join("config.toml");
        let parsed = fs::read_to_string(&file)
            .ok()
            .and_then(|text| toml::from_str::<FileConfig>(&text).ok())
            .unwrap_or_default();
        Self::from_config(parsed)
    }

    pub fn from_config(parsed: FileConfig) -> Self {
        Self {
            margin: parsed.margin,
            namespace: parsed
                .namespace
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "r2-d2-notifications".into()),
            timeout_low_ms: parsed.timeout_low_ms.unwrap_or(4_000).max(1),
            timeout_normal_ms: parsed.timeout_normal_ms.unwrap_or(5_000).max(1),
            history_max: clamp_history_max(parsed.history_max),
            history_persist: parsed.history_persist.unwrap_or(true),
        }
    }
}

fn clamp_history_max(value: Option<u64>) -> usize {
    value
        .unwrap_or(DEFAULT_HISTORY_MAX as u64)
        .min(HISTORY_MAX_LIMIT as u64) as usize
}

pub fn load_theme() -> Theme {
    let mut palette = Palette::fallback_dark();
    let mut source = String::from("fallback");
    if let Some(live) = read_theme_json(&theme_json_path()) {
        palette = live;
        source = "theme.json".into();
    }

    let mut raw = TEMPLATE_CSS.to_string();
    let mut from_file = false;
    for path in style_paths() {
        if let Ok(text) = fs::read_to_string(&path) {
            if !text.trim().is_empty() {
                raw = text;
                from_file = true;
                source = if source == "theme.json" {
                    format!("{}+theme.json", display_path(&path))
                } else {
                    display_path(&path)
                };
                break;
            }
        }
    }

    // A rendered stylesheet already has hex in it. theme.json still wins when
    // it is newer, so a wallpaper change recolors the daemon even if the
    // stylesheet copy in the sync list has not been wired up yet.
    let json_newer = from_file && theme_json_is_newer_than_style();
    let mut css = render_placeholders(&raw, &palette);
    if source.contains("theme.json") || json_newer || !from_file {
        css = apply_define_colors(&css, &palette);
        if from_file && json_newer && !source.contains("theme.json") {
            source = format!("{source}+theme.json");
        }
    }
    if css.contains("{{") {
        eprintln!("den-den-mushi: stylesheet still has an unfilled placeholder");
    }
    Theme { css, source }
}

fn theme_json_is_newer_than_style() -> bool {
    let json_time = mtime(&theme_json_path());
    let style_time = style_paths().into_iter().find_map(|path| mtime(&path));
    match (json_time, style_time) {
        (Some(json), Some(style)) => json >= style,
        (Some(_), None) => true,
        _ => false,
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn render_placeholders(css: &str, palette: &Palette) -> String {
    let mut out = css.to_string();
    for role in ROLES {
        if let Some(hex) = palette.get(role) {
            let token = format!("{{{{colors.{role}.default.hex}}}}");
            out = out.replace(&token, hex);
        }
    }
    out
}

pub fn apply_define_colors(css: &str, palette: &Palette) -> String {
    css.lines()
        .map(|line| {
            let trimmed = line.trim();
            let Some(rest) = trimmed.strip_prefix("@define-color ") else {
                return line.to_string();
            };
            let name = rest.split_whitespace().next().unwrap_or("");
            match palette.get(name) {
                Some(hex) => format!("@define-color {name} {hex};"),
                None => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const ROLES: &[&str] = &[
    "bg",
    "surface",
    "line",
    "accent",
    "on_accent",
    "text",
    "muted",
    "danger",
    "on_danger",
    "warning",
    "on_warning",
];

fn read_theme_json(path: &Path) -> Option<Palette> {
    let text = fs::read_to_string(path).ok()?;
    parse_theme_json(&text)
}

pub fn parse_theme_json(text: &str) -> Option<Palette> {
    let value: Value = serde_json::from_str(text).ok()?;
    let dark = value.get("colors")?.get("dark")?;
    let mut palette = Palette::fallback_dark();
    let mut saw = false;
    for role in ROLES {
        if let Some(hex) = dark.get(*role).and_then(|item| item.as_str()) {
            if let Some(normalized) = normalize_hex(hex) {
                palette.set(role, normalized);
                saw = true;
            }
        }
    }
    saw.then_some(palette)
}

fn normalize_hex(value: &str) -> Option<String> {
    let value = value.trim();
    if value.len() != 7 || !value.starts_with('#') {
        return None;
    }
    if !value.as_bytes()[1..]
        .iter()
        .all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_placeholders_fill_with_the_dark_fallback() {
        let filled = render_placeholders(TEMPLATE_CSS, &Palette::fallback_dark());
        assert!(!filled.contains("{{"));
        assert!(filled.contains("#0c0c0c"));
        assert!(filled.contains("#ff2a4a"));
        assert!(filled.contains("#ffb000"));
        assert!(filled.contains("JetBrainsMono Nerd Font"));
    }

    #[test]
    fn theme_json_dark_roles_override_fallback() {
        let json = r##"{
            "colors": {
                "dark": {
                    "accent": "#00E5FF",
                    "on_accent": "#042028",
                    "bg": "#061216",
                    "surface": "#0E1E26",
                    "text": "#E7FBFF",
                    "muted": "#7EC8D4",
                    "line": "#1E6A78"
                },
                "light": { "accent": "#007A96" }
            }
        }"##;
        let palette = parse_theme_json(json).unwrap();
        assert_eq!(palette.accent, "#00e5ff");
        assert_eq!(palette.bg, "#061216");
        assert_eq!(palette.danger, "#ff2a4a");
        let css = apply_define_colors(&render_placeholders(TEMPLATE_CSS, &palette), &palette);
        assert!(css.contains("@define-color accent #00e5ff;"));
        assert!(!css.contains("{{"));
    }

    #[test]
    fn broken_theme_json_is_ignored() {
        assert!(parse_theme_json("{").is_none());
        assert!(parse_theme_json(r#"{"colors":{"dark":{"accent":"blue"}}}"#).is_none());
    }

    #[test]
    fn history_settings_default_to_a_private_capped_log() {
        let defaults = Settings::from_config(FileConfig::default());
        assert_eq!(defaults.history_max, DEFAULT_HISTORY_MAX);
        assert!(defaults.history_persist);
        assert_eq!(defaults.timeout_normal_ms, 5_000);

        let custom: FileConfig =
            toml::from_str("history_max = 25\nhistory_persist = false\ntimeout_low_ms = 1500\n")
                .unwrap();
        let settings = Settings::from_config(custom);
        assert_eq!(settings.history_max, 25);
        assert!(!settings.history_persist);
        assert_eq!(settings.timeout_low_ms, 1_500);

        let zero: FileConfig = toml::from_str("history_max = 0\n").unwrap();
        assert_eq!(Settings::from_config(zero).history_max, 0);

        let huge: FileConfig = toml::from_str("history_max = 999999\n").unwrap();
        assert_eq!(Settings::from_config(huge).history_max, HISTORY_MAX_LIMIT);
    }
}
