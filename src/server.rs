//! org.freedesktop.Notifications on the session bus.
//!
//! The GTK thread never runs inside a D-Bus method. Notifications cross
//! to the UI on a channel, and clicks come back the same way.

use crate::model::{
    allows_popup, decode_image_data, expire_ms, glyph_for, is_battery, pair_actions,
    parse_exec_argv, IconSource, Note, Urgency, REASON_CLOSED, REASON_DISMISSED,
};
use crate::theme::{self, Settings};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use zbus::fdo::RequestNameFlags;
use zbus::object_server::SignalContext;
use zbus::zvariant::{OwnedValue, Structure, Value};
use zbus::{interface, ConnectionBuilder};

#[derive(Clone, Debug)]
pub enum UiEvent {
    Upsert(Note),
    Dismiss { id: u32 },
    ReloadTheme,
}

#[derive(Debug)]
pub enum UiCommand {
    Closed { id: u32, reason: u32 },
    Action { id: u32, key: String },
}

enum OutSignal {
    Closed { id: u32, reason: u32 },
    Action { id: u32, key: String },
}

struct Inner {
    next_id: u32,
    active: HashMap<u32, Note>,
}

pub struct NotificationService {
    inner: Arc<Mutex<Inner>>,
    dnd: Arc<AtomicBool>,
    ui_tx: std::sync::mpsc::Sender<UiEvent>,
    signal_tx: tokio::sync::mpsc::UnboundedSender<OutSignal>,
    low_timeout: u64,
    normal_timeout: u64,
}

struct Controller {
    inner: Arc<Mutex<Inner>>,
    dnd: Arc<AtomicBool>,
    ui_tx: std::sync::mpsc::Sender<UiEvent>,
}

#[interface(name = "org.freedesktop.Notifications")]
impl NotificationService {
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let app_name = clamp(app_name, 256);
        let summary = clamp(summary, 4_000);
        let body = clamp(body, 8_000);
        let urgency = hint_urgency(&hints);
        let resident = hint_bool(&hints, "resident");
        let category = hint_string(&hints, "category").unwrap_or_default();
        let battery = is_battery(&category, &app_name, &summary, &body);
        let glyph_hint = hint_string(&hints, "r2-d2-glyph");
        let (action_pairs, mut has_default) = pair_actions(&actions);
        let exec = hint_string(&hints, "r2-d2-exec-argv").and_then(|value| parse_exec_argv(&value));
        if exec.is_some() {
            has_default = true;
        }
        let expire = expire_ms(
            expire_timeout,
            urgency,
            resident,
            self.low_timeout,
            self.normal_timeout,
        );

        let mut guard = self.inner.lock().expect("notification map");
        if !allows_popup(self.dnd.load(Ordering::SeqCst), &app_name, urgency) {
            let id = guard.allocate(replaces_id);
            drop(guard);
            eprintln!("den-den-mushi: suppressed id={id} app={app_name} (do not disturb)");
            let _ = self.signal_tx.send(OutSignal::Closed {
                id,
                reason: REASON_DISMISSED,
            });
            return id;
        }

        let id = if replaces_id != 0 && guard.active.contains_key(&replaces_id) {
            replaces_id
        } else {
            guard.allocate(0)
        };
        let note = Note {
            id,
            app_name: if app_name.is_empty() {
                "notify".into()
            } else {
                app_name.clone()
            },
            summary,
            body,
            icon: icon_from_hints(app_icon, &hints),
            glyph_fallback: glyph_for(&app_name, "", battery, urgency, glyph_hint.as_deref()),
            actions: action_pairs,
            has_default,
            urgency,
            battery,
            expire_ms: expire,
            exec,
            created_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or(0),
        };
        eprintln!(
            "den-den-mushi: notify id={id} app={} urgency={:?}",
            note.app_name, note.urgency
        );
        guard.active.insert(id, note.clone());
        drop(guard);
        let _ = self.ui_tx.send(UiEvent::Upsert(note));
        id
    }

    fn close_notification(&self, id: u32) {
        eprintln!("den-den-mushi: close id={id}");
        let removed = self
            .inner
            .lock()
            .expect("notification map")
            .active
            .remove(&id)
            .is_some();
        if !removed {
            return;
        }
        let _ = self.ui_tx.send(UiEvent::Dismiss { id });
        let _ = self.signal_tx.send(OutSignal::Closed {
            id,
            reason: REASON_CLOSED,
        });
    }

    fn get_capabilities(&self) -> Vec<String> {
        vec![
            "actions".into(),
            "body".into(),
            "body-markup".into(),
            "icon-static".into(),
        ]
    }

    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "den-den-mushi".into(),
            "skyguy".into(),
            env!("CARGO_PKG_VERSION").into(),
            "1.2".into(),
        )
    }

    #[zbus(signal)]
    async fn notification_closed(
        ctxt: &SignalContext<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        ctxt: &SignalContext<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;
}

