//! Notification records, urgency, do-not-disturb, and body markup.
//!
//! Nothing in this module touches GTK or D-Bus, so the rules can be tested
//! without a session bus or a compositor.

use serde_json::Value;
use std::path::PathBuf;

pub const REASON_EXPIRED: u32 = 1;
pub const REASON_DISMISSED: u32 = 2;
pub const REASON_CLOSED: u32 = 3;

/// Nerd Font (Material) fallbacks used when a notification has no image.
/// Code points match JetBrainsMono Nerd Font 3.3.
pub const GLYPH_BELL: &str = "\u{f009a}";
pub const GLYPH_ALERT: &str = "\u{f0026}";
pub const GLYPH_BATTERY: &str = "\u{f007b}";
pub const GLYPH_MESSAGE: &str = "\u{f036a}";
pub const GLYPH_EMAIL: &str = "\u{f01ee}";
pub const GLYPH_MUSIC: &str = "\u{f075a}";
pub const GLYPH_WEB: &str = "\u{f059f}";
pub const GLYPH_TERMINAL: &str = "\u{f018d}";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Urgency {
    Low = 0,
    Normal = 1,
    Critical = 2,
}

impl Urgency {
    pub fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Low,
            2 => Self::Critical,
            _ => Self::Normal,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IconSource {
    Glyph(String),
    File(PathBuf),
    IconName(String),
    Rgba {
        width: i32,
        height: i32,
        rgba: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
pub struct Note {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub icon: IconSource,
    /// Drawn when `icon` is a theme name the icon theme does not have.
    pub glyph_fallback: String,
    pub actions: Vec<(String, String)>,
    pub has_default: bool,
    pub urgency: Urgency,
    pub battery: bool,
    /// `None` stays up until the user or a client dismisses it.
    pub expire_ms: Option<u64>,
    pub exec: Option<Vec<String>>,
    pub created_unix: i64,
}

/// Critical notifications, and the desktop's own action toasts, stay visible
/// while do-not-disturb is on. Everything else is dropped.
pub fn allows_popup(dnd: bool, app_name: &str, urgency: Urgency) -> bool {
    if !dnd {
        return true;
    }
    if urgency == Urgency::Critical {
        return true;
    }
    matches!(app_name, "r2-d2-action" | "k2so" | "den-den-mushi")
}

pub fn is_battery(category: &str, app: &str, summary: &str, body: &str) -> bool {
    let category = category.to_ascii_lowercase();
    if category.split('.').any(|part| part == "battery") {
        return true;
    }
    let app = app.to_ascii_lowercase();
    if app.contains("battery") || app == "upower" || app == "powerdevil" {
        return true;
    }
    let hay = format!("{summary} {body}").to_ascii_lowercase();
    hay.contains("low battery") || hay.contains("battery low") || hay.contains("battery is low")
}

pub fn expire_ms(
    requested: i32,
    urgency: Urgency,
    resident: bool,
    low_default: u64,
    normal_default: u64,
) -> Option<u64> {
    // Critical cards stay until the user or the sender dismisses them,
    // even when the client asked for a timeout.
    if urgency == Urgency::Critical || resident || requested == 0 {
        return None;
    }
    if requested > 0 {
        return Some(requested as u64);
    }
    match urgency {
        Urgency::Low => Some(low_default),
        Urgency::Normal => Some(normal_default),
        Urgency::Critical => None,
    }
}

/// GTK CSS has no `text-transform`, so the app line is uppercased here.
pub fn display_app(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        "NOTIFY".into()
    } else {
        name.to_uppercase()
    }
}

/// Freedesktop actions arrive as a flat list of key, label, key, label.
/// The `default` key is the left-click action and is not drawn as a button.
pub fn pair_actions(actions: &[String]) -> (Vec<(String, String)>, bool) {
    let mut pairs = Vec::new();
    let mut has_default = false;
    let mut index = 0;
    while index + 1 < actions.len() {
        let key = actions[index].clone();
        let label = actions[index + 1].clone();
        if key == "default" {
            has_default = true;
        } else if !key.is_empty() && !label.is_empty() {
            pairs.push((key, label));
        }
        index += 2;
    }
    (pairs, has_default)
}

pub fn glyph_for(
    app: &str,
    summary: &str,
    battery: bool,
    urgency: Urgency,
    hint: Option<&str>,
) -> String {
    if let Some(hint) = hint.map(str::trim).filter(|hint| !hint.is_empty()) {
        return hint.to_string();
    }
    if battery {
        return GLYPH_BATTERY.to_string();
    }
    let hay = format!("{app} {summary}").to_ascii_lowercase();
    if hay.contains("mail") || hay.contains("thunderbird") || hay.contains("geary") {
        return GLYPH_EMAIL.to_string();
    }
    if hay.contains("spotify") || hay.contains("music") || hay.contains("rhythmbox") {
        return GLYPH_MUSIC.to_string();
    }
    if hay.contains("firefox")
        || hay.contains("chromium")
        || hay.contains("brave")
        || hay.contains("chrome")
        || hay.contains("browser")
    {
        return GLYPH_WEB.to_string();
    }
    if hay.contains("message")
        || hay.contains("chat")
        || hay.contains("slack")
        || hay.contains("discord")
        || hay.contains("signal")
    {
        return GLYPH_MESSAGE.to_string();
    }
    if hay.contains("terminal")
        || hay.contains("alacritty")
        || hay.contains("kitty")
        || hay.contains("foot")
        || hay.contains("shell")
    {
        return GLYPH_TERMINAL.to_string();
    }
    if urgency == Urgency::Critical {
        return GLYPH_ALERT.to_string();
    }
    GLYPH_BELL.to_string()
}

/// `r2-d2-exec-argv` is a JSON array of strings. A leading-dash program is
/// rejected so the hint cannot be read as an option. This does not sandbox
/// the command: any session peer can set the hint.
pub fn parse_exec_argv(value: &str) -> Option<Vec<String>> {
    let parsed: Value = serde_json::from_str(value).ok()?;
    let array = parsed.as_array()?;
    if array.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(array.len());
    for item in array {
        out.push(item.as_str()?.to_string());
    }
    if out[0].is_empty() || out[0].starts_with('-') {
        return None;
    }
    Some(out)
}

/// Drop `<img>` tags without splicing the surrounding bytes into a new one.
///
/// A `<` opens a tag that runs to the next `>`. Only a tag whose own name is
/// `img` is removed. Deleting a shorter inner match would turn
/// `<im<img src="decoy">g src="beacon">` into a live image tag.
pub fn strip_image_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        let close = after
            .find('>')
            .map(|index| index + 1)
            .unwrap_or(after.len());
        let tag = &after[..close];
        if !is_image_tag(tag) {
            out.push_str(tag);
        }
        rest = &after[close..];
    }
    out.push_str(rest);
    out
}

