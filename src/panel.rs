//! An options panel, drawn on the tube.
//!
//! Every variable the command line can set, adjustable while the machine runs.
//! It is drawn over the tube's own frame with the tube's own stroke font, so it
//! needs no widget library and reads as part of the instrument rather than a
//! window on top of it.
//!
//! Dragging a slider changes the *value* immediately - the panel has to follow
//! the mouse - but the **tube is recomposed only once the dragging has settled**,
//! `SETTLE_SECONDS` after the last change.  A fullscreen frame costs ~35 ms and a
//! drag would otherwise recompose the whole screen sixty times a second to show
//! the same picture with one number different.  The panel's own drawing is cheap
//! (text and tracks over the frame that is already there), so it can update every
//! frame while the costly half waits.

use crate::tube::Tube;
use std::time::Instant;

/// How long after the last change the tube is refreshed.
pub const SETTLE_SECONDS: f32 = 0.25;

/// What kind of control a row is.
pub enum Kind {
    /// A value between two bounds, dragged with the mouse.
    Slider { min: f32, max: f32 },
    /// A box that toggles.
    Toggle,
    /// A list stepped through with the mouse or the keyboard; `value` is the index
    /// into `list`.  An index one past the end means "custom" - the caller sets it
    /// when the running values match no entry - and it is what is shown then.
    ///
    /// The list is owned rather than borrowed for the whole program's life: the
    /// preset chooser is the built-ins plus the names the user has saved, and a
    /// saved name is not known when the program is compiled.
    Choice(Vec<String>),
}

/// One adjustable variable: a name, a control, its value.
pub struct Row {
    pub name: &'static str,
    pub kind: Kind,
    /// Sliders and toggles hold their value directly; a choice holds the index
    /// into its list, which is why it is an `f32` with the rest.
    pub value: f32,
    /// Whether changing it means the tube has to be reconfigured and the page
    /// written again (the face, the device, the geometry) rather than just
    /// recomposed.
    pub structural: bool,
    /// Whether the value can only be honoured by *relaunching* the program with
    /// it, rather than by feeding it to the running tube.  A different target or a
    /// different window size is fixed when the process starts, so the panel still
    /// steps and draws the row like any other, but the caller works out a fresh
    /// command line when the change settles instead of applying the number to a
    /// machine that cannot read it.
    ///
    /// Only a *press* does that: a step through the list with the wheel (or a
    /// drag) moves the number and says so on the note line, so browsing cannot end
    /// a session by itself.
    pub launch: bool,
}

impl Row {
    /// The value as it is shown beside the control.
    pub fn shown(&self) -> String {
        match &self.kind {
            Kind::Slider { .. } => format!("{:.3}", self.value),
            Kind::Toggle => if self.value > 0.5 { "on" } else { "off" }.to_string(),
            // An index at or past the end is not an error: it is the custom state,
            // which the caller parks the row in when the running values match no
            // entry, and "custom" is what the user is shown.
            Kind::Choice(list) => list
                .get(self.value as usize)
                .cloned()
                .unwrap_or_else(|| "custom".to_string()),
        }
    }
}

/// The panel's layout for a window.  Everything scales with the *height* and with
/// how many rows there are, so the panel always fits and its text is as large as
/// it can be - a 7 pixel cell is unreadable on a 2560 wide screen.
struct Metrics {
    cell_w: i32,
    cell_h: i32,
    row_h: i32,
    label_w: i32,
    track_w: i32,
}

/// A key the panel understands.  The caller maps its window's keys onto these,
/// so the panel itself never learns about the window system.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelKey {
    Up,
    Down,
    Enter,
    Escape,
}

/// What a caller did to a row.  Both the mouse and the keyboard arrive at
/// `press` carrying one of these, so a control cannot step one way under the
/// pointer and another under Enter.
#[derive(Clone, Copy)]
enum Act {
    /// A press where the pointer is; only a slider looks at the position.
    Click(f32),
    /// Enter, at no position - the keyboard has no pointer to jump to.
    Enter,
    /// The wheel, turned by this much over the row.
    Wheel(f32),
}

