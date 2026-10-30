//! org.freedesktop.Notifications on the session bus.
//!
//! The GTK thread never runs inside a D-Bus method. Notifications cross
//! to the UI on a channel, and clicks come back the same way.

use crate::history::HistoryLog;
use crate::model::{
    allows_popup, decode_image_data, expire_ms, glyph_for, is_battery, pair_actions,
    parse_exec_argv, IconSource, Note, Urgency, VisibleCards, REASON_CLOSED, REASON_DISMISSED,
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

// `Note` is the event payload. Boxing it would only exist to satisfy the lint.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum UiEvent {
    Upsert(Note),
    Dismiss { id: u32 },
    ReloadTheme,
}

#[derive(Debug)]
pub enum UiCommand {
    Closed {
        id: u32,
        reason: u32,
    },
    Action {
        id: u32,
        key: String,
    },
    /// The do-not-disturb file changed outside `SetDoNotDisturb`.
    SetDoNotDisturb(bool),
}

#[derive(Debug, PartialEq, Eq)]
enum OutSignal {
    Closed { id: u32, reason: u32 },
    Action { id: u32, key: String },
    HistoryChanged,
}

struct Inner {
    next_id: u32,
    active: HashMap<u32, Note>,
    order: VisibleCards,
}

/// State shared by the freedesktop server and the control interface.
///
/// Dismiss, invoke, and do-not-disturb all go through here, whether the
/// caller is the CLI, a card click, or a later control dashboard.
struct Shared {
    inner: Mutex<Inner>,
    history: Mutex<HistoryLog>,
    dnd: Arc<AtomicBool>,
    ui_tx: std::sync::mpsc::Sender<UiEvent>,
    signal_tx: tokio::sync::mpsc::UnboundedSender<OutSignal>,
}

pub struct NotificationService {
    shared: Arc<Shared>,
    low_timeout: u64,
    normal_timeout: u64,
}

struct Controller {
    shared: Arc<Shared>,
}

#[interface(name = "org.freedesktop.Notifications")]
impl NotificationService {
    // Freedesktop `Notify` has this many arguments; the signature is fixed.
    #[allow(clippy::too_many_arguments)]
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

        let suppressed = !allows_popup(self.shared.dnd.load(Ordering::SeqCst), &app_name, urgency);
        let mut guard = self.shared.inner.lock().expect("notification map");
        let id = if suppressed {
            guard.allocate(replaces_id)
        } else if replaces_id != 0 && guard.active.contains_key(&replaces_id) {
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
        if suppressed {
            drop(guard);
            eprintln!("den-den-mushi: suppressed id={id} app={app_name} (do not disturb)");
            // Received, then closed immediately. Keep it in history as dismissed.
            self.shared.remember(&note, &actions, true);
            let _ = self.shared.signal_tx.send(OutSignal::Closed {
                id,
                reason: REASON_DISMISSED,
            });
            return id;
        }

        eprintln!(
            "den-den-mushi: notify id={id} app={} urgency={:?}",
            note.app_name, note.urgency
        );
        guard.upsert(note.clone());
        drop(guard);
        self.shared.remember(&note, &actions, false);
        let _ = self.shared.ui_tx.send(UiEvent::Upsert(note));
        id
    }

