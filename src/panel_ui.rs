//! Notification panel.
//!
//! The window is a layer-shell overlay. Every read and write goes through
//! `com.skyguy.DenDenMushi` (`History`, `ClearHistory`, `RemoveHistory`,
//! `InvokeHistory`, `SetDoNotDisturb`, `Rules`, `SetRule`), the same object
//! the CLI uses. `TogglePanel` is the only extra signal into this process:
//! `den-den-mushi panel` calls it, and Esc or a click outside hides the
//! window again.

use crate::gaps::Insets;
use crate::history::parse_entries;
use crate::model::{display_app, relative_time, GLYPH_CHEVRON_DOWN, GLYPH_CHEVRON_RIGHT};
use crate::panel::{entry_urgency, filter_history, group_glyph, group_history, rule_rows, RuleRow};
use crate::rules::{rules_from_json, AppRule, RuleSet, RuleTimeout};
use crate::server::{ControlProxy, UiEvent};
use crate::theme::Settings;
use crate::ui::{self, SlideBin};
use futures_util::StreamExt;
use gtk::gdk;
use gtk::glib::Propagation;
use gtk::prelude::*;
use gtk::{graphene, Align, Orientation, Overflow};
use gtk4 as gtk;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

const PANEL_WIDTH: i32 = 380;
const OPEN_MS: u64 = 200;
const CLOSE_MS: u64 = 140;

#[derive(Debug)]
pub(crate) enum PanelCall {
    ClearHistory,
    RemoveHistory(u32),
    InvokeHistory(u32),
    SetDoNotDisturb(bool),
    SetRule {
        app: String,
        muted: bool,
        timeout: String,
        bypass_dnd: bool,
    },
}

struct PanelModel {
    history: Vec<crate::history::HistoryEntry>,
    rules: Vec<AppRule>,
    query: String,
    displayed_rows: Vec<RuleRow>,
}

struct PanelMotion {
    start: Instant,
    duration: Duration,
    from: f64,
    to: f64,
}

struct HistoryRow {
    root: gtk::Box,
    time: gtk::Label,
    close: gtk::Button,
    corner: gtk::Stack,
    created_unix: i64,
    hovered: bool,
}

struct GroupMark {
    app_name: String,
    glyph: String,
    count: usize,
    critical: bool,
    header: gtk::Box,
}

pub(crate) struct Panel {
    window: gtk::ApplicationWindow,
    slide: SlideBin,
    search: gtk::Entry,
    stack: gtk::Stack,
    empty: gtk::Label,
    history_box: gtk::Box,
    scrolled: gtk::ScrolledWindow,
    pin: gtk::Box,
    pin_glyph: gtk::Label,
    pin_name: gtk::Label,
    pin_count: gtk::Label,
    rules_box: gtk::Box,
    clear: gtk::Button,
    dnd_switch: gtk::Switch,
    dnd_line: gtk::Box,
    rows: Vec<HistoryRow>,
    groups: Vec<GroupMark>,
    model: Rc<RefCell<PanelModel>>,
    updating: Rc<Cell<bool>>,
    query_dirty: Rc<Cell<bool>>,
    close_requested: Rc<Cell<bool>>,
    settled: Rc<Cell<bool>>,
    target_flag: Rc<Cell<bool>>,
    calls: UnboundedSender<PanelCall>,
    target_open: bool,
    motion: Option<PanelMotion>,
}

pub(crate) fn spawn_panel_client(
    handle: tokio::runtime::Handle,
    ui_tx: std::sync::mpsc::Sender<UiEvent>,
) -> UnboundedSender<PanelCall> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    handle.spawn(async move {
        if let Err(error) = panel_client(&mut rx, &ui_tx).await {
            eprintln!("den-den-mushi: panel: {error}");
        }
    });
    tx
}

