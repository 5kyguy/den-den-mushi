//! Card motion, with no GTK types so the curves can be tested on their own.
//!
//! Enter is 180ms, ease-out `cubic-bezier(0.2, 0.8, 0.2, 1)`, from +24px
//! to 0 with opacity 0 to 1. Exit is 120ms, CSS `ease-in`
//! `cubic-bezier(0.42, 0, 1, 1)`, from 0 to +24px with opacity to 0.
//! Restack is a critically damped spring (stiffness 400, damping 40, mass 1),
//! so it does not overshoot. It starts when the exit finishes
//! (`RESTACK_DELAY_MS`), after the dismissed card is gone.

use std::time::Duration;

pub const SLIDE_PX: f64 = 24.0;
pub const ENTER_MS: u64 = 180;
pub const EXIT_MS: u64 = 120;
pub const CROSSFADE_MS: u64 = 120;
/// Matches `.toast { margin-bottom: 8px }` in the stylesheet.
pub const STACK_GAP: f64 = 8.0;

/// Enter ease-out.
pub const ENTER_CURVE: [f64; 4] = [0.2, 0.8, 0.2, 1.0];
/// CSS standard `ease-in`.
pub const EXIT_CURVE: [f64; 4] = [0.42, 0.0, 1.0, 1.0];

pub const SPRING_STIFFNESS: f64 = 400.0;
/// `2 * sqrt(stiffness * mass)` — critical damping, no overshoot.
pub const SPRING_DAMPING: f64 = 40.0;
pub const SPRING_MASS: f64 = 1.0;
/// The leaving card keeps its slot for the whole exit, then the cards
/// below spring up. Starting earlier still overlaps, because the exit is
/// ease-in and the card is still on screen at 60ms.
pub const RESTACK_DELAY_MS: u64 = EXIT_MS;

pub fn enter_duration(enabled: bool) -> Duration {
    duration_ms(enabled, ENTER_MS)
}

pub fn exit_duration(enabled: bool) -> Duration {
    duration_ms(enabled, EXIT_MS)
}

pub fn crossfade_duration(enabled: bool) -> Duration {
    duration_ms(enabled, CROSSFADE_MS)
}

pub fn restack_delay(enabled: bool) -> Duration {
    duration_ms(enabled, RESTACK_DELAY_MS)
}

/// A card that is not leaving always keeps its slot. A leaving card keeps
/// it until `RESTACK_DELAY_MS`. Motion off lets the slot go immediately.
pub fn holds_stack_slot(leaving: bool, elapsed: Duration, enabled: bool) -> bool {
    if !leaving {
        return true;
    }
    enabled && elapsed < restack_delay(true)
}

fn duration_ms(enabled: bool, ms: u64) -> Duration {
    if enabled {
        Duration::from_millis(ms)
    } else {
        Duration::ZERO
    }
}

pub fn unit_progress(elapsed: Duration, duration: Duration) -> f64 {
    if duration.is_zero() {
        1.0
    } else {
        (elapsed.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0)
    }
}

/// Seconds to feed the integrator. A gap longer than a frame means the
/// animation was idle, so the first step is one frame and not that gap.
pub fn animation_dt(since_secs: f64) -> f64 {
    if since_secs <= 0.0 {
        0.0
    } else if since_secs > 0.05 {
        1.0 / 60.0
    } else {
        since_secs
    }
}

pub fn enter_offset(t: f64) -> f64 {
    SLIDE_PX * (1.0 - enter_eased(t))
}

pub fn enter_opacity(t: f64) -> f64 {
    enter_eased(t)
}

pub fn exit_offset(t: f64) -> f64 {
    SLIDE_PX * exit_eased(t)
}

pub fn exit_opacity(t: f64) -> f64 {
    1.0 - exit_eased(t)
}

/// Opacity of the incoming card face. The outgoing face is `1 - blend`.
pub fn crossfade_blend(t: f64) -> f64 {
    t.clamp(0.0, 1.0)
}

fn enter_eased(t: f64) -> f64 {
    cubic_bezier(
        ENTER_CURVE[0],
        ENTER_CURVE[1],
        ENTER_CURVE[2],
        ENTER_CURVE[3],
        t,
    )
    .clamp(0.0, 1.0)
}

fn exit_eased(t: f64) -> f64 {
    cubic_bezier(
        EXIT_CURVE[0],
        EXIT_CURVE[1],
        EXIT_CURVE[2],
        EXIT_CURVE[3],
        t,
    )
    .clamp(0.0, 1.0)
}

