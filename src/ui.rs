//! Layer-shell window. Cards stack in the top-right, slide in from the
//! right, and fade out on the way back.

use gtk4 as gtk;

use crate::gaps::{self, Insets};
use crate::model::{display_app, IconSource, Note, REASON_DISMISSED, REASON_EXPIRED};
use crate::server::{UiCommand, UiEvent};
use crate::theme::{self, Settings};
use gtk::gdk::MemoryFormat;
use gtk::glib::{self, ControlFlow};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene, Align, Orientation, Overflow};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const SLIDE_IN: Duration = Duration::from_millis(180);
const SLIDE_OUT: Duration = Duration::from_millis(120);
const SLIDE_DISTANCE: f64 = 96.0;
const CARD_WIDTH: i32 = 380;
const MAX_CARDS: usize = 8;

pub fn run(
    settings: Settings,
    dnd: std::sync::Arc<AtomicBool>,
    events: Receiver<UiEvent>,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
) {
    gtk::init().expect("gtk");
    if let Some(gtk_settings) = gtk::Settings::default() {
        gtk_settings.set_gtk_application_prefer_dark_theme(true);
    }

    let application = gtk::Application::new(
        Some("com.skyguy.den-den-mushi"),
        gtk::gio::ApplicationFlags::NON_UNIQUE,
    );
    let events = Rc::new(RefCell::new(Some(events)));
    let application_for_signals = application.clone();
    application.connect_activate(move |application| {
        let Some(events) = events.borrow_mut().take() else {
            return;
        };
        // The guard releases the hold on drop. Keep it for the life of the process
        // so an empty notification stack does not quit the daemon.
        std::mem::forget(application.hold());
        let shell = Shell::new(application, settings.clone(), dnd.clone(), commands.clone());
        let shell = Rc::new(RefCell::new(shell));
        let events = Rc::new(RefCell::new(events));
        let shell_for_tick = Rc::clone(&shell);
        let events_for_tick = Rc::clone(&events);
        glib::timeout_add_local(Duration::from_millis(16), move || {
            let pending = {
                let incoming = events_for_tick.borrow();
                let mut pending = Vec::new();
                while let Ok(event) = incoming.try_recv() {
                    pending.push(event);
                }
                pending
            };
            shell_for_tick.borrow_mut().tick(pending);
            ControlFlow::Continue
        });
        let shell_for_signal = Rc::clone(&shell);
        glib::unix_signal_add_local(libc::SIGUSR1, move || {
            shell_for_signal.borrow_mut().reload_theme();
            ControlFlow::Continue
        });
        let quit = application_for_signals.clone();
        glib::unix_signal_add_local(libc::SIGTERM, move || {
            quit.quit();
            ControlFlow::Break
        });
        let quit = application_for_signals.clone();
        glib::unix_signal_add_local(libc::SIGINT, move || {
            quit.quit();
            ControlFlow::Break
        });
    });
    application.run_with_args::<&str>(&[]);
}

struct LiveCard {
    id: u32,
    slide: SlideBin,
    root: gtk::Box,
    app: gtk::Label,
    title: gtk::Label,
    body: gtk::Label,
    actions: gtk::Box,
    progress: gtk::Box,
    close: gtk::Button,
    icon_slot: gtk::Box,
    expire_ms: Option<u64>,
    remaining_ms: f64,
    paused: Rc<Cell<bool>>,
    leaving: bool,
    motion: Motion,
    has_default: Rc<Cell<bool>>,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
}

struct Motion {
    start: Instant,
    duration: Duration,
    leaving: bool,
}

struct Shell {
    window: gtk::ApplicationWindow,
    column: gtk::Box,
    cards: Vec<LiveCard>,
    provider: gtk::CssProvider,
    settings: Settings,
    dnd: std::sync::Arc<AtomicBool>,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
    insets: Insets,
    inset_checks: u32,
    watched: Watched,
}

struct Watched {
    style: Vec<(std::path::PathBuf, Option<std::time::SystemTime>)>,
    theme_json: Option<std::time::SystemTime>,
    dnd: Option<std::time::SystemTime>,
}