async fn panel_client(
    calls: &mut UnboundedReceiver<PanelCall>,
    ui_tx: &std::sync::mpsc::Sender<UiEvent>,
) -> Result<(), String> {
    let connection = zbus::Connection::session()
        .await
        .map_err(|error| error.to_string())?;
    let proxy = ControlProxy::builder(&connection)
        .cache_properties(zbus::proxy::CacheProperties::Yes)
        .build()
        .await
        .map_err(|error| error.to_string())?;
    let mut history = proxy
        .receive_history_changed()
        .await
        .map_err(|error| error.to_string())?;
    let mut rules = proxy
        .receive_rules_changed()
        .await
        .map_err(|error| error.to_string())?;
    let mut dnd = proxy.receive_do_not_disturb_changed().await;
    push_state(&proxy, ui_tx).await;
    loop {
        tokio::select! {
            call = calls.recv() => {
                let Some(call) = call else {
                    return Ok(());
                };
                if let Err(error) = dispatch(&proxy, call).await {
                    eprintln!("den-den-mushi: panel: {error}");
                }
                push_state(&proxy, ui_tx).await;
            }
            changed = history.next() => {
                if changed.is_none() {
                    return Ok(());
                }
                push_state(&proxy, ui_tx).await;
            }
            changed = rules.next() => {
                if changed.is_none() {
                    return Ok(());
                }
                push_state(&proxy, ui_tx).await;
            }
            changed = dnd.next() => {
                if changed.is_none() {
                    return Ok(());
                }
                push_state(&proxy, ui_tx).await;
            }
        }
    }
}

async fn push_state(proxy: &ControlProxy<'_>, ui_tx: &std::sync::mpsc::Sender<UiEvent>) {
    let history_json = match proxy.history().await {
        Ok(json) => json,
        Err(error) => {
            eprintln!("den-den-mushi: panel history: {error}");
            return;
        }
    };
    let rules_json = match proxy.rules().await {
        Ok(json) => json,
        Err(error) => {
            eprintln!("den-den-mushi: panel rules: {error}");
            return;
        }
    };
    let dnd = match proxy.do_not_disturb().await {
        Ok(on) => on,
        Err(error) => {
            eprintln!("den-den-mushi: panel do not disturb: {error}");
            return;
        }
    };
    let _ = ui_tx.send(UiEvent::PanelState {
        history_json,
        rules_json,
        dnd,
    });
}

async fn dispatch(proxy: &ControlProxy<'_>, call: PanelCall) -> Result<(), String> {
    match call {
        PanelCall::ClearHistory => proxy
            .clear_history()
            .await
            .map_err(|error| error.to_string())?,
        PanelCall::RemoveHistory(id) => proxy
            .remove_history(id)
            .await
            .map_err(|error| error.to_string())?,
        PanelCall::InvokeHistory(id) => proxy
            .invoke_history(id)
            .await
            .map_err(|error| error.to_string())?,
        PanelCall::SetDoNotDisturb(on) => proxy
            .set_do_not_disturb(on)
            .await
            .map_err(|error| error.to_string())?,
        PanelCall::SetRule {
            app,
            muted,
            timeout,
            bypass_dnd,
        } => proxy
            .set_rule(&app, muted, &timeout, bypass_dnd)
            .await
            .map_err(|error| error.to_string())?,
    }
    Ok(())
}

