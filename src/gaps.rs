//! Screen insets from Hyprland.
//!
//! Layer-shell anchors sit on the monitor edge, so the stack is pushed in by
//! the full `general:gaps_out` (the same outer gap tiled windows clear) and
//! by the monitor's reserved top (the bar's exclusive zone). The 8px gap
//! between cards lives in the stylesheet, not here.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insets {
    pub top: i32,
    pub right: i32,
}

pub fn resolve_insets(
    margin_override: Option<i32>,
    gaps_json: Option<&str>,
    monitors_json: Option<&str>,
) -> Insets {
    let auto = gaps_json.and_then(parse_gaps_out).unwrap_or(8);
    let margin = margin_override.unwrap_or(auto).max(0);
    let reserved = monitors_json
        .and_then(parse_top_reserved)
        .unwrap_or(0)
        .max(0);
    Insets {
        top: reserved.saturating_add(margin),
        right: margin,
    }
}

/// First number of `general:gaps_out`, used whole so the cards clear the
/// outer gap instead of sitting halfway into it.
pub fn parse_gaps_out(json: &str) -> Option<i32> {
    let value: Value = serde_json::from_str(json).ok()?;
    let full = if let Some(css) = value.get("css").and_then(|item| item.as_str()) {
        first_number(css)?
    } else {
        value.get("int")?.as_i64()? as i32
    };
    Some(full.max(0))
}

/// `hyprctl monitors -j` reports `reserved` as top, bottom, left, right.
pub fn parse_top_reserved(json: &str) -> Option<i32> {
    let value: Value = serde_json::from_str(json).ok()?;
    let monitors = value.as_array()?;
    let mut best: Option<i32> = None;
    for monitor in monitors {
        let top = monitor.get("reserved")?.as_array()?.first()?.as_i64()? as i32;
        let focused = monitor
            .get("focused")
            .and_then(|item| item.as_bool())
            .unwrap_or(false);
        if focused {
            return Some(top.max(0));
        }
        best = Some(best.map_or(top, |current| current.max(top)));
    }
    best.map(|value| value.max(0))
}

fn first_number(text: &str) -> Option<i32> {
    let mut digits = String::new();
    for character in text.chars() {
        if character.is_ascii_digit() || (digits.is_empty() && character == '-') {
            digits.push(character);
        } else if !digits.is_empty() {
            break;
        }
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outer_gap_and_reserved_top_are_added() {
        let gaps = r#"{"option":"general:gaps_out","int":2,"css":"2"}"#;
        let monitors = r#"[{"name":"eDP-1","focused":false,"reserved":[0,0,0,0]},{"name":"DP-1","focused":true,"reserved":[26,0,0,0]}]"#;
        let insets = resolve_insets(None, Some(gaps), Some(monitors));
        assert_eq!(insets, Insets { top: 28, right: 2 });
    }

    #[test]
    fn missing_hyprland_uses_an_eight_pixel_inset() {
        assert_eq!(
            resolve_insets(None, None, None),
            Insets { top: 8, right: 8 }
        );
    }

    #[test]
    fn margin_override_replaces_the_gap_but_keeps_the_bar() {
        let monitors = r#"[{"focused":true,"reserved":[30,0,0,0]}]"#;
        let insets = resolve_insets(Some(8), Some(r#"{"int":40}"#), Some(monitors));
        assert_eq!(insets, Insets { top: 38, right: 8 });
    }
}