    fn close_notification(&self, id: u32) {
        eprintln!("den-den-mushi: close id={id}");
        self.shared.close(id, REASON_CLOSED);
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

/// Control API for the CLI and a future dashboard.
///
/// One object on the session bus: `com.skyguy.DenDenMushi` at
/// `/com/skyguy/DenDenMushi`, served by the owner of
/// `org.freedesktop.Notifications`. `DoNotDisturb` is a property.
/// `SetDoNotDisturb` writes it and emits `PropertiesChanged` when it changes.
/// History methods are the same object: a dashboard calls `History`,
/// `ClearHistory`, `RemoveHistory`, and `InvokeHistory`, and listens for
/// `HistoryChanged`.
#[interface(name = "com.skyguy.DenDenMushi")]
impl Controller {
    /// Close the newest visible card, or every card when `all` is true.
    ///
    /// Each card emits `NotificationClosed` with reason 2 (dismissed by the user).
    fn dismiss(&self, all: bool) {
        self.shared.dismiss(all);
    }

    /// Run the newest card's default action when it has one, then close it.
    ///
    /// A card that offered `default` emits `ActionInvoked` with that key,
    /// then `NotificationClosed` with reason 2. A card that did not is only
    /// dismissed, with that same close reason and no action signal.
    fn invoke(&self) {
        self.shared.invoke_newest();
    }

    /// Set do not disturb. Emits `PropertiesChanged` for `DoNotDisturb` when
    /// the value changes.
    #[zbus(name = "SetDoNotDisturb")]
    async fn set_do_not_disturb(&self, on: bool, #[zbus(signal_context)] ctxt: SignalContext<'_>) {
        publish_dnd(self, on, &ctxt).await;
    }

    /// Read-only view of do not disturb. Changes are signaled on
    /// `org.freedesktop.DBus.Properties.PropertiesChanged`.
    #[zbus(property)]
    fn do_not_disturb(&self) -> bool {
        self.shared.dnd.load(Ordering::SeqCst)
    }

    fn get_do_not_disturb(&self) -> bool {
        self.do_not_disturb()
    }

    fn reload_theme(&self) {
        let _ = self.shared.ui_tx.send(UiEvent::ReloadTheme);
    }

    fn visible(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self
            .shared
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

    /// Saved notifications, newest first, as a JSON array.
    ///
    /// Image pixel data is not included. A dashboard calls this, then
    /// `ClearHistory`, `RemoveHistory`, or `InvokeHistory`.
    fn history(&self) -> String {
        self.shared.history.lock().expect("history").to_json()
    }

    fn clear_history(&self) {
        self.shared.clear_history();
    }

    fn remove_history(&self, id: u32) {
        self.shared.remove_history(id);
    }

    /// Default action for a notification that is still on screen.
    ///
    /// The sender is still waiting on that id, so this emits `ActionInvoked`
    /// with key `default` when the card offered one, then closes it. A card
    /// with no default action is only dismissed. An id that is only in
    /// history is left alone: the app was already told the notification closed.
    fn invoke_history(&self, id: u32) {
        self.shared.invoke_history(id);
    }

    #[zbus(signal)]
    async fn history_changed(ctxt: &SignalContext<'_>) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "com.skyguy.DenDenMushi",
    default_service = "org.freedesktop.Notifications",
    default_path = "/com/skyguy/DenDenMushi"
)]
pub trait Control {
    fn dismiss(&self, all: bool) -> zbus::Result<()>;
    fn invoke(&self) -> zbus::Result<()>;
    fn set_do_not_disturb(&self, on: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn do_not_disturb(&self) -> zbus::Result<bool>;
    fn get_do_not_disturb(&self) -> zbus::Result<bool>;
    fn reload_theme(&self) -> zbus::Result<()>;
    fn visible(&self) -> zbus::Result<Vec<u32>>;
    fn history(&self) -> zbus::Result<String>;
    fn clear_history(&self) -> zbus::Result<()>;
    fn remove_history(&self, id: u32) -> zbus::Result<()>;
    fn invoke_history(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn history_changed(&self) -> zbus::Result<()>;
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

    fn upsert(&mut self, note: Note) {
        let id = note.id;
        self.order.insert_new(id);
        self.active.insert(id, note);
    }

    fn remove(&mut self, id: u32) -> Option<Note> {
        self.order.remove(id);
        self.active.remove(&id)
    }

    fn newest(&self) -> Option<u32> {
        self.order.newest()
    }

    fn dismiss(&mut self, all: bool) -> Vec<u32> {
        let ids = self.order.dismiss(all);
        for id in &ids {
            self.active.remove(id);
        }
        ids
    }
}

impl Shared {
    /// Returns whether the stored value changed.
    fn apply_dnd(&self, on: bool) -> bool {
        if self.dnd.swap(on, Ordering::SeqCst) == on {
            return false;
        }
        if let Err(error) = theme::write_dnd(on) {
            eprintln!("den-den-mushi: could not store do-not-disturb: {error}");
        }
        eprintln!(
            "den-den-mushi: do not disturb {}",
            if on { "on" } else { "off" }
        );
        true
    }

    fn emit_closed(&self, id: u32, reason: u32) {
        if reason == REASON_DISMISSED {
            self.mark_history_dismissed(id);
        }
        let _ = self.signal_tx.send(OutSignal::Closed { id, reason });
        let _ = self.ui_tx.send(UiEvent::Dismiss { id });
    }

    fn close(&self, id: u32, reason: u32) {
        let removed = self
            .inner
            .lock()
            .expect("notification map")
            .remove(id)
            .is_some();
        if removed {
            self.emit_closed(id, reason);
        }
    }

    fn dismiss(&self, all: bool) {
        let ids = self.inner.lock().expect("notification map").dismiss(all);
        for id in ids {
            eprintln!("den-den-mushi: dismiss id={id}");
            self.emit_closed(id, REASON_DISMISSED);
        }
    }

    fn invoke_id(&self, id: u32, key: &str) {
        let exec = {
            let mut guard = self.inner.lock().expect("notification map");
            let Some(note) = guard.remove(id) else {
                return;
            };
            if key == "default" {
                note.exec
            } else {
                None
            }
        };
        self.emit_action(id, key, exec);
    }

    fn invoke_newest(&self) {
        let (id, has_default, exec) = {
            let mut guard = self.inner.lock().expect("notification map");
            let Some(id) = guard.newest() else {
                return;
            };
            let Some(note) = guard.remove(id) else {
                return;
            };
            (id, note.has_default, note.exec)
        };
        self.finish_default(id, has_default, exec);
    }

    /// `InvokeHistory`. No signal when the notification is already closed.
    fn invoke_history(&self, id: u32) {
        let (has_default, exec) = {
            let mut guard = self.inner.lock().expect("notification map");
            let Some(note) = guard.remove(id) else {
                return;
            };
            (note.has_default, note.exec)
        };
        self.finish_default(id, has_default, exec);
    }

    fn finish_default(&self, id: u32, has_default: bool, exec: Option<Vec<String>>) {
        if has_default {
            self.emit_action(id, "default", exec);
        } else {
            eprintln!("den-den-mushi: dismiss id={id}");
            self.emit_closed(id, REASON_DISMISSED);
        }
    }

    fn remember(&self, note: &Note, actions: &[String], dismissed: bool) {
        let mut history = self.history.lock().expect("history");
        if !history.record(note, actions, dismissed) {
            return;
        }
        history.store();
        let _ = self.signal_tx.send(OutSignal::HistoryChanged);
    }

    fn mark_history_dismissed(&self, id: u32) {
        let mut history = self.history.lock().expect("history");
        if !history.mark_dismissed(id) {
            return;
        }
        history.store();
        let _ = self.signal_tx.send(OutSignal::HistoryChanged);
    }

    fn clear_history(&self) {
        let mut history = self.history.lock().expect("history");
        if !history.clear() {
            return;
        }
        history.store();
        eprintln!("den-den-mushi: history cleared");
        let _ = self.signal_tx.send(OutSignal::HistoryChanged);
    }

    fn remove_history(&self, id: u32) {
        let mut history = self.history.lock().expect("history");
        if !history.remove(id) {
            return;
        }
        history.store();
        eprintln!("den-den-mushi: history remove id={id}");
        let _ = self.signal_tx.send(OutSignal::HistoryChanged);
    }

    fn emit_action(&self, id: u32, key: &str, exec: Option<Vec<String>>) {
        if let Some(argv) = exec {
            spawn_exec(&argv);
        }
        eprintln!("den-den-mushi: invoke id={id} action={key}");
        let _ = self.signal_tx.send(OutSignal::Action {
            id,
            key: key.to_string(),
        });
        self.emit_closed(id, REASON_DISMISSED);
    }
}

async fn publish_dnd(controller: &Controller, on: bool, ctxt: &SignalContext<'_>) {
    if controller.shared.apply_dnd(on) {
        if let Err(error) = controller.do_not_disturb_changed(ctxt).await {
            eprintln!("den-den-mushi: do-not-disturb signal: {error}");
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
    let (signal_tx, mut signal_rx) = tokio::sync::mpsc::unbounded_channel();
    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            next_id: 1,
            active: HashMap::new(),
            order: VisibleCards::default(),
        }),
        history: Mutex::new(HistoryLog::load(
            settings.history_max,
            settings.history_persist,
            theme::history_path(),
        )),
        dnd,
        ui_tx,
        signal_tx,
    });
    let service = NotificationService {
        shared: Arc::clone(&shared),
        low_timeout: settings.timeout_low_ms,
        normal_timeout: settings.timeout_normal_ms,
    };
    let controller = Controller {
        shared: Arc::clone(&shared),
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
    let control_context = SignalContext::new(&connection, "/com/skyguy/DenDenMushi")
        .map_err(|error| error.to_string())?;
    let history_context = SignalContext::new(&connection, "/com/skyguy/DenDenMushi")
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
                OutSignal::HistoryChanged => Controller::history_changed(&history_context).await,
            };
            if let Err(error) = result {
                eprintln!("den-den-mushi: signal: {error}");
            }
        }
    });