impl Panel {
    pub(crate) fn new(
        application: &gtk::Application,
        settings: &Settings,
        calls: UnboundedSender<PanelCall>,
    ) -> Self {
        let window = gtk::ApplicationWindow::builder()
            .application(application)
            .decorated(false)
            // Resizable so a top+bottom layer-shell anchor can stretch the
            // window to the monitor. A fixed-size window stays at its content
            // height and the compositor only shows that strip.
            .resizable(true)
            .title("Notifications")
            .default_width(PANEL_WIDTH)
            .build();
        window.add_css_class("panel-window");
        window.set_width_request(PANEL_WIDTH);

        if gtk4_layer_shell::is_supported() {
            window.init_layer_shell();
            window.set_layer(Layer::Overlay);
            window.set_namespace(Some(settings.namespace.as_str()));
            window.set_anchor(Edge::Top, true);
            window.set_anchor(Edge::Bottom, true);
            window.set_anchor(Edge::Right, true);
            window.set_anchor(Edge::Left, false);
            window.set_exclusive_zone(0);
            window.set_keyboard_mode(KeyboardMode::None);
        } else {
            eprintln!("den-den-mushi: layer shell is unavailable; the panel is an ordinary window");
        }

        let panel = gtk::Box::new(Orientation::Vertical, 0);
        panel.add_css_class("panel");
        panel.set_overflow(Overflow::Hidden);
        panel.set_hexpand(true);
        panel.set_vexpand(true);
        panel.set_size_request(PANEL_WIDTH, -1);

        let header = gtk::Box::new(Orientation::Horizontal, 0);
        header.add_css_class("panel-header");
        header.set_size_request(-1, 48);
        let title = gtk::Label::new(Some("NOTIFICATIONS"));
        title.add_css_class("app");
        title.set_valign(Align::Center);
        title.set_halign(Align::Start);
        let dnd_label = gtk::Label::new(Some("DND"));
        dnd_label.add_css_class("dnd-label");
        dnd_label.set_valign(Align::Center);
        let dnd_switch = gtk::Switch::new();
        dnd_switch.add_css_class("dnd-switch");
        dnd_switch.set_valign(Align::Center);
        dnd_switch.set_halign(Align::Start);
        let spacer = gtk::Box::new(Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let clear = gtk::Button::with_label("Clear all");
        clear.add_css_class("clear-all");
        clear.set_valign(Align::Center);
        clear.set_halign(Align::End);
        // Nothing to clear until the first history snapshot arrives.
        clear.set_sensitive(false);
        header.append(&title);
        header.append(&dnd_label);
        header.append(&dnd_switch);
        header.append(&spacer);
        header.append(&clear);

        let dnd_line = gtk::Box::new(Orientation::Horizontal, 0);
        dnd_line.add_css_class("dnd-line");
        dnd_line.set_size_request(-1, 2);
        dnd_line.set_visible(false);

        let search = gtk::Entry::new();
        search.add_css_class("panel-search");
        search.set_placeholder_text(Some("Search"));
        search.set_has_frame(false);
        search.set_hexpand(true);
        search.set_size_request(-1, 32);

        let history_box = gtk::Box::new(Orientation::Vertical, 0);
        history_box.add_css_class("history-list");
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scrolled.set_propagate_natural_height(false);
        scrolled.set_vexpand(true);
        scrolled.set_hexpand(true);
        scrolled.set_child(Some(&history_box));

        let (pin, pin_glyph, pin_name, pin_count) = header_widgets(true);
        pin.set_halign(Align::Fill);
        pin.set_valign(Align::Start);
        pin.set_margin_start(12);
        pin.set_margin_end(12);
        pin.set_visible(false);
        let overlay = gtk::Overlay::new();
        overlay.set_vexpand(true);
        overlay.set_child(Some(&scrolled));
        overlay.add_overlay(&pin);

        let empty = gtk::Label::new(Some("All caught up."));
        empty.add_css_class("empty-state");
        empty.set_halign(Align::Center);
        empty.set_valign(Align::Center);
        empty.set_vexpand(true);
        empty.set_hexpand(true);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&overlay, Some("list"));
        stack.add_named(&empty, Some("empty"));
        stack.set_visible_child_name("empty");

        let rules = gtk::Box::new(Orientation::Vertical, 0);
        rules.add_css_class("rules");
        // Headless screenshots set this so the rules section starts open.
        let rules_open = std::env::var_os("DEN_DEN_MUSHI_RULES_OPEN").is_some();
        let rules_button = gtk::Button::new();
        rules_button.add_css_class("rules-toggle");
        rules_button.set_hexpand(true);
        let rules_chevron = gtk::Label::new(Some(if rules_open {
            GLYPH_CHEVRON_DOWN
        } else {
            GLYPH_CHEVRON_RIGHT
        }));
        rules_chevron.add_css_class("glyph");
        rules_chevron.set_valign(Align::Center);
        let rules_text = gtk::Label::new(Some("RULES"));
        rules_text.add_css_class("app");
        rules_text.set_valign(Align::Center);
        let rules_heading = gtk::Box::new(Orientation::Horizontal, 6);
        rules_heading.set_halign(Align::Start);
        rules_heading.append(&rules_chevron);
        rules_heading.append(&rules_text);
        rules_button.set_child(Some(&rules_heading));
        let rules_box = gtk::Box::new(Orientation::Vertical, 0);
        rules_box.add_css_class("rules-list");
        let rules_scroll = gtk::ScrolledWindow::new();
        rules_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        rules_scroll.set_propagate_natural_height(true);
        rules_scroll.set_max_content_height(220);
        rules_scroll.set_child(Some(&rules_box));
        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);
        revealer.set_transition_duration(150);
        revealer.set_reveal_child(rules_open);
        revealer.set_child(Some(&rules_scroll));
        rules.append(&rules_button);
        rules.append(&revealer);

        panel.append(&header);
        panel.append(&dnd_line);
        panel.append(&search);
        panel.append(&stack);
        panel.append(&rules);

        let slide = SlideBin::new();
        slide.set_overflow(Overflow::Hidden);
        slide.set_hexpand(true);
        slide.set_vexpand(true);
        slide.set_child(Some(panel.upcast_ref()));
        slide.set_offset(f64::from(PANEL_WIDTH));
        window.set_child(Some(&slide));
        window.set_visible(false);