#[interface(name = "com.skyguy.DenDenMushi")]
impl Controller {
    fn set_do_not_disturb(&self, on: bool) {
        self.dnd.store(on, Ordering::SeqCst);
        if let Err(error) = theme::write_dnd(on) {
            eprintln!("den-den-mushi: could not store do-not-disturb: {error}");
        }
        eprintln!(
            "den-den-mushi: do not disturb {}",
            if on { "on" } else { "off" }
        );
    }

    fn get_do_not_disturb(&self) -> bool {
        self.dnd.load(Ordering::SeqCst)
    }

    fn reload_theme(&self) {
        let _ = self.ui_tx.send(UiEvent::ReloadTheme);
    }

    fn visible(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self
            .inner
            .lock()
            .expect("notification map")
            .active
            .keys()
            .copied()
            .collect();
        ids.sort_unstable();
        ids
    }
}

#[zbus::proxy(
    interface = "com.skyguy.DenDenMushi",
    default_service = "org.freedesktop.Notifications",
    default_path = "/com/skyguy/DenDenMushi"
)]
pub trait Control {
    fn set_do_not_disturb(&self, on: bool) -> zbus::Result<()>;
    fn get_do_not_disturb(&self) -> zbus::Result<bool>;
    fn reload_theme(&self) -> zbus::Result<()>;
    fn visible(&self) -> zbus::Result<Vec<u32>>;
}

impl Inner {
    fn allocate(&mut self, preferred: u32) -> u32 {
        if preferred != 0 && !self.active.contains_key(&preferred) {
            return preferred;
        }
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if id != 0 && !self.active.contains_key(&id) {
                return id;
            }
        }
    }
}

pub async fn serve(
    settings: Settings,
    dnd: Arc<AtomicBool>,
    ui_tx: std::sync::mpsc::Sender<UiEvent>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<UiCommand>,
    replace: bool,
) -> Result<(), String> {
    let inner = Arc::new(Mutex::new(Inner {
        next_id: 1,
        active: HashMap::new(),
    }));
    let (signal_tx, mut signal_rx) = tokio::sync::mpsc::unbounded_channel();
    let command_ui = ui_tx.clone();
    let service = NotificationService {
        inner: Arc::clone(&inner),
        dnd: Arc::clone(&dnd),
        ui_tx: ui_tx.clone(),
        signal_tx: signal_tx.clone(),
        low_timeout: settings.timeout_low_ms,
        normal_timeout: settings.timeout_normal_ms,
    };
    let controller = Controller {
        inner: Arc::clone(&inner),
        dnd,
        ui_tx,
    };

    let connection = ConnectionBuilder::session()
        .map_err(|error| error.to_string())?
        .serve_at("/org/freedesktop/Notifications", service)
        .map_err(|error| error.to_string())?
        .serve_at("/com/skyguy/DenDenMushi", controller)
        .map_err(|error| error.to_string())?
        .build()
        .await
        .map_err(|error| error.to_string())?;

    let mut flags = RequestNameFlags::AllowReplacement | RequestNameFlags::DoNotQueue;
    if replace {
        flags |= RequestNameFlags::ReplaceExisting;
    }
    match connection
        .request_name_with_flags("org.freedesktop.Notifications", flags)
        .await
    {
        Ok(
            zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner,
        ) => {}
        Ok(zbus::fdo::RequestNameReply::Exists | zbus::fdo::RequestNameReply::InQueue) => {
            return Err(
                "org.freedesktop.Notifications is already owned. Stop the other daemon (the r2-d2 shell notification server also claims this name) or start den-den-mushi with --replace."
                    .into(),
            );
        }
        Err(error) => return Err(error.to_string()),
    }

    let context = SignalContext::new(&connection, "/org/freedesktop/Notifications")
        .map_err(|error| error.to_string())?;
    tokio::spawn(async move {
        while let Some(signal) = signal_rx.recv().await {
            let result = match signal {
                OutSignal::Closed { id, reason } => {
                    NotificationService::notification_closed(&context, id, reason).await
                }
                OutSignal::Action { id, key } => {
                    NotificationService::action_invoked(&context, id, &key).await
                }
            };
            if let Err(error) = result {
                eprintln!("den-den-mushi: signal: {error}");
            }
        }
    });

    let command_inner = Arc::clone(&inner);
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            match command {
                UiCommand::Closed { id, reason } => {
                    let removed = command_inner
                        .lock()
                        .expect("notification map")
                        .active
                        .remove(&id)
                        .is_some();
                    if removed {
                        let _ = signal_tx.send(OutSignal::Closed { id, reason });
                        let _ = command_ui.send(UiEvent::Dismiss { id });
                    }
                }
                UiCommand::Action { id, key } => {
                    let exec = {
                        let guard = command_inner.lock().expect("notification map");
                        guard.active.get(&id).and_then(|note| {
                            if key == "default" {
                                note.exec.clone()
                            } else {
                                None
                            }
                        })
                    };
                    if let Some(argv) = exec {
                        spawn_exec(&argv);
                    }
                    let _ = signal_tx.send(OutSignal::Action {
                        id,
                        key: key.clone(),
                    });
                    let removed = command_inner
                        .lock()
                        .expect("notification map")
                        .active
                        .remove(&id)
                        .is_some();
                    if removed {
                        let _ = signal_tx.send(OutSignal::Closed {
                            id,
                            reason: REASON_DISMISSED,
                        });
                        let _ = command_ui.send(UiEvent::Dismiss { id });
                    }
                }
            }
        }
    });

    // Keep the connection alive for the process lifetime.
    std::mem::forget(connection);
    eprintln!("den-den-mushi: listening on org.freedesktop.Notifications");
    Ok(())
}