impl Shell {
    fn new(
        application: &gtk::Application,
        settings: Settings,
        dnd: std::sync::Arc<AtomicBool>,
        commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
    ) -> Self {
        let window = gtk::ApplicationWindow::builder()
            .application(application)
            .decorated(false)
            .resizable(false)
            .title("den-den-mushi")
            .build();
        window.add_css_class("den-den-mushi");

        if gtk4_layer_shell::is_supported() {
            window.init_layer_shell();
            window.set_layer(Layer::Overlay);
            window.set_namespace(Some(settings.namespace.as_str()));
            window.set_anchor(Edge::Top, true);
            window.set_anchor(Edge::Right, true);
            window.set_anchor(Edge::Bottom, false);
            window.set_anchor(Edge::Left, false);
            window.set_exclusive_zone(0);
            window.set_keyboard_mode(KeyboardMode::None);
        } else {
            eprintln!("den-den-mushi: layer shell is unavailable; using an ordinary window");
        }

        // The 8px stack gap is `.toast { margin-bottom: 8px }` in the stylesheet.
        let column = gtk::Box::new(Orientation::Vertical, 0);
        column.set_halign(Align::End);
        column.set_valign(Align::Start);
        window.set_child(Some(&column));

        let theme = theme::load_theme();
        eprintln!("den-den-mushi: theme {}", theme.source);
        let provider = gtk::CssProvider::new();
        load_css(&provider, &theme.css);
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let insets = current_insets(&settings);
        apply_insets(&window, insets);

        Self {
            window,
            column,
            cards: Vec::new(),
            provider,
            settings,
            dnd,
            commands,
            insets,
            inset_checks: 0,
            watched: Watched::capture(),
        }
    }

    fn tick(&mut self, events: Vec<UiEvent>) {
        for event in events {
            match event {
                UiEvent::Upsert(note) => self.upsert(note),
                UiEvent::Dismiss { id } => self.begin_leave(id),
                UiEvent::ReloadTheme => self.reload_theme(),
            }
        }

        self.inset_checks = self.inset_checks.wrapping_add(1);
        if self.inset_checks % 120 == 0 {
            let insets = current_insets(&self.settings);
            if insets != self.insets {
                self.insets = insets;
                apply_insets(&self.window, insets);
            }
            if self.watched.changed() {
                self.reload_theme();
                if let Ok(text) = std::fs::read_to_string(theme::dnd_path()) {
                    let on = text.trim() == "1"
                        || text.trim().eq_ignore_ascii_case("on")
                        || text.trim().eq_ignore_ascii_case("true");
                    self.dnd.store(on, Ordering::SeqCst);
                }
                self.watched = Watched::capture();
            }
        }

        let mut finished = Vec::new();
        for card in &mut self.cards {
            step_motion(card);
            update_hover(card);
            if !card.leaving && !card.paused.get() {
                if let Some(total) = card.expire_ms {
                    card.remaining_ms = (card.remaining_ms - 16.0).max(0.0);
                    let fraction = if total == 0 {
                        0.0
                    } else {
                        (card.remaining_ms / total as f64).clamp(0.0, 1.0)
                    };
                    let width = ((CARD_WIDTH as f64) * fraction).round() as i32;
                    card.progress.set_size_request(width.max(0), 2);
                    if card.remaining_ms <= 0.0 {
                        finished.push(card.id);
                    }
                }
            }
        }
        for id in finished {
            self.user_close(id, REASON_EXPIRED);
        }
        self.cards.retain(|card| {
            let done = card.leaving && card.motion.start.elapsed() >= card.motion.duration;
            if done {
                self.column.remove(&card.slide);
            }
            !done
        });
        self.sync_window();
    }