        let model = Rc::new(RefCell::new(PanelModel {
            history: Vec::new(),
            rules: Vec::new(),
            query: String::new(),
            displayed_rows: Vec::new(),
        }));
        let updating = Rc::new(Cell::new(false));
        let query_dirty = Rc::new(Cell::new(false));
        let close_requested = Rc::new(Cell::new(false));
        let settled = Rc::new(Cell::new(false));
        let target_flag = Rc::new(Cell::new(false));

        let calls_clear = calls.clone();
        clear.connect_clicked(move |_| {
            let _ = calls_clear.send(PanelCall::ClearHistory);
        });

        let query_model = Rc::clone(&model);
        let query_flag = Rc::clone(&query_dirty);
        search.connect_changed(move |entry| {
            query_model.borrow_mut().query = entry.text().to_string();
            query_flag.set(true);
        });

        let calls_dnd = calls.clone();
        let updating_dnd = Rc::clone(&updating);
        let line_dnd = dnd_line.clone();
        dnd_switch.connect_active_notify(move |switch| {
            if updating_dnd.get() {
                return;
            }
            let on = switch.is_active();
            line_dnd.set_visible(on);
            let _ = calls_dnd.send(PanelCall::SetDoNotDisturb(on));
        });

        let chevron_toggle = rules_chevron.clone();
        let revealer_toggle = revealer.clone();
        rules_button.connect_clicked(move |_| {
            let open = !revealer_toggle.reveals_child();
            revealer_toggle.set_reveal_child(open);
            chevron_toggle.set_text(if open {
                GLYPH_CHEVRON_DOWN
            } else {
                GLYPH_CHEVRON_RIGHT
            });
        });

        let settled_cb = Rc::clone(&settled);
        let target_cb = Rc::clone(&target_flag);
        let close_cb = Rc::clone(&close_requested);
        window.connect_is_active_notify(move |window| {
            if window.is_active() || !settled_cb.get() || !target_cb.get() {
                return;
            }
            // On-demand keyboard focus leaves this window when the pointer
            // activates something else. A popover (the timeout menu) keeps
            // the pointer inside, so that is not a click outside.
            if pointer_outside(window) {
                close_cb.set(true);
            }
        });