/// CSS cubic-bezier. `x` is normalized time. Control points are `(x1, y1)`,
/// `(x2, y2)`; the curve runs from `(0, 0)` to `(1, 1)`.
pub fn cubic_bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    if x == 0.0 {
        return 0.0;
    }
    if x == 1.0 {
        return 1.0;
    }
    let mut t = x;
    for _ in 0..8 {
        let dx = bezier_component(t, x1, x2) - x;
        let slope = bezier_slope(t, x1, x2);
        if slope.abs() < 1e-6 {
            break;
        }
        t = (t - dx / slope).clamp(0.0, 1.0);
    }
    bezier_component(t, y1, y2)
}

fn bezier_component(t: f64, p1: f64, p2: f64) -> f64 {
    let c = 3.0 * p1;
    let b = 3.0 * (p2 - p1) - c;
    let a = 1.0 - c - b;
    ((a * t + b) * t + c) * t
}

fn bezier_slope(t: f64, p1: f64, p2: f64) -> f64 {
    let c = 3.0 * p1;
    let b = 3.0 * (p2 - p1) - c;
    let a = 1.0 - c - b;
    (3.0 * a * t + 2.0 * b) * t + c
}

/// Vertical spring for restack. `position` and `target` are pixels.
#[derive(Clone, Debug)]
pub struct Spring {
    pub position: f64,
    pub velocity: f64,
    pub target: f64,
}

impl Spring {
    pub fn at(position: f64) -> Self {
        Self {
            position,
            velocity: 0.0,
            target: position,
        }
    }

    /// Semi-implicit Euler with 1ms substeps, from the frame-clock tick.
    pub fn step(&mut self, dt: f64) {
        if dt <= 0.0 {
            return;
        }
        const SUB: f64 = 0.001;
        let mut left = dt.min(0.1);
        while left > 0.0 {
            let h = left.min(SUB);
            let displacement = self.position - self.target;
            let accel =
                (-SPRING_STIFFNESS * displacement - SPRING_DAMPING * self.velocity) / SPRING_MASS;
            self.velocity += accel * h;
            self.position += self.velocity * h;
            left -= h;
        }
    }

    pub fn snap(&mut self) {
        self.position = self.target;
        self.velocity = 0.0;
    }

    pub fn resting(&self) -> bool {
        (self.position - self.target).abs() < 0.4 && self.velocity.abs() < 0.5
    }
}

/// Y of one card. The first sample anchors. Later samples spring only while
/// a card is leaving or the spring is already moving; a height correction
/// on a resting stack snaps.
#[derive(Clone, Debug)]
pub struct YTrack {
    pub spring: Spring,
    pub anchored: bool,
    /// Set once a restack has started, so a later sample cannot snap it
    /// after the leaving card has already been removed.
    coasting: bool,
}

impl YTrack {
    pub fn new() -> Self {
        Self {
            spring: Spring::at(0.0),
            anchored: false,
            coasting: false,
        }
    }

    pub fn follow(&mut self, y: f64, any_leaving: bool) {
        if self.spring.resting() {
            self.coasting = false;
        }
        let slow = self.spring.velocity.abs() < 0.5;
        if !self.anchored || (slow && !any_leaving && !self.coasting) {
            self.spring.position = y;
            self.spring.velocity = 0.0;
            self.anchored = true;
            self.coasting = false;
        } else {
            self.coasting = true;
        }
        self.spring.target = y;
    }
}

impl Default for YTrack {
    fn default() -> Self {
        Self::new()
    }
}

/// Target y for each card, top to bottom. `None` means the card is leaving
/// and keeps the y it already has.
pub fn restack_targets(heights: &[f64], leaving: &[bool], gap: f64) -> Vec<Option<f64>> {
    let mut y = 0.0;
    let mut targets = vec![None; heights.len()];
    let flowing: Vec<usize> = leaving
        .iter()
        .enumerate()
        .filter(|(_, leave)| !**leave)
        .map(|(index, _)| index)
        .collect();
    for (n, index) in flowing.iter().copied().enumerate() {
        targets[index] = Some(y);
        let trailing = if n + 1 == flowing.len() { 0.0 } else { gap };
        y += heights.get(index).copied().unwrap_or(0.0).max(0.0) + trailing;
    }
    targets
}