fn spawn_exec(argv: &[String]) {
    if argv.is_empty() {
        return;
    }
    match std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .spawn()
    {
        Ok(_) => eprintln!("den-den-mushi: ran {}", argv[0]),
        Err(error) => eprintln!("den-den-mushi: could not run {}: {error}", argv[0]),
    }
}

fn clamp(value: &str, max: usize) -> String {
    let mut out = value.to_string();
    if out.chars().count() > max {
        out = out.chars().take(max).collect();
    }
    out
}

fn hint_string(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = hints.get(key).or_else(|| {
        let alternate = if key.contains('-') {
            key.replace('-', "_")
        } else {
            key.replace('_', "-")
        };
        hints.get(&alternate)
    })?;
    <&str>::try_from(value)
        .ok()
        .map(str::to_string)
        .or_else(|| {
            value
                .try_clone()
                .ok()
                .and_then(|owned| String::try_from(owned).ok())
        })
}

fn hint_bool(hints: &HashMap<String, OwnedValue>, key: &str) -> bool {
    hints
        .get(key)
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

fn hint_urgency(hints: &HashMap<String, OwnedValue>) -> Urgency {
    let Some(value) = hints.get("urgency") else {
        return Urgency::Normal;
    };
    if let Ok(byte) = u8::try_from(value) {
        return Urgency::from_byte(byte);
    }
    if let Ok(number) = i32::try_from(value) {
        return Urgency::from_byte(number.clamp(0, 2) as u8);
    }
    if let Ok(number) = u32::try_from(value) {
        return Urgency::from_byte(number.min(2) as u8);
    }
    Urgency::Normal
}

fn icon_from_hints(app_icon: &str, hints: &HashMap<String, OwnedValue>) -> IconSource {
    if let Some(image) = hints
        .get("image-data")
        .or_else(|| hints.get("image_data"))
        .and_then(decode_hint_image)
    {
        return image;
    }
    if let Some(path) = hint_string(hints, "image-path").filter(|path| Path::new(path).is_file()) {
        return IconSource::File(path.into());
    }
    if Path::new(app_icon).is_file() {
        return IconSource::File(app_icon.into());
    }
    if !app_icon.is_empty() {
        return IconSource::IconName(app_icon.to_string());
    }
    IconSource::Glyph(String::new())
}

fn decode_hint_image(value: &OwnedValue) -> Option<IconSource> {
    let structure = <&Structure>::try_from(value).ok()?;
    let fields = structure.fields();
    if fields.len() < 7 {
        return None;
    }
    let width: i32 = field_i32(&fields[0])?;
    let height: i32 = field_i32(&fields[1])?;
    let stride: i32 = field_i32(&fields[2])?;
    let has_alpha: bool = bool::try_from(&fields[3]).ok()?;
    let bits: i32 = field_i32(&fields[4])?;
    let channels: i32 = field_i32(&fields[5])?;
    let data = Vec::<u8>::try_from(fields[6].try_clone().ok()?).ok()?;
    decode_image_data(width, height, stride, has_alpha, bits, channels, &data)
}

fn field_i32(value: &Value<'_>) -> Option<i32> {
    i32::try_from(value)
        .ok()
        .or_else(|| u32::try_from(value).ok().map(|number| number as i32))
}