        let close_key = Rc::clone(&close_requested);
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::Escape {
                close_key.set(true);
                Propagation::Stop
            } else {
                Propagation::Proceed
            }
        });
        window.add_controller(key);

        let close_wm = Rc::clone(&close_requested);
        window.connect_close_request(move |_| {
            close_wm.set(true);
            Propagation::Stop
        });

        Self {
            window,
            slide,
            search,
            stack,
            empty,
            history_box,
            scrolled,
            pin,
            pin_glyph,
            pin_name,
            pin_count,
            rules_box,
            clear,
            dnd_switch,
            dnd_line,
            rows: Vec::new(),
            groups: Vec::new(),
            model,
            updating,
            query_dirty,
            close_requested,
            settled,
            target_flag,
            calls,
            target_open: false,
            motion: None,
        }
    }

    pub(crate) fn apply_insets(&self, insets: Insets) {
        if self.window.is_layer_window() {
            // Top is the bar plus gaps_out. Bottom and right are gaps_out.
            // `Insets::right` is that outer gap; the bar is only on top.
            self.window.set_margin(Edge::Top, insets.top);
            self.window.set_margin(Edge::Bottom, insets.right);
            self.window.set_margin(Edge::Right, insets.right);
            self.window.set_margin(Edge::Left, 0);
        }
    }

    pub(crate) fn toggle(&mut self) {
        if self.target_open {
            self.begin_close();
        } else {
            self.begin_open();
        }
    }

    pub(crate) fn set_state(&mut self, history_json: &str, rules_json: &str, dnd: bool) {
        self.apply_dnd(dnd);
        let history = match parse_entries(history_json) {
            Ok(entries) => entries,
            Err(error) => {
                eprintln!("den-den-mushi: panel history: {error}");
                return;
            }
        };
        let rules = match rules_from_json(rules_json) {
            Ok(rules) => rules,
            Err(error) => {
                eprintln!("den-den-mushi: panel rules: {error}");
                return;
            }
        };
        let history_changed = self.model.borrow().history != history;
        let rules_changed = self.model.borrow().rules != rules;
        if !history_changed && !rules_changed {
            return;
        }
        let rows = {
            let mut model = self.model.borrow_mut();
            if history_changed {
                model.history = history;
            }
            if rules_changed {
                model.rules = rules;
            }
            rule_rows(&model.history, &model.rules)
        };
        let rules_rows_changed = rows != self.model.borrow().displayed_rows;
        if rules_rows_changed {
            self.model.borrow_mut().displayed_rows = rows.clone();
        }
        if history_changed {
            self.rebuild_history_from_model();
        }
        if rules_rows_changed {
            self.rebuild_rules(&rows);
        }
    }

    pub(crate) fn tick(&mut self) {
        if self.close_requested.replace(false) {
            self.begin_close();
        }
        if self.query_dirty.replace(false) {
            self.rebuild_history_from_model();
        }
        self.step_motion();
        if !self.window.is_visible() {
            return;
        }
        let now = ui::unix_now();
        for row in &mut self.rows {
            let next = relative_time(row.created_unix, now);
            if row.time.text().as_str() != next {
                row.time.set_text(&next);
            }
            let over = ui::pointer_over(&row.root);
            if over == row.hovered {
                continue;
            }
            row.hovered = over;
            if over {
                row.root.add_css_class("hover");
                row.close.set_can_target(true);
                row.corner.set_visible_child_name("close");
            } else {
                row.root.remove_css_class("hover");
                row.close.set_can_target(false);
                row.corner.set_visible_child_name("time");
            }
        }
        self.update_sticky();
    }

    fn apply_dnd(&self, on: bool) {
        self.dnd_line.set_visible(on);
        if self.dnd_switch.is_active() == on {
            return;
        }
        self.updating.set(true);
        self.dnd_switch.set_active(on);
        self.updating.set(false);
    }

    fn begin_open(&mut self) {
        if self.target_open && self.motion.is_none() {
            return;
        }
        let from = self.slide.offset();
        self.close_requested.set(false);
        self.target_open = true;
        self.target_flag.set(true);
        self.settled.set(false);
        self.window.set_visible(true);
        if self.window.is_layer_window() {
            self.window.set_keyboard_mode(KeyboardMode::OnDemand);
        }
        self.motion = Some(PanelMotion {
            start: Instant::now(),
            duration: Duration::from_millis(OPEN_MS),
            from,
            to: 0.0,
        });
        eprintln!("den-den-mushi: panel open");
        // On-demand keyboard mode only takes focus while a control here wants it.
        let _ = self.search.grab_focus();
    }

    fn begin_close(&mut self) {
        if !self.target_open {
            return;
        }
        let from = self.slide.offset();
        self.target_open = false;
        self.target_flag.set(false);
        self.settled.set(false);
        self.motion = Some(PanelMotion {
            start: Instant::now(),
            duration: Duration::from_millis(CLOSE_MS),
            from,
            to: f64::from(PANEL_WIDTH),
        });
        eprintln!("den-den-mushi: panel close");
    }

    fn step_motion(&mut self) {
        let Some(motion) = self.motion.as_ref() else {
            return;
        };
        let elapsed = motion.start.elapsed().as_secs_f64();
        let duration = motion.duration.as_secs_f64().max(0.001);
        let t = (elapsed / duration).clamp(0.0, 1.0);
        // Cubic ease-out, same curve as the cards.
        let eased = 1.0 - (1.0 - t).powi(3);
        let offset = motion.from + (motion.to - motion.from) * eased;
        let opening = motion.to == 0.0;
        let done = t >= 1.0;
        self.slide.set_offset(offset);
        if !done {
            return;
        }
        self.motion = None;
        if opening {
            self.settled.set(true);
        } else {
            self.settled.set(false);
            self.window.set_visible(false);
            if self.window.is_layer_window() {
                self.window.set_keyboard_mode(KeyboardMode::None);
            }
        }
    }

    fn rebuild_history_from_model(&mut self) {
        self.query_dirty.set(false);
        let (history, query) = {
            let model = self.model.borrow();
            (model.history.clone(), model.query.clone())
        };
        self.clear.set_sensitive(!history.is_empty());
        self.rows.clear();
        self.groups.clear();
        self.pin.set_visible(false);
        while let Some(child) = self.history_box.first_child() {
            self.history_box.remove(&child);
        }
        let filtered = filter_history(&history, &query);
        if filtered.is_empty() {
            self.empty.set_text(if history.is_empty() {
                "All caught up."
            } else {
                "No matches."
            });
            self.stack.set_visible_child_name("empty");
            return;
        }
        self.stack.set_visible_child_name("list");
        for group in group_history(&filtered) {
            let newest = group.entries[0];
            let critical = group
                .entries
                .iter()
                .any(|entry| entry.urgency == "critical");
            let glyph = group_glyph(group.app_name, &newest.icon, entry_urgency(&newest.urgency));
            let count = group.count();
            let (header, glyph_label, name_label, count_label) = header_widgets(false);
            if critical {
                header.add_css_class("critical");
            }
            glyph_label.set_text(&glyph);
            name_label.set_text(&display_app(group.app_name));
            count_label.set_text(&count.to_string());
            let group_box = gtk::Box::new(Orientation::Vertical, 0);
            group_box.add_css_class("history-group");
            group_box.append(&header);
            for entry in &group.entries {
                let row = history_card(entry, &self.calls);
                group_box.append(&row.root);
                self.rows.push(row);
            }
            self.history_box.append(&group_box);
            self.groups.push(GroupMark {
                app_name: group.app_name.to_string(),
                glyph,
                count,
                critical,
                header,
            });
        }
    }

    fn rebuild_rules(&self, rows: &[RuleRow]) {
        while let Some(child) = self.rules_box.first_child() {
            self.rules_box.remove(&child);
        }
        if rows.is_empty() {
            let empty = gtk::Label::new(Some("No apps yet."));
            empty.add_css_class("empty-state");
            empty.set_halign(Align::Start);
            empty.set_margin_start(12);
            empty.set_margin_bottom(8);
            self.rules_box.append(&empty);
            return;
        }
        let columns = RuleColumns::new();
        self.rules_box.append(&rules_header(&columns));
        for row in rows {
            self.rules_box
                .append(&rule_row(row, &self.calls, &self.model, &columns));
        }
    }

    fn update_sticky(&self) {
        if self.groups.is_empty() || self.stack.visible_child_name().as_deref() != Some("list") {
            self.pin.set_visible(false);
            return;
        }
        let scroll = self.scrolled.vadjustment().value();
        let mut current = None;
        for (index, group) in self.groups.iter().enumerate() {
            let Some(y) = widget_top(&group.header, &self.history_box) else {
                continue;
            };
            if y <= scroll + 1.0 {
                current = Some((index, y));
            }
        }
        let Some((index, header_y)) = current else {
            self.pin.set_visible(false);
            return;
        };
        if header_y >= scroll - 1.0 {
            self.pin.set_visible(false);
            return;
        }
        let group = &self.groups[index];
        if group.critical {
            self.pin.add_css_class("critical");
        } else {
            self.pin.remove_css_class("critical");
        }
        self.pin_glyph.set_text(&group.glyph);
        self.pin_name.set_text(&display_app(&group.app_name));
        self.pin_count.set_text(&group.count.to_string());
        self.pin.set_visible(true);
        let mut shift = 0.0;
        if let Some(next) = self.groups.get(index + 1) {
            if let Some(next_y) = widget_top(&next.header, &self.history_box) {
                let pin_h = f64::from(self.pin.height().max(1));
                let overlap = (scroll + pin_h) - next_y;
                if overlap > 0.0 {
                    shift = -overlap;
                }
            }
        }
        self.pin.set_margin_top(shift.round() as i32);
    }
}

