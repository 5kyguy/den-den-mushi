//! Per-app notification rules.
//!
//! A rule can mute popups, replace the timeout, or bypass do-not-disturb.
//! The daemon applies the rule when a notification arrives. The control
//! panel reads and writes the same rules through `Rules` and `SetRule`.
//!
//! Mute hides the popup and the notification is still recorded in history
//! (already dismissed, same as do-not-disturb), unless it is transient.
//! Mute wins over bypass. A timeout of `default` keeps the timeout the
//! daemon already computed from the sender and the urgency.

use crate::model::{allows_popup, Urgency};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const TIMEOUT_5S_MS: u64 = 5_000;
pub const TIMEOUT_10S_MS: u64 = 10_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleTimeout {
    #[default]
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "5s")]
    FiveSeconds,
    #[serde(rename = "10s")]
    TenSeconds,
    #[serde(rename = "sticky")]
    Sticky,
}

impl RuleTimeout {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::FiveSeconds => "5s",
            Self::TenSeconds => "10s",
            Self::Sticky => "sticky",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "default" => Self::Default,
            "5s" => Self::FiveSeconds,
            "10s" => Self::TenSeconds,
            "sticky" => Self::Sticky,
            _ => return None,
        })
    }

    pub fn index(self) -> u32 {
        match self {
            Self::Default => 0,
            Self::FiveSeconds => 1,
            Self::TenSeconds => 2,
            Self::Sticky => 3,
        }
    }

    pub fn from_index(index: u32) -> Option<Self> {
        Some(match index {
            0 => Self::Default,
            1 => Self::FiveSeconds,
            2 => Self::TenSeconds,
            3 => Self::Sticky,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRule {
    pub app: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub timeout: RuleTimeout,
    #[serde(default)]
    pub bypass_dnd: bool,
}

impl AppRule {
    /// A default rule changes nothing, so it is not stored.
    pub fn is_default(&self) -> bool {
        !self.muted && !self.bypass_dnd && self.timeout == RuleTimeout::Default
    }
}

/// Popup and timeout after the app's rule is applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub show_popup: bool,
    pub expire_ms: Option<u64>,
}

/// Apply `rule` on top of do-not-disturb and the timeout already chosen.
///
/// `expire_ms` is that earlier choice (`expire_ms` in the daemon). `default`
/// keeps it. `5s`, `10s`, and `sticky` replace it, including for a critical
/// notification. A muted app does not pop up even when it bypasses
/// do-not-disturb or the notification is critical.
pub fn apply_rule(
    dnd: bool,
    app_name: &str,
    urgency: Urgency,
    expire_ms: Option<u64>,
    rule: Option<&AppRule>,
) -> Delivery {
    let muted = rule.is_some_and(|rule| rule.muted);
    let bypass = rule.is_some_and(|rule| rule.bypass_dnd);
    let show_popup = if muted {
        false
    } else if bypass {
        true
    } else {
        allows_popup(dnd, app_name, urgency)
    };
    let expire_ms = match rule.map(|rule| rule.timeout) {
        Some(RuleTimeout::FiveSeconds) => Some(TIMEOUT_5S_MS),
        Some(RuleTimeout::TenSeconds) => Some(TIMEOUT_10S_MS),
        Some(RuleTimeout::Sticky) => None,
        Some(RuleTimeout::Default) | None => expire_ms,
    };
    Delivery {
        show_popup,
        expire_ms,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleSet {
    rules: Vec<AppRule>,
}

impl RuleSet {
    pub fn new(rules: Vec<AppRule>) -> Self {
        Self { rules }
    }

    pub fn get(&self, app: &str) -> Option<&AppRule> {
        self.rules.iter().find(|rule| rule.app == app)
    }

    pub fn rules(&self) -> &[AppRule] {
        &self.rules
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.rules).unwrap_or_else(|_| "[]".to_string())
    }

    /// Insert, replace, or drop `rule`. A fully default rule is removed.
    /// Returns whether the saved list changed.
    pub fn set(&mut self, rule: AppRule) -> bool {
        if rule.app.trim().is_empty() {
            return false;
        }
        let default = rule.is_default();
        if let Some(index) = self
            .rules
            .iter()
            .position(|existing| existing.app == rule.app)
        {
            if default {
                self.rules.remove(index);
                return true;
            }
            if self.rules[index] == rule {
                return false;
            }
            self.rules[index] = rule;
            return true;
        }
        if default {
            return false;
        }
        self.rules.push(rule);
        true
    }
}

pub fn rules_from_json(json: &str) -> Result<Vec<AppRule>, String> {
    serde_json::from_str(json).map_err(|error| format!("rules response was not a list ({error})"))
}

#[derive(Serialize)]
struct RulesDocument<'a> {
    rules: &'a [AppRule],
}

#[derive(Deserialize)]
struct RulesDocumentOwned {
    #[serde(default)]
    rules: Vec<AppRule>,
}

/// Rules the daemon should apply.
///
/// `config.toml` is never written. `[[rules]]` there are defaults, and only
/// when `rules.toml` is missing. A present `rules.toml` is the whole list,
/// including an empty one, so a cleared rule does not come back from the
/// defaults.
pub fn load_rules(defaults: &[AppRule], path: &Path) -> Vec<AppRule> {
    if path.as_os_str().is_empty() {
        return defaults.to_vec();
    }
    match fs::read_to_string(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => defaults.to_vec(),
        Err(error) => {
            eprintln!("den-den-mushi: ignoring rules.toml ({error})");
            defaults.to_vec()
        }
        Ok(text) => match parse_rules_file(&text) {
            Ok(rules) => rules,
            Err(error) => {
                eprintln!("den-den-mushi: ignoring rules.toml ({error})");
                defaults.to_vec()
            }
        },
    }
}

pub fn parse_rules_file(text: &str) -> Result<Vec<AppRule>, String> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let document: RulesDocumentOwned =
        toml::from_str(text).map_err(|error| format!("rules.toml could not be read ({error})"))?;
    Ok(document.rules)
}

/// Write `rules.toml` by renaming a temp file in the same directory.
///
/// This does not open or replace `config.toml`. An empty path is a no-op.
/// The file mode is `0600` and the directory mode is `0700`, same as
/// `history.json`.
pub fn store_rules(path: &Path, rules: &[AppRule]) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    let mut text = toml::to_string_pretty(&RulesDocument { rules })
        .map_err(|error| format!("could not encode rules ({error})"))?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    write_private(path, text.as_bytes()).map_err(|error| error.to_string())
}