pub struct Panel {
    pub open: bool,
    pub rows: Vec<Row>,
    /// The row being dragged, if any.
    dragging: Option<usize>,
    /// Which row the keyboard is on, and the row the drawing marks.  It starts at
    /// the top whenever the panel is opened.
    selected: usize,
    /// When the last change was made, for the settle.
    changed_at: Option<Instant>,
    /// Which row that was.
    last_changed: Option<usize>,
    /// Whether that change was a *press* - Enter, or a click on the row - rather
    /// than a step.  A launch row only restarts the program on a press, so that
    /// walking a list with the wheel can never end a session by accident.
    last_press: bool,
    /// A line under the panel saying what the last change did.
    pub note: String,
    /// True if a settled change needs the tube reconfigured and rewritten.
    pub needs_rebuild: bool,
}

impl Panel {
    pub fn new(rows: Vec<Row>) -> Self {
        Panel {
            open: false,
            rows,
            dragging: None,
            selected: 0,
            changed_at: None,
            last_changed: None,
            last_press: false,
            note: "F2 closes; click a row, or walk with the arrows and Enter".to_string(),
            needs_rebuild: false,
        }
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.dragging = None;
        // the highlight starts at the top every time the panel comes up, so it is
        // never left on a row the user has to hunt for
        self.selected = 0;
    }

    fn metrics(&self, h: usize) -> Metrics {
        let rows = self.rows.len().max(1) as i32;
        let row_h = (((h as i32 - 16) / (rows + 2)).min(34)).max(10);
        let cell_h = (row_h * 8 / 10).max(7);
        let cell_w = (cell_h * 6 / 10).max(4);
        Metrics {
            cell_w,
            cell_h,
            row_h,
            label_w: cell_w * 13,
            track_w: cell_w * 11,
        }
    }

    /// The panel's rectangle in window pixels: down the left, centred.
    fn rect(&self, w: usize, h: usize) -> (i32, i32, i32, i32) {
        let m = self.metrics(h);
        let ph = (self.rows.len() as i32 + 2) * m.row_h + 8;
        let pw = (m.label_w + m.track_w + m.cell_w * 16 + 16).min(w as i32 - 8);
        (8, ((h as i32 - ph) / 2).max(4), pw, ph)
    }

    /// Where a row's control starts.
    pub fn control_x(&self, h: usize) -> i32 {
        self.rect(4096, h).0 + self.metrics(h).label_w
    }

    pub fn row_y(&self, w: usize, h: usize, i: usize) -> (i32, i32) {
        let (x, y, _, _) = self.rect(w, h);
        (
            x,
            y + self.metrics(h).row_h + i as i32 * self.metrics(h).row_h,
        )
    }

    /// The row with this name, for callers that work in names rather than
    /// indices.  The panel's own tests and the composition test in main.rs use
    /// these; the running loop works in indices and reads the pointer instead.
    #[allow(dead_code)]
    pub fn row_index(&self, name: &str) -> Option<usize> {
        self.rows.iter().position(|r| r.name == name)
    }

    /// How wide a slider's track - and a list's control, which is the same
    /// rectangle - is.
    #[allow(dead_code)]
    pub fn track_width(&self, h: usize) -> i32 {
        self.metrics(h).track_w
    }

    /// A change was made: note when, whether it was a press, and whether the row
    /// that changed needs the tube reconfigured.  It is the *row* that decides,
    /// not the drag: a choice or a toggle has no drag and can still be structural.
    fn touched(&mut self, row: usize, press: bool) {
        self.changed_at = Some(Instant::now());
        self.last_changed = Some(row);
        self.last_press = press;
        self.needs_rebuild |= self.rows[row].structural;
    }

    /// Has the dragging settled?  `Some((row, press))` once, `SETTLE_SECONDS`
    /// after the last change, naming the row that changed and whether the change
    /// was a press, so the caller can apply the part that only that variable needs
    /// and can tell a deliberate choice from a step through a list.  A drag
    /// refreshes the tube when it stops rather than while it moves.
    pub fn settle_due(&mut self, now: Instant) -> Option<(usize, bool)> {
        match self.changed_at {
            Some(t) if now.duration_since(t).as_secs_f32() >= SETTLE_SECONDS => {
                self.changed_at = None;
                self.last_changed.map(|row| (row, self.last_press))
            }
            _ => None,
        }
    }