fn pointer_outside(widget: &impl IsA<gtk::Widget>) -> bool {
    let widget = widget.upcast_ref::<gtk::Widget>();
    let Some(seat) = widget.display().default_seat() else {
        return false;
    };
    if seat.pointer().is_none() {
        return false;
    }
    !ui::pointer_over(widget)
}

fn header_widgets(pinned: bool) -> (gtk::Box, gtk::Label, gtk::Label, gtk::Label) {
    let root = gtk::Box::new(Orientation::Horizontal, 6);
    root.add_css_class("group-header");
    // Fill the row so the name gets the space between the glyph and the count.
    // A 1-character width request would otherwise stay an ellipsis.
    root.set_hexpand(true);
    if pinned {
        root.add_css_class("pinned");
    }
    let glyph = gtk::Label::new(None);
    glyph.add_css_class("glyph");
    glyph.set_valign(Align::Center);
    let name = gtk::Label::new(None);
    name.add_css_class("app");
    name.set_halign(Align::Start);
    name.set_hexpand(true);
    name.set_xalign(0.0);
    // No ellipsize. With it on, GTK sizes the label to the ellipsis and
    // "VENMO" draws as "VENM…", the same bug the card app line avoids.
    let count = gtk::Label::new(None);
    count.add_css_class("group-count");
    count.set_valign(Align::Center);
    root.append(&glyph);
    root.append(&name);
    root.append(&count);
    (root, glyph, name, count)
}

