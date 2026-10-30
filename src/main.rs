//! den-den-mushi — a freedesktop notification daemon for Hyprland.

mod cli;
mod gaps;
mod history;
mod model;
mod panel;
mod panel_ui;
mod rules;
mod server;
mod theme;
mod ui;

use cli::{
    format_dnd, format_history, next_dnd, parse_dismiss_all, parse_dnd_args, parse_history_args,
    parse_invoke_args, parse_panel_args, DndArgs, HistoryArgs,
};
use futures_util::StreamExt;
use history::parse_entries;
use server::ControlProxy;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::signal::unix::{signal, SignalKind};

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("-h") | Some("--help") | Some("help") => {
            print_help();
        }
        Some("dnd") => {
            let rest: Vec<String> = args.collect();
            let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
            let parsed = match parse_dnd_args(&refs) {
                Ok(parsed) => parsed,
                Err(error) => {
                    eprintln!("den-den-mushi: {error}");
                    std::process::exit(2);
                }
            };
            if let Err(error) = dnd(parsed) {
                eprintln!("den-den-mushi: {error}");
                std::process::exit(1);
            }
        }
        Some("dismiss") => {
            let rest: Vec<String> = args.collect();
            let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
            let all = match parse_dismiss_all(&refs) {
                Ok(all) => all,
                Err(error) => {
                    eprintln!("den-den-mushi: {error}");
                    std::process::exit(2);
                }
            };
            if let Err(error) = dismiss(all) {
                eprintln!("den-den-mushi: {error}");
                std::process::exit(1);
            }
        }
        Some("invoke") => {
            let rest: Vec<String> = args.collect();
            let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
            if let Err(error) = parse_invoke_args(&refs) {
                eprintln!("den-den-mushi: {error}");
                std::process::exit(2);
            }
            if let Err(error) = invoke() {
                eprintln!("den-den-mushi: {error}");
                std::process::exit(1);
            }
        }
        Some("panel") => {
            let rest: Vec<String> = args.collect();
            let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
            if let Err(error) = parse_panel_args(&refs) {
                eprintln!("den-den-mushi: {error}");
                std::process::exit(2);
            }
            if let Err(error) = toggle_panel() {
                eprintln!("den-den-mushi: {error}");
                std::process::exit(1);
            }
        }
        Some("history") => {
            let rest: Vec<String> = args.collect();
            let refs: Vec<&str> = rest.iter().map(String::as_str).collect();
            let parsed = match parse_history_args(&refs) {
                Ok(parsed) => parsed,
                Err(error) => {
                    eprintln!("den-den-mushi: {error}");
                    std::process::exit(2);
                }
            };
            if let Err(error) = history_cmd(parsed) {
                eprintln!("den-den-mushi: {error}");
                std::process::exit(1);
            }
        }
        Some("reload") => {
            if let Err(error) = reload() {
                eprintln!("den-den-mushi: {error}");
                std::process::exit(1);
            }
        }
        Some("visible") => match visible() {
            Ok(ids) => {
                for id in ids {
                    println!("{id}");
                }
            }
            Err(error) => {
                eprintln!("den-den-mushi: {error}");
                std::process::exit(1);
            }
        },
        Some("--replace") => run_daemon(true),
        Some(other) => {
            eprintln!("den-den-mushi: unknown command '{other}'");
            print_help();
            std::process::exit(2);
        }
        None => run_daemon(false),
    }
}

