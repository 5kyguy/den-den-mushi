//! den-den-mushi — a freedesktop notification daemon for Hyprland.

mod gaps;
mod model;
mod server;
mod theme;
mod ui;

use server::ControlProxy;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn main() {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("-h") | Some("--help") | Some("help") => {
            print_help();
        }
        Some("dnd") => {
            let action = args.next().unwrap_or_else(|| "toggle".into());
            if args.next().is_some() {
                eprintln!("den-den-mushi: unexpected extra arguments");
                std::process::exit(2);
            }
            if let Err(error) = dnd(&action) {
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
  den-den-mushi dnd          Toggle do not disturb
  den-den-mushi dnd on|off|toggle|status
  den-den-mushi reload       Reread the theme (also SIGUSR1)
  den-den-mushi visible      Print the ids currently on screen

Do-not-disturb hides low and normal notifications. Critical notifications,
and toasts from r2-d2-action, k2so, and den-den-mushi, still show."
    );
}

fn run_daemon(replace: bool) {
    if env::var_os("WAYLAND_DISPLAY").is_some() && env::var_os("GDK_BACKEND").is_none() {
        env::set_var("GDK_BACKEND", "wayland");
    }
    let _pid = PidFile::write();
    let settings = theme::Settings::load();
    let dnd = Arc::new(AtomicBool::new(theme::read_dnd()));
    let (ui_tx, ui_rx) = std::sync::mpsc::channel();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio");
    if let Err(error) = runtime.block_on(server::serve(
        settings.clone(),
        Arc::clone(&dnd),
        ui_tx,
        cmd_rx,
        replace,
    )) {
        eprintln!("den-den-mushi: {error}");
        std::process::exit(1);
    }
    ui::run(settings, dnd, ui_rx, cmd_tx);
}

fn dnd(action: &str) -> Result<(), String> {
    let current = theme::read_dnd();
    let next = match action {
        "on" => true,
        "off" => false,
        "toggle" => !current,
        "status" => {
            let live = runtime().block_on(async {
                if let Ok(proxy) = control().await {
                    proxy.get_do_not_disturb().await.ok()
                } else {
                    None
                }
            });
            println!("{}", if live.unwrap_or(current) { "on" } else { "off" });
            return Ok(());
        }
        other => {
            return Err(format!(
                "unknown dnd action '{other}' (use on, off, toggle, or status)"
            ))
        }
    };
    theme::write_dnd(next).map_err(|error| error.to_string())?;
    let _ = runtime().block_on(async {
        if let Ok(proxy) = control().await {
            proxy.set_do_not_disturb(next).await.ok()
        } else {
            None
        }
    });
    println!("{}", if next { "on" } else { "off" });
    Ok(())
}

fn reload() -> Result<(), String> {
    runtime().block_on(async {
        let proxy = control().await?;
        proxy
            .reload_theme()
            .await
            .map_err(|error| error.to_string())
    })
}

fn visible() -> Result<Vec<u32>, String> {
    runtime().block_on(async {
        let proxy = control().await?;
        proxy.visible().await.map_err(|error| error.to_string())
    })
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio")
}

async fn control() -> Result<ControlProxy<'static>, String> {
    let connection = zbus::Connection::session()
        .await
        .map_err(|error| error.to_string())?;
    // The proxy borrows the connection. This process exits as soon as the call
    // returns, so leaking the connection for the rest of main is enough.
    let connection = Box::leak(Box::new(connection));
    ControlProxy::new(connection)
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