    fn upsert(&mut self, note: Note) {
        if let Some(card) = self
            .cards
            .iter_mut()
            .find(|card| card.id == note.id && !card.leaving)
        {
            refresh_card(card, &note);
            return;
        }
        while self.cards.iter().filter(|card| !card.leaving).count() >= MAX_CARDS {
            if let Some(oldest) = self
                .cards
                .iter()
                .find(|card| !card.leaving)
                .map(|card| card.id)
            {
                self.user_close(oldest, REASON_EXPIRED);
            } else {
                break;
            }
        }
        let card = build_card(&note, self.commands.clone());
        self.column.append(&card.slide);
        self.cards.push(card);
        self.sync_window();
    }

    fn begin_leave(&mut self, id: u32) {
        if let Some(card) = self
            .cards
            .iter_mut()
            .find(|card| card.id == id && !card.leaving)
        {
            card.leaving = true;
            card.motion = Motion {
                start: Instant::now(),
                duration: SLIDE_OUT,
                leaving: true,
            };
        }
    }

    fn user_close(&mut self, id: u32, reason: u32) {
        if self.cards.iter().any(|card| card.id == id && !card.leaving) {
            self.begin_leave(id);
            let _ = self.commands.send(UiCommand::Closed { id, reason });
        }
    }

    fn reload_theme(&self) {
        let theme = theme::load_theme();
        eprintln!("den-den-mushi: theme {}", theme.source);
        load_css(&self.provider, &theme.css);
    }

    fn sync_window(&self) {
        let visible = !self.cards.is_empty();
        if self.window.is_visible() != visible {
            self.window.set_visible(visible);
        }
    }
}

impl Watched {
    fn capture() -> Self {
        let style = theme::style_paths()
            .into_iter()
            .map(|path| {
                let modified = std::fs::metadata(&path)
                    .and_then(|meta| meta.modified())
                    .ok();
                (path, modified)
            })
            .collect();
        let theme_json = std::fs::metadata(theme::theme_json_path())
            .and_then(|meta| meta.modified())
            .ok();
        let dnd = std::fs::metadata(theme::dnd_path())
            .and_then(|meta| meta.modified())
            .ok();
        Self {
            style,
            theme_json,
            dnd,
        }
    }

    fn changed(&self) -> bool {
        let fresh = Self::capture();
        fresh.style != self.style || fresh.theme_json != self.theme_json || fresh.dnd != self.dnd
    }
}

fn current_insets(settings: &Settings) -> Insets {
    let gaps = hyprctl(&["-j", "getoption", "general:gaps_out"]);
    let monitors = hyprctl(&["-j", "monitors"]);
    gaps::resolve_insets(settings.margin, gaps.as_deref(), monitors.as_deref())
}