fn is_image_tag(tag: &str) -> bool {
    let bytes = tag.as_bytes();
    if bytes.first() != Some(&b'<') {
        return false;
    }
    let mut index = 1;
    while index < bytes.len() && !bytes[index].is_ascii_alphanumeric() {
        index += 1;
    }
    let start = index;
    while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
        index += 1;
    }
    bytes[start..index].eq_ignore_ascii_case(b"img")
}

pub fn prepare_body(body: &str) -> String {
    let stripped = strip_image_tags(body);
    stripped
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<br>", "\n")
}

/// image-data is `(width, height, rowstride, has_alpha, bits, channels, bytes)`.
pub fn decode_image_data(
    width: i32,
    height: i32,
    rowstride: i32,
    has_alpha: bool,
    bits: i32,
    channels: i32,
    data: &[u8],
) -> Option<IconSource> {
    if width <= 0 || height <= 0 || width > 2048 || height > 2048 {
        return None;
    }
    if bits != 8 || rowstride <= 0 {
        return None;
    }
    let channels = if has_alpha { 4 } else { channels };
    if channels != 3 && channels != 4 {
        return None;
    }
    let stride = rowstride as usize;
    let need = stride.saturating_mul(height as usize);
    if data.len() < need || stride < (width as usize).saturating_mul(channels as usize) {
        return None;
    }
    let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for row in 0..height as usize {
        let start = row * stride;
        for col in 0..width as usize {
            let pixel = start + col * channels as usize;
            let red = *data.get(pixel)?;
            let green = *data.get(pixel + 1)?;
            let blue = *data.get(pixel + 2)?;
            let alpha = if channels == 4 {
                *data.get(pixel + 3)?
            } else {
                255
            };
            rgba.extend_from_slice(&[red, green, blue, alpha]);
        }
    }
    Some(IconSource::Rgba {
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dnd_keeps_critical_and_desktop_actions() {
        assert!(!allows_popup(true, "Spotify", Urgency::Normal));
        assert!(!allows_popup(true, "notify-send", Urgency::Low));
        assert!(allows_popup(true, "notify-send", Urgency::Critical));
        assert!(allows_popup(true, "r2-d2-action", Urgency::Low));
        assert!(allows_popup(true, "k2so", Urgency::Normal));
        assert!(allows_popup(false, "Spotify", Urgency::Low));
    }

    #[test]
    fn battery_category_and_wording() {
        assert!(is_battery("device.battery", "", "", ""));
        assert!(is_battery("device", "UPower", "", ""));
        assert!(is_battery("", "GNOME", "Low battery", "12% remaining"));
        assert!(!is_battery(
            "im",
            "Messages",
            "bring a battery",
            "for the lantern"
        ));
    }

    #[test]
    fn timeouts_follow_the_client_and_urgency() {
        assert_eq!(expire_ms(1500, Urgency::Critical, false, 4000, 5000), None);
        assert_eq!(expire_ms(-1, Urgency::Critical, false, 4000, 5000), None);
        assert_eq!(expire_ms(0, Urgency::Normal, false, 4000, 5000), None);
        assert_eq!(expire_ms(-1, Urgency::Normal, true, 4000, 5000), None);
        assert_eq!(expire_ms(-1, Urgency::Low, false, 4000, 5000), Some(4000));
        assert_eq!(
            expire_ms(2500, Urgency::Normal, false, 4000, 5000),
            Some(2500)
        );
    }

    #[test]
    fn app_line_is_uppercased_in_code() {
        assert_eq!(display_app("Spotify"), "SPOTIFY");
        assert_eq!(display_app("  "), "NOTIFY");
    }

    #[test]
    fn actions_split_and_hide_the_default_key() {
        let (pairs, has_default) = pair_actions(&[
            "default".into(),
            "Open".into(),
            "pay".into(),
            "Pay".into(),
            "decline".into(),
            "Decline".into(),
        ]);
        assert!(has_default);
        assert_eq!(
            pairs,
            vec![
                ("pay".into(), "Pay".into()),
                ("decline".into(), "Decline".into())
            ]
        );
    }

    #[test]
    fn image_tag_strip_does_not_manufacture_a_tag() {
        let hostile = "<im<img src=\"http://a/decoy.png\">g src=\"http://a/beacon.png\">";
        // The inner match stays inside the outer tag, so nothing new is spliced together.
        assert_eq!(strip_image_tags(hostile), hostile);
        assert_eq!(
            strip_image_tags("hello <img src=\"http://x\"> there"),
            "hello  there"
        );
        assert_eq!(strip_image_tags("<b>bold</b>"), "<b>bold</b>");
    }

    #[test]
    fn exec_argv_rejects_a_leading_dash() {
        assert_eq!(
            parse_exec_argv(r#"["notify-send","hi"]"#).as_deref(),
            Some(["notify-send".to_string(), "hi".to_string()].as_slice())
        );
        assert!(parse_exec_argv(r#"["-rf","/"]"#).is_none());
        assert!(parse_exec_argv("not json").is_none());
    }

    #[test]
    fn image_data_expands_rgb_rows() {
        let data = [1, 2, 3, 4, 5, 6, 9, 9];
        let IconSource::Rgba {
            width,
            height,
            rgba,
        } = decode_image_data(2, 1, 8, false, 8, 3, &data).unwrap()
        else {
            panic!("expected rgba");
        };
        assert_eq!((width, height), (2, 1));
        assert_eq!(rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }
}