fn history_card(
    entry: &crate::history::HistoryEntry,
    calls: &UnboundedSender<PanelCall>,
) -> HistoryRow {
    let root = gtk::Box::new(Orientation::Vertical, 0);
    root.add_css_class("history-card");
    if entry.urgency == "critical" {
        root.add_css_class("critical");
    }
    root.set_hexpand(true);

    let content = gtk::Box::new(Orientation::Vertical, 0);
    content.add_css_class("history-content");

    let title_row = gtk::Box::new(Orientation::Horizontal, 8);
    title_row.add_css_class("title-row");
    let title = gtk::Label::new(Some(&entry.summary));
    title.add_css_class("title");
    ui::clamp_lines(&title, 1);
    title.set_visible(!entry.summary.is_empty());

    let time = gtk::Label::new(Some(&relative_time(entry.timestamp, ui::unix_now())));
    time.add_css_class("time");
    time.set_valign(Align::Center);
    time.set_halign(Align::End);

    let close = gtk::Button::with_label("×");
    close.add_css_class("close");
    close.set_valign(Align::Center);
    close.set_halign(Align::End);
    close.set_can_target(false);
    let calls_close = calls.clone();
    let close_id = entry.id;
    close.connect_clicked(move |_| {
        let _ = calls_close.send(PanelCall::RemoveHistory(close_id));
    });

    let corner = gtk::Stack::new();
    corner.add_css_class("corner");
    corner.set_transition_type(gtk::StackTransitionType::Crossfade);
    corner.set_transition_duration(150);
    corner.set_hhomogeneous(false);
    corner.set_vhomogeneous(false);
    corner.set_halign(Align::End);
    corner.set_valign(Align::Center);
    corner.add_named(&time, Some("time"));
    corner.add_named(&close, Some("close"));
    corner.set_visible_child_name("time");

    title_row.append(&title);
    title_row.append(&corner);

    let body = gtk::Label::new(None);
    body.add_css_class("body");
    ui::clamp_lines(&body, 2);
    ui::set_body(&body, &entry.body);
    body.set_visible(!entry.body.is_empty());

    content.append(&title_row);
    content.append(&body);
    root.append(&content);

    let calls_click = calls.clone();
    let click_id = entry.id;
    let root_for_pick = root.clone();
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    click.connect_released(move |gesture, _, x, y| {
        if let Some(widget) = root_for_pick.pick(x, y, gtk::PickFlags::DEFAULT) {
            if ui::is_button(&widget) {
                return;
            }
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
        let _ = calls_click.send(PanelCall::InvokeHistory(click_id));
    });
    root.add_controller(click);

    HistoryRow {
        root,
        time,
        close,
        corner,
        created_unix: entry.timestamp,
        hovered: false,
    }
}

fn rule_row(
    row: &RuleRow,
    calls: &UnboundedSender<PanelCall>,
    model: &Rc<RefCell<PanelModel>>,
    columns: &RuleColumns,
) -> gtk::Box {
    let name = gtk::Label::new(Some(&display_app(&row.app_name)));
    name.add_css_class("rule-name");
    name.set_halign(Align::Start);
    name.set_hexpand(true);
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let mute = gtk::Switch::new();
    mute.set_active(row.muted);
    mute.set_valign(Align::Center);
    mute.set_halign(Align::Center);

    let timeout = gtk::DropDown::from_strings(&["default", "5s", "10s", "sticky"]);
    timeout.add_css_class("timeout");
    timeout.set_selected(row.timeout.index());
    timeout.set_valign(Align::Center);
    timeout.set_halign(Align::Center);

    let bypass = gtk::Switch::new();
    bypass.set_active(row.bypass_dnd);
    bypass.set_valign(Align::Center);
    bypass.set_halign(Align::Center);

    let root = rule_line(&name, &mute, &timeout, &bypass, columns);
    wire_rule(calls, model, &row.app_name, &mute, &timeout, &bypass);
    root
}

/// Shared widths so MUTE, TIMEOUT, and DND sit under those header words.
struct RuleColumns {
    mute: gtk::SizeGroup,
    timeout: gtk::SizeGroup,
    dnd: gtk::SizeGroup,
}

impl RuleColumns {
    fn new() -> Self {
        Self {
            mute: gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal),
            timeout: gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal),
            dnd: gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal),
        }
    }
}

