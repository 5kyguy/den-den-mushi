//! Layer-shell window. Cards stack in the top-right. Enter slides in from
//! +24px, exit slides back out, and a leaving card lets the ones below
//! spring upward. A replace crossfades the face in place.

use gtk4 as gtk;

use crate::gaps::{self, Insets};
use crate::model::{
    display_app, relative_time, IconSource, Note, REASON_DISMISSED, REASON_EXPIRED,
};
use crate::motion::{self, YTrack};
use crate::panel_ui::{self, Panel};
use crate::server::{UiCommand, UiEvent};
use crate::theme::{self, Settings};
use gtk::gdk::MemoryFormat;
use gtk::glib::{self, ControlFlow};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, graphene, Align, Orientation, Overflow};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const CARD_WIDTH: i32 = 380;
const MAX_CARDS: usize = 8;
/// Matches `.glyph` font-size in the theme stylesheet.
const GLYPH_PX: i32 = 11;

pub fn run(
    settings: Settings,
    dnd: std::sync::Arc<AtomicBool>,
    events: Receiver<UiEvent>,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
    runtime: tokio::runtime::Handle,
    ui_tx: std::sync::mpsc::Sender<UiEvent>,
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
        let panel_calls = panel_ui::spawn_panel_client(runtime.clone(), ui_tx.clone());
        let shell = Shell::new(
            application,
            settings.clone(),
            dnd.clone(),
            commands.clone(),
            panel_calls,
        );
        let shell = Rc::new(RefCell::new(shell));
        shell.borrow_mut().self_weak = Rc::downgrade(&shell);
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
    time: gtk::Label,
    corner: gtk::Stack,
    actions: gtk::Box,
    progress: gtk::Box,
    progress_track: gtk::Box,
    created_unix: i64,
    close: gtk::Button,
    icon_slot: gtk::Box,
    expire_ms: Option<u64>,
    remaining_ms: f64,
    paused: Rc<Cell<bool>>,
    leaving: bool,
    motion: Motion,
    track: YTrack,
    fade: CrossfadeBin,
    crossfade_start: Option<Instant>,
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
    column: ToastColumn,
    cards: Vec<LiveCard>,
    self_weak: Weak<RefCell<Shell>>,
    frame_armed: bool,
    last_integrate: Instant,
    last_frame_us: i64,
    provider: gtk::CssProvider,
    settings: Settings,
    dnd: std::sync::Arc<AtomicBool>,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
    insets: Insets,
    inset_checks: u32,
    watched: Watched,
    panel: Panel,
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
        panel_calls: tokio::sync::mpsc::UnboundedSender<panel_ui::PanelCall>,
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

        // The 8px stack gap is applied while laying cards out. It matches
        // `.toast { margin-bottom: 8px }` in the stylesheet.
        let column = ToastColumn::new();
        column.set_halign(Align::End);
        column.set_valign(Align::Start);
        window.set_child(Some(&column));
        if !settings.motion {
            eprintln!("den-den-mushi: motion off");
        }

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
        let panel = Panel::new(application, &settings, panel_calls);
        panel.apply_insets(insets);

        Self {
            window,
            column,
            cards: Vec::new(),
            self_weak: Weak::new(),
            frame_armed: false,
            last_integrate: Instant::now(),
            last_frame_us: 0,
            provider,
            settings,
            dnd,
            commands,
            insets,
            inset_checks: 0,
            watched: Watched::capture(),
            panel,
        }
    }

    fn tick(&mut self, events: Vec<UiEvent>) {
        for event in events {
            match event {
                UiEvent::Upsert(note) => self.upsert(note),
                UiEvent::Dismiss { id } => self.begin_leave(id),
                UiEvent::ReloadTheme => self.reload_theme(),
                UiEvent::TogglePanel => self.panel.toggle(),
                UiEvent::PanelState {
                    history_json,
                    rules_json,
                    dnd,
                } => self.panel.set_state(&history_json, &rules_json, dnd),
            }
        }

        self.inset_checks = self.inset_checks.wrapping_add(1);
        if self.inset_checks % 120 == 0 {
            let insets = current_insets(&self.settings);
            if insets != self.insets {
                self.insets = insets;
                apply_insets(&self.window, insets);
                self.panel.apply_insets(insets);
            }
            if self.watched.changed() {
                self.reload_theme();
                let on = theme::read_dnd();
                if on != self.dnd.load(Ordering::SeqCst) {
                    let _ = self.commands.send(UiCommand::SetDoNotDisturb(on));
                }
                self.watched = Watched::capture();
            }
        }

        let mut finished = Vec::new();
        for card in &mut self.cards {
            update_hover(card);
            update_relative_time(card);
            if !card.leaving && !card.paused.get() {
                if let Some(total) = card.expire_ms {
                    card.remaining_ms = (card.remaining_ms - 16.0).max(0.0);
                    let fraction = if total == 0 {
                        0.0
                    } else {
                        (card.remaining_ms / total as f64).clamp(0.0, 1.0)
                    };
                    // Use the track's real width so a full bar reaches both
                    // rounded corners instead of a fixed card constant.
                    let track = card.progress_track.width().max(1);
                    let width = ((track as f64) * fraction).round() as i32;
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
        // The frame clock integrates the spring. This 16ms tick is the
        // fallback when that clock has not run, and it still owns expiry.
        if self.cards_need_frames() && self.last_integrate.elapsed() >= Duration::from_millis(30) {
            let dt = motion::animation_dt(self.last_integrate.elapsed().as_secs_f64());
            self.integrate(dt);
        }
        if self.cards_need_frames() {
            self.arm_frame();
        }
        self.sync_input_region();
        self.sync_window();
        self.panel.tick();
    }

    fn on_frame(&mut self, clock: &gdk::FrameClock) -> bool {
        let now = clock.frame_time();
        let dt = if self.last_frame_us == 0 || now <= self.last_frame_us {
            motion::animation_dt(self.last_integrate.elapsed().as_secs_f64())
        } else {
            let secs = (now - self.last_frame_us) as f64 / 1_000_000.0;
            motion::animation_dt(secs)
        };
        self.last_frame_us = now;
        self.integrate(dt);
        let keep = self.cards_need_frames();
        if !keep {
            self.frame_armed = false;
        }
        self.sync_input_region();
        keep
    }

    fn arm_frame(&mut self) {
        if self.frame_armed {
            return;
        }
        self.frame_armed = true;
        let weak = self.self_weak.clone();
        self.window.add_tick_callback(move |_widget, clock| {
            let Some(shell) = weak.upgrade() else {
                return ControlFlow::Break;
            };
            if shell.borrow_mut().on_frame(clock) {
                ControlFlow::Continue
            } else {
                ControlFlow::Break
            }
        });
    }

    fn integrate(&mut self, dt: f64) {
        if dt <= 0.0 {
            return;
        }
        self.last_integrate = Instant::now();
        let enabled = self.settings.motion;
        self.retarget();
        for card in &mut self.cards {
            if enabled {
                card.track.spring.step(dt);
                if card.track.spring.resting() {
                    card.track.spring.snap();
                }
            } else {
                card.track.spring.snap();
            }
            card.slide.set_layout_y(card.track.spring.position);
            step_slide(card);
            step_crossfade(card);
        }
        let column = self.column.clone();
        self.cards.retain(|card| {
            let done = card.leaving
                && motion::unit_progress(card.motion.start.elapsed(), card.motion.duration) >= 1.0;
            if done {
                column.remove_card(&card.slide);
            }
            !done
        });
        self.retarget();
        for card in &mut self.cards {
            if !enabled {
                card.track.spring.snap();
            }
            card.slide.set_layout_y(card.track.spring.position);
        }
        self.column.queue_resize();
        self.sync_window();
    }

    fn cards_need_frames(&self) -> bool {
        self.cards.iter().any(|card| {
            let sliding =
                motion::unit_progress(card.motion.start.elapsed(), card.motion.duration) < 1.0;
            let fading = card.crossfade_start.is_some_and(|start| {
                motion::unit_progress(start.elapsed(), motion::crossfade_duration(true)) < 1.0
            });
            sliding
                || fading
                || card.leaving
                || !card.track.spring.resting()
                || !card.track.anchored
        })
    }

    fn retarget(&mut self) {
        let heights: Vec<f64> = self.cards.iter().map(card_height).collect();
        let elapsed: Vec<Duration> = self
            .cards
            .iter()
            .map(|card| card.motion.start.elapsed())
            .collect();
        let leaving: Vec<bool> = self
            .cards
            .iter()
            .zip(&elapsed)
            .map(|(card, elapsed)| {
                card.leaving && !motion::holds_stack_slot(true, *elapsed, self.settings.motion)
            })
            .collect();
        let any_leaving = self.cards.iter().any(|card| card.leaving);
        let targets = motion::restack_targets(&heights, &leaving, motion::STACK_GAP);
        for (card, target) in self.cards.iter_mut().zip(targets) {
            if card.leaving {
                continue;
            }
            if let Some(y) = target {
                // Motion off snaps inside `follow` because `any_leaving` is
                // ignored, and `integrate` snaps the spring on top of that.
                card.track.follow(y, any_leaving && self.settings.motion);
            }
        }
    }

    fn upsert(&mut self, note: Note) {
        if let Some(index) = self
            .cards
            .iter()
            .position(|card| card.id == note.id && !card.leaving)
        {
            if self.settings.motion {
                self.cards[index].begin_replace(&note);
                self.arm_frame();
            } else {
                refresh_card(&mut self.cards[index], &note);
            }
            self.retarget();
            self.place_cards();
            self.column.queue_resize();
            self.sync_input_region();
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
        let card = build_card(&note, self.commands.clone(), self.settings.motion);
        self.column.add_card(&card.slide);
        self.cards.push(card);
        self.retarget();
        self.place_cards();
        self.column.queue_resize();
        self.sync_window();
        self.arm_frame();
    }

    fn place_cards(&self) {
        for card in &self.cards {
            card.slide.set_layout_y(card.track.spring.position);
        }
    }

    fn begin_leave(&mut self, id: u32) {
        let Some(index) = self
            .cards
            .iter()
            .position(|card| card.id == id && !card.leaving)
        else {
            return;
        };
        {
            let card = &mut self.cards[index];
            card.leaving = true;
            card.track.spring.target = card.track.spring.position;
            card.track.spring.velocity = 0.0;
            card.motion = Motion {
                start: Instant::now(),
                duration: motion::exit_duration(self.settings.motion),
                leaving: true,
            };
            // Draw the leaving card above the ones that spring up into its slot.
            let slide = card.slide.clone();
            self.column.remove_card(&slide);
            self.column.add_card(&slide);
        }
        self.retarget();
        self.place_cards();
        self.arm_frame();
    }

    fn sync_input_region(&self) {
        let Some(surface) = self.window.surface() else {
            return;
        };
        let scale = surface.scale_factor();
        let region = cairo::Region::create();
        let root = self.window.upcast_ref::<gtk::Widget>();
        for card in &self.cards {
            let Some(bounds) = card.slide.compute_bounds(root) else {
                continue;
            };
            let Some((x, y, width, height)) = motion::visible_input_rect(
                f64::from(bounds.x()),
                f64::from(bounds.y()),
                f64::from(bounds.width()),
                f64::from(bounds.height()),
                card.slide.offset(),
                card.slide.opacity(),
                scale,
            ) else {
                continue;
            };
            let _ = region.union_rectangle(&cairo::RectangleInt::new(x, y, width, height));
        }
        surface.set_input_region(&region);
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

fn step_slide(card: &mut LiveCard) {
    let t = motion::unit_progress(card.motion.start.elapsed(), card.motion.duration);
    if card.motion.leaving {
        card.slide.set_offset(motion::exit_offset(t));
        card.slide.set_opacity(motion::exit_opacity(t));
    } else {
        card.slide.set_offset(motion::enter_offset(t));
        card.slide.set_opacity(motion::enter_opacity(t));
    }
}

fn step_crossfade(card: &mut LiveCard) {
    let Some(start) = card.crossfade_start else {
        return;
    };
    let t = motion::unit_progress(start.elapsed(), motion::crossfade_duration(true));
    card.fade.set_blend(motion::crossfade_blend(t));
    if t >= 1.0 {
        card.crossfade_start = None;
    }
}

fn card_height(card: &LiveCard) -> f64 {
    let (_, natural, _, _) = card.slide.measure(Orientation::Vertical, CARD_WIDTH);
    if natural > 0 {
        f64::from(natural)
    } else {
        f64::from(card.slide.height().max(1))
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
    card.created_unix = note.created_unix;
    card.time
        .set_text(&relative_time(note.created_unix, unix_now()));
    card.progress_track.set_visible(note.expire_ms.is_some());
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

fn build_card(
    note: &Note,
    commands: tokio::sync::mpsc::UnboundedSender<UiCommand>,
    motion_enabled: bool,
) -> LiveCard {
    let body = build_body(note, commands.clone());
    let fade = CrossfadeBin::new();
    fade.set_current(body.root.upcast_ref());
    let slide = SlideBin::new();
    slide.add_css_class("toast");
    slide.set_overflow(Overflow::Hidden);
    slide.set_child(Some(fade.upcast_ref()));
    if motion_enabled {
        slide.set_offset(motion::SLIDE_PX);
        slide.set_opacity(0.0);
    } else {
        slide.set_offset(0.0);
        slide.set_opacity(1.0);
    }
    LiveCard {
        id: note.id,
        slide,
        root: body.root,
        app: body.app,
        title: body.title,
        body: body.body,
        time: body.time,
        corner: body.corner,
        actions: body.actions,
        progress: body.progress,
        progress_track: body.progress_track,
        created_unix: note.created_unix,
        close: body.close,
        icon_slot: body.icon_slot,
        expire_ms: note.expire_ms,
        remaining_ms: note.expire_ms.unwrap_or(0) as f64,
        paused: body.paused,
        leaving: false,
        motion: Motion {
            start: Instant::now(),
            duration: motion::enter_duration(motion_enabled),
            leaving: false,
        },
        track: YTrack::new(),
        fade,
        crossfade_start: None,
        has_default: body.has_default,
        commands,
    }
}

fn build_body(note: &Note, commands: tokio::sync::mpsc::UnboundedSender<UiCommand>) -> CardBody {
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
    header.add_css_class("header");
    let icon_slot = gtk::Box::new(Orientation::Horizontal, 0);
    icon_slot.set_valign(Align::Center);
    refill_icon(&icon_slot, note);

    // Natural width, and ellipsize only when the row is actually out of
    // room. Start-align plus ellipsize made GTK use the ellipsis as the
    // width, so "SPOTIFY" drew as "SPOTIF…".
    let app = gtk::Label::new(Some(&display_app(&note.app_name)));
    app.add_css_class("app");
    app.set_halign(Align::Start);
    app.set_hexpand(false);
    app.set_xalign(0.0);
    // No ellipsize here. With ellipsize on, GTK sizes the label to the
    // ellipsis and "SPOTIFY" draws as "SPOTIF…". The clock is the hexpand
    // child, so it shrinks first if the row actually runs out of room.

    let time = gtk::Label::new(Some(&relative_time(note.created_unix, unix_now())));
    time.add_css_class("time");
    time.set_valign(Align::Center);
    time.set_halign(Align::End);
    time.set_xalign(1.0);

    let close = gtk::Button::with_label("×");
    close.add_css_class("close");
    close.set_valign(Align::Center);
    close.set_halign(Align::End);
    // The button is not the visible stack child until hover, so it must
    // not take clicks while the time is showing.
    close.set_can_target(false);
    let commands_close = commands.clone();
    let close_id = note.id;
    close.connect_clicked(move |_| {
        let _ = commands_close.send(UiCommand::Closed {
            id: close_id,
            reason: REASON_DISMISSED,
        });
    });

    // One slot at the right inset. Idle shows the time flush with that
    // inset; hover crossfades to the ×. A hidden button beside the time
    // left a hollow gap.
    let corner = gtk::Stack::new();
    corner.add_css_class("corner");
    corner.set_transition_type(gtk::StackTransitionType::Crossfade);
    corner.set_transition_duration(150);
    corner.set_hhomogeneous(false);
    corner.set_vhomogeneous(false);
    corner.set_interpolate_size(true);
    corner.set_halign(Align::End);
    corner.set_valign(Align::Center);
    corner.add_named(&time, Some("time"));
    corner.add_named(&close, Some("close"));
    corner.set_visible_child_name("time");

    let spacer = gtk::Box::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    header.append(&icon_slot);
    header.append(&app);
    header.append(&spacer);
    header.append(&corner);

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
    progress_track.set_hexpand(true);
    // Clip the bar to the card's bottom radius. Overflow on the card alone
    // was leaving a square end short of the corner.
    progress_track.set_overflow(Overflow::Hidden);
    let progress = gtk::Box::new(Orientation::Horizontal, 0);
    progress.add_css_class("progress-fill");
    progress.set_hexpand(false);
    progress.set_halign(Align::Start);
    progress.set_vexpand(false);
    progress_track.append(&progress);
    progress_track.set_visible(note.expire_ms.is_some());

    root.append(&row);
    root.append(&progress_track);

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

    // Right click dismisses this card with reason 2, the same close the
    // `dismiss` command uses for the newest card.
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

    CardBody {
        root,
        app,
        title,
        body,
        time,
        corner,
        actions,
        progress,
        progress_track,
        close,
        icon_slot,
        paused,
        has_default,
    }
}

impl LiveCard {
    /// `replaces_id` and a stack tag both land here: the slot stays put and
    /// the face crossfades. The outer height stays at the current card so
    /// neighbors do not move during the 120ms.
    fn begin_replace(&mut self, note: &Note) {
        let body = build_body(note, self.commands.clone());
        let old_height = self.slide.height();
        self.fade.start(body.root.upcast_ref(), old_height);
        self.crossfade_start = Some(Instant::now());
        self.root = body.root;
        self.app = body.app;
        self.title = body.title;
        self.body = body.body;
        self.time = body.time;
        self.corner = body.corner;
        self.actions = body.actions;
        self.progress = body.progress;
        self.progress_track = body.progress_track;
        self.close = body.close;
        self.icon_slot = body.icon_slot;
        self.paused = body.paused;
        self.has_default = body.has_default;
        self.expire_ms = note.expire_ms;
        self.remaining_ms = note.expire_ms.unwrap_or(0) as f64;
        self.created_unix = note.created_unix;
    }
}

struct CardBody {
    root: gtk::Box,
    app: gtk::Label,
    title: gtk::Label,
    body: gtk::Label,
    time: gtk::Label,
    corner: gtk::Stack,
    actions: gtk::Box,
    progress: gtk::Box,
    progress_track: gtk::Box,
    close: gtk::Button,
    icon_slot: gtk::Box,
    paused: Rc<Cell<bool>>,
    has_default: Rc<Cell<bool>>,
}

pub(crate) fn is_button(widget: &gtk::Widget) -> bool {
    let mut current = Some(widget.clone());
    while let Some(widget) = current {
        if widget.is::<gtk::Button>() {
            return true;
        }
        current = widget.parent();
    }
    false
}

pub(crate) fn clamp_lines(label: &gtk::Label, lines: i32) {
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
    if over != card.paused.get() {
        if over {
            card.root.add_css_class("hover");
            card.close.add_css_class("shown");
            card.close.set_can_target(true);
            card.corner.set_visible_child_name("close");
        } else {
            card.root.remove_css_class("hover");
            card.close.remove_css_class("shown");
            card.close.set_can_target(false);
            card.corner.set_visible_child_name("time");
        }
    }
    card.paused.set(over);
}

/// True when the seat pointer is inside `widget`, including its children.
/// Enter/leave on the card itself flickers when the pointer moves onto the
/// close button, so the hover state is the pointer position.
pub(crate) fn pointer_over(widget: &impl IsA<gtk::Widget>) -> bool {
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

pub(crate) fn unix_now() -> i64 {
    glib::real_time() / 1_000_000
}

fn update_relative_time(card: &LiveCard) {
    let next = relative_time(card.created_unix, unix_now());
    if card.time.text().as_str() != next {
        card.time.set_text(&next);
    }
}

pub(crate) fn set_body(label: &gtk::Label, body: &str) {
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
        IconSource::File(path) => rounded_icon(gtk::Image::from_file(path)),
        IconSource::IconName(name) if icon_exists(name) => {
            rounded_icon(gtk::Image::from_icon_name(name))
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
    rounded_icon(gtk::Image::from_paintable(Some(&texture)))
}

fn rounded_icon(image: gtk::Image) -> gtk::Widget {
    image.set_pixel_size(GLYPH_PX);
    image.add_css_class("app-icon");
    let clip = gtk::Box::new(Orientation::Horizontal, 0);
    clip.add_css_class("icon-clip");
    clip.set_overflow(Overflow::Hidden);
    clip.set_size_request(GLYPH_PX, GLYPH_PX);
    clip.set_valign(Align::Center);
    clip.append(&image);
    clip.upcast()
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
        pub(super) layout_y: Cell<f64>,
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
        fn contains(&self, x: f64, y: f64) -> bool {
            let widget = self.obj();
            let width = f64::from(widget.width());
            let height = f64::from(widget.height());
            if widget.opacity() <= 0.01 || x < 0.0 || y < 0.0 || x >= width || y >= height {
                return false;
            }
            // The face is translated right by `offset` and clipped, so the
            // uncovered strip at the left is not part of the card.
            x >= self.offset.get()
        }

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
    pub(crate) fn new() -> Self {
        glib::Object::builder().build()
    }

    pub(crate) fn set_child(&self, child: Option<&gtk::Widget>) {
        let imp = self.imp();
        if let Some(old) = imp.child.borrow_mut().take() {
            old.unparent();
        }
        if let Some(child) = child {
            child.set_parent(self);
            *imp.child.borrow_mut() = Some(child.clone());
        }
    }

    pub(crate) fn set_offset(&self, offset: f64) {
        self.imp().offset.set(offset);
        self.queue_draw();
    }

    pub(crate) fn offset(&self) -> f64 {
        self.imp().offset.get()
    }

    pub(crate) fn set_layout_y(&self, y: f64) {
        self.imp().layout_y.set(y);
    }

    pub(crate) fn layout_y(&self) -> f64 {
        self.imp().layout_y.get()
    }
}

impl Default for SlideBin {
    fn default() -> Self {
        Self::new()
    }
}

mod column {
    use super::*;

    #[derive(Default)]
    pub struct ToastColumn;

    #[glib::object_subclass]
    impl ObjectSubclass for ToastColumn {
        const NAME: &'static str = "DenDenToastColumn";
        type Type = super::ToastColumn;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ToastColumn {
        fn dispose(&self) {
            let obj = self.obj();
            while let Some(child) = obj.first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for ToastColumn {
        fn measure(&self, orientation: Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == Orientation::Horizontal {
                return (CARD_WIDTH, CARD_WIDTH, -1, -1);
            }
            let width = if for_size < 0 { CARD_WIDTH } else { for_size };
            let mut bottom = 0;
            let mut child = self.obj().first_child();
            while let Some(widget) = child {
                let next = widget.next_sibling();
                let y = widget
                    .downcast_ref::<super::SlideBin>()
                    .map(super::SlideBin::layout_y)
                    .unwrap_or(0.0);
                let (_, height, _, _) = widget.measure(Orientation::Vertical, width);
                bottom = bottom.max(y.round() as i32 + height.max(0));
                child = next;
            }
            let bottom = bottom.max(0);
            (bottom, bottom, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            let mut child = self.obj().first_child();
            while let Some(widget) = child {
                let next = widget.next_sibling();
                let y = widget
                    .downcast_ref::<super::SlideBin>()
                    .map(super::SlideBin::layout_y)
                    .unwrap_or(0.0);
                let (_, child_height, _, _) = widget.measure(Orientation::Vertical, width);
                widget.size_allocate(
                    &gtk::Allocation::new(0, y.round() as i32, width, child_height.max(1)),
                    -1,
                );
                child = next;
            }
        }
    }
}

glib::wrapper! {
    pub struct ToastColumn(ObjectSubclass<column::ToastColumn>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ToastColumn {
    fn new() -> Self {
        glib::Object::builder().build()
    }

    fn add_card(&self, card: &SlideBin) {
        if card.parent().is_some() {
            card.unparent();
        }
        card.set_parent(self);
    }

    fn remove_card(&self, card: &SlideBin) {
        if card.parent().is_some() {
            card.unparent();
        }
    }
}

mod fade {
    use super::*;

    pub struct CrossfadeBin {
        pub(super) current: RefCell<Option<gtk::Widget>>,
        pub(super) outgoing: RefCell<Option<gtk::Widget>>,
        pub(super) blend: Cell<f64>,
        pub(super) locked: Cell<i32>,
    }

    impl Default for CrossfadeBin {
        fn default() -> Self {
            Self {
                current: RefCell::new(None),
                outgoing: RefCell::new(None),
                blend: Cell::new(1.0),
                locked: Cell::new(-1),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CrossfadeBin {
        const NAME: &'static str = "DenDenCrossfadeBin";
        type Type = super::CrossfadeBin;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for CrossfadeBin {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_overflow(Overflow::Hidden);
        }

        fn dispose(&self) {
            if let Some(child) = self.current.borrow_mut().take() {
                child.unparent();
            }
            if let Some(child) = self.outgoing.borrow_mut().take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for CrossfadeBin {
        fn measure(&self, orientation: Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == Orientation::Vertical && self.locked.get() > 0 {
                let height = self.locked.get();
                return (height, height, -1, -1);
            }
            self.current
                .borrow()
                .as_ref()
                .map(|child| child.measure(orientation, for_size))
                .unwrap_or((0, 0, -1, -1))
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            let allocation = gtk::Allocation::new(0, 0, width, height);
            if let Some(child) = self.current.borrow().as_ref() {
                child.size_allocate(&allocation, baseline);
            }
            if let Some(child) = self.outgoing.borrow().as_ref() {
                child.size_allocate(&allocation, baseline);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let blend = self.blend.get().clamp(0.0, 1.0) as f32;
            let has_outgoing = self.outgoing.borrow().is_some();
            if has_outgoing && blend < 1.0 {
                if let Some(old) = self.outgoing.borrow().as_ref() {
                    let alpha = 1.0 - blend;
                    if alpha > 0.0 {
                        snapshot.push_opacity(f64::from(alpha));
                        self.obj().snapshot_child(old, snapshot);
                        snapshot.pop();
                    }
                }
            }
            if let Some(current) = self.current.borrow().as_ref() {
                let alpha = if has_outgoing { blend } else { 1.0 };
                if alpha > 0.0 {
                    let pushed = alpha < 1.0;
                    if pushed {
                        snapshot.push_opacity(f64::from(alpha));
                    }
                    self.obj().snapshot_child(current, snapshot);
                    if pushed {
                        snapshot.pop();
                    }
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct CrossfadeBin(ObjectSubclass<fade::CrossfadeBin>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CrossfadeBin {
    fn new() -> Self {
        glib::Object::builder().build()
    }

    fn set_current(&self, child: &gtk::Widget) {
        let imp = self.imp();
        if let Some(old) = imp.current.borrow_mut().take() {
            old.unparent();
        }
        if let Some(old) = imp.outgoing.borrow_mut().take() {
            old.unparent();
        }
        child.set_parent(self);
        *imp.current.borrow_mut() = Some(child.clone());
        imp.blend.set(1.0);
        imp.locked.set(-1);
    }

    /// Keep the outgoing face and fade to `new_child`. `old_height` is the
    /// card's current height, held for the length of the crossfade.
    fn start(&self, new_child: &gtk::Widget, old_height: i32) {
        let imp = self.imp();
        if let Some(previous) = imp.outgoing.borrow_mut().take() {
            previous.unparent();
        }
        if let Some(old) = imp.current.borrow_mut().take() {
            old.set_can_target(false);
            *imp.outgoing.borrow_mut() = Some(old);
        }
        new_child.set_parent(self);
        *imp.current.borrow_mut() = Some(new_child.clone());
        let (_, measured, _, _) = new_child.measure(Orientation::Vertical, CARD_WIDTH);
        let locked = if old_height > 1 {
            old_height
        } else {
            measured.max(1)
        };
        imp.locked.set(locked);
        imp.blend.set(0.0);
        self.queue_resize();
    }

    fn set_blend(&self, blend: f64) {
        let blend = blend.clamp(0.0, 1.0);
        let imp = self.imp();
        imp.blend.set(blend);
        if blend >= 1.0 {
            if let Some(old) = imp.outgoing.borrow_mut().take() {
                old.unparent();
            }
            if imp.locked.replace(-1) >= 0 {
                self.queue_resize();
            }
        }
        self.queue_draw();
    }
}