fn hyprctl(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("hyprctl")
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn apply_insets(window: &gtk::ApplicationWindow, insets: Insets) {
    if window.is_layer_window() {
        window.set_margin(Edge::Top, insets.top);
        window.set_margin(Edge::Right, insets.right);
        window.set_margin(Edge::Bottom, 0);
        window.set_margin(Edge::Left, 0);
    }
}

fn load_css(provider: &gtk::CssProvider, css: &str) {
    provider.load_from_string(css);
}

fn step_motion(card: &mut LiveCard) {
    let elapsed = card.motion.start.elapsed().as_secs_f64();
    let duration = card.motion.duration.as_secs_f64().max(0.001);
    let t = (elapsed / duration).clamp(0.0, 1.0);
    let eased = 1.0 - (1.0 - t).powi(3);
    if card.motion.leaving {
        card.slide.set_offset(SLIDE_DISTANCE * eased);
        card.slide.set_opacity(1.0 - eased);
    } else {
        card.slide.set_offset(SLIDE_DISTANCE * (1.0 - eased));
        card.slide.set_opacity(eased);
    }
}

fn refresh_card(card: &mut LiveCard, note: &Note) {
    card.app.set_text(&display_app(&note.app_name));
    card.title.set_text(&note.summary);
    card.title.set_visible(!note.summary.is_empty());
    set_body(&card.body, &note.body);
    card.body.set_visible(!note.body.is_empty());
    card.expire_ms = note.expire_ms;
    card.remaining_ms = note.expire_ms.unwrap_or(0) as f64;
    card.progress.set_visible(note.expire_ms.is_some());
    card.progress.set_size_request(CARD_WIDTH, 2);
    card.has_default.set(note.has_default);
    card.root.remove_css_class("critical");
    card.root.remove_css_class("battery");
    if note.urgency == crate::model::Urgency::Critical {
        card.root.add_css_class("critical");
    } else if note.battery {
        card.root.add_css_class("battery");
    }
    refill_icon(&card.icon_slot, note);
    let commands = card.commands.clone();
    let id = card.id;
    refill_actions(&card.actions, note, move |key| {
        let _ = commands.send(UiCommand::Action { id, key });
    });
}

fn build_card(note: &Note, commands: tokio::sync::mpsc::UnboundedSender<UiCommand>) -> LiveCard {
    let root = gtk::Box::new(Orientation::Vertical, 0);
    root.add_css_class("card");
    root.set_size_request(CARD_WIDTH, -1);
    root.set_overflow(Overflow::Hidden);
    if note.urgency == crate::model::Urgency::Critical {
        root.add_css_class("critical");
    } else if note.battery {
        root.add_css_class("battery");
    }

    let row = gtk::Box::new(Orientation::Horizontal, 0);
    let edge = gtk::Box::new(Orientation::Vertical, 0);
    edge.add_css_class("edge");
    edge.set_size_request(2, -1);
    edge.set_vexpand(true);

    let content = gtk::Box::new(Orientation::Vertical, 0);
    content.add_css_class("content");
    content.set_hexpand(true);

    let header = gtk::Box::new(Orientation::Horizontal, 6);
    let icon_slot = gtk::Box::new(Orientation::Horizontal, 0);
    icon_slot.set_valign(Align::Center);
    refill_icon(&icon_slot, note);

    let app = gtk::Label::new(Some(&display_app(&note.app_name)));
    app.add_css_class("app");
    app.set_halign(Align::Start);
    app.set_hexpand(true);
    app.set_ellipsize(gtk::pango::EllipsizeMode::End);
    app.set_xalign(0.0);

    let time = gtk::Label::new(Some(&format_time(note.created_unix)));
    time.add_css_class("time");
    time.set_valign(Align::Center);

    let close = gtk::Button::with_label("×");
    close.add_css_class("close");
    close.set_valign(Align::Center);
    // Hidden until the pointer is over the card. GTK 4.14 does not let a
    // later CSS opacity rule override `button.close { opacity: 0 }`, so the
    // daemon sets the widget opacity directly. can_target stays off so the
    // invisible button cannot take the click.
    close.set_opacity(0.0);
    close.set_can_target(false);
    let commands_close = commands.clone();
    let close_id = note.id;
    close.connect_clicked(move |_| {
        let _ = commands_close.send(UiCommand::Closed {
            id: close_id,
            reason: REASON_DISMISSED,
        });
    });

    header.append(&icon_slot);
    header.append(&app);
    header.append(&time);
    header.append(&close);

    let title = gtk::Label::new(Some(&note.summary));
    title.add_css_class("title");
    clamp_lines(&title, 1);
    title.set_visible(!note.summary.is_empty());

    let body = gtk::Label::new(None);
    body.add_css_class("body");
    clamp_lines(&body, 3);
    set_body(&body, &note.body);
    body.set_visible(!note.body.is_empty());

    let actions = gtk::Box::new(Orientation::Horizontal, 6);
    actions.add_css_class("actions");
    actions.set_visible(!note.actions.is_empty());
    let commands_actions = commands.clone();
    let action_id = note.id;
    refill_actions(&actions, note, move |key| {
        let _ = commands_actions.send(UiCommand::Action { id: action_id, key });
    });

    content.append(&header);
    content.append(&title);
    content.append(&body);
    content.append(&actions);
    row.append(&edge);
    row.append(&content);

    let progress_track = gtk::Box::new(Orientation::Horizontal, 0);
    progress_track.add_css_class("progress-track");
    let progress = gtk::Box::new(Orientation::Horizontal, 0);
    progress.add_css_class("progress-fill");
    progress.set_size_request(CARD_WIDTH, 2);
    progress.set_halign(Align::Start);
    progress_track.append(&progress);
    progress_track.set_visible(note.expire_ms.is_some());

    root.append(&row);
    root.append(&progress_track);

    let slide = SlideBin::new();
    slide.add_css_class("toast");
    slide.set_overflow(Overflow::Hidden);
    slide.set_child(Some(root.upcast_ref()));
    slide.set_offset(SLIDE_DISTANCE);
    slide.set_opacity(0.0);

    let paused = Rc::new(Cell::new(false));

    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    let commands_click = commands.clone();
    let click_id = note.id;
    let has_default = Rc::new(Cell::new(note.has_default));
    let has_default_click = Rc::clone(&has_default);
    let content_for_pick = content.clone();
    click.connect_released(move |gesture, _n, x, y| {
        if let Some(widget) = content_for_pick.pick(x, y, gtk::PickFlags::DEFAULT) {
            if is_button(&widget) {
                return;
            }
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if has_default_click.get() {
            let _ = commands_click.send(UiCommand::Action {
                id: click_id,
                key: "default".into(),
            });
        }
    });
    content.add_controller(click);

    let right = gtk::GestureClick::new();
    right.set_button(gdk::BUTTON_SECONDARY);
    let commands_right = commands.clone();
    let right_id = note.id;
    right.connect_pressed(move |gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let _ = commands_right.send(UiCommand::Closed {
            id: right_id,
            reason: REASON_DISMISSED,
        });
    });
    root.add_controller(right);

    LiveCard {
        id: note.id,
        slide,
        root,
        app,
        title,
        body,
        actions,
        progress,
        close,
        icon_slot,
        expire_ms: note.expire_ms,
        remaining_ms: note.expire_ms.unwrap_or(0) as f64,
        paused,
        leaving: false,
        motion: Motion {
            start: Instant::now(),
            duration: SLIDE_IN,
            leaving: false,
        },
        has_default,
        commands,
    }
}

fn is_button(widget: &gtk::Widget) -> bool {
    let mut current = Some(widget.clone());
    while let Some(widget) = current {
        if widget.is::<gtk::Button>() {
            return true;
        }
        current = widget.parent();
    }
    false
}

fn clamp_lines(label: &gtk::Label, lines: i32) {
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_lines(lines);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    // A tiny natural width lets the 380px card allocate the label, so the
    // ellipsis happens at the card edge instead of the card growing.
    label.set_max_width_chars(1);
    label.set_hexpand(true);
    label.set_halign(Align::Fill);
    label.set_xalign(0.0);
}

fn update_hover(card: &LiveCard) {
    let over = pointer_over(&card.root);
    if over {
        card.root.add_css_class("hover");
        card.close.add_css_class("shown");
        card.close.set_opacity(1.0);
        card.close.set_can_target(true);
    } else {
        card.root.remove_css_class("hover");
        card.close.remove_css_class("shown");
        card.close.set_opacity(0.0);
        card.close.set_can_target(false);
    }
    card.paused.set(over);
}

/// True when the seat pointer is inside `widget`, including its children.
/// Enter/leave on the card itself flickers when the pointer moves onto the
/// close button, so the hover state is the pointer position.
fn pointer_over(widget: &impl IsA<gtk::Widget>) -> bool {
    let widget = widget.upcast_ref::<gtk::Widget>();
    let Some(root) = widget.root() else {
        return false;
    };
    let Some(native) = root.native() else {
        return false;
    };
    let Some(surface) = native.surface() else {
        return false;
    };
    let display = widget.display();
    let Some(seat) = display.default_seat() else {
        return false;
    };
    let Some(pointer) = seat.pointer() else {
        return false;
    };
    let Some((x, y, _)) = surface.device_position(&pointer) else {
        return false;
    };
    let root_widget = root.upcast_ref::<gtk::Widget>();
    let Some(point) = root_widget.compute_point(widget, &graphene::Point::new(x as f32, y as f32))
    else {
        return false;
    };
    let width = widget.width() as f32;
    let height = widget.height() as f32;
    point.x() >= 0.0 && point.y() >= 0.0 && point.x() < width && point.y() < height
}

fn format_time(unix: i64) -> String {
    glib::DateTime::from_unix_local(unix)
        .and_then(|date| date.format("%a %l:%M %p"))
        .map(|text| text.replace("  ", " "))
        .unwrap_or_default()
}

fn set_body(label: &gtk::Label, body: &str) {
    let prepared = crate::model::prepare_body(body);
    if prepared.contains('<') && gtk::pango::parse_markup(&prepared, '\0').is_ok() {
        label.set_markup(&prepared);
    } else {
        label.set_text(&prepared);
    }
}

fn refill_icon(slot: &gtk::Box, note: &Note) {
    while let Some(child) = slot.first_child() {
        slot.remove(&child);
    }
    let widget = match &note.icon {
        IconSource::Rgba {
            width,
            height,
            rgba,
        } => texture_image(width, height, rgba),
        IconSource::File(path) => {
            let image = gtk::Image::from_file(path);
            image.set_pixel_size(16);
            image.upcast()
        }
        IconSource::IconName(name) if icon_exists(name) => {
            let image = gtk::Image::from_icon_name(name);
            image.set_pixel_size(16);
            image.upcast()
        }
        IconSource::IconName(_) | IconSource::Glyph(_) => glyph_widget(&note.glyph_fallback),
    };
    slot.append(&widget);
}

fn texture_image(width: &i32, height: &i32, rgba: &[u8]) -> gtk::Widget {
    let bytes = glib::Bytes::from(rgba);
    let texture = gdk::MemoryTexture::new(
        *width,
        *height,
        MemoryFormat::R8g8b8a8,
        &bytes,
        (*width * 4) as usize,
    );
    let image = gtk::Image::from_paintable(Some(&texture));
    image.set_pixel_size(16);
    image.upcast()
}

fn glyph_widget(text: &str) -> gtk::Widget {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("glyph");
    label.upcast()
}

fn icon_exists(name: &str) -> bool {
    gdk::Display::default()
        .map(|display| gtk::IconTheme::for_display(&display).has_icon(name))
        .unwrap_or(false)
}

fn refill_actions(box_: &gtk::Box, note: &Note, invoke: impl Fn(String) + Clone + 'static) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
    for (key, label) in &note.actions {
        let button = gtk::Button::with_label(label);
        button.add_css_class("action");
        let invoke = invoke.clone();
        let key = key.clone();
        button.connect_clicked(move |_| invoke(key.clone()));
        box_.append(&button);
    }
    box_.set_visible(!note.actions.is_empty());
}

mod slide {
    use super::*;

    #[derive(Default)]
    pub struct SlideBin {
        pub(super) offset: Cell<f64>,
        pub(super) child: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SlideBin {
        const NAME: &'static str = "DenDenSlideBin";
        type Type = super::SlideBin;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SlideBin {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_layout_manager(Some(gtk::BinLayout::new()));
            obj.set_overflow(Overflow::Hidden);
        }

        fn dispose(&self) {
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for SlideBin {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let offset = self.offset.get() as f32;
            if offset != 0.0 {
                snapshot.save();
                snapshot.translate(&graphene::Point::new(offset, 0.0));
            }
            if let Some(child) = self.child.borrow().as_ref() {
                self.obj().snapshot_child(child, snapshot);
            }
            if offset != 0.0 {
                snapshot.restore();
            }
        }
    }
}

glib::wrapper! {
    pub struct SlideBin(ObjectSubclass<slide::SlideBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SlideBin {
    fn new() -> Self {
        glib::Object::builder().build()
    }

    fn set_child(&self, child: Option<&gtk::Widget>) {
        let imp = self.imp();
        if let Some(old) = imp.child.borrow_mut().take() {
            old.unparent();
        }
        if let Some(child) = child {
            child.set_parent(self);
            *imp.child.borrow_mut() = Some(child.clone());
        }
    }

    fn set_offset(&self, offset: f64) {
        self.imp().offset.set(offset);
        self.queue_draw();
    }
}

impl Default for SlideBin {
    fn default() -> Self {
        Self::new()
    }
}