    /// Act on one row, whatever kind it is.  True if something changed.
    ///
    /// This is the one place a control changes, so a click and Enter cannot drift
    /// apart: both come through the match below.  `h` is here because only the
    /// geometry knows where a slider's track starts, and the keyboard's `Enter`
    /// carries no pointer, so it steps a slider rather than jumping.
    fn press(&mut self, i: usize, h: usize, act: Act) -> bool {
        let m = self.metrics(h);
        let cx = self.control_x(h);
        let mut changed = false;
        match &self.rows[i].kind {
            Kind::Slider { min, max } => {
                let (min, max) = (*min, *max);
                match act {
                    Act::Click(px) => {
                        let (t0, t1) = (cx as f32, (cx + m.track_w) as f32);
                        let t = ((px - t0) / (t1 - t0)).clamp(0.0, 1.0);
                        let v = min + (max - min) * t;
                        if (v - self.rows[i].value).abs() > 1e-9 {
                            self.rows[i].value = v;
                            changed = true;
                        }
                    }
                    Act::Enter => {
                        // no pointer to jump to, so step: a twentieth of the range is
                        // coarse enough to feel like a press, and it wraps past the top
                        // back to the bottom so the whole range is reachable from the
                        // keyboard alone.
                        let step = (max - min) / 20.0;
                        let v = self.rows[i].value + step;
                        let v = if v > max { min } else { v };
                        if (v - self.rows[i].value).abs() > 1e-9 {
                            self.rows[i].value = v;
                            changed = true;
                        }
                    }
                    Act::Wheel(d) => {
                        let step = (max - min) / 200.0;
                        let v = (self.rows[i].value + d * step).clamp(min, max);
                        if (v - self.rows[i].value).abs() > 1e-9 {
                            self.rows[i].value = v;
                            changed = true;
                        }
                    }
                }
            }
            Kind::Toggle => {
                // a box flips on a press; the wheel is not a press, so it leaves
                // the box alone rather than toggling it under a stray scroll
                if !matches!(act, Act::Wheel(_)) {
                    self.rows[i].value = if self.rows[i].value > 0.5 { 0.0 } else { 1.0 };
                    changed = true;
                }
            }
            Kind::Choice(list) => {
                let n = list.len();
                if n == 0 {
                    return false;
                }
                // An index at or past the end is the custom state; treat it as one
                // step past the last entry so a press always lands back inside the
                // list.  The custom index is never reached by stepping - only the
                // caller parks a row there.
                let cur = (self.rows[i].value as usize).min(n);
                let next = match act {
                    Act::Wheel(d) if d < 0.0 => {
                        if cur == 0 {
                            n - 1
                        } else {
                            cur - 1
                        }
                    }
                    _ => {
                        if cur + 1 >= n {
                            0
                        } else {
                            cur + 1
                        }
                    }
                };
                if self.rows[i].value != next as f32 {
                    self.rows[i].value = next as f32;
                    changed = true;
                }
            }
        }
        if changed {
            // A press is Enter, or a click on the row: the two acts that mean
            // "this one".  The wheel steps a value without meaning to commit to
            // it, which is what makes a list safe to walk through - and the reason
            // a launch row cannot restart the program under a browsing hand.
            let press = matches!(act, Act::Enter | Act::Click(_));
            self.touched(i, press);
            // A row that can only be honoured by starting over says so here,
            // before anything happens: the note line exists so the machine never
            // does something the user was not told about, and a session being
            // replaced is the largest thing it does.  A *step* through such a row
            // says what the step did instead, because by itself it changes
            // nothing but the number shown.
            self.note = match (self.rows[i].launch, press) {
                (true, true) => format!(
                    "{} = {} - relaunch: session ends",
                    self.rows[i].name,
                    self.rows[i].shown()
                ),
                (true, false) => format!(
                    "{} = {} - press the row to restart with it",
                    self.rows[i].name,
                    self.rows[i].shown()
                ),
                (false, _) => format!("{} = {}", self.rows[i].name, self.rows[i].shown()),
            };
        }
        changed
    }

    /// Which row the keyboard is on.  The panel's own tests read this; the
    /// running loop does not need it yet, because the highlight is drawn from the
    /// same field the keys move.
    #[allow(dead_code)]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Move the highlight, or act on the selected row.  Returns true if a row
    /// changed, so the caller applies it exactly as it does for a mouse press
    /// (the same settle-to-apply path).  Escape closes the panel.
    ///
    /// The arrows stop at the ends rather than wrapping: with the eyes on the
    /// tube, a highlight that jumps from the last row to the first reads as a lost
    /// mark rather than as a wrap.
    pub fn key(&mut self, k: PanelKey, _w: usize, h: usize) -> bool {
        if !self.open {
            return false;
        }
        let n = self.rows.len();
        match k {
            PanelKey::Up => {
                self.selected = self.selected.saturating_sub(1);
                false
            }
            PanelKey::Down => {
                if n > 0 {
                    self.selected = (self.selected + 1).min(n - 1);
                }
                false
            }
            PanelKey::Enter => {
                if n == 0 {
                    return false;
                }
                self.press(self.selected.min(n - 1), h, Act::Enter)
            }
            PanelKey::Escape => {
                self.open = false;
                self.dragging = None;
                false
            }
        }
    }