/// Readers see the previous file or the new file. A crash during the write
/// leaves the previous file in place.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    prepare_parent(path)?;
    let tmp = temp_path(path);
    if let Err(error) = write_temp(&tmp, bytes) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    fs::rename(&tmp, path)?;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

fn write_temp(tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn prepare_parent(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn temp_path(path: &Path) -> std::path::PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("rules.toml");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let tmp_name = format!(".{name}.{}.{nanos}.tmp", std::process::id());
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => std::path::PathBuf::from(tmp_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn rule(app: &str, muted: bool, timeout: RuleTimeout, bypass_dnd: bool) -> AppRule {
        AppRule {
            app: app.into(),
            muted,
            timeout,
            bypass_dnd,
        }
    }

    #[test]
    fn timeout_names_round_trip() {
        for timeout in [
            RuleTimeout::Default,
            RuleTimeout::FiveSeconds,
            RuleTimeout::TenSeconds,
            RuleTimeout::Sticky,
        ] {
            assert_eq!(RuleTimeout::parse(timeout.as_str()), Some(timeout));
            assert_eq!(RuleTimeout::from_index(timeout.index()), Some(timeout));
        }
        assert!(RuleTimeout::parse("1s").is_none());
        assert!(RuleTimeout::from_index(4).is_none());
    }

    #[test]
    fn mute_hides_the_popup_and_wins_over_bypass_and_critical() {
        let muted = rule("Spotify", true, RuleTimeout::FiveSeconds, true);
        let hidden = apply_rule(
            false,
            "Spotify",
            Urgency::Critical,
            Some(1500),
            Some(&muted),
        );
        assert!(!hidden.show_popup);
        // The timeout is still reported; the daemon does not start it
        // because the popup is not shown.
        assert_eq!(hidden.expire_ms, Some(TIMEOUT_5S_MS));

        let plain = apply_rule(true, "Spotify", Urgency::Normal, Some(1500), None);
        assert!(!plain.show_popup);
        assert_eq!(plain.expire_ms, Some(1500));
    }

    #[test]
    fn bypass_shows_a_normal_notification_while_dnd_is_on() {
        let bypass = rule("Mail", false, RuleTimeout::Default, true);
        let shown = apply_rule(true, "Mail", Urgency::Low, Some(4_000), Some(&bypass));
        assert!(shown.show_popup);
        assert_eq!(shown.expire_ms, Some(4_000));

        let off = apply_rule(false, "Mail", Urgency::Normal, Some(2_500), Some(&bypass));
        assert!(off.show_popup);
    }

    #[test]
    fn timeout_override_replaces_the_computed_timeout() {
        let five = rule("Timer", false, RuleTimeout::FiveSeconds, false);
        assert_eq!(
            apply_rule(false, "Timer", Urgency::Normal, Some(1_500), Some(&five)).expire_ms,
            Some(TIMEOUT_5S_MS)
        );
        let ten = rule("Timer", false, RuleTimeout::TenSeconds, false);
        assert_eq!(
            apply_rule(false, "Timer", Urgency::Critical, None, Some(&ten)).expire_ms,
            Some(TIMEOUT_10S_MS)
        );
        let sticky = rule("Timer", false, RuleTimeout::Sticky, false);
        assert_eq!(
            apply_rule(false, "Timer", Urgency::Normal, Some(1_500), Some(&sticky)).expire_ms,
            None
        );
        let keep = rule("Timer", false, RuleTimeout::Default, false);
        assert_eq!(
            apply_rule(false, "Timer", Urgency::Critical, None, Some(&keep)),
            Delivery {
                show_popup: true,
                expire_ms: None,
            }
        );
        assert_eq!(
            apply_rule(false, "Timer", Urgency::Normal, Some(2_500), Some(&keep)).expire_ms,
            Some(2_500)
        );
    }

    #[test]
    fn dnd_without_a_rule_still_keeps_critical_and_desktop_actions() {
        assert!(!apply_rule(true, "Spotify", Urgency::Normal, Some(5_000), None).show_popup);
        assert!(apply_rule(true, "Spotify", Urgency::Critical, None, None).show_popup);
        assert!(apply_rule(true, "r2-d2-action", Urgency::Low, Some(4_000), None).show_popup);
        assert!(apply_rule(false, "Spotify", Urgency::Low, Some(4_000), None).show_popup);
    }

    #[test]
    fn default_rules_are_not_stored_and_updates_replace() {
        let mut set = RuleSet::new(Vec::new());
        assert!(!set.set(rule("Mail", false, RuleTimeout::Default, false)));
        assert!(set.rules().is_empty());
        assert!(set.set(rule("Spotify", true, RuleTimeout::FiveSeconds, false)));
        assert!(!set.set(rule("Spotify", true, RuleTimeout::FiveSeconds, false)));
        assert!(set.set(rule("Spotify", true, RuleTimeout::TenSeconds, true)));
        assert_eq!(set.rules().len(), 1);
        assert!(set.rules()[0].bypass_dnd);
        assert_eq!(set.rules()[0].timeout, RuleTimeout::TenSeconds);
        assert!(set.set(rule("Spotify", false, RuleTimeout::Default, false)));
        assert!(set.rules().is_empty());
        assert!(!set.set(rule("  ", true, RuleTimeout::Sticky, false)));
    }

    #[test]
    fn rules_json_round_trips() {
        let set = RuleSet::new(vec![rule("Spotify", true, RuleTimeout::Sticky, true)]);
        let parsed = rules_from_json(&set.to_json()).unwrap();
        assert_eq!(parsed, set.rules());
        assert!(rules_from_json("nope").is_err());
    }

    #[test]
    fn rules_toml_wins_over_config_defaults_and_is_private() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "den-den-mushi-rules-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let rules_path = dir.join("rules.toml");
        let defaults = vec![rule("Mail", true, RuleTimeout::TenSeconds, false)];

        assert_eq!(load_rules(&defaults, &rules_path), defaults);

        store_rules(
            &rules_path,
            &[rule("Spotify", true, RuleTimeout::FiveSeconds, false)],
        )
        .unwrap();
        let mode = fs::metadata(&rules_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let loaded = load_rules(&defaults, &rules_path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].app, "Spotify");
        assert!(loaded[0].muted);
        assert_eq!(loaded[0].timeout, RuleTimeout::FiveSeconds);

        // An empty file is a saved empty list, not a missing file.
        store_rules(&rules_path, &[]).unwrap();
        assert!(load_rules(&defaults, &rules_path).is_empty());

        let _ = fs::remove_dir_all(&dir);
    }
}