/// Visible, clickable rect of a card in surface pixels. `offset` is the
/// horizontal slide; the part that has moved out of the card is not included.
pub fn visible_input_rect(
    origin_x: f64,
    origin_y: f64,
    width: f64,
    height: f64,
    offset: f64,
    opacity: f64,
    scale: i32,
) -> Option<(i32, i32, i32, i32)> {
    if opacity <= 0.01 || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let scale = f64::from(scale.max(1));
    let shift = offset.max(0.0);
    let visible_w = width - shift;
    if visible_w <= 0.0 {
        return None;
    }
    let x = (origin_x * scale + shift * scale).round() as i32;
    let y = (origin_y * scale).round() as i32;
    let w = (visible_w * scale).round() as i32;
    let h = (height * scale).round() as i32;
    if w <= 0 || h <= 0 {
        None
    } else {
        Some((x, y, w, h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
        cubic_bezier(x1, y1, x2, y2, x)
    }

    #[test]
    fn enter_and_exit_hit_the_designer_endpoints() {
        assert!((enter_offset(0.0) - 24.0).abs() < 1e-6);
        assert!(enter_opacity(0.0).abs() < 1e-6);
        assert!(enter_offset(1.0).abs() < 1e-6);
        assert!((enter_opacity(1.0) - 1.0).abs() < 1e-6);

        assert!(exit_offset(0.0).abs() < 1e-6);
        assert!((exit_opacity(0.0) - 1.0).abs() < 1e-6);
        assert!((exit_offset(1.0) - 24.0).abs() < 1e-6);
        assert!(exit_opacity(1.0).abs() < 1e-6);
        assert_eq!(enter_duration(false), Duration::ZERO);
        assert_eq!(exit_duration(true), Duration::from_millis(120));
        assert_eq!(crossfade_duration(true), Duration::from_millis(120));
    }

    #[test]
    fn enter_is_ease_out_and_exit_is_ease_in() {
        // Ease-out is ahead of linear early. Ease-in is behind it.
        assert!(enter_opacity(0.25) > 0.25);
        assert!(exit_offset(0.25) / SLIDE_PX < 0.25);
        let mut previous = 0.0;
        for step in 1..=20 {
            let t = f64::from(step) / 20.0;
            let value = enter_opacity(t);
            assert!(value + 1e-9 >= previous, "enter not monotonic at {t}");
            previous = value;
        }
        previous = 0.0;
        for step in 1..=20 {
            let t = f64::from(step) / 20.0;
            let value = exit_offset(t);
            assert!(value + 1e-9 >= previous, "exit not monotonic at {t}");
            previous = value;
        }
        // The enter control points, sampled directly.
        let mid = curve(0.2, 0.8, 0.2, 1.0, 0.5);
        assert!(mid > 0.5, "enter midpoint {mid}");
        let ease_in = curve(0.42, 0.0, 1.0, 1.0, 0.5);
        assert!(ease_in < 0.5, "exit midpoint {ease_in}");
    }

    #[test]
    fn crossfade_keeps_the_two_faces_complementary() {
        assert!((crossfade_blend(0.0)).abs() < 1e-9);
        assert!((crossfade_blend(1.0) - 1.0).abs() < 1e-9);
        let mid = crossfade_blend(0.5);
        assert!((mid + (1.0 - mid) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn spring_is_critically_damped_and_does_not_overshoot() {
        let critical = 2.0 * (SPRING_STIFFNESS * SPRING_MASS).sqrt();
        assert!(
            (SPRING_DAMPING - critical).abs() < 1e-9,
            "damping {} is not critical ({critical})",
            SPRING_DAMPING
        );

        let distance = 64.0;
        let mut spring = Spring::at(0.0);
        spring.target = distance;
        let mut overshoot = 0.0_f64;
        let mut t = 0.0;
        let dt = 0.001;
        let mut at_quarter = 0.0;
        let mut settled_at = None;
        while t < 0.8 {
            spring.step(dt);
            t += dt;
            overshoot = overshoot.max(spring.position - distance);
            if at_quarter == 0.0 && t >= 0.25 {
                at_quarter = spring.position;
            }
            if settled_at.is_none() && (spring.position - distance).abs() <= 1.0 {
                settled_at = Some(t);
            }
        }
        // ζ = 1 has no overshoot. The tail of a 64px step is about 2.6px at
        // 250ms and inside 1px just after that, which is the quarter-second settle.
        assert!(
            overshoot <= 1e-3,
            "overshoot {overshoot}px, want 0 within float tolerance"
        );
        assert!(
            (at_quarter - distance).abs() <= 3.0,
            "64px step still far at 250ms: {at_quarter}"
        );
        let settled_at = settled_at.expect("spring did not settle");
        assert!(
            settled_at <= 0.32,
            "within 1px at {settled_at}s, want about 250ms"
        );

        let mut upward = Spring::at(distance);
        upward.target = 0.0;
        let mut undershoot = 0.0_f64;
        t = 0.0;
        while t < 0.8 {
            upward.step(dt);
            t += dt;
            undershoot = undershoot.max(-upward.position);
        }
        assert!(
            undershoot <= 1e-3,
            "undershoot {undershoot}px, want 0 within float tolerance"
        );
        assert!(upward.resting(), "position {}", upward.position);
    }

    #[test]
    fn restack_waits_out_the_exit_unless_motion_is_off() {
        assert_eq!(RESTACK_DELAY_MS, EXIT_MS);
        assert!(holds_stack_slot(false, Duration::ZERO, true));
        assert!(holds_stack_slot(true, Duration::from_millis(60), true));
        assert!(holds_stack_slot(true, Duration::from_millis(119), true));
        assert!(!holds_stack_slot(
            true,
            Duration::from_millis(RESTACK_DELAY_MS),
            true
        ));
        assert!(!holds_stack_slot(true, Duration::ZERO, false));
        assert_eq!(restack_delay(false), Duration::ZERO);
    }

    #[test]
    fn restack_targets_pull_the_cards_below_a_gap() {
        let heights = [40.0, 40.0, 50.0];
        let settled = restack_targets(&heights, &[false, false, false], STACK_GAP);
        assert_eq!(settled, vec![Some(0.0), Some(48.0), Some(96.0)]);

        let leaving_middle = restack_targets(&heights, &[false, true, false], STACK_GAP);
        assert_eq!(leaving_middle, vec![Some(0.0), None, Some(48.0)]);

        let leaving_top = restack_targets(&heights, &[true, false, false], STACK_GAP);
        assert_eq!(leaving_top, vec![None, Some(0.0), Some(48.0)]);
    }

    #[test]
    fn y_track_snaps_height_corrections_and_springs_a_restack() {
        let mut track = YTrack::new();
        track.follow(96.0, false);
        assert!(track.anchored);
        assert!((track.spring.position - 96.0).abs() < 1e-9);

        // Own-height correction while the stack is resting.
        track.follow(104.0, false);
        assert!((track.spring.position - 104.0).abs() < 1e-9);

        // A card above left. Hold the current y and aim at the new one.
        track.follow(48.0, true);
        assert!((track.spring.position - 104.0).abs() < 1e-9);
        assert!((track.spring.target - 48.0).abs() < 1e-9);

        track.spring.step(1.0 / 60.0);
        assert!(track.spring.velocity < 0.0);
        let mid = track.spring.position;
        // Leave animation ended; the spring is still moving, so it must not snap.
        track.follow(48.0, false);
        assert!((track.spring.position - mid).abs() < 1e-6);

        for _ in 0..40 {
            track.spring.step(1.0 / 60.0);
            track.follow(48.0, false);
        }
        assert!(track.spring.resting() || (track.spring.position - 48.0).abs() < 1.0);
    }

    #[test]
    fn input_rect_follows_the_slide_and_ignores_a_transparent_card() {
        let full = visible_input_rect(10.0, 20.0, 380.0, 80.0, 0.0, 1.0, 1).unwrap();
        assert_eq!(full, (10, 20, 380, 80));
        let slid = visible_input_rect(10.0, 20.0, 380.0, 80.0, 24.0, 1.0, 1).unwrap();
        assert_eq!(slid, (34, 20, 356, 80));
        assert!(visible_input_rect(0.0, 0.0, 380.0, 80.0, 0.0, 0.0, 1).is_none());
        assert!(visible_input_rect(0.0, 0.0, 24.0, 80.0, 24.0, 1.0, 1).is_none());
        let hidpi = visible_input_rect(10.0, 4.0, 20.0, 8.0, 0.0, 1.0, 2).unwrap();
        assert_eq!(hidpi, (20, 8, 40, 16));
    }

    #[test]
    fn idle_gap_does_not_become_the_spring_step() {
        assert!((animation_dt(2.0) - 1.0 / 60.0).abs() < 1e-9);
        assert!((animation_dt(0.016) - 0.016).abs() < 1e-9);
        assert_eq!(animation_dt(0.0), 0.0);
    }
}