    /// Handle the mouse.  Returns true if a value changed.
    ///
    /// The hit tests live here, where the geometry is - a press in a slider's
    /// track jumps the value there and starts a drag, which is what makes a slider
    /// feel like one; a click on a box or a list steps it; the wheel nudges
    /// whatever row is under the pointer.  What each hit *means* is `press`, which
    /// the keyboard also runs.
    pub fn handle(&mut self, w: usize, h: usize, pos: (f32, f32), down: bool, wheel: f32) -> bool {
        if !self.open {
            self.dragging = None;
            return false;
        }
        let (px, py) = (pos.0, pos.1);
        if !down {
            self.dragging = None;
        }
        let m = self.metrics(h);
        let cx = self.control_x(h);
        let (t0, t1) = (cx as f32, (cx + m.track_w) as f32);
        for i in 0..self.rows.len() {
            let (_, y) = self.row_y(w, h, i);
            if py < (y - 2) as f32 || py > (y + m.row_h - 2) as f32 {
                continue;
            }
            let act = match &self.rows[i].kind {
                Kind::Slider { .. } => {
                    if down && px >= t0 - 6.0 && px <= t1 + 6.0 {
                        self.dragging = Some(i);
                        Some(Act::Click(px))
                    } else if wheel.abs() > 0.0 && px >= cx as f32 {
                        Some(Act::Wheel(wheel))
                    } else {
                        None
                    }
                }
                Kind::Toggle => {
                    if down && px >= cx as f32 && px <= (cx + m.cell_h) as f32 {
                        Some(Act::Click(px))
                    } else {
                        None
                    }
                }
                Kind::Choice(_) => {
                    // the list's control is as wide as a slider's track, so the hit
                    // test and the drawing are the same rectangle
                    if down && px >= t0 && px <= t1 {
                        Some(Act::Click(px))
                    } else if wheel.abs() > 0.0 && px >= cx as f32 {
                        Some(Act::Wheel(wheel))
                    } else {
                        None
                    }
                }
            };
            // the row under the pointer is the only one that matters, hit or miss
            if let Some(act) = act {
                if matches!(act, Act::Click(_)) {
                    // the mark follows the mouse, so Enter then acts on what was
                    // just clicked rather than on some row the user forgot about
                    self.selected = i;
                }
                return self.press(i, h, act);
            }
            break;
        }
        false
    }