    let publisher = Controller {
        shared: Arc::clone(&shared),
    };
    tokio::spawn(async move {
        while let Some(command) = commands.recv().await {
            match command {
                UiCommand::Closed { id, reason } => publisher.shared.close(id, reason),
                UiCommand::Action { id, key } => publisher.shared.invoke_id(id, &key),
                UiCommand::SetDoNotDisturb(on) => {
                    publish_dnd(&publisher, on, &control_context).await;
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

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
mod tests {
    use super::*;
    use crate::model::Urgency;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::Mutex as StdMutex;

    static STATE_LOCK: StdMutex<()> = StdMutex::new(());

    struct StateDir {
        previous: Option<OsString>,
        dir: PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl StateDir {
        fn lock() -> Self {
            let _guard = STATE_LOCK.lock().expect("state dir lock");
            let dir = std::env::temp_dir().join(format!(
                "den-den-mushi-dnd-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0)
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("state dir");
            let previous = std::env::var_os("XDG_STATE_HOME");
            std::env::set_var("XDG_STATE_HOME", &dir);
            Self {
                previous,
                dir,
                _guard,
            }
        }
    }

    impl Drop for StateDir {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("XDG_STATE_HOME", value),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn test_shared() -> (
        Arc<Shared>,
        std::sync::mpsc::Receiver<UiEvent>,
        tokio::sync::mpsc::UnboundedReceiver<OutSignal>,
    ) {
        let (ui_tx, ui_rx) = std::sync::mpsc::channel();
        let (signal_tx, signal_rx) = tokio::sync::mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                next_id: 1,
                active: HashMap::new(),
                order: VisibleCards::default(),
            }),
            history: Mutex::new(HistoryLog::memory(100)),
            dnd: Arc::new(AtomicBool::new(false)),
            ui_tx,
            signal_tx,
        });
        (shared, ui_rx, signal_rx)
    }

    fn blank(id: u32) -> Note {
        Note {
            id,
            app_name: "notify-send".into(),
            summary: format!("note {id}"),
            body: String::new(),
            icon: IconSource::Glyph(String::new()),
            glyph_fallback: String::new(),
            actions: Vec::new(),
            has_default: true,
            urgency: Urgency::Normal,
            battery: false,
            expire_ms: None,
            exec: None,
            created_unix: 0,
        }
    }

    fn show(shared: &Shared, id: u32) {
        shared
            .inner
            .lock()
            .expect("notification map")
            .upsert(blank(id));
    }

    #[test]
    fn dismiss_closes_newest_with_reason_dismissed() {
        let (shared, ui_rx, mut signals) = test_shared();
        show(&shared, 4);
        show(&shared, 7);
        show(&shared, 4);
        shared.dismiss(false);
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 7,
                reason: REASON_DISMISSED,
            }
        );
        assert!(signals.try_recv().is_err());
        match ui_rx.try_recv().unwrap() {
            UiEvent::Dismiss { id: 7 } => {}
            other => panic!("unexpected ui event {other:?}"),
        }
        assert!(ui_rx.try_recv().is_err());
        shared.dismiss(false);
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 4,
                reason: REASON_DISMISSED,
            }
        );
        shared.dismiss(false);
        assert!(signals.try_recv().is_err());
    }

    #[test]
    fn dismiss_all_closes_newest_first() {
        let (shared, ui_rx, mut signals) = test_shared();
        show(&shared, 1);
        show(&shared, 2);
        show(&shared, 3);
        shared.dismiss(true);
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 3,
                reason: REASON_DISMISSED,
            }
        );
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 2,
                reason: REASON_DISMISSED,
            }
        );
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 1,
                reason: REASON_DISMISSED,
            }
        );
        assert!(signals.try_recv().is_err());
        let ids: Vec<u32> = std::iter::from_fn(|| ui_rx.try_recv().ok())
            .map(|event| match event {
                UiEvent::Dismiss { id } => id,
                other => panic!("unexpected ui event {other:?}"),
            })
            .collect();
        assert_eq!(ids, vec![3, 2, 1]);
    }

    #[test]
    fn invoke_runs_default_on_the_newest_card_and_closes_it() {
        let (shared, ui_rx, mut signals) = test_shared();
        show(&shared, 1);
        show(&shared, 2);
        shared.invoke_newest();
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Action {
                id: 2,
                key: "default".into(),
            }
        );
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 2,
                reason: REASON_DISMISSED,
            }
        );
        match ui_rx.try_recv().unwrap() {
            UiEvent::Dismiss { id: 2 } => {}
            other => panic!("unexpected ui event {other:?}"),
        }
        shared.invoke_newest();
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Action {
                id: 1,
                key: "default".into(),
            }
        );
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 1,
                reason: REASON_DISMISSED,
            }
        );
        shared.invoke_newest();
        assert!(signals.try_recv().is_err());
    }

    #[test]
    fn invoke_without_a_default_action_only_dismisses() {
        let (shared, ui_rx, mut signals) = test_shared();
        let mut note = blank(5);
        note.has_default = false;
        // An exec hint without a default action must not be launched.
        note.exec = Some(vec!["should-not-run".into()]);
        shared.inner.lock().expect("notification map").upsert(note);
        shared.invoke_newest();
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 5,
                reason: REASON_DISMISSED,
            }
        );
        assert!(signals.try_recv().is_err());
        match ui_rx.try_recv().unwrap() {
            UiEvent::Dismiss { id: 5 } => {}
            other => panic!("unexpected ui event {other:?}"),
        }
    }

    #[test]
    fn click_action_and_timeout_share_the_close_path() {
        let (shared, _, mut signals) = test_shared();
        show(&shared, 8);
        shared.invoke_id(8, "pay");
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Action {
                id: 8,
                key: "pay".into(),
            }
        );
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 8,
                reason: REASON_DISMISSED,
            }
        );
        show(&shared, 9);
        shared.close(9, crate::model::REASON_EXPIRED);
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 9,
                reason: crate::model::REASON_EXPIRED,
            }
        );
        shared.close(9, REASON_DISMISSED);
        assert!(signals.try_recv().is_err());
    }

    #[test]
    fn history_follows_dismiss_and_invoke_only_while_the_card_is_up() {
        let (shared, _, mut signals) = test_shared();
        let mut first = blank(1);
        first.summary = "older".into();
        let mut second = blank(2);
        second.summary = "newer".into();
        second.has_default = true;
        shared.remember(&first, &[], false);
        shared.inner.lock().expect("notification map").upsert(first);
        shared.remember(&second, &["default".into(), "Open".into()], false);
        shared
            .inner
            .lock()
            .expect("notification map")
            .upsert(second);
        assert_eq!(signals.try_recv().unwrap(), OutSignal::HistoryChanged);
        assert_eq!(signals.try_recv().unwrap(), OutSignal::HistoryChanged);

        shared.dismiss(false);
        assert_eq!(signals.try_recv().unwrap(), OutSignal::HistoryChanged);
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 2,
                reason: REASON_DISMISSED,
            }
        );
        let entries = shared.history.lock().expect("history").entries().to_vec();
        assert_eq!(entries[0].id, 2);
        assert!(entries[0].dismissed);
        assert_eq!(entries[1].id, 1);
        assert!(!entries[1].dismissed);

        shared.invoke_history(1);
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Action {
                id: 1,
                key: "default".into(),
            }
        );
        assert_eq!(signals.try_recv().unwrap(), OutSignal::HistoryChanged);
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 1,
                reason: REASON_DISMISSED,
            }
        );
        assert!(signals.try_recv().is_err());
        assert!(shared.history.lock().expect("history").entries()[1].dismissed);

        // Already closed: the app was told, so there is no ActionInvoked.
        shared.invoke_history(2);
        assert!(signals.try_recv().is_err());

        let mut third = blank(3);
        third.has_default = false;
        shared.remember(&third, &[], false);
        shared.inner.lock().expect("notification map").upsert(third);
        assert_eq!(signals.try_recv().unwrap(), OutSignal::HistoryChanged);
        shared.invoke_history(3);
        assert_eq!(signals.try_recv().unwrap(), OutSignal::HistoryChanged);
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 3,
                reason: REASON_DISMISSED,
            }
        );
        assert!(signals.try_recv().is_err());

        shared.clear_history();
        assert_eq!(signals.try_recv().unwrap(), OutSignal::HistoryChanged);
        assert!(shared.history.lock().expect("history").entries().is_empty());
        shared.clear_history();
        shared.remove_history(1);
        assert!(signals.try_recv().is_err());
    }

    #[test]
    fn expiry_does_not_mark_history_dismissed() {
        let (shared, _, mut signals) = test_shared();
        let note = blank(9);
        shared.remember(&note, &[], false);
        shared.inner.lock().expect("notification map").upsert(note);
        assert_eq!(signals.try_recv().unwrap(), OutSignal::HistoryChanged);
        shared.close(9, crate::model::REASON_EXPIRED);
        assert_eq!(
            signals.try_recv().unwrap(),
            OutSignal::Closed {
                id: 9,
                reason: crate::model::REASON_EXPIRED,
            }
        );
        assert!(signals.try_recv().is_err());
        let history = shared.history.lock().expect("history");
        assert_eq!(history.entries().len(), 1);
        assert!(!history.entries()[0].dismissed);
    }

    #[test]
    fn do_not_disturb_is_stored_once_per_change() {
        let _state = StateDir::lock();
        let (shared, _, _) = test_shared();
        assert!(shared.apply_dnd(true));
        assert!(theme::read_dnd());
        assert!(!shared.apply_dnd(true));
        assert!(shared.apply_dnd(false));
        assert!(!theme::read_dnd());
        assert!(!shared.dnd.load(Ordering::SeqCst));
    }

    /// The CLI and a future dashboard both talk to this object. Drive it on a
    /// private session bus, without GTK, and check the signals they observe.
    #[test]
    fn control_api_signals_on_a_private_bus() {
        let _state = StateDir::lock();
        let bus = PrivateBus::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio");
        runtime.block_on(exercise_control_api());
        drop(bus);
    }

    struct PrivateBus {
        child: std::process::Child,
        socket: PathBuf,
        previous: Option<OsString>,
    }

    impl PrivateBus {
        fn start() -> Self {
            let socket =
                std::env::temp_dir().join(format!("den-den-mushi-bus-{}", std::process::id()));
            let _ = std::fs::remove_file(&socket);
            let address = format!("unix:path={}", socket.display());
            let mut child = std::process::Command::new("dbus-daemon")
                .args([
                    "--session",
                    "--nofork",
                    "--address",
                    &address,
                    "--print-pid",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("dbus-daemon");
            let stdout = child.stdout.take().expect("dbus pid pipe");
            let mut pid_line = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(stdout), &mut pid_line)
                .expect("dbus pid");
            assert!(
                !pid_line.trim().is_empty(),
                "dbus-daemon did not print a pid"
            );
            let previous = std::env::var_os("DBUS_SESSION_BUS_ADDRESS");
            std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address);
            Self {
                child,
                socket,
                previous,
            }
        }
    }

    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_file(&self.socket);
            match self.previous.take() {
                Some(value) => std::env::set_var("DBUS_SESSION_BUS_ADDRESS", value),
                None => std::env::remove_var("DBUS_SESSION_BUS_ADDRESS"),
            }
        }
    }

    #[zbus::proxy(
        interface = "org.freedesktop.Notifications",
        default_service = "org.freedesktop.Notifications",
        default_path = "/org/freedesktop/Notifications"
    )]
    trait NoteBus {
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
        ) -> zbus::Result<u32>;

        #[zbus(signal)]
        fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;

        #[zbus(signal)]
        fn action_invoked(&self, id: u32, action_key: &str) -> zbus::Result<()>;
    }

    async fn exercise_control_api() {
        let (ui_tx, ui_rx) = std::sync::mpsc::channel();
        let (_cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let settings = crate::theme::Settings {
            margin: None,
            namespace: "r2-d2-notifications".into(),
            timeout_low_ms: 4_000,
            timeout_normal_ms: 5_000,
            history_max: 100,
            history_persist: false,
        };
        serve(
            settings,
            Arc::new(AtomicBool::new(false)),
            ui_tx,
            cmd_rx,
            false,
        )
        .await
        .expect("serve");

        let connection = zbus::Connection::session().await.expect("session bus");
        let control = ControlProxy::builder(&connection)
            .cache_properties(zbus::proxy::CacheProperties::Yes)
            .build()
            .await
            .expect("control proxy");
        assert!(!control.do_not_disturb().await.expect("read dnd"));
        let mut dnd_changes = control.receive_do_not_disturb_changed().await;

        control.set_do_not_disturb(true).await.expect("set dnd");
        assert!(next_dnd_value(&mut dnd_changes, true).await);
        assert!(theme::read_dnd());
        control.set_do_not_disturb(false).await.expect("clear dnd");
        assert!(!next_dnd_value(&mut dnd_changes, false).await);
        assert!(!theme::read_dnd());
        // Setting the same value again does not emit another change.
        control
            .set_do_not_disturb(false)
            .await
            .expect("set dnd again");

        let notes = NoteBusProxy::new(&connection).await.expect("note proxy");
        let first = notes
            .notify(
                "Spotify",
                0,
                "",
                "Now playing",
                "Jet Plane",
                vec![],
                HashMap::new(),
                -1,
            )
            .await
            .expect("notify");
        let second = notes
            .notify(
                "Mail",
                0,
                "",
                "New message",
                "From sky",
                vec!["default".into(), "Open".into()],
                HashMap::new(),
                5_000,
            )
            .await
            .expect("notify");
        assert!(second > first);

        let mut closed = notes
            .receive_notification_closed()
            .await
            .expect("closed stream");
        let mut invoked = notes.receive_action_invoked().await.expect("action stream");

        control.invoke().await.expect("invoke");
        let action = next_stream(&mut invoked).await;
        let action_args = action.args().expect("action args");
        assert_eq!(action_args.id, second);
        assert_eq!(action_args.action_key, "default");
        let close = next_stream(&mut closed).await;
        let close_args = close.args().expect("close args");
        assert_eq!(close_args.id, second);
        assert_eq!(close_args.reason, REASON_DISMISSED);
        assert_eq!(take_dismissals(&ui_rx), vec![second]);

        control.dismiss(false).await.expect("dismiss newest");
        let close = next_stream(&mut closed).await;
        let close_args = close.args().expect("close args");
        assert_eq!(close_args.id, first);
        assert_eq!(close_args.reason, REASON_DISMISSED);

        let third = notes
            .notify("System", 0, "", "One", "", vec![], HashMap::new(), -1)
            .await
            .expect("notify");
        let fourth = notes
            .notify("System", 0, "", "Two", "", vec![], HashMap::new(), -1)
            .await
            .expect("notify");
        control.dismiss(true).await.expect("dismiss all");
        let newest = next_stream(&mut closed).await;
        let older = next_stream(&mut closed).await;
        assert_eq!(newest.args().expect("close").id, fourth);
        assert_eq!(newest.args().expect("close").reason, REASON_DISMISSED);
        assert_eq!(older.args().expect("close").id, third);
        assert_eq!(older.args().expect("close").reason, REASON_DISMISSED);

        control.dismiss(false).await.expect("dismiss empty");
        control.invoke().await.expect("invoke empty");
        exercise_history(&control, &notes).await;
    }

    async fn exercise_history(control: &ControlProxy<'_>, notes: &NoteBusProxy<'_>) {
        use futures_util::StreamExt;

        // Let HistoryChanged from the earlier notifications leave the bus
        // before this stream is subscribed.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let mut changed = control
            .receive_history_changed()
            .await
            .expect("history stream");

        let before = history_entries(control).await;
        assert!(before.len() >= 4, "earlier notifications were recorded");
        assert!(before.iter().all(|entry| entry.dismissed));
        assert!(before.windows(2).all(|pair| pair[0].id > pair[1].id));

        control.clear_history().await.expect("clear history");
        next_stream(&mut changed).await;
        assert!(history_entries(control).await.is_empty());
        control.clear_history().await.expect("clear empty");

        let live = notes
            .notify(
                "Spotify",
                0,
                "spotify",
                "Now playing",
                "Jet Plane",
                vec!["default".into(), "Open".into(), "pay".into(), "Pay".into()],
                HashMap::new(),
                -1,
            )
            .await
            .expect("notify live");
        next_stream(&mut changed).await;
        let listed = history_entries(control).await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, live);
        assert_eq!(listed[0].app_name, "Spotify");
        assert_eq!(listed[0].summary, "Now playing");
        assert_eq!(listed[0].body, "Jet Plane");
        assert_eq!(listed[0].urgency, "normal");
        assert!(!listed[0].dismissed);
        assert_eq!(
            listed[0].icon.kind,
            crate::history::HistoryIconKind::IconName
        );
        assert_eq!(listed[0].icon.value, "spotify");
        assert_eq!(listed[0].actions.len(), 2);
        assert_eq!(listed[0].actions[0].key, "default");

        let mut invoked = notes.receive_action_invoked().await.expect("action stream");
        let mut closed = notes
            .receive_notification_closed()
            .await
            .expect("closed stream");
        control.invoke_history(live).await.expect("invoke history");
        let action = next_stream(&mut invoked).await;
        let action_args = action.args().expect("action args");
        assert_eq!(action_args.id, live);
        assert_eq!(action_args.action_key, "default");
        let close = next_stream(&mut closed).await;
        assert_eq!(close.args().expect("close").reason, REASON_DISMISSED);
        next_stream(&mut changed).await;
        assert!(history_entries(control).await[0].dismissed);

        let gone = notes
            .notify(
                "Mail",
                0,
                "",
                "Old",
                "already closed",
                vec![],
                HashMap::new(),
                -1,
            )
            .await
            .expect("notify gone");
        next_stream(&mut changed).await;
        control.dismiss(false).await.expect("dismiss gone");
        next_stream(&mut changed).await;
        let mut late = notes
            .receive_action_invoked()
            .await
            .expect("late action stream");
        control
            .invoke_history(gone)
            .await
            .expect("invoke closed history");
        let none = tokio::time::timeout(std::time::Duration::from_millis(200), late.next()).await;
        assert!(
            none.is_err(),
            "InvokeHistory must not emit ActionInvoked after the notification closed"
        );

        let older = notes
            .notify("System", 0, "", "Keep", "", vec![], HashMap::new(), -1)
            .await
            .expect("notify keep");
        next_stream(&mut changed).await;
        let newer = notes
            .notify("System", 0, "", "Drop", "", vec![], HashMap::new(), -1)
            .await
            .expect("notify drop");
        next_stream(&mut changed).await;
        control.remove_history(newer).await.expect("remove history");
        next_stream(&mut changed).await;
        let left = history_entries(control).await;
        assert!(left.iter().any(|entry| entry.id == older));
        assert!(left.iter().all(|entry| entry.id != newer));
        control.remove_history(newer).await.expect("remove missing");

        control
            .set_do_not_disturb(true)
            .await
            .expect("dnd for history");
        let hidden = notes
            .notify(
                "Spotify",
                0,
                "",
                "hidden",
                "suppressed",
                vec![],
                HashMap::new(),
                -1,
            )
            .await
            .expect("notify suppressed");
        next_stream(&mut changed).await;
        let hidden_entry = history_entries(control)
            .await
            .into_iter()
            .find(|entry| entry.id == hidden)
            .expect("suppressed notification is history");
        assert!(hidden_entry.dismissed);
        assert_eq!(hidden_entry.summary, "hidden");
        control.set_do_not_disturb(false).await.expect("dnd off");
    }

    async fn history_entries(control: &ControlProxy<'_>) -> Vec<crate::history::HistoryEntry> {
        let json = control.history().await.expect("history");
        crate::history::parse_entries(&json).expect("history json")
    }

    fn take_dismissals(events: &std::sync::mpsc::Receiver<UiEvent>) -> Vec<u32> {
        let mut ids = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let UiEvent::Dismiss { id } = event {
                ids.push(id);
            }
        }
        ids
    }

    async fn next_dnd_value(
        changes: &mut zbus::proxy::PropertyStream<'_, bool>,
        want: bool,
    ) -> bool {
        use futures_util::StreamExt;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let change = tokio::time::timeout_at(deadline, changes.next())
                .await
                .expect("timed out waiting for DoNotDisturb")
                .expect("dnd stream ended");
            let on = change.get().await.expect("dnd value");
            if on == want {
                return on;
            }
        }
    }

    async fn next_stream<S>(stream: &mut S) -> S::Item
    where
        S: futures_util::Stream + Unpin,
    {
        use futures_util::StreamExt;

        tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("timed out waiting for a signal")
            .expect("signal stream ended")
    }
}