fn rules_header(columns: &RuleColumns) -> gtk::Box {
    let app = column_title("APP", Align::Start, true);
    let mute = column_title("MUTE", Align::Center, false);
    let timeout = column_title("TIMEOUT", Align::Center, false);
    let dnd = column_title("DND", Align::Center, false);
    rule_line(&app, &mute, &timeout, &dnd, columns)
}

fn column_title(text: &str, align: Align, expand: bool) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("app");
    label.set_halign(align);
    label.set_valign(Align::Center);
    label.set_hexpand(expand);
    if align == Align::Center {
        label.set_xalign(0.5);
    } else {
        label.set_xalign(0.0);
    }
    label
}

fn rule_line(
    app: &impl IsA<gtk::Widget>,
    mute: &impl IsA<gtk::Widget>,
    timeout: &impl IsA<gtk::Widget>,
    dnd: &impl IsA<gtk::Widget>,
    columns: &RuleColumns,
) -> gtk::Box {
    columns.mute.add_widget(mute);
    columns.timeout.add_widget(timeout);
    columns.dnd.add_widget(dnd);
    let root = gtk::Box::new(Orientation::Horizontal, 8);
    root.add_css_class("rule-line");
    root.set_hexpand(true);
    root.append(app);
    root.append(mute);
    root.append(timeout);
    root.append(dnd);
    root
}

fn push_rule(
    calls: &UnboundedSender<PanelCall>,
    model: &Rc<RefCell<PanelModel>>,
    app: &str,
    muted: bool,
    chosen: RuleTimeout,
    bypass_dnd: bool,
) {
    {
        let mut model = model.borrow_mut();
        let mut set = RuleSet::new(model.rules.clone());
        set.set(AppRule {
            app: app.to_string(),
            muted,
            timeout: chosen,
            bypass_dnd,
        });
        model.rules = set.rules().to_vec();
        model.displayed_rows = rule_rows(&model.history, &model.rules);
    }
    let _ = calls.send(PanelCall::SetRule {
        app: app.to_string(),
        muted,
        timeout: chosen.as_str().to_string(),
        bypass_dnd,
    });
}

fn wire_rule(
    calls: &UnboundedSender<PanelCall>,
    model: &Rc<RefCell<PanelModel>>,
    app: &str,
    mute: &gtk::Switch,
    timeout: &gtk::DropDown,
    bypass: &gtk::Switch,
) {
    let calls_mute = calls.clone();
    let model_mute = Rc::clone(model);
    let app_mute = app.to_string();
    let timeout_mute = timeout.clone();
    let bypass_mute = bypass.clone();
    mute.connect_active_notify(move |mute| {
        let Some(chosen) = RuleTimeout::from_index(timeout_mute.selected()) else {
            return;
        };
        push_rule(
            &calls_mute,
            &model_mute,
            &app_mute,
            mute.is_active(),
            chosen,
            bypass_mute.is_active(),
        );
    });

    let calls_timeout = calls.clone();
    let model_timeout = Rc::clone(model);
    let app_timeout = app.to_string();
    let mute_timeout = mute.clone();
    let bypass_timeout = bypass.clone();
    timeout.connect_selected_notify(move |timeout| {
        let Some(chosen) = RuleTimeout::from_index(timeout.selected()) else {
            return;
        };
        push_rule(
            &calls_timeout,
            &model_timeout,
            &app_timeout,
            mute_timeout.is_active(),
            chosen,
            bypass_timeout.is_active(),
        );
    });

    let calls_bypass = calls.clone();
    let model_bypass = Rc::clone(model);
    let app_bypass = app.to_string();
    let mute_bypass = mute.clone();
    let timeout_bypass = timeout.clone();
    bypass.connect_active_notify(move |bypass| {
        let Some(chosen) = RuleTimeout::from_index(timeout_bypass.selected()) else {
            return;
        };
        push_rule(
            &calls_bypass,
            &model_bypass,
            &app_bypass,
            mute_bypass.is_active(),
            chosen,
            bypass.is_active(),
        );
    });
}

fn widget_top(widget: &impl IsA<gtk::Widget>, ancestor: &impl IsA<gtk::Widget>) -> Option<f64> {
    let point = widget.compute_point(ancestor, &graphene::Point::new(0.0, 0.0))?;
    Some(f64::from(point.y()))
}