    /// Draw the panel over a frame that is already composed.
    pub fn draw(&self, buf: &mut [u32], w: usize, h: usize, tube: &Tube) {
        if !self.open {
            return;
        }
        let m = self.metrics(h);
        let (x, y, pw, ph) = self.rect(w, h);
        // a dark backdrop, so the text reads on a lit tube as well as a dark one
        for yy in y.max(0)..(y + ph).min(h as i32) {
            for xx in x.max(0)..(x + pw).min(w as i32) {
                let i = (yy * w as i32 + xx) as usize;
                if i < buf.len() {
                    let p = buf[i];
                    let dim = |c: u32| ((c & 0xff) as f32 * 0.2) as u32;
                    buf[i] = 0x0020_0000
                        | (dim((p >> 16) & 0xff) << 16)
                        | (dim((p >> 8) & 0xff) << 8)
                        | dim(p & 0xff);
                }
            }
        }
        let title = 0x00ff_e9b0u32;
        let label = 0x00c8_dcb4u32;
        let track_c = 0x0060_6060u32;
        let knob = 0x00ff_d070u32;
        let text = |buf: &mut [u32], x: i32, y: i32, s: &str, colour: u32| {
            text(buf, w, h, tube, x, y, s, colour, m.cell_w, m.cell_h)
        };
        text(buf, x + 4, y + 3, "OPTIONS  (F2 closes)", title);
        let cx = self.control_x(h);
        for (i, row) in self.rows.iter().enumerate() {
            let (rx, ry) = self.row_y(w, h, i);
            // The keyboard's mark sits in the cell to the left of the labels, so
            // the names line up whether or not the mark is on them.  A filled box
            // in the knob's amber, because that is the colour the panel already
            // uses for "this is the live one" - no new colour, no layout change.
            if i == self.selected {
                rect(buf, w, h, rx, ry + 1, (m.cell_w / 3).max(3), m.cell_h, knob);
            }
            text(buf, rx + m.cell_w + 2, ry, row.name, label);
            let mid = ry + m.row_h / 2;
            match &row.kind {
                Kind::Slider { min, max } => {
                    let (min, max) = (*min, *max);
                    rect(buf, w, h, cx, mid - 1, m.track_w, 3, track_c);
                    let t = if max > min {
                        ((row.value - min) / (max - min)).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let kx = cx + (t * m.track_w as f32) as i32;
                    rect(
                        buf,
                        w,
                        h,
                        kx - m.cell_w / 3,
                        ry + 1,
                        (m.cell_w * 2 / 3).max(3),
                        m.row_h - 2,
                        knob,
                    );
                    text(buf, cx + m.track_w + m.cell_w, ry, &row.shown(), title);
                }
                Kind::Toggle => {
                    let on = row.value > 0.5;
                    rect(
                        buf,
                        w,
                        h,
                        cx,
                        ry + 1,
                        m.cell_h,
                        m.cell_h,
                        if on { knob } else { track_c },
                    );
                    text(buf, cx + m.cell_h + 4, ry, &row.shown(), title);
                }
                Kind::Choice(_) => {
                    // the chosen entry, framed by chevrons pointing at it, so a list
                    // reads as something to step rather than as another number in the
                    // column.  Centred in the track's width, where a slider's track
                    // and a box sit.
                    let shown = row.shown();
                    let width = shown.chars().count() as i32 * m.cell_w;
                    let tx = cx + ((m.track_w - width) / 2).max(m.cell_w);
                    text(buf, cx, ry, "<", label);
                    text(buf, cx + m.track_w - m.cell_w, ry, ">", label);
                    text(buf, tx, ry, &shown, title);
                }
            }
        }
        let (_, ty) = self.row_y(w, h, self.rows.len());
        text(buf, x + 4, ty + 2, &self.note, title);
    }
}

/// A filled rectangle, clipped to the frame.
fn rect(buf: &mut [u32], w: usize, h: usize, x: i32, y: i32, rw: i32, rh: i32, colour: u32) {
    for yy in y.max(0)..(y + rh).min(h as i32) {
        for xx in x.max(0)..(x + rw).min(w as i32) {
            let i = (yy * w as i32 + xx) as usize;
            if i < buf.len() {
                buf[i] = colour;
            }
        }
    }
}

/// A line, drawn with a two pixel brush so panel text reads at small sizes.
fn line(buf: &mut [u32], w: usize, h: usize, x0: f32, y0: f32, x1: f32, y1: f32, colour: u32) {
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).ceil().max(1.0) as i32;
    for s in 0..=steps {
        let t = s as f32 / steps as f32;
        let x = (x0 + (x1 - x0) * t).round() as i32;
        let y = (y0 + (y1 - y0) * t).round() as i32;
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let (px, py) = (x + dx, y + dy);
            if px >= 0 && py >= 0 && px < w as i32 && py < h as i32 {
                buf[(py * w as i32 + px) as usize] = colour;
            }
        }
    }
}

