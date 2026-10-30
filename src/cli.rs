//! Command-line parsing and do-not-disturb text.
//!
//! The daemon owns the live state. This module only decides what a command
//! means and how to print it.

use serde_json::json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DndAction {
    On,
    Off,
    Toggle,
    Status,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DndArgs {
    pub action: DndAction,
    pub json: bool,
    pub watch: bool,
}

impl DndArgs {
    /// `dnd` with no action toggles. `--watch` with no action only observes,
    /// so a bar can follow the state without flipping it when it starts.
    pub fn resolve(action: Option<DndAction>, json: bool, watch: bool) -> Self {
        let action = action.unwrap_or(if watch {
            DndAction::Status
        } else {
            DndAction::Toggle
        });
        Self {
            action,
            json,
            watch,
        }
    }
}

pub fn parse_dnd_args(args: &[&str]) -> Result<DndArgs, String> {
    let mut action = None;
    let mut json = false;
    let mut watch = false;
    for arg in args {
        match *arg {
            "--json" => json = true,
            "--watch" => watch = true,
            "on" => assign_action(&mut action, DndAction::On)?,
            "off" => assign_action(&mut action, DndAction::Off)?,
            "toggle" => assign_action(&mut action, DndAction::Toggle)?,
            "status" => assign_action(&mut action, DndAction::Status)?,
            other => {
                return Err(format!(
                    "unknown dnd argument '{other}' (use on, off, toggle, status, --json, --watch)"
                ));
            }
        }
    }
    Ok(DndArgs::resolve(action, json, watch))
}

fn assign_action(slot: &mut Option<DndAction>, action: DndAction) -> Result<(), String> {
    if slot.is_some() {
        return Err("dnd takes only one of on, off, toggle, or status".into());
    }
    *slot = Some(action);
    Ok(())
}

/// `None` means the command does not change the state.
pub fn next_dnd(current: bool, action: DndAction) -> Option<bool> {
    match action {
        DndAction::On => Some(true),
        DndAction::Off => Some(false),
        DndAction::Toggle => Some(!current),
        DndAction::Status => None,
    }
}

/// One line of do-not-disturb state. JSON is a single object, `{"dnd":true}`.
pub fn format_dnd(on: bool, json: bool) -> String {
    if json {
        json!({ "dnd": on }).to_string()
    } else if on {
        "on".to_string()
    } else {
        "off".to_string()
    }
}

pub fn parse_dismiss_all(args: &[&str]) -> Result<bool, String> {
    let mut all = false;
    for arg in args {
        match *arg {
            "--all" => {
                if all {
                    return Err("dismiss --all was given twice".into());
                }
                all = true;
            }
            other => {
                return Err(format!(
                    "unknown dismiss argument '{other}' (use dismiss or dismiss --all)"
                ));
            }
        }
    }
    Ok(all)
}

pub fn parse_invoke_args(args: &[&str]) -> Result<(), String> {
    if let Some(extra) = args.first() {
        return Err(format!("invoke does not take arguments ('{extra}')"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dnd_defaults_to_toggle_unless_it_is_only_watching() {
        assert_eq!(
            parse_dnd_args(&[]).unwrap(),
            DndArgs {
                action: DndAction::Toggle,
                json: false,
                watch: false,
            }
        );
        assert_eq!(
            parse_dnd_args(&["--json"]).unwrap(),
            DndArgs {
                action: DndAction::Toggle,
                json: true,
                watch: false,
            }
        );
        assert_eq!(
            parse_dnd_args(&["--watch"]).unwrap(),
            DndArgs {
                action: DndAction::Status,
                json: false,
                watch: true,
            }
        );
        assert_eq!(
            parse_dnd_args(&["--json", "--watch"]).unwrap(),
            DndArgs {
                action: DndAction::Status,
                json: true,
                watch: true,
            }
        );
    }

    #[test]
    fn dnd_flags_can_surround_the_action() {
        assert_eq!(
            parse_dnd_args(&["--json", "on", "--watch"]).unwrap(),
            DndArgs {
                action: DndAction::On,
                json: true,
                watch: true,
            }
        );
        assert_eq!(
            parse_dnd_args(&["status", "--json"]).unwrap().action,
            DndAction::Status
        );
        assert_eq!(
            parse_dnd_args(&["toggle", "--watch"]).unwrap().action,
            DndAction::Toggle
        );
        assert!(parse_dnd_args(&["on", "off"]).is_err());
        assert!(parse_dnd_args(&["sometimes"]).is_err());
    }

    #[test]
    fn next_dnd_status_does_not_change_the_state() {
        assert_eq!(next_dnd(false, DndAction::On), Some(true));
        assert_eq!(next_dnd(true, DndAction::Off), Some(false));
        assert_eq!(next_dnd(false, DndAction::Toggle), Some(true));
        assert_eq!(next_dnd(true, DndAction::Toggle), Some(false));
        assert_eq!(next_dnd(true, DndAction::Status), None);
        assert_eq!(next_dnd(false, DndAction::Status), None);
    }

    #[test]
    fn dnd_json_is_one_object() {
        assert_eq!(format_dnd(true, true), r#"{"dnd":true}"#);
        assert_eq!(format_dnd(false, true), r#"{"dnd":false}"#);
        assert_eq!(format_dnd(true, false), "on");
        assert_eq!(format_dnd(false, false), "off");
    }

    #[test]
    fn dismiss_and_invoke_arguments() {
        assert!(!parse_dismiss_all(&[]).unwrap());
        assert!(parse_dismiss_all(&["--all"]).unwrap());
        assert!(parse_dismiss_all(&["--all", "--all"]).is_err());
        assert!(parse_dismiss_all(&["all"]).is_err());
        assert!(parse_invoke_args(&[]).is_ok());
        assert!(parse_invoke_args(&["--all"]).is_err());
    }
}
