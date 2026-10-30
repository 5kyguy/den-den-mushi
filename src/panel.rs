//! Pure panel list logic: search, app groups, and the rules rows.
//!
//! The widgets call these and then `History`, `InvokeHistory`,
//! `RemoveHistory`, and `SetRule`. Nothing here talks to GTK or D-Bus.

use crate::history::{HistoryEntry, HistoryIcon, HistoryIconKind};
use crate::model::{glyph_for, Urgency};
use crate::rules::{AppRule, RuleTimeout};

/// Case-insensitive match on the app name, the title, and the body.
///
/// A blank query keeps every entry. The icon name and the action keys are
/// not part of the search.
pub fn entry_matches(entry: &HistoryEntry, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let needle = query.to_lowercase();
    contains_ignore_case(&entry.app_name, &needle)
        || contains_ignore_case(&entry.summary, &needle)
        || contains_ignore_case(&entry.body, &needle)
}

pub fn filter_history(entries: &[HistoryEntry], query: &str) -> Vec<HistoryEntry> {
    entries
        .iter()
        .filter(|entry| entry_matches(entry, query))
        .cloned()
        .collect()
}

fn contains_ignore_case(haystack: &str, needle_lowercase: &str) -> bool {
    haystack.to_lowercase().contains(needle_lowercase)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryGroup<'a> {
    pub app_name: &'a str,
    pub entries: Vec<&'a HistoryEntry>,
}

impl HistoryGroup<'_> {
    pub fn count(&self) -> usize {
        self.entries.len()
    }
}

/// Group `entries` by app. The slice is newest first, and so are the groups
/// and the entries inside each group: a group is placed when its newest
/// entry is seen.
pub fn group_history(entries: &[HistoryEntry]) -> Vec<HistoryGroup<'_>> {
    let mut groups: Vec<HistoryGroup<'_>> = Vec::new();
    for entry in entries {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.app_name == entry.app_name)
        {
            group.entries.push(entry);
        } else {
            groups.push(HistoryGroup {
                app_name: &entry.app_name,
                entries: vec![entry],
            });
        }
    }
    groups
}

/// Glyph drawn on a group header. A stored glyph is used as-is. An icon
/// name or a file falls back to the same app glyph as a live card, including
/// the alert glyph for a critical notification.
pub fn group_glyph(app_name: &str, icon: &HistoryIcon, urgency: Urgency) -> String {
    if icon.kind == HistoryIconKind::Glyph && !icon.value.is_empty() {
        return icon.value.clone();
    }
    glyph_for(app_name, "", false, urgency, None)
}