/// Text, stroked through the tube's own font.
///
/// The tube's font works in *its* coordinates - y up from the cell's bottom - and
/// the frame is top down, so the glyph is flipped into the buffer as it is drawn.
/// (Drawn the other way round the text comes out mirrored, which is what the
/// first version of this did.)
fn text(
    buf: &mut [u32],
    w: usize,
    h: usize,
    tube: &Tube,
    x: i32,
    y: i32,
    s: &str,
    colour: u32,
    cell_w: i32,
    cell_h: i32,
) {
    let mut cx = x;
    for ch in s.chars() {
        let c = if ch.is_ascii() { ch as u8 } else { b'?' };
        if c != b' ' {
            tube.glyph_strokes(c, 0.0, 0.0, cell_w, cell_h, |a, b, cc, d| {
                let flip = |v: f32| (y + cell_h) as f32 - v;
                line(
                    buf,
                    w,
                    h,
                    cx as f32 + a,
                    flip(b),
                    cx as f32 + cc,
                    flip(d),
                    colour,
                );
            });
        }
        cx += cell_w;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// An owned choice list, the way the caller now has to build one.
    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn panel() -> Panel {
        Panel::new(vec![
            Row {
                name: "beam",
                kind: Kind::Slider { min: 0.0, max: 1.0 },
                value: 0.5,
                structural: false,
                launch: false,
            },
            Row {
                name: "cell",
                kind: Kind::Slider { min: 0.5, max: 3.0 },
                value: 1.0,
                structural: true,
                launch: false,
            },
            Row {
                name: "camera",
                kind: Kind::Toggle,
                value: 1.0,
                structural: false,
                launch: false,
            },
        ])
    }

    const W: usize = 1024;
    const H: usize = 780;

    /// A panel whose first row is a list, for the choice and keyboard tests.
    fn choice_panel() -> Panel {
        Panel::new(vec![
            Row {
                name: "face",
                kind: Kind::Choice(names(&["mono", "gothic", "simplex"])),
                value: 0.0,
                structural: false,
                launch: false,
            },
            Row {
                name: "beam",
                kind: Kind::Slider { min: 0.0, max: 1.0 },
                value: 0.5,
                structural: false,
                launch: false,
            },
        ])
    }

    /// A press in a slider's track jumps the value there, and dragging keeps it
    /// under the pointer.
    #[test]
    fn a_slider_follows_the_mouse() {
        let mut p = panel();
        p.open = true;
        let (_, ry) = p.row_y(W, H, 0);
        let cx = p.control_x(H);
        let track = p.metrics(H).track_w;
        let y = (ry + 4) as f32;
        let x = (cx + track / 4) as f32;
        assert!(
            p.handle(W, H, (x, y), true, 0.0),
            "a press changes the value"
        );
        assert!(
            (p.rows[0].value - 0.25).abs() < 0.05,
            "pressed a quarter in, value {:.3}",
            p.rows[0].value
        );
        let x2 = (cx + track * 3 / 4) as f32;
        assert!(p.handle(W, H, (x2, y), true, 0.0));
        assert!(
            (p.rows[0].value - 0.75).abs() < 0.05,
            "{:.3}",
            p.rows[0].value
        );
        p.handle(W, H, (x, y), false, 0.0);
        assert!(
            !p.handle(W, H, (x, y), false, 0.0),
            "nothing held, nothing changes"
        );
    }

    /// A toggle flips, and a structural row asks for a rebuild even when it is
    /// changed by a press in its track rather than by a drag.

    #[test]
    fn a_press_changes_the_row_it_lands_on() {
        let mut p = panel();
        p.open = true;
        let cx = p.control_x(H);
        let (_, ry1) = p.row_y(W, H, 1);
        let track = p.metrics(H).track_w;
        assert!(p.handle(W, H, ((cx + track / 2) as f32, (ry1 + 4) as f32), true, 0.0));
        assert!(
            (p.rows[1].value - 1.75).abs() < 0.1,
            "the cell slider followed the press: {:.3}",
            p.rows[1].value
        );
        assert!(p.needs_rebuild, "the cell size is structural");
        // the flag is a latch that the caller clears after the rebuild
        p.needs_rebuild = false;
        let (_, ry2) = p.row_y(W, H, 2);
        assert!(p.handle(W, H, ((cx + 5) as f32, (ry2 + 4) as f32), true, 0.0));
        assert_eq!(p.rows[2].value, 0.0, "the toggle flipped off");
        assert!(!p.needs_rebuild, "and it did not ask for a rebuild");
    }

    /// The tube is refreshed once the dragging has settled, not while it moves,
    /// and the caller is told which row changed.
    #[test]
    fn the_refresh_waits_for_the_drag_to_stop() {
        let mut p = panel();
        p.open = true;
        let cx = p.control_x(H);
        let (_, ry) = p.row_y(W, H, 0);
        let y = (ry + 4) as f32;
        p.handle(W, H, ((cx + 10) as f32, y), true, 0.0);
        let now = Instant::now();
        assert!(p.settle_due(now).is_none(), "not while the drag is fresh");
        p.handle(W, H, ((cx + 40) as f32, y), true, 0.0);
        assert!(
            p.settle_due(now + Duration::from_millis(200)).is_none(),
            "a later change restarts the wait"
        );
        assert_eq!(
            p.settle_due(now + Duration::from_millis(200 + 300)),
            Some((0, true)),
            "and it names the row once the dragging has stopped, as a press"
        );
        assert!(
            p.settle_due(now + Duration::from_millis(400)).is_none(),
            "only once"
        );
    }

    /// The panel is laid out for the window it is in: it fits, and the rows are
    /// the same height for the mouse as for the drawing.
    #[test]
    fn the_layout_fits_the_window() {
        let mut p = panel();
        p.rows = (0..30)
            .map(|i| Row {
                name: "row",
                kind: Kind::Slider { min: 0.0, max: 1.0 },
                value: i as f32,
                structural: false,
                launch: false,
            })
            .collect();
        for (w, h) in [(1024usize, 780usize), (2560, 1440), (640, 400)] {
            let (_, _, _, ph) = p.rect(w, h);
            assert!(ph <= h as i32 - 8, "the panel must fit {w}x{h}: {ph} tall");
            let m = p.metrics(h);
            assert!(m.cell_h >= 7 && m.row_h > m.cell_h, "readable cells at {h}");
            // the last row is inside the window
            let (_, ly) = p.row_y(w, h, p.rows.len() - 1);
            assert!(ly + m.row_h <= h as i32, "the last row is on screen at {h}");
        }
    }

    /// A choice steps to the next entry on a press - by the mouse or by Enter,
    /// which is the same press - and the last entry wraps to the first, so a
    /// three way setting is reachable without a pointer landing exactly right.
    #[test]
    fn a_choice_steps_through_its_entries_and_wraps() {
        let mut p = choice_panel();
        p.open = true;
        assert_eq!(p.rows[0].shown(), "mono");
        assert!(p.key(PanelKey::Enter, W, H), "the press changed the row");
        assert_eq!(p.rows[0].shown(), "gothic");
        p.key(PanelKey::Enter, W, H);
        assert_eq!(p.rows[0].shown(), "simplex");
        p.key(PanelKey::Enter, W, H);
        assert_eq!(
            p.rows[0].shown(),
            "mono",
            "the last entry wraps to the first"
        );

        // a click in the list's control is the same step as Enter
        let cx = p.control_x(H);
        let (_, ry) = p.row_y(W, H, 0);
        assert!(p.handle(W, H, ((cx + 5) as f32, (ry + 4) as f32), true, 0.0));
        assert_eq!(p.rows[0].shown(), "gothic", "the click stepped the list");

        // and a step settles like any other change, naming the row
        assert_eq!(
            p.settle_due(Instant::now() + Duration::from_millis(400)),
            Some((0, true)),
            "a click is a press"
        );
    }

    /// An index at or past the end of the list is not an error: it is how the
    /// caller says the running values match no entry, and the panel shows
    /// "custom" - then a press brings the row back onto the list.
    #[test]
    fn an_index_past_the_end_shows_custom() {
        let mut p = choice_panel();
        p.rows[0].value = 3.0; // one past a three entry list
        assert_eq!(p.rows[0].shown(), "custom");
        p.open = true;
        assert!(p.key(PanelKey::Enter, W, H));
        assert_eq!(p.rows[0].shown(), "mono", "a press returns to the list");
    }

    /// The arrows move the highlight and Enter acts on the row it is on, through
    /// the same press the mouse uses - so a slider steps and the settle names the
    /// row the keyboard changed, exactly as it would for a drag.
    #[test]
    fn the_arrows_move_the_highlight_and_enter_presses_the_row() {
        let mut p = panel();
        p.open = true;
        assert_eq!(p.selected(), 0, "the panel opens at the top");
        assert!(!p.key(PanelKey::Down, W, H), "moving changes no value");
        assert_eq!(p.selected(), 1);
        p.key(PanelKey::Down, W, H);
        assert_eq!(p.selected(), 2);
        p.key(PanelKey::Down, W, H);
        assert_eq!(p.selected(), 2, "the highlight stops at the last row");
        p.key(PanelKey::Up, W, H);
        assert_eq!(p.selected(), 1);

        let before = p.rows[1].value;
        assert!(p.key(PanelKey::Enter, W, H), "Enter changed the row");
        assert!(
            p.rows[1].value > before,
            "the slider stepped up: {:.3} -> {:.3}",
            before,
            p.rows[1].value
        );
        assert!(p.needs_rebuild, "the cell size is structural");
        assert_eq!(
            p.settle_due(Instant::now() + Duration::from_millis(400)),
            Some((1, true)),
            "the settle names the row the keyboard changed, and that Enter is a press"
        );
    }

    /// Escape closes the panel the way F2 does, and a closed panel takes no keys;
    /// reopening it starts the highlight at the top again.
    #[test]
    fn escape_closes_the_panel() {
        let mut p = panel();
        p.open = true;
        assert!(!p.key(PanelKey::Escape, W, H), "closing changes no row");
        assert!(!p.open, "the panel is closed");
        assert!(!p.key(PanelKey::Enter, W, H), "a closed panel ignores keys");

        p.toggle();
        assert!(p.open);
        p.key(PanelKey::Down, W, H);
        assert_eq!(p.selected(), 1);
        p.toggle();
        p.toggle();
        assert!(p.open, "reopened");
        assert_eq!(p.selected(), 0, "and the highlight is back at the top");
    }

    /// A row that can only be honoured by starting the program again says so on
    /// the note line the moment it is pressed - the note is the only warning the
    /// user gets that the session is about to be replaced - and it is not a
    /// reconfigure of the tube, which could not make the change.
    #[test]
    fn a_launch_row_says_the_session_ends() {
        let mut p = Panel::new(vec![Row {
            name: "device",
            kind: Kind::Choice(names(&["4010", "4014"])),
            value: 0.0,
            structural: false,
            launch: true,
        }]);
        p.open = true;
        assert!(p.key(PanelKey::Enter, W, H), "the press moved the row");
        assert_eq!(p.rows[0].shown(), "4014");
        assert!(
            p.note.contains("session ends"),
            "the note must say the session ends: {}",
            p.note
        );
        assert!(!p.needs_rebuild, "a relaunch is not a tube reconfigure");
        assert_eq!(
            p.settle_due(Instant::now() + Duration::from_millis(400)),
            Some((0, true)),
            "and a press is what commits it"
        );
    }

    /// The wheel walks a launch row without committing to it.  The number moves,
    /// the note says what a press would do instead, and the caller is told this was
    /// a *step* - so browsing the list of targets cannot restart the program under
    /// a hand that was only looking.
    #[test]
    fn a_step_through_a_launch_row_browses_without_committing() {
        let mut p = Panel::new(vec![Row {
            name: "zoom",
            kind: Kind::Choice(names(&["1", "2", "3", "4"])),
            value: 0.0,
            structural: false,
            launch: true,
        }]);
        p.open = true;
        let cx = p.control_x(H);
        let (_, ry) = p.row_y(W, H, 0);
        assert!(p.handle(
            W,
            H,
            ((cx + 5) as f32, (ry + 4) as f32),
            false,
            -1.0
        ));
        assert_eq!(
            p.rows[0].shown(),
            "4",
            "the wheel stepped back past the first entry"
        );
        assert!(
            p.note.contains("press the row to restart"),
            "the note has to say how it is committed: {}",
            p.note
        );
        assert_eq!(
            p.settle_due(Instant::now() + Duration::from_millis(400)),
            Some((0, false)),
            "and the caller is told this was a step, not a press"
        );
    }

    /// A structural row - the face - is applied by writing the page again rather
    /// than by a relaunch, and the panel says so with its own flag rather than on
    /// the note line: nothing is ending.
    #[test]
    fn a_structural_row_asks_for_a_rebuild_not_a_restart() {
        let mut p = Panel::new(vec![Row {
            name: "font",
            kind: Kind::Choice(names(&["simplex", "mono", "gothic"])),
            value: 0.0,
            structural: true,
            launch: false,
        }]);
        p.open = true;
        let cx = p.control_x(H);
        let (_, ry) = p.row_y(W, H, 0);
        p.handle(W, H, ((cx + 5) as f32, (ry + 4) as f32), false, -1.0);
        assert_eq!(p.rows[0].shown(), "gothic", "the wheel stepped the face");
        assert!(p.needs_rebuild, "the tube has to be configured again");
        assert!(
            !p.note.contains("session ends"),
            "a face is not a session: {}",
            p.note
        );
        assert_eq!(
            p.settle_due(Instant::now() + Duration::from_millis(400)),
            Some((0, false)),
            "and it is not a launch, whoever asked for it"
        );
    }

    /// A choice list built while the program runs - the built-in presets plus
    /// whatever the user has saved - is stepped exactly like a compiled-in one,
    /// and the entries past the compiled-in ones are reachable.
    #[test]
    fn a_choice_list_built_at_run_time_can_be_stepped() {
        let mut p = Panel::new(vec![Row {
            name: "preset",
            kind: Kind::Choice(names(&["wyse", "tek", "user 1", "user 2"])),
            value: 0.0,
            structural: false,
            launch: false,
        }]);
        p.open = true;
        for want in ["tek", "user 1", "user 2", "wyse"] {
            p.key(PanelKey::Enter, W, H);
            assert_eq!(p.rows[0].shown(), want);
        }
    }
}