fn print_help() {
    println!(
        "\
den-den-mushi — freedesktop notification daemon

Usage:
  den-den-mushi              Run the daemon
  den-den-mushi --replace    Replace another owner of org.freedesktop.Notifications
  den-den-mushi dismiss      Close the newest card
  den-den-mushi dismiss --all
                             Close every card
  den-den-mushi invoke       Default action if the newest card has one, then close it
  den-den-mushi history [--json] [--limit N]
                             List saved notifications, newest first
  den-den-mushi history clear
                             Delete saved notifications
  den-den-mushi panel        Show or hide the notification panel
  den-den-mushi dnd          Toggle do not disturb
  den-den-mushi dnd on|off|toggle|status [--json] [--watch]
  den-den-mushi reload       Reread the theme (also SIGUSR1)
  den-den-mushi visible      Print the ids currently on screen

Do-not-disturb hides low and normal notifications. Critical notifications,
and toasts from r2-d2-action, k2so, and den-den-mushi, still show.
`--json` prints {{\"dnd\":true}} for do not disturb, or a JSON array for
history. `--watch` prints the current state, then another line whenever
it changes, and exits on SIGINT or a closed pipe.

These commands call com.skyguy.DenDenMushi on the session bus
(Dismiss, Invoke, History, ClearHistory, TogglePanel,
SetDoNotDisturb, and the DoNotDisturb property). The panel is a
client of that same object."
    );
}

fn run_daemon(replace: bool) {
    if env::var_os("WAYLAND_DISPLAY").is_some() && env::var_os("GDK_BACKEND").is_none() {
        env::set_var("GDK_BACKEND", "wayland");
    }
    let _pid = PidFile::write();
    let config = theme::FileConfig::load();
    let settings = theme::Settings::from_config(config.clone());
    let rules = rules::RuleSet::new(config.rules);
    let dnd = Arc::new(AtomicBool::new(theme::read_dnd()));
    let (ui_tx, ui_rx) = std::sync::mpsc::channel();
    let ui_tx_panel = ui_tx.clone();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio");
    if let Err(error) = runtime.block_on(server::serve(
        settings.clone(),
        rules,
        theme::config_path(),
        Arc::clone(&dnd),
        ui_tx,
        cmd_rx,
        replace,
    )) {
        eprintln!("den-den-mushi: {error}");
        std::process::exit(1);
    }
    ui::run(
        settings,
        dnd,
        ui_rx,
        cmd_tx,
        runtime.handle().clone(),
        ui_tx_panel,
    );
}

fn dnd(args: DndArgs) -> Result<(), String> {
    runtime().block_on(dnd_async(args))
}

async fn dnd_async(args: DndArgs) -> Result<(), String> {
    let session = zbus::Connection::session().await.ok();
    let proxy = match &session {
        Some(connection) if args.watch => ControlProxy::builder(connection)
            .cache_properties(zbus::proxy::CacheProperties::Yes)
            .build()
            .await
            .map_err(|error| format!("den-den-mushi is not running ({error})"))?,
        Some(connection) => ControlProxy::new(connection)
            .await
            .map_err(|error| error.to_string())?,
        None if args.watch => return Err("den-den-mushi is not running".into()),
        None => {
            return dnd_offline(args);
        }
    };

    // Subscribe before reading so a change during the command is not lost.
    let changes = if args.watch {
        Some(proxy.receive_do_not_disturb_changed().await)
    } else {
        None
    };
    let live = proxy.do_not_disturb().await;
    let current = match live {
        Ok(value) => value,
        Err(error) if args.watch => {
            return Err(format!("den-den-mushi is not running ({error})"));
        }
        Err(_) => return dnd_offline(args),
    };
    let shown = if let Some(next) = next_dnd(current, args.action) {
        proxy
            .set_do_not_disturb(next)
            .await
            .map_err(|error| error.to_string())?;
        next
    } else {
        current
    };
    if print_line(&format_dnd(shown, args.json)).is_err() {
        return Ok(());
    }
    let Some(mut changes) = changes else {
        return Ok(());
    };
    let mut last = shown;
    let mut sigint = signal(SignalKind::interrupt()).map_err(|error| error.to_string())?;
    loop {
        tokio::select! {
            _ = sigint.recv() => return Ok(()),
            change = changes.next() => {
                let Some(change) = change else {
                    return Ok(());
                };
                let Ok(on) = change.get().await else {
                    return Ok(());
                };
                if on == last {
                    continue;
                }
                last = on;
                if print_line(&format_dnd(on, args.json)).is_err() {
                    return Ok(());
                }
            }
        }
    }
}

fn dnd_offline(args: DndArgs) -> Result<(), String> {
    if args.watch {
        return Err("den-den-mushi is not running".into());
    }
    let current = theme::read_dnd();
    let shown = if let Some(next) = next_dnd(current, args.action) {
        theme::write_dnd(next).map_err(|error| error.to_string())?;
        next
    } else {
        current
    };
    let _ = print_line(&format_dnd(shown, args.json));
    Ok(())
}

fn dismiss(all: bool) -> Result<(), String> {
    runtime().block_on(async {
        let connection = session().await?;
        let proxy = ControlProxy::new(&connection)
            .await
            .map_err(|error| error.to_string())?;
        proxy.dismiss(all).await.map_err(|error| error.to_string())
    })
}

fn toggle_panel() -> Result<(), String> {
    runtime().block_on(async {
        let connection = session().await?;
        let proxy = ControlProxy::new(&connection)
            .await
            .map_err(|error| error.to_string())?;
        proxy
            .toggle_panel()
            .await
            .map_err(|error| error.to_string())
    })
}

fn invoke() -> Result<(), String> {
    runtime().block_on(async {
        let connection = session().await?;
        let proxy = ControlProxy::new(&connection)
            .await
            .map_err(|error| error.to_string())?;
        proxy.invoke().await.map_err(|error| error.to_string())
    })
}

fn reload() -> Result<(), String> {
    runtime().block_on(async {
        let connection = session().await?;
        let proxy = ControlProxy::new(&connection)
            .await
            .map_err(|error| error.to_string())?;
        proxy
            .reload_theme()
            .await
            .map_err(|error| error.to_string())
    })
}

fn history_cmd(args: HistoryArgs) -> Result<(), String> {
    let json = runtime().block_on(async {
        let connection = session().await?;
        let proxy = ControlProxy::new(&connection)
            .await
            .map_err(|error| error.to_string())?;
        if args.clear {
            proxy
                .clear_history()
                .await
                .map_err(|error| error.to_string())?;
            return Ok(String::new());
        }
        proxy.history().await.map_err(|error| error.to_string())
    })?;
    if args.clear {
        return Ok(());
    }
    let entries = parse_entries(&json)?;
    let text = format_history(&entries, args.json, args.limit);
    if args.json || !text.is_empty() {
        let _ = print_line(&text);
    }
    Ok(())
}

fn visible() -> Result<Vec<u32>, String> {
    runtime().block_on(async {
        let connection = session().await?;
        let proxy = ControlProxy::new(&connection)
            .await
            .map_err(|error| error.to_string())?;
        proxy.visible().await.map_err(|error| error.to_string())
    })
}

fn print_line(line: &str) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{line}")?;
    stdout.flush()
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio")
}

async fn session() -> Result<zbus::Connection, String> {
    zbus::Connection::session()
        .await
        .map_err(|error| error.to_string())
}

struct PidFile(Option<PathBuf>);

impl PidFile {
    fn write() -> Self {
        let Some(dir) = env::var_os("XDG_RUNTIME_DIR") else {
            return Self(None);
        };
        let path = PathBuf::from(dir).join("den-den-mushi.pid");
        if fs::write(&path, format!("{}\n", std::process::id())).is_err() {
            return Self(None);
        }
        Self(Some(path))
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}