pub fn entry_urgency(urgency: &str) -> Urgency {
    match urgency {
        "low" => Urgency::Low,
        "critical" => Urgency::Critical,
        _ => Urgency::Normal,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleRow {
    pub app_name: String,
    pub muted: bool,
    pub timeout: RuleTimeout,
    pub bypass_dnd: bool,
}

/// One row per app that has history or a saved rule.
///
/// History order wins: the app with the newest notification is first.
/// Saved rules for apps that are not in the history follow, sorted by name.
/// An app with no saved rule shows the defaults.
pub fn rule_rows(entries: &[HistoryEntry], rules: &[AppRule]) -> Vec<RuleRow> {
    let mut apps = Vec::new();
    for entry in entries {
        if !apps.iter().any(|app: &String| app == &entry.app_name) {
            apps.push(entry.app_name.clone());
        }
    }
    let mut extra: Vec<String> = rules
        .iter()
        .map(|rule| rule.app.clone())
        .filter(|app| !apps.iter().any(|existing| existing == app))
        .collect();
    extra.sort_by(|left, right| {
        left.to_lowercase()
            .cmp(&right.to_lowercase())
            .then_with(|| left.cmp(right))
    });
    apps.extend(extra);
    apps.into_iter()
        .map(|app_name| {
            let rule = rules.iter().find(|rule| rule.app == app_name);
            RuleRow {
                app_name,
                muted: rule.is_some_and(|rule| rule.muted),
                timeout: rule
                    .map(|rule| rule.timeout)
                    .unwrap_or(RuleTimeout::Default),
                bypass_dnd: rule.is_some_and(|rule| rule.bypass_dnd),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{HistoryAction, HistoryIcon, HistoryIconKind};
    use crate::model::{Urgency, GLYPH_MUSIC};

    fn entry(id: u32, app: &str, summary: &str, body: &str) -> HistoryEntry {
        HistoryEntry {
            id,
            app_name: app.into(),
            icon: HistoryIcon {
                kind: HistoryIconKind::IconName,
                value: "spotify".into(),
            },
            summary: summary.into(),
            body: body.into(),
            urgency: "normal".into(),
            timestamp: i64::from(id),
            actions: vec![HistoryAction {
                key: "default".into(),
                label: "Open".into(),
            }],
            dismissed: false,
        }
    }

    fn sample() -> Vec<HistoryEntry> {
        vec![
            entry(3, "Spotify", "Now playing", "Jet Plane — local file"),
            entry(2, "Mail", "New message", "From sky"),
            entry(1, "Spotify", "Earlier", "Old track"),
        ]
    }

    #[test]
    fn search_matches_app_title_and_body_without_case() {
        let entries = sample();
        assert_eq!(filter_history(&entries, "").len(), 3);
        assert_eq!(filter_history(&entries, "   ").len(), 3);
        assert_eq!(filter_history(&entries, "SPOT").len(), 2);
        assert_eq!(filter_history(&entries, "jet plane").len(), 1);
        assert_eq!(filter_history(&entries[0..1], "jet plane")[0].id, 3);
        assert_eq!(filter_history(&entries, "FROM SKY").len(), 1);
        assert_eq!(filter_history(&entries, "from sky")[0].app_name, "Mail");
        assert!(filter_history(&entries, "new")
            .iter()
            .any(|entry| entry.id == 2));
        assert!(filter_history(&entries, "no such text").is_empty());
        // The icon value and the action key are not searchable.
        assert!(filter_history(&entries, "spotify-icon").is_empty());
        let only_icon = entry(9, "Notes", "", "");
        assert!(filter_history(&[only_icon], "spotify").is_empty());
    }

    #[test]
    fn search_then_group_keeps_newest_first_and_counts() {
        let entries = sample();
        let filtered = filter_history(&entries, "spotify");
        let groups = group_history(&filtered);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].app_name, "Spotify");
        assert_eq!(groups[0].count(), 2);
        assert_eq!(groups[0].entries[0].id, 3);
        assert_eq!(groups[0].entries[1].id, 1);

        let all = group_history(&entries);
        assert_eq!(all[0].app_name, "Spotify");
        assert_eq!(all[0].count(), 2);
        assert_eq!(all[1].app_name, "Mail");
        assert_eq!(all[1].count(), 1);

        let mail_filtered = filter_history(&entries, "message");
        let mail_only = group_history(&mail_filtered);
        assert_eq!(mail_only.len(), 1);
        assert_eq!(mail_only[0].app_name, "Mail");
        assert_eq!(mail_only[0].entries[0].summary, "New message");
    }

    #[test]
    fn group_glyph_uses_a_stored_glyph_or_the_app_glyph() {
        let icon = HistoryIcon {
            kind: HistoryIconKind::Glyph,
            value: "★".into(),
        };
        assert_eq!(group_glyph("Mail", &icon, Urgency::Normal), "★");
        let named = HistoryIcon {
            kind: HistoryIconKind::IconName,
            value: "spotify".into(),
        };
        assert_eq!(group_glyph("Spotify", &named, Urgency::Normal), GLYPH_MUSIC);
        assert_ne!(group_glyph("Spotify", &named, Urgency::Normal), "spotify");
        assert_eq!(
            group_glyph("System", &named, Urgency::Critical),
            crate::model::GLYPH_ALERT
        );
    }

    #[test]
    fn rule_rows_follow_history_then_saved_apps() {
        let entries = sample();
        let rules = vec![
            AppRule {
                app: "Mail".into(),
                muted: true,
                timeout: RuleTimeout::TenSeconds,
                bypass_dnd: false,
            },
            AppRule {
                app: "Venmo".into(),
                muted: false,
                timeout: RuleTimeout::Sticky,
                bypass_dnd: true,
            },
            AppRule {
                app: "alarm".into(),
                muted: false,
                timeout: RuleTimeout::FiveSeconds,
                bypass_dnd: false,
            },
        ];
        let rows = rule_rows(&entries, &rules);
        assert_eq!(
            rows.iter()
                .map(|row| row.app_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Spotify", "Mail", "alarm", "Venmo"]
        );
        assert!(!rows[0].muted);
        assert_eq!(rows[0].timeout, RuleTimeout::Default);
        assert!(rows[1].muted);
        assert_eq!(rows[1].timeout, RuleTimeout::TenSeconds);
        assert_eq!(rows[2].timeout, RuleTimeout::FiveSeconds);
        assert!(rows[3].bypass_dnd);
        assert_eq!(rows[3].timeout, RuleTimeout::Sticky);
        assert_eq!(rule_rows(&[], &[]), Vec::new());
    }
}
