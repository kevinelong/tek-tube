//! Terminal mode: a real shell rendered through the storage tube.
//!
//! This is M2-M4 of `docs/terminal-mode.md`.  Three pieces:
//!
//! * **the process** - a shell under a pseudoconsole (`portable-pty`), with its
//!   output read on a thread into a channel and its input written when we have
//!   something to say.  Two things bit hard enough to write down: the console
//!   host sends `CSI 6n` ("where is your cursor?") and *blocks* until answered,
//!   and the reader thread must never be joined, because it sits in a pty read
//!   that only ends when the pty does.
//! * **the screen** - `vt::Screen` and `vt::Parser`, driven by whatever the
//!   shell emits.
//! * **the image** - the part that is actually interesting.  A storage tube
//!   cannot erase a character, so a terminal on one has to work the way the
//!   existing plot renderer works: content that is *moving* is written into the
//!   transient population only, so it flashes hot and decays, and content that
//!   has *settled* is committed to the stored image by erasing the old stored
//!   image and re-stroking the current screen.  During the transient phase the
//!   previous frame is still decaying underneath the new one, which is the
//!   smear; the commit is the resolve.

use crate::panel;
use crate::protocol::{LineStyle, Style, Write};
use crate::tube::Tube;
use crate::vt;
use crate::vt::PageChange;
use minifb::Key;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write as IoWrite};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

/// The tube's own page at scale 1, which is what it advertises to the shell:
/// 74x35 gives cells of about 13.8 x 22.3 dots, a 0.62 aspect that suits the 4:3
/// face far better than an 80x24 grid would.
pub const PAGE_COLS: usize = 74;
pub const PAGE_ROWS: usize = 35;

/// The cell size as a multiple of the 4010's own 74x35 cell.  Scale 2 halves
/// the page in each direction, which doubles the glyphs: 37 columns and 18 rows,
/// which is about as coarse as a shell can still be read at on this face.
pub const CELL_SCALE_DEFAULT: f32 = 2.0;

/// The 4010's own cell width in dots: what "scale 1" means on the face.
pub const CELL_DOTS: f32 = 1024.0 / PAGE_COLS as f32;

/// The cell size a `tube_w` dot wide target is showing with `cols` columns, as
/// a multiple of the 4010's own cell.
///
/// In terminal mode the tube *is* the window, so a fullscreen window is a
/// bigger target with the same page shape and the same fraction of the screen
/// per glyph.  Everything measured in cell units has to follow that or the text
/// goes spindly: the stroke weight above all.
pub fn cell_scale_of(tube_w: i32, cols: usize) -> f32 {
    (tube_w as f32 / cols.max(1) as f32) / CELL_DOTS
}

/// Target dots per second the pen draws at unless told otherwise.
///
/// Measured, so that the number means something.  A plot file's beam sweeps at
/// 4,587 to 15,193 of the 4010's dots per second on the sample vector plots at
/// the default 19,200 baud (11,096 for `05_snoopy`, 15,193 for `09_wizard`), and
/// at 401 dots/s on a point plot, which draws every byte as a dot instead of a
/// stroke.  The pen was being run at 400 - ten to thirty times slower than the
/// plots it sits beside - and that was the whole of the complaint about the
/// character drawing.  4,000 fixed that and then overshot: a letter is about 60
/// dots, so 4,000 is about 65 letters a second and reads as a blur.  2,500 is
/// about 42 a second - still inside the plot band, still clearly being written,
/// and `--write-rate` moves it either way.
pub const WRITE_RATE_DEFAULT: f32 = 2500.0;

/// The page for a cell scale.
pub fn page(scale: f32) -> (usize, usize) {
    let scale = scale.max(0.5);
    let cols = ((PAGE_COLS as f32 / scale).round() as usize).clamp(8, PAGE_COLS);
    let rows = ((PAGE_ROWS as f32 / scale).round() as usize).clamp(2, PAGE_ROWS);
    (cols, rows)
}

/// The page for a window, at a cell scale: the same ratio as the tube's own
/// page, so a resize adds or removes whole cells instead of stretching glyphs.
pub fn page_for_window(w: usize, h: usize, scale: f32) -> (usize, usize) {
    let scale = scale.max(0.5);
    let cols = ((w as f32 * PAGE_COLS as f32) / (1024.0 * scale)).round() as usize;
    let rows = ((h as f32 * PAGE_ROWS as f32) / (780.0 * scale)).round() as usize;
    (cols.clamp(8, 400), rows.clamp(2, 400))
}

/// Above this share of the page, a change is a page replacement rather than
/// something to trace.
///
/// A clear, an alt screen switch or a resize changes the whole page at once and
/// is reported as such; this is the backstop for anything else that redraws most
/// of the page without saying so.  A *scroll* is not one of these - the image
/// moves with it - so it is handled before this is asked.
pub fn wholesale(stale: usize, cells: usize) -> bool {
    stale * 4 > cells
}

/// Seconds of quiet after which the image is committed (the "resolve").
pub const COMMIT_QUIET: f32 = 0.30;
/// ... and the longest a continuously changing screen may stay un-committed, so
/// that a busy screen still resolves instead of smearing for ever.
pub const COMMIT_MAX: f32 = 0.55;
/// Rows per second a scroll slides at.  A scroll is a *move* of the committed
/// image, not a redraw, so this is only how fast the eye sees it happen.
pub const SLIDE_RATE: f32 = 30.0;

/// Input the window hands to the terminal.
pub enum Input {
    Text(char),
    Key(Key, bool),
}

/// The modifier keys the window has reported.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// What a batch of window input asks the terminal to do.
#[derive(Default)]
pub struct InputBatch {
    /// The bytes for the shell.
    pub bytes: Vec<u8>,
    pub mods: Mods,
    pub fullscreen: bool,
    pub scroll_lines: i32,
    /// The options panel's keys.  Empty unless the panel is open, in which case
    /// the four navigation keys become panel keys and produce no bytes: the shell
    /// under the panel must not see the arrow that moved the selection.
    pub panel_keys: Vec<panel::PanelKey>,
}

/// Translate one batch of window input into the bytes the shell should receive,
/// plus the terminal's own actions.
///
/// A control character **never** comes from the character path.  A real key
/// press arrives from Windows twice - `WM_KEYDOWN` as a key event and the
/// `WM_CHAR` translated from it as a character event, and a second `WM_CHAR`
/// follows - and both paths translate the same key, so one press of Enter put
/// three carriage returns on the wire: the shell ran the command plus two empty
/// ones and the tube showed two prompts where there should be one, which is
/// exactly the "two prompts after an echo or a cls" fault.  Deduplicating within
/// a batch is not enough, because the stray `WM_CHAR` can arrive a frame later;
/// the key event is the report that carries the modifiers, so the key path owns
/// every control key and the character path sends text only.
///
/// The cost is a pasted line break, which arrives as a character with no key
/// event.  This is a terminal with no bracketed paste, and a doubled Enter on
/// every press is a far worse trade than that.
///
/// Bracketed paste is not implemented, and deliberately: minifb's window has no
/// paste of its own - it never reads the clipboard, so a paste is not something
/// the window can even see - and the one signal that might suggest one, a burst
/// of character events inside a frame, is indistinguishable from typing, a held
/// key's repeats or an IME committing a word.  Wrapping a guess would turn
/// ordinary keystrokes into `ESC [ 200 ~` messes, and the mode only earns its
/// keep if it is reliable.  This has to be revisited with a real paste source in
/// front of the window, not guessed from timing.
///
/// With `panel_open` the options panel has taken the keyboard: the four
/// navigation keys come back in `panel_keys` rather than going down the wire, so
/// moving the panel's selection cannot also move the shell's cursor or run its
/// command line.  Everything else - F11, the scrollback chords, plain typing -
/// behaves as it always did, panel or no panel.
pub fn window_input(
    events: &[Input],
    mut mods: Mods,
    scroll_step: i32,
    app_cursor: bool,
    panel_open: bool,
) -> InputBatch {
    let mut out = InputBatch::default();
    for ev in events {
        match ev {
            Input::Text(c) => {
                // text only: see the note above
                let ch = *c;
                if (ch as u32) < 0x20 || ch as u32 == 0x7f {
                    continue;
                }
                let mut buf = [0u8; 4];
                out.bytes
                    .extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
            Input::Key(key, down) => {
                match key {
                    Key::LeftCtrl | Key::RightCtrl => mods.ctrl = *down,
                    Key::LeftAlt | Key::RightAlt => mods.alt = *down,
                    Key::LeftShift | Key::RightShift => mods.shift = *down,
                    _ => {}
                }
                if !down {
                    continue;
                }
                // F11 is the window's, not the shell's: it toggles fullscreen, so
                // it must not go down the wire as a key sequence the shell would
                // see as text
                if *key == Key::F11 {
                    out.fullscreen = true;
                // Shift+PageUp/PageDown read the scrollback, so they are consumed
                // here rather than sent to the shell
                } else if mods.shift && matches!(key, Key::PageUp | Key::PageDown) {
                    out.scroll_lines = if *key == Key::PageUp {
                        scroll_step
                    } else {
                        -scroll_step
                    };
                // the panel's keys, before the shell's: `panel_key` only names
                // the four the panel can use, so anything else falls through to
                // the normal translation below
                } else if let Some(k) = panel_open.then_some(panel_key(*key)).flatten() {
                    out.panel_keys.push(k);
                } else if let Some(bytes) = key_sequence(*key, mods.ctrl, mods.alt, app_cursor) {
                    out.bytes.extend_from_slice(&bytes);
                }
            }
        }
    }
    out.mods = mods;
    out
}

/// The window's mouse for one frame: where the pointer is, which buttons are
/// down, and how far the wheel turned.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct MouseState {
    /// The pointer in window pixels, or `None` when it is outside the window.
    pub pos: Option<(f32, f32)>,
    pub left: bool,
    pub middle: bool,
    pub right: bool,
    /// Wheel notches this frame, positive up.
    pub wheel: i32,
}

/// Wheel notches from the window's own scroll units.
///
/// minifb reports the wheel in tenths of a Windows notch - a notch is 120
/// units, so one notch is 12 here - and a trackpad can report a fraction of
/// one.  A shell is told about notches and has no way to say "a bit of a
/// scroll", so a fraction of a notch is still one notch.
pub fn wheel_notches(dy: f32) -> i32 {
    if dy == 0.0 {
        return 0;
    }
    ((dy.abs() / 12.0).round() as i32).max(1) * if dy > 0.0 { 1 } else { -1 }
}

/// Keep a report the encoder produced.  A mode that does not want an event
/// answers `None`, and there is nothing to send for it.
fn push_report(out: &mut Vec<Vec<u8>>, report: Option<Vec<u8>>) {
    if let Some(bytes) = report {
        out.push(bytes);
    }
}

/// Turns the window's per-frame mouse *state* into the *edges* a shell wants.
///
/// The shell hears about a press when the button goes down and a release when
/// it comes up, never once per frame the button is held, or sixty reports a
/// second would bury it.  Motion is one report per cell the pointer crosses,
/// and only where the tracking mode asks for it: `mouse_tracks_drag` for a
/// move with a button down, `mouse_tracks_motion` for a bare move.  A plain
/// `?1000` application asked for clicks and gets clicks, not a drag report
/// every time the pointer twitches.
///
/// The button state is remembered even on frames where nothing is reported, so
/// opening the panel or turning a tracking mode on mid-session cannot produce
/// a phantom edge: the first reported frame compares against what is really
/// down.
#[derive(Default)]
pub struct Mouse {
    /// Buttons down at the end of the last frame: bit 0 left, 1 middle, 2 right.
    buttons: u8,
    /// The cell last seen, so motion is one report per cell crossed rather than
    /// one per pixel of a slow drag.
    cell: Option<(usize, usize)>,
}

impl Mouse {
    /// One frame's mouse, encoded, in the order it happened.  Empty when no
    /// tracking mode is on, or while the panel is open - the panel is on top,
    /// so the click that moved a slider is the panel's and not the shell's.
    #[allow(clippy::too_many_arguments)]
    pub fn poll(
        &mut self,
        screen: &vt::Screen,
        state: MouseState,
        w: i32,
        h: i32,
        cols: usize,
        rows: usize,
        panel_open: bool,
    ) -> Vec<Vec<u8>> {
        let now = (state.left as u8) | ((state.middle as u8) << 1) | ((state.right as u8) << 2);
        // The cell under the pointer.  A pointer outside the window is not a
        // move: reporting it as cell (0, 0) would drag the shell's cursor into
        // the corner every time the window lost the pointer.
        let mut moved = false;
        if let Some((x, y)) = state.pos {
            let cw = (w.max(1) as f32 / cols.max(1) as f32).max(1.0);
            let ch = (h.max(1) as f32 / rows.max(1) as f32).max(1.0);
            let cell = (
                (x / cw).floor().clamp(0.0, cols.saturating_sub(1) as f32) as usize,
                (y / ch).floor().clamp(0.0, rows.saturating_sub(1) as f32) as usize,
            );
            // The first frame that sees the pointer is not a move: there was no
            // cell to move from, and calling it one made the click that also
            // brought the pointer into the window cost a press *and* a drag.
            moved = self.cell.is_some() && self.cell != Some(cell);
            self.cell = Some(cell);
        }

        let mut out: Vec<Vec<u8>> = Vec::new();
        if !panel_open && screen.wants_mouse() {
            if let Some((cx, cy)) = self.cell {
                // the button edges: a press when it goes down, a release when
                // it comes up, each exactly once
                for (i, mask) in [(0u8, 1u8), (1, 2), (2, 4)] {
                    let was = self.buttons & mask != 0;
                    let is = now & mask != 0;
                    if is && !was {
                        push_report(&mut out, screen.mouse_report(i, cx, cy, false, 0));
                    } else if was && !is {
                        push_report(&mut out, screen.mouse_report(i, cx, cy, true, 0));
                    }
                }
                // motion, gated by the mode that asked for it.  A drag is a move
                // with a button already down *before* this frame; a button that
                // goes down in the same frame went down after the move, so that
                // move was a bare one.  With a button held the report names it;
                // bare motion has none.
                if moved {
                    if self.buttons != 0 {
                        if screen.mouse_tracks_drag() {
                            let button = self.buttons.trailing_zeros() as u8;
                            push_report(&mut out, screen.mouse_report(button, cx, cy, false, 0));
                        }
                    } else if screen.mouse_tracks_motion() {
                        push_report(&mut out, screen.mouse_report(3, cx, cy, false, 0));
                    }
                }
                // the wheel: one report per notch, not one per frame
                let step = state.wheel.signum();
                for _ in 0..state.wheel.abs() {
                    push_report(&mut out, screen.mouse_report(0, cx, cy, false, step));
                }
            }
        }
        self.buttons = now;
        out
    }
}

struct WindowInput {
    tx: Sender<Input>,
}

impl minifb::InputCallback for WindowInput {
    fn add_char(&mut self, uni_char: u32) {
        if let Some(c) = char::from_u32(uni_char) {
            let _ = self.tx.send(Input::Text(c));
        }
    }

    fn set_key_state(&mut self, key: Key, state: bool) {
        let _ = self.tx.send(Input::Key(key, state));
    }
}

/// A pen that draws the screen in reading order, tracing each glyph along its
/// own strokes so the writing can be watched.
///
/// It only traces cells whose character differs from what it has already drawn,
/// so typing one letter traces one letter; a scroll changes every row and is
/// traced as a whole, like a plotter rewriting the page.  `rate` is in target
/// dots per second and is what makes it slow enough to see.
pub struct Pen {
    /// Position in the page, and how far through the current glyph.
    cell: usize,
    seg: usize,
    offset: f32,
    /// What has already been drawn, so unchanged cells cost nothing.  The
    /// reverse flag is part of the record: a reversed cell is a different
    /// picture from a plain one with the same letter, and a reversed *space* is
    /// a bar of ink, so it cannot be skipped the way a plain space can.
    drawn: Vec<(char, bool)>,
    /// Where the beam tip currently is, for the bright spot.
    tip: Option<(f32, f32)>,
    /// Scratch: the current glyph's strokes.
    segs: Vec<(f32, f32, f32, f32)>,
}

impl Pen {
    fn new(cells: usize) -> Self {
        Pen {
            cell: 0,
            seg: 0,
            offset: 0.0,
            drawn: vec![(' ', false); cells],
            tip: None,
            segs: Vec::new(),
        }
    }

    fn resize(&mut self, cells: usize) {
        self.drawn.clear();
        self.drawn.resize(cells, (' ', false));
        self.cell = self.cell.min(cells.saturating_sub(1));
        self.seg = 0;
        self.offset = 0.0;
    }

    /// Stroke everything the pen has drawn so far into the stored image.
    ///
    /// The character stored is the one the pen *drew*, never what the screen
    /// holds now.  The two differ whenever content changed after the pen went
    /// past, and taking it from the screen put a glyph into the image before the
    /// beam had drawn it - which the beam then traced again over the top.  That
    /// is the doubled drawing this prevents.  A cell whose content changed keeps
    /// what was drawn until the pen reaches it, and a cell the pen has erased
    /// (walking over a space costs it nothing, so the record simply empties)
    /// drops out of the image at the next settle.  A change too large to trace
    /// never comes through here at all: see `wholesale`.
    fn settle_drawn(&mut self, tube: &mut Tube, screen: &vt::Screen, cols: usize, rows: usize) {
        let cw = (tube.w / cols as i32).max(1);
        let ch = (tube.h / rows as i32).max(1);
        let st = Style {
            line: LineStyle::Solid,
            defocus: false,
            write: Write::Settle,
        };
        for i in 0..cols * rows {
            // the pen's own record, including whether it drew the cell in
            // reverse - a reversed space is a bar, not something to skip
            let (g, reverse) = self.drawn.get(i).copied().unwrap_or((' ', false));
            if g == ' ' && !reverse {
                continue;
            }
            let col = i % cols;
            let row = i / cols;
            let g = crate::tube::glyph_code(g);
            let y = tube.h - ch - (row as f32 * ch as f32).round() as i32;
            let attr = screen.visible_attr(col, row);
            let energy = match attr {
                vt::Attr::Dim => 0.6,
                _ => 1.0,
            };
            tube.character(
                g,
                (col as i32 * cw) as f32,
                y as f32,
                cw,
                ch,
                st,
                energy,
                reverse,
            );
        }
    }

    /// What the screen holds in cell `i`, as `drawn` records it: the character
    /// and whether the cell is reversed.  One place, so `stale`, `sync` and
    /// `busy` cannot disagree about what "already drawn" means.
    fn drawn_want(screen: &vt::Screen, i: usize) -> (char, bool) {
        let (x, y) = (i % screen.cols, i / screen.cols);
        (
            screen.visible_char(x, y),
            matches!(screen.visible_attr(x, y), vt::Attr::Reverse),
        )
    }

    /// How many cells differ from what the pen has drawn.
    pub fn stale(&self, screen: &vt::Screen) -> usize {
        (0..screen.cols * screen.rows)
            .filter(|i| {
                self.drawn.get(*i).copied().unwrap_or((' ', false)) != Self::drawn_want(screen, *i)
            })
            .count()
    }

    /// Take the screen as already drawn, without drawing it: what a page
    /// replacement is, and what the pen is told after one.
    pub fn sync(&mut self, screen: &vt::Screen) {
        self.drawn.clear();
        self.drawn
            .extend((0..screen.cols * screen.rows).map(|i| Self::drawn_want(screen, i)));
        self.cell = 0;
        self.seg = 0;
        self.offset = 0.0;
        self.tip = None;
    }

    /// Is there anything left to trace?
    pub fn busy(&self, screen: &vt::Screen) -> bool {
        if self.seg > 0 || self.offset > 0.0 {
            return true;
        }
        (0..screen.cols * screen.rows).any(|i| {
            self.drawn.get(i).copied().unwrap_or((' ', false)) != Self::drawn_want(screen, i)
        })
    }

    /// Draw up to `budget` target dots of the page.
    fn advance(
        &mut self,
        budget: f32,
        screen: &vt::Screen,
        tube: &mut Tube,
        cols: usize,
        rows: usize,
    ) {
        let cw = (tube.w / cols as i32).max(1);
        let ch_h = (tube.h / rows as i32).max(1);
        let st = Style {
            line: LineStyle::Solid,
            defocus: false,
            write: Write::Flash,
        };
        let cells = cols * rows;
        let mut budget = budget;
        self.tip = None;
        while budget > 0.0 && self.cell < cells {
            let col = self.cell % cols;
            let row = self.cell / cols;
            let g = crate::tube::glyph_code(screen.visible_char(col, row));
            let attr = screen.visible_attr(col, row);
            let reverse = matches!(attr, vt::Attr::Reverse);
            let want = (g as char, reverse);
            if self.drawn.get(self.cell).copied().unwrap_or((' ', false)) == want {
                // nothing to do here; walk on for free
                self.cell += 1;
                self.seg = 0;
                self.offset = 0.0;
                continue;
            }
            if g == b' ' && !reverse {
                // erasing is not a stroke: the cell is simply marked as done
                self.drawn[self.cell] = (' ', false);
                self.cell += 1;
                self.seg = 0;
                self.offset = 0.0;
                continue;
            }
            // the top of the page is at the top of the target
            let y = tube.h - ch_h - (row as f32 * ch_h as f32).round() as i32;
            let energy = match attr {
                vt::Attr::Dim => 0.6,
                _ => 1.0,
            };
            self.segs.clear();
            // A reversed cell is traced as the stencil, not the glyph: the pen
            // is what spends the write-rate budget, and drawing the glyph here
            // while the settle drew bars would be two different pictures of the
            // same cell - and the bars are exactly the writing that has to be
            // paced, since they are several times a glyph's ink.  A reversed
            // space comes through here too: its glyph is empty, so the stencil
            // fills the cell.
            if reverse {
                tube.stencil_strokes(
                    g,
                    (col as i32 * cw) as f32,
                    y as f32,
                    cw,
                    ch_h,
                    |x0, y0, x1, y1| {
                        self.segs.push((x0, y0, x1, y1));
                    },
                );
            } else {
                tube.glyph_strokes(
                    g,
                    (col as i32 * cw) as f32,
                    y as f32,
                    cw,
                    ch_h,
                    |x0, y0, x1, y1| {
                        self.segs.push((x0, y0, x1, y1));
                    },
                );
            }
            let mut finished = true;
            for i in self.seg..self.segs.len() {
                let (x0, y0, x1, y1) = self.segs[i];
                let len = ((x1 - x0) * (x1 - x0) + (y1 - y0) * (y1 - y0)).sqrt();
                let from = if i == self.seg { self.offset } else { 0.0 };
                if len <= from {
                    continue;
                }
                let take = (len - from).min(budget);
                let t0 = from / len;
                let t1 = (from + take) / len;
                let (ax, ay) = (x0 + (x1 - x0) * t0, y0 + (y1 - y0) * t0);
                let (bx, by) = (x0 + (x1 - x0) * t1, y0 + (y1 - y0) * t1);
                tube.line(ax as i32, ay as i32, bx as i32, by as i32, st, energy);
                self.tip = Some((bx, by));
                budget -= take;
                if budget > 0.0 {
                    self.seg = i + 1;
                    self.offset = 0.0;
                } else {
                    self.seg = i;
                    self.offset = from + take;
                    finished = false;
                    break;
                }
            }
            if finished {
                // the glyph is done: the cell counts as drawn even though its
                // last dot may not have been reached
                self.drawn[self.cell] = want;
                self.cell += 1;
                self.seg = 0;
                self.offset = 0.0;
            }
        }
        if self.cell >= cells {
            // a full pass: start again, so the next change is picked up
            self.cell = 0;
        }
        // the beam tip, hot, so the writing can be seen happening
        if let Some((tx, ty)) = self.tip {
            let st = Style {
                line: LineStyle::Solid,
                defocus: false,
                write: Write::Flash,
            };
            tube.dot(tx as i32, ty as i32, 1.0, st);
        }
    }
}

pub struct Terminal {
    pub screen: vt::Screen,
    parser: vt::Parser,
    input_rx: Receiver<Input>,
    input_tx: Sender<Input>,
    child: Box<dyn Child + Send + Sync>,
    /// The pseudoconsole.  Holding this is not optional: dropping the master
    /// closes the pseudoconsole and the shell dies within milliseconds - it
    /// exits after 14 ms with zero bytes of output, which looks exactly like the
    /// spawn having failed.  It also carries `resize`.
    _master: Box<dyn MasterPty + Send>,
    writer: Box<dyn IoWrite + Send>,
    rows_rx: Receiver<Vec<u8>>,
    stop: Arc<AtomicBool>,
    pub cols: usize,
    pub rows: usize,

    /// Seconds since the screen last changed, and since the last commit.
    quiet: f32,
    since_commit: f32,
    committed: bool,
    /// How far the display still has to slide up, in rows, and how many lines of
    /// scroll the committed image has not been moved for yet.
    slide: f32,
    pending_scroll: usize,
    /// Rows a second the slide runs at.  Zero makes a scroll jump.
    slide_rate: f32,
    /// The page as the last commit left it, so the next one can see which cells
    /// it is writing over.
    last_screen: Vec<char>,
    /// Set when the shell moved the cursor since the last frame: the cursor is
    /// transient, so it does not mark a row dirty.
    cursor_moved: bool,
    /// The page as the last *render* left it.  A flash is deposited once, when a
    /// cell changes, and then cools; re-depositing it every frame would hold it
    /// at full brightness until the commit and cost a stroke per cell per frame.
    /// The reverse flag is part of the reading: a reversed space is a bar of ink
    /// and must be told apart from the plain space that leaves nothing.
    flash_screen: Vec<(char, bool)>,
    /// The writing pen, used only when a write rate is set.
    pen: Pen,
    /// Target dots per second the pen draws at; None writes instantly.
    write_rate: Option<f32>,
    /// The screen changed too much to trace, so the next commit must settle the
    /// whole page rather than the pen's cell-by-cell record.
    page_replaced: bool,
    /// F11 was pressed: the window should change between fullscreen and a
    /// windowed frame.  Set in `poll` so the key never reaches the shell.
    toggle_fullscreen: bool,
    /// Whether the options panel is open, so the four navigation keys are the
    /// panel's and never reach the shell.  Set by the window loop, which is what
    /// owns the panel; `poll` only reads it.
    pub panel_open: bool,
    /// The panel keys the last `poll` translated out of the window's input, for
    /// the window loop to act on.  Taken by `take_panel_keys`.
    panel_keys: Vec<panel::PanelKey>,
    /// The mouse edge tracker: the window hands the frame's state in and this
    /// turns it into the events a shell asked for (see `Mouse`).
    mouse: Mouse,
    /// Modifier state, tracked because minifb reports keys, not chords.
    ctrl: bool,
    alt: bool,
    shift: bool,
    /// True once the reader thread has ended, i.e. the shell's output is all in.
    eof: bool,
    /// Everything the shell sent, when `TEK_DUMP` names a file.  The point is to
    /// settle questions like "why two prompts" from the bytes rather than from
    /// reasoning about what a shell might do.
    dump: Option<std::fs::File>,
    /// Counters, so a headless run can be shown to have done something.
    pub bytes_in: usize,
    /// Bytes the link has actually carried to the screen.  Smaller than
    /// `bytes_in` while `--baud` is pacing, by exactly the backlog.
    pub bytes_shown: usize,
    pub commits: usize,
    pub keys_in: usize,
    /// Bytes a second the link carries (`baud`), or `None` for unpaced.
    baud: Option<f32>,
    /// Fractional bytes carried between frames, and the part of a chunk the link
    /// has not carried yet.
    byte_credit: f32,
    pending: Vec<u8>,
}

/// Split a command line into the program and its arguments, the way a Windows
/// command line is quoted.
///
/// Whitespace separates, and `"` groups: `"C:\Program Files\Git\bin\bash.exe" -l`
/// is one program and one argument, which is the whole reason this is not
/// `split_whitespace` - the shells the setup offers are installed under
/// `Program Files`, and the same is true of a WSL path with a space in it.  A
/// doubled quote inside a quoted run is a literal quote (`"say ""hi"""`), and a
/// token may be part quoted (`--flag="a b"`), both of which is what
/// `CommandLineToArgvW` does with the same text.  Backslashes are left alone:
/// this is not the full VC++ rule set, because the command is the setup's own
/// text and a quote after a backslash is a path far more often than an escape.
/// An unclosed quote takes the rest of the line, rather than refusing to start.
fn split_command(command: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                // "" inside a quoted run: one literal quote
                chars.next();
                cur.push('"');
            }
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(cur);
    }
    args
}

impl Terminal {
    pub fn spawn(
        command: &str,
        cols: usize,
        rows: usize,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let pty = native_pty_system();
        let pair = pty.openpty(PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let args = split_command(command);
        let program = args.first().ok_or("empty shell command")?;
        let mut cmd = CommandBuilder::new(program);
        for arg in &args[1..] {
            cmd.arg(arg);
        }
        // Say what kind of terminal this is.  The value is *forced*, not
        // inherited: what is advertised has to be true, because over ssh it is
        // what the far end believes and acts on.  A parent shell's
        // `xterm-256color` asks the remote for 256 colours and a mouse on a
        // monochrome storage tube, and its `dumb` makes a full screen program
        // refuse to draw at all; `vt100` is the honest name now that the parser
        // draws the special graphics set, carries DECSC/DECRC and sends
        // application cursor keys.
        //
        // TEK_TUBE_TERM overrides it, for the one case where the lie is the
        // point: seeing what a real xterm-256color session would send.
        let term = std::env::var("TEK_TUBE_TERM").unwrap_or_else(|_| "vt100".to_string());
        cmd.env("TERM", term);
        cmd.env("TERM_PROGRAM", "tek-tube");
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        // keep the master alive for as long as the terminal is
        let master = pair.master;
        let (rows_tx, rows_rx) = mpsc::channel::<Vec<u8>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_reader = Arc::clone(&stop);
        // detached on purpose - see the module note
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while !stop_reader.load(Ordering::Relaxed) {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if rows_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let (input_tx, input_rx) = mpsc::channel::<Input>();
        Ok(Terminal {
            screen: vt::Screen::new(cols, rows),
            parser: vt::Parser::new(),
            input_rx,
            input_tx,
            child,
            _master: master,
            writer,
            rows_rx,
            stop,
            cols,
            rows,
            last_screen: vec![' '; cols * rows],
            cursor_moved: false,
            flash_screen: vec![(' ', false); cols * rows],
            pen: Pen::new(cols * rows),
            page_replaced: false,
            write_rate: Some(WRITE_RATE_DEFAULT),
            dump: std::env::var("TEK_DUMP")
                .ok()
                .and_then(|path| std::fs::File::create(path).ok()),
            toggle_fullscreen: false,
            panel_open: false,
            panel_keys: Vec::new(),
            mouse: Mouse::default(),
            quiet: 0.0,
            since_commit: 0.0,
            committed: false,
            slide: 0.0,
            pending_scroll: 0,
            slide_rate: SLIDE_RATE,
            ctrl: false,
            alt: false,
            shift: false,
            eof: false,
            bytes_in: 0,
            bytes_shown: 0,
            commits: 0,
            keys_in: 0,
            baud: None,
            byte_credit: 0.0,
            pending: Vec::new(),
        })
    }

    pub fn input_handler(&self) -> Box<dyn minifb::InputCallback> {
        Box::new(WindowInput {
            tx: self.input_tx.clone(),
        })
    }

    /// A window title: what the shell called itself, or the command.
    pub fn title(&self) -> String {
        let t = self.screen.title.trim();
        if t.is_empty() {
            format!("{}x{} terminal", self.cols, self.rows)
        } else {
            t.to_string()
        }
    }

    /// Has the reader thread finished?  When a short-lived command exits, the
    /// last of its output is still in flight, so the exit check must not be the
    /// thing that ends the session.
    /// Scroll the visible page back into the scrollback (positive is further
    /// back) - the mouse wheel and Shift+PageUp both land here.
    pub fn scroll_view(&mut self, lines: i32) {
        self.screen.scroll_view(lines);
    }

    /// Make the pen draw at `dots` target dots per second.  Without this the
    /// screen is written instantly, as it always was.
    pub fn set_write_rate(&mut self, dots: f32) {
        self.write_rate = if dots > 0.0 { Some(dots) } else { None };
    }

    /// Rows a second the display slides a scroll at.  Zero makes a scroll jump,
    /// which is what a raster terminal does: its screen is static between writes
    /// and the movement the eye reads is the residual image, not the redraw.
    pub fn set_slide_rate(&mut self, rows_per_second: f32) {
        self.slide_rate = rows_per_second.max(0.0);
    }

    /// Bytes a second the link carries, as a baud rate: 0 or less leaves it
    /// unpaced, which is what a disk-fed screen looks like.  At 9600 baud a
    /// listing takes the time a listing took.
    pub fn set_baud(&mut self, baud: f32) {
        self.baud = if baud > 0.0 { Some(baud) } else { None };
    }

    /// F11 was pressed.  Taken rather than read, so one press toggles once.
    pub fn take_fullscreen_toggle(&mut self) -> bool {
        std::mem::take(&mut self.toggle_fullscreen)
    }

    /// The panel keys the last `poll` translated, in order.  Taken rather than
    /// read, so one press is handled once; empty unless the panel is open.
    pub fn take_panel_keys(&mut self) -> Vec<panel::PanelKey> {
        std::mem::take(&mut self.panel_keys)
    }

    /// Report one frame's mouse to the shell when a tracking mode is on.
    ///
    /// The window loop reads minifb's mouse state - the keys path cannot carry
    /// it, since minifb's callback reports keys and characters only - and hands
    /// the frame in here, where the edges are worked out and written to the pty
    /// (see `Mouse`).  The panel is on top: while it is open the mouse is the
    /// panel's and nothing is reported.
    pub fn report_mouse(&mut self, state: MouseState, w: i32, h: i32, panel_open: bool) {
        let (cols, rows) = (self.cols, self.rows);
        let reports = self
            .mouse
            .poll(&self.screen, state, w, h, cols, rows, panel_open);
        if reports.is_empty() {
            return;
        }
        for bytes in &reports {
            let _ = self.writer.write_all(bytes);
        }
        let _ = self.writer.flush();
    }

    /// Is the pen still tracing?
    pub fn pen_busy(&self) -> bool {
        self.write_rate.is_some() && self.pen.busy(&self.screen)
    }

    /// How many lines the scrollback holds, for the window title.
    pub fn scrollback_len(&self) -> usize {
        self.screen.history_len()
    }

    /// How far back the view is, in lines.
    pub fn view_offset(&self) -> usize {
        self.screen.view_offset()
    }

    pub fn in_alt_screen(&self) -> bool {
        self.screen.in_alt_screen()
    }

    pub fn eof(&self) -> bool {
        self.eof
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Feed the parser and answer it.
    ///
    /// Split out so the paced path and the unpaced one cannot drift apart: both
    /// go through here, and both count what the link has actually carried.
    fn feed_parser(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.bytes_shown += bytes.len();
        let before = (self.screen.cx, self.screen.cy, self.screen.cursor_visible);
        let replies = self.parser.feed(bytes, &mut self.screen);
        // The cursor is drawn transiently and never sets a row dirty, so a move
        // has to be noticed here or the underline would not follow the shell.
        if (self.screen.cx, self.screen.cy, self.screen.cursor_visible) != before {
            self.cursor_moved = true;
        }
        if !replies.is_empty() {
            let _ = self.writer.write_all(&replies);
            let _ = self.writer.flush();
        }
    }

    /// Read whatever the shell has produced, feed the parser, answer it, and
    /// handle input.  Returns true if the image needs redrawing.
    pub fn poll(&mut self, dt: f32) -> bool {
        // the cursor is transient: a move is its own reason to redraw, and moves
        // are noticed as the bytes are parsed
        self.cursor_moved = false;
        // The link.  A serial terminal is paced by its line, so `--baud` can pace
        // this one: the screen is fed at the line's character rate instead of at
        // disk speed, and a listing scrolls at a rate that looks like a terminal.
        // The credit is kept in fractions so a rate slower than the frame rate
        // still averages out exactly, and a chunk that does not fit waits in
        // `pending` - the order is preserved, nothing is fed twice, and the wait
        // is the line's back-pressure.
        let mut budget = usize::MAX;
        if let Some(baud) = self.baud {
            self.byte_credit += baud / 10.0 * dt;
            let whole = self.byte_credit.floor();
            self.byte_credit -= whole;
            budget = whole.max(0.0) as usize;
        }
        while budget > 0 && !self.pending.is_empty() {
            let take = budget.min(self.pending.len());
            let chunk: Vec<u8> = self.pending.drain(..take).collect();
            self.feed_parser(&chunk);
            budget -= take;
        }
        loop {
            match self.rows_rx.try_recv() {
                Ok(chunk) => {
                    self.bytes_in += chunk.len();
                    // the dump is everything the shell sent, whether or not the
                    // link has carried it to the screen yet
                    if let Some(f) = self.dump.as_mut() {
                        use std::io::Write;
                        let _ = f.write_all(&chunk);
                    }
                    let take = budget.min(chunk.len());
                    self.feed_parser(&chunk[..take]);
                    budget -= take;
                    if take < chunk.len() {
                        self.pending.extend_from_slice(&chunk[take..]);
                        break;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.eof = true;
                    break;
                }
            }
        }

        // input from the window, drained as a batch first: a real key press
        // arrives twice, and `window_input` is what keeps it from going down the
        // wire twice (see there)
        let mut events = Vec::new();
        while let Ok(ev) = self.input_rx.try_recv() {
            events.push(ev);
        }
        let batch = window_input(
            &events,
            Mods {
                ctrl: self.ctrl,
                alt: self.alt,
                shift: self.shift,
            },
            self.rows as i32 - 1,
            self.screen.app_cursor,
            self.panel_open,
        );
        self.ctrl = batch.mods.ctrl;
        self.alt = batch.mods.alt;
        self.shift = batch.mods.shift;
        // The panel's keys are the window loop's to act on - it owns the panel -
        // so they are kept for `take_panel_keys` rather than handled here.
        self.panel_keys = batch.panel_keys;
        if batch.fullscreen {
            self.toggle_fullscreen = true;
        }
        if batch.scroll_lines != 0 {
            self.scroll_view(batch.scroll_lines);
        }
        let typed = batch.bytes;
        let typed_input = !typed.is_empty();
        if typed_input {
            self.keys_in += typed.len();
            let _ = self.writer.write_all(&typed);
            let _ = self.writer.flush();
        }

        // the screen may also have changed without new bytes (a scroll)
        let dirty = self.screen.take_dirty();
        // What needs drawing is decided by the *screen*, not by the bytes: a
        // console host emits title updates, focus events and window bookkeeping
        // that change nothing on the page, and treating those as a redraw kept
        // the tube's transient alive - and a core compositing - for the whole
        // trail after every one of them.  Measured on a live session: 220,350
        // frames drawn over 7.5 hours, 28 % of the wall clock, most of it for
        // sequences that never touched the screen.
        let changed = typed_input || dirty.iter().any(|d| *d) || self.cursor_moved;
        // A page-wide change is a move or a replacement, and the screen says
        // which.  A *scroll* carries the committed image with it: the image is
        // moved, the record is synced to the moved page, and only the lines that
        // scrolled in are new - so nothing has to be traced, and the display
        // slides the whole thing into place over the next few frames.  A clear,
        // an alt screen switch or a resize replaces the page instead: the record
        // is synced and the whole page is settled at once.
        //
        // The cell-share backstop is for anything that redraws most of the page
        // without saying so.  It does not catch a scroll on a sparse page: a
        // listing changes every *row* but only three or four cells in each, which
        // on a 93x32 page is a few per cent.
        match self.screen.take_page_change() {
            PageChange::Scroll(lines) => {
                self.pending_scroll += lines;
                // The slide starts where the page was: the image has moved up and
                // the offset brings it back into place.  It is capped at one page,
                // because a listing of two hundred lines is not a two hundred line
                // movement - the visible part of it is one screenful rising into
                // view, and an uncapped offset would push the whole page off the
                // bottom and show nothing for a second.
                let cap = self.rows.max(2) as f32 - 1.0;
                self.slide = (self.slide + lines as f32).min(cap);
                // settle it now, so the moved page is what slides
                self.committed = false;
                self.quiet = COMMIT_QUIET;
            }
            PageChange::Wholesale => {
                self.pen.sync(&self.screen);
                self.page_replaced = true;
                self.committed = false;
                self.pending_scroll = 0;
                self.slide = 0.0;
            }
            PageChange::None => {}
        }
        if self.write_rate.is_some()
            && !self.page_replaced
            && wholesale(self.pen.stale(&self.screen), self.cols * self.rows)
        {
            self.pen.sync(&self.screen);
            self.page_replaced = true;
            self.committed = false;
        }

        // the slide decays toward zero: rows per second, so a long scroll is a
        // quick slide rather than a jump - and a rate of zero makes it a jump,
        // which is what a raster terminal does, since its movement is only ever
        // the residual image
        let was_sliding = self.slide > 0.0;
        if was_sliding {
            self.slide = if self.slide_rate <= 0.0 {
                0.0
            } else {
                (self.slide - self.slide_rate * dt).max(0.0)
            };
        }
        let sliding = was_sliding || self.slide > 0.0;

        // commit bookkeeping
        if changed {
            self.quiet = 0.0;
            self.committed = false;
        } else {
            self.quiet += dt;
        }
        self.since_commit += dt;
        let pen_work = self.pen_busy();
        if pen_work {
            self.quiet = 0.0;
            self.committed = false;
        }
        let due =
            !self.committed && (self.quiet >= COMMIT_QUIET || self.since_commit >= COMMIT_MAX);

        changed || sliding || due || pen_work
    }

    /// The character the last commit left in this cell.
    fn last_char(&self, i: usize) -> char {
        self.last_screen.get(i).copied().unwrap_or(' ')
    }

    /// Remember the page as it is now: the next commit compares against this to
    /// see which cells it is writing over.
    fn snapshot_screen(&mut self) {
        if self.last_screen.len() != self.cols * self.rows {
            self.last_screen.clear();
            self.last_screen.resize(self.cols * self.rows, ' ');
        }
        for i in 0..self.cols * self.rows {
            self.last_screen[i] = self.screen.visible_char(i % self.cols, i / self.cols);
        }
    }

    /// Should the caller commit this frame?
    pub fn commit_due(&self) -> bool {
        !self.committed
            && (self.page_replaced || self.quiet >= COMMIT_QUIET || self.since_commit >= COMMIT_MAX)
    }

    /// Write the settled image, replacing the stored one wholesale.
    ///
    /// With the pen running this settles exactly what the pen has drawn so far,
    /// not the whole screen - a commit that drew ahead of the pen would give the
    /// game away, and *not* committing until the pen finished meant a slow pen
    /// never resolved at all, so the image faded all the way to nothing instead
    /// of cooling from the bright flash to the stored green.
    ///
    /// A page replacement is not that at all: see `commit_to`, which erases the
    /// transient glow with the stored image so the page before it cannot shine
    /// through the new one.
    ///
    /// A *scroll* is neither: it moves the committed image up the face, which is
    /// done here because this is where the tube is.  The record is synced to the
    /// moved page, so the pen has nothing to trace and the lines that scrolled in
    /// are stamped with the rest rather than written out one glyph at a time; the
    /// glow is not erased, so the trail travels up with the page it belongs to.
    pub fn commit(&mut self, tube: &mut Tube) {
        let mut traced = self.write_rate.is_some() && !self.page_replaced;
        if self.pending_scroll > 0 {
            // The page moved, and what was on the tube stays glowing where it
            // was for as long as `--ghost` asks: the previous screenful still
            // legible underneath the new one, which is what a raster terminal
            // looks like.  The new page is stamped in place by the commit below.
            tube.capture_ghost();
            self.pen.sync(&self.screen);
            self.pending_scroll = 0;
            // Only the pen's path can settle what it has drawn.  With no pen the
            // record is not being kept, so settling it would clear the stored
            // image and stamp nothing - the page disappeared for as long as the
            // next commit took.  A penless scroll is a whole-page stroke.
            traced = self.write_rate.is_some();
        } else if self.page_replaced {
            // A clear leaves its light as well: blanking a cell does not switch
            // off the phosphor that was in it, which is why a raster terminal's
            // page still glows for a moment after it has been wiped.
            tube.capture_ghost();
        } else {
            // An incremental commit writes over cells that held something else.
            // Each one keeps the light it already had: this is the ghost you see
            // while typing, and the same one a raster terminal shows when it
            // writes a character over one that was there.  It is decided by what
            // the page held at the last commit, not by the pen's record, so it
            // works the same whether a beam is drawing or not.
            let cw = (tube.w / self.cols.max(1) as i32).max(1);
            let ch = (tube.h / self.rows.max(1) as i32).max(1);
            for i in 0..self.cols * self.rows {
                let now = self.screen.visible_char(i % self.cols, i / self.cols);
                if self.last_char(i) != now {
                    let col = (i % self.cols) as i32;
                    let row = (i / self.cols) as i32;
                    tube.ghost_cell(col * cw, tube.h - ch - row * ch, cw, ch);
                }
            }
        }
        // remember the page as it now stands, for the next commit to compare
        self.snapshot_screen();
        commit_to(
            tube,
            &self.screen,
            &mut self.pen,
            self.cols,
            self.rows,
            traced,
        );
        self.page_replaced = false;
        self.committed = true;
        self.commits += 1;
        self.since_commit = 0.0;
    }

    /// Draw the current screen into the transient population, leaving the
    /// previous frames to decay underneath: the smear.
    pub fn render(&mut self, tube: &mut Tube, dt: f32) {
        // where the slide has got to: the image is shown displaced down the face
        // until it has climbed into place
        let ch = (tube.h / self.rows.max(1) as i32).max(1);
        tube.render_offset = (self.slide * ch as f32).round() as i32;
        match self.write_rate {
            Some(rate) => {
                // the pen traces only what changed, so a typed letter is drawn
                // as one letter while a scroll moves the page
                self.pen
                    .advance(rate * dt, &self.screen, tube, self.cols, self.rows);
                draw_cursor(&self.screen, self.cols, self.rows, tube, Write::Flash);
            }
            None => {
                self.stroke_changed(tube, Write::Flash, true);
                // What has just been flashed is the reference for the next frame,
                // so a change is deposited once and then cools - which is the
                // trail.  Re-depositing it every frame would hold it at full
                // brightness until the commit and cost a stroke a frame.
                self.snapshot_flash();
            }
        }
    }

    /// Remember the page as the flash has just drawn it.
    fn snapshot_flash(&mut self) {
        if self.flash_screen.len() != self.cols * self.rows {
            self.flash_screen.clear();
            self.flash_screen
                .resize(self.cols * self.rows, (' ', false));
        }
        for i in 0..self.cols * self.rows {
            let want = Pen::drawn_want(&self.screen, i);
            self.flash_screen[i] = want;
        }
    }

    fn stroke_changed(&mut self, tube: &mut Tube, write: Write, cursor: bool) {
        stroke_changed(
            &self.screen,
            &self.flash_screen,
            self.cols,
            self.rows,
            tube,
            write,
            cursor,
        );
    }

    /// Change the page size and tell the shell.  The tube follows the window,
    /// so the cell size stays about the same and the glyphs do not have to be
    /// re-scaled.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.screen.resize(cols, rows);
        // The page under the shell has changed shape, so the old rows are stale:
        // its next repaint assumes the new geometry and would otherwise land
        // beside them.  See `Screen::clear_visible`.
        self.screen.clear_visible();
        self.pen.resize(cols * rows);
        self.last_screen.clear();
        self.last_screen.resize(cols * rows, ' ');
        self.flash_screen.clear();
        self.flash_screen.resize(cols * rows, (' ', false));
        // the record no longer describes this page, so the next poll sees every
        // cell stale and replaces the page whole
        self.page_replaced = true;
        self.cols = cols;
        self.rows = rows;
        let _ = self._master.resize(PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: 0,
            pixel_height: 0,
        });
        // the committed image was for the old geometry
        self.committed = false;
        self.slide = 0.0;
        self.pending_scroll = 0;
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
    }
}

/// Stroke a screen into the tube.
///
/// A free function rather than a method so the render path can be exercised
/// without a pseudoconsole, which is the only way to test it here.
pub fn stroke_screen(
    screen: &vt::Screen,
    cols: usize,
    rows: usize,
    tube: &mut Tube,
    write: Write,
    cursor: bool,
) {
    let cw = (tube.w / cols as i32).max(1);
    let ch = (tube.h / rows as i32).max(1);
    let st = Style {
        line: LineStyle::Solid,
        defocus: false,
        write,
    };
    for r in 0..rows {
        // screen row 0 is the top and the tube's origin is bottom left; a scroll
        // moves the image itself, so nothing here needs an offset
        let y = tube.h - ch - (r as f32 * ch as f32).round() as i32;
        if y + ch < 0 || y > tube.h {
            continue;
        }
        for c in 0..cols {
            // the *view*, which may be scrolled back into the history
            let g = screen.visible_char(c, r);
            // SGR brightness: a storage tube cannot go above full charge, so
            // "bright" is the ordinary level and only "dim" is expressible
            let attr = screen.visible_attr(c, r);
            let reverse = matches!(attr, vt::Attr::Reverse);
            // a reversed space is a bar of ink, not an empty cell
            if g == ' ' && !reverse {
                continue;
            }
            // the stroke font is ASCII; anything else draws as '?' rather than
            // silently vanishing
            let glyph = crate::tube::glyph_code(g);
            let energy = match attr {
                vt::Attr::Dim => 0.6,
                _ => 1.0,
            };
            tube.character(
                glyph,
                (c as i32 * cw) as f32,
                y as f32,
                cw,
                ch,
                st,
                energy,
                reverse,
            );
        }
    }
    if cursor {
        draw_cursor(screen, cols, rows, tube, write);
    }
}

/// Write a committed image to the tube: what `Terminal::commit` does, as a free
/// function so it can be exercised without a pseudoconsole.
///
/// `traced` is the pen's path - the screen has *not* moved as a whole, so the
/// image is settled from the cells the pen has drawn and the beam's own flash is
/// left to cool into it.  Everything else is a page replacement, and a
/// replacement has to take the *glow* as well as the stored charge: the previous
/// page is still in the transient population, so clearing only the stored image
/// left the old glyphs lit for the whole trail.  That is what put a second
/// prompt on the tube after an `echo` or a `cls`, with the erased tail of the
/// previous command line still glowing under the new one - measured on the live
/// window with a 1.1 s trail, so the ghost outlived a whole command.
pub fn commit_to(
    tube: &mut Tube,
    screen: &vt::Screen,
    pen: &mut Pen,
    cols: usize,
    rows: usize,
    traced: bool,
) {
    if traced {
        tube.clear_stored();
        pen.settle_drawn(tube, screen, cols, rows);
    } else {
        // no pen at all, or a page replacement: the whole page in one go, and
        // nothing of the page before it
        tube.erase();
        stroke_screen(screen, cols, rows, tube, Write::Settle, false);
    }
}

/// Stroke only the cells whose content differs from `last` into the tube.
///
/// The penless path used to re-stroke the whole page on every frame that had
/// input, so a shell emitting one byte a second - a title update, a window
/// bookkeeping sequence - kept the tube's transient alive and a core compositing
/// for the whole trail.  Only the cells that actually changed need writing, and
/// they are the ones that carry the trail; the cells that did not are already in
/// the stored image.
pub fn stroke_changed(
    screen: &vt::Screen,
    last: &[(char, bool)],
    cols: usize,
    rows: usize,
    tube: &mut Tube,
    write: Write,
    cursor: bool,
) {
    let cw = (tube.w / cols as i32).max(1);
    let ch = (tube.h / rows as i32).max(1);
    let st = Style {
        line: LineStyle::Solid,
        defocus: false,
        write,
    };
    for r in 0..rows {
        let y = tube.h - ch - (r as f32 * ch as f32).round() as i32;
        if y + ch < 0 || y > tube.h {
            continue;
        }
        for c in 0..cols {
            let g = screen.visible_char(c, r);
            let attr = screen.visible_attr(c, r);
            let reverse = matches!(attr, vt::Attr::Reverse);
            // a reversed space is a bar of ink, not an empty cell
            if g == ' ' && !reverse {
                continue;
            }
            if last.get(r * cols + c).copied().unwrap_or((' ', false)) == (g, reverse) {
                continue;
            }
            let glyph = crate::tube::glyph_code(g);
            let energy = match attr {
                vt::Attr::Dim => 0.6,
                _ => 1.0,
            };
            tube.character(
                glyph,
                (c as i32 * cw) as f32,
                y as f32,
                cw,
                ch,
                st,
                energy,
                reverse,
            );
        }
    }
    if cursor {
        draw_cursor(screen, cols, rows, tube, write);
    }
}

/// The cursor: an underline, drawn transiently.  On a storage tube the cursor
/// *is* the beam position, so it is never committed.
pub fn draw_cursor(screen: &vt::Screen, cols: usize, rows: usize, tube: &mut Tube, write: Write) {
    if !screen.cursor_visible {
        return;
    }
    let cw = (tube.w / cols as i32).max(1);
    let ch = (tube.h / rows as i32).max(1);
    let st = Style {
        line: LineStyle::Solid,
        defocus: false,
        write,
    };
    let cx = screen.cx.min(cols - 1);
    let cy = screen.cy.min(rows - 1);
    let y = tube.h - ch - (cy as f32 * ch as f32).round() as i32;
    tube.line(
        cx as i32 * cw + 2,
        y + 2,
        cx as i32 * cw + cw - 2,
        y + 2,
        st,
        1.0,
    );
}

/// The panel key a window key names, if it is one the options panel uses.
///
/// Four keys and no more: the panel is a list of rows, so it needs a move and a
/// commit, plus Escape to close it.  Anything else - a letter, a chord - is not
/// the panel's and is left to the shell.
fn panel_key(key: Key) -> Option<panel::PanelKey> {
    Some(match key {
        Key::Up => panel::PanelKey::Up,
        Key::Down => panel::PanelKey::Down,
        Key::Enter => panel::PanelKey::Enter,
        Key::Escape => panel::PanelKey::Escape,
        _ => return None,
    })
}

/// The bytes a key sends to the shell, or `None` if the key is not one we send.
///
/// Printable text does *not* come through here: minifb reports it through
/// `add_char`, which is Unicode from `WM_CHAR` and therefore already correct for
/// the user's keyboard layout, dead keys and AltGr.  A plain letter is therefore
/// **not** ours to send, and sending one - which an earlier version did, mapping
/// every letter to a C0 control unconditionally - puts a stray control character
/// on the wire for every key typed.  Letters belong here only with Ctrl held.
pub fn key_sequence(key: Key, ctrl: bool, alt: bool, app_cursor: bool) -> Option<Vec<u8>> {
    let body: &[u8] = if ctrl {
        ctrl_code(key)?
    } else {
        named_key(key, app_cursor)?
    };
    if alt {
        let mut out = Vec::with_capacity(body.len() + 1);
        out.push(0x1b); // Alt is an ESC prefix
        out.extend_from_slice(body);
        Some(out)
    } else {
        Some(body.to_vec())
    }
}

/// Ctrl+key: the C0 controls.
fn ctrl_code(key: Key) -> Option<&'static [u8]> {
    Some(match key {
        Key::A => b"\x01",
        Key::B => b"\x02",
        Key::C => b"\x03",
        Key::D => b"\x04",
        Key::E => b"\x05",
        Key::F => b"\x06",
        Key::G => b"\x07",
        Key::H => b"\x08",
        Key::I => b"\x09",
        Key::J => b"\x0a",
        Key::K => b"\x0b",
        Key::L => b"\x0c",
        Key::M => b"\x0d",
        Key::N => b"\x0e",
        Key::O => b"\x0f",
        Key::P => b"\x10",
        Key::Q => b"\x11",
        Key::R => b"\x12",
        Key::S => b"\x13",
        Key::T => b"\x14",
        Key::U => b"\x15",
        Key::V => b"\x16",
        Key::W => b"\x17",
        Key::X => b"\x18",
        Key::Y => b"\x19",
        Key::Z => b"\x1a",
        Key::LeftBracket => b"\x1b",
        Key::Backslash => b"\x1c",
        Key::RightBracket => b"\x1d",
        _ => return None,
    })
}

/// Keys that have no character of their own.
fn named_key(key: Key, app_cursor: bool) -> Option<&'static [u8]> {
    Some(match key {
        Key::Enter => b"\r",
        Key::Backspace => b"\x7f",
        Key::Tab => b"\t",
        Key::Escape => b"\x1b",
        // DECCKM: in application cursor mode the arrows and Home/End are sent
        // with SS3 (`ESC O`) rather than CSI, which is what vim and less ask for
        // and what they match on.
        Key::Up => {
            if app_cursor {
                b"\x1bOA"
            } else {
                b"\x1b[A"
            }
        }
        Key::Down => {
            if app_cursor {
                b"\x1bOB"
            } else {
                b"\x1b[B"
            }
        }
        Key::Right => {
            if app_cursor {
                b"\x1bOC"
            } else {
                b"\x1b[C"
            }
        }
        Key::Left => {
            if app_cursor {
                b"\x1bOD"
            } else {
                b"\x1b[D"
            }
        }
        Key::Home => {
            if app_cursor {
                b"\x1bOH"
            } else {
                b"\x1b[H"
            }
        }
        Key::End => {
            if app_cursor {
                b"\x1bOF"
            } else {
                b"\x1b[F"
            }
        }
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        Key::Insert => b"\x1b[2~",
        Key::Delete => b"\x1b[3~",
        Key::F1 => b"\x1bOP",
        Key::F2 => b"\x1bOQ",
        Key::F3 => b"\x1bOR",
        Key::F4 => b"\x1bOS",
        Key::F5 => b"\x1b[15~",
        Key::F6 => b"\x1b[17~",
        Key::F7 => b"\x1b[18~",
        Key::F8 => b"\x1b[19~",
        Key::F9 => b"\x1b[20~",
        Key::F10 => b"\x1b[21~",
        Key::F11 => b"\x1b[23~",
        Key::F12 => b"\x1b[24~",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_the_expected_sequences() {
        assert_eq!(
            key_sequence(Key::Enter, false, false, false),
            Some(b"\r".to_vec())
        );
        assert_eq!(
            key_sequence(Key::Backspace, false, false, false),
            Some(b"\x7f".to_vec())
        );
        assert_eq!(
            key_sequence(Key::Up, false, false, false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_sequence(Key::Delete, false, false, false),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            key_sequence(Key::F5, false, false, false),
            Some(b"\x1b[15~".to_vec())
        );
        // Ctrl+C is C0 0x03 - PSReadLine needs this for copy and for interrupt
        assert_eq!(
            key_sequence(Key::C, true, false, false),
            Some(b"\x03".to_vec())
        );
        // Alt is an ESC prefix
        assert_eq!(
            key_sequence(Key::Enter, false, true, false),
            Some(b"\x1b\r".to_vec())
        );

        // A *plain* letter is not ours to send: it arrives through add_char, and
        // sending a C0 control here as well would put a stray byte on the wire
        // for every key typed.  That was a real bug.
        assert_eq!(key_sequence(Key::A, false, false, false), None);
        assert_eq!(key_sequence(Key::Z, false, false, false), None);
        assert_eq!(
            key_sequence(Key::A, true, false, false),
            Some(b"\x01".to_vec())
        );
        // and keys we have no mapping for
        assert_eq!(key_sequence(Key::LeftShift, false, false, false), None);
        assert_eq!(key_sequence(Key::NumPad5, false, false, false), None);
    }

    /// A screen with a mouse tracking mode already switched on, as the parser
    /// would leave it.
    fn mouse_screen(mode: &[u8]) -> crate::vt::Screen {
        let mut screen = crate::vt::Screen::new(20, 5);
        let mut parser = crate::vt::Parser::new();
        parser.feed(mode, &mut screen);
        screen
    }

    /// A press and a release are each one report, and motion is one report per
    /// cell crossed and only in the modes that asked for it.  A plain `?1000`
    /// application that heard a drag would move a selection the user never
    /// dragged, and one that heard a report every frame a button was held would
    /// be buried.  (Press and release are two reports in every tracking mode -
    /// xterm reports the release in all of them; what the drag modes add is the
    /// motion between them.)
    #[test]
    fn a_press_and_release_report_two_events_and_only_a_drag_mode_reports_motion() {
        let press = MouseState {
            pos: Some((5.0, 5.0)),
            left: true,
            ..MouseState::default()
        };
        let drag = MouseState {
            pos: Some((30.0, 5.0)),
            left: true,
            ..MouseState::default()
        };
        let release = MouseState {
            pos: Some((30.0, 5.0)),
            ..MouseState::default()
        };
        // 100 px over 20 columns is a five pixel cell, so x 5 and x 30 are
        // different cells and the move is a real one
        let screen = mouse_screen(b"\x1b[?1002h");
        let mut mouse = Mouse::default();
        assert_eq!(
            mouse.poll(&screen, press, 100, 60, 20, 5, false).len(),
            1,
            "the press"
        );
        assert_eq!(
            mouse.poll(&screen, drag, 100, 60, 20, 5, false).len(),
            1,
            "the drag"
        );
        assert_eq!(
            mouse.poll(&screen, release, 100, 60, 20, 5, false).len(),
            1,
            "the release"
        );

        // ?1000 hears the same two edges and nothing else
        let screen = mouse_screen(b"\x1b[?1000h");
        let mut mouse = Mouse::default();
        assert_eq!(
            mouse.poll(&screen, press, 100, 60, 20, 5, false).len(),
            1,
            "the press"
        );
        assert_eq!(
            mouse.poll(&screen, drag, 100, 60, 20, 5, false).len(),
            0,
            "a click mode drops the motion it never asked for"
        );
        assert_eq!(
            mouse.poll(&screen, release, 100, 60, 20, 5, false).len(),
            1,
            "the release"
        );
    }

    /// The panel is on top: while it is open the mouse is its own, and a button
    /// it took is not reported to the shell as a fresh press on the frame after
    /// it closes.
    #[test]
    fn the_mouse_path_is_silent_while_the_panel_is_open() {
        let screen = mouse_screen(b"\x1b[?1002h");
        let mut mouse = Mouse::default();
        let down = MouseState {
            pos: Some((5.0, 5.0)),
            left: true,
            ..MouseState::default()
        };
        assert!(
            mouse.poll(&screen, down, 100, 60, 20, 5, true).is_empty(),
            "the panel owns the mouse"
        );
        assert!(
            mouse.poll(&screen, down, 100, 60, 20, 5, false).is_empty(),
            "the button was already down, so closing the panel is not a press"
        );
        let up = MouseState {
            pos: Some((5.0, 5.0)),
            ..MouseState::default()
        };
        assert_eq!(
            mouse.poll(&screen, up, 100, 60, 20, 5, false).len(),
            1,
            "the release is the first thing the shell hears"
        );
    }

    /// A wheel notch is one report, whatever the frame rate, and the fraction of
    /// a notch a trackpad sends is still a notch - there is no way for a shell
    /// to say "a bit of a scroll", so it rounds up.
    #[test]
    fn the_wheel_reports_one_event_per_notch() {
        let screen = mouse_screen(b"\x1b[?1000h");
        let mut mouse = Mouse::default();
        let up = MouseState {
            pos: Some((5.0, 5.0)),
            wheel: 1,
            ..MouseState::default()
        };
        assert_eq!(
            mouse.poll(&screen, up, 100, 60, 20, 5, false).len(),
            1,
            "one notch, one report"
        );
        assert_eq!(
            mouse.poll(&screen, up, 100, 60, 20, 5, false).len(),
            1,
            "the next notch is the next report"
        );
        assert_eq!(wheel_notches(12.0), 1);
        assert_eq!(wheel_notches(24.0), 2);
        assert_eq!(wheel_notches(-12.0), -1);
        assert_eq!(wheel_notches(3.0), 1);
        assert_eq!(wheel_notches(0.0), 0);
    }

    /// One press of Enter reaches the window twice - Windows sends `WM_KEYDOWN`
    /// as a key event and the `WM_CHAR` translated from it as a character event,
    /// and a second `WM_CHAR` follows the key release - and both paths translate
    /// it.  Three carriage returns on the wire is the command plus two empty
    /// ones: the two-extra-prompts fault, seen after `echo` and after `cls`.
    #[test]
    fn one_enter_press_sends_one_carriage_return() {
        let events = [
            Input::Key(Key::Enter, true),
            Input::Text('\r'),
            Input::Key(Key::Enter, false),
            Input::Text('\r'),
        ];
        let batch = window_input(&events, Mods::default(), 17, false, false);
        assert_eq!(batch.bytes, b"\r");
    }

    /// In application cursor mode the arrows go out as SS3 - `ESC O A` - which
    /// is what vim and less ask for with DECCKM and what they match on.  Sent as
    /// CSI anyway, they are read as a plain escape and a letter.
    #[test]
    fn arrows_follow_the_cursor_key_mode() {
        let up = [Input::Key(Key::Up, true)];
        assert_eq!(
            window_input(&up, Mods::default(), 17, false, false).bytes,
            b"\x1b[A"
        );
        assert_eq!(
            window_input(&up, Mods::default(), 17, true, false).bytes,
            b"\x1bOA"
        );
        let home = [Input::Key(Key::Home, true)];
        assert_eq!(
            window_input(&home, Mods::default(), 17, true, false).bytes,
            b"\x1bOH"
        );
    }

    /// With the options panel open the four navigation keys belong to it: they
    /// come back as panel keys and produce no bytes, so Enter on a row cannot
    /// also run the shell's command line underneath and an arrow cannot move the
    /// shell's cursor.  With the panel closed the same four keys are the shell's
    /// again, exactly as before the panel existed.
    #[test]
    fn the_panel_takes_the_navigation_keys_from_the_shell() {
        let events = [
            Input::Key(Key::Up, true),
            Input::Key(Key::Down, true),
            Input::Key(Key::Enter, true),
            Input::Key(Key::Escape, true),
        ];
        let batch = window_input(&events, Mods::default(), 17, false, true);
        assert!(
            matches!(
                batch.panel_keys.as_slice(),
                [
                    crate::panel::PanelKey::Up,
                    crate::panel::PanelKey::Down,
                    crate::panel::PanelKey::Enter,
                    crate::panel::PanelKey::Escape,
                ]
            ),
            "the four navigation keys become panel keys"
        );
        assert!(
            batch.bytes.is_empty(),
            "and none of them reaches the shell: {:?}",
            batch.bytes
        );

        let batch = window_input(&events, Mods::default(), 17, false, false);
        assert!(batch.panel_keys.is_empty(), "a closed panel takes nothing");
        assert_eq!(batch.bytes, b"\x1b[A\x1b[B\r\x1b");
    }

    /// The same for Backspace, which the two paths spell differently (0x7f from
    /// the key, 0x08 from the character), and for Ctrl+letter, which the
    /// character path reports as the C0 control itself.
    #[test]
    fn the_other_doubled_keys_are_sent_once_too() {
        let bs = [
            Input::Key(Key::Backspace, true),
            Input::Text('\x08'),
            Input::Key(Key::Backspace, false),
            Input::Text('\x08'),
        ];
        assert_eq!(
            window_input(&bs, Mods::default(), 17, false, false).bytes,
            b"\x7f"
        );

        let ctrl_c = [
            Input::Key(Key::LeftCtrl, true),
            Input::Key(Key::C, true),
            Input::Text('\x03'),
            Input::Key(Key::C, false),
            Input::Key(Key::LeftCtrl, false),
        ];
        let batch = window_input(&ctrl_c, Mods::default(), 17, false, false);
        assert_eq!(batch.bytes, b"\x03");
        assert!(!batch.mods.ctrl, "the modifier state comes back with it");
    }

    /// A control character never comes from the character path, because it is
    /// always reported as a key as well.  The one casualty is a pasted line
    /// break, which arrives as a character with no key event - this is a
    /// terminal with no bracketed paste, and one doubled Enter per press is a
    /// much worse trade than that.
    #[test]
    fn control_characters_never_come_from_the_character_path() {
        let events = [
            Input::Text('a'),
            Input::Text('\r'),
            Input::Text('b'),
            Input::Text('\t'),
        ];
        assert_eq!(
            window_input(&events, Mods::default(), 17, false, false).bytes,
            b"ab"
        );
    }

    /// The heart of terminal mode: content written while it is still moving
    /// lives in the transient population, so it fades; committing it with
    /// `Write::Settle` makes it permanent and crisp; and a commit *replaces* the
    /// stored image rather than merging, or whatever moved away would ghost for
    /// ever.
    #[test]
    fn transient_content_fades_while_settled_content_holds() {
        let mut screen = vt::Screen::new(20, 4);
        let mut parser = vt::Parser::new();
        parser.feed(b"first", &mut screen);

        // transient: nothing is stored, and the flash decays away
        let mut tube = Tube::new(1024, 780);
        stroke_screen(&screen, 20, 4, &mut tube, Write::Flash, false);
        // stats() is (mean light, fraction of the target carrying stored
        // charge); the second is the one that says whether anything stored
        let (_, stored) = tube.stats();
        assert!(stored == 0.0, "a transient write must not store anything");
        tube.decay(4.0);
        let mut buf = Vec::new();
        tube.render_into(&mut buf, 1, 1);
        let band_max = |buf: &[u32], y0: i32, y1: i32| {
            (y0..y1)
                .flat_map(|y| (0..1024).map(move |x| (x, y)))
                .map(|(x, y)| (buf[(y * 1024 + x) as usize] >> 8) & 0xff)
                .max()
                .unwrap_or(0)
        };
        // the first cell of a 4 row page spans the whole top quarter
        let faded = band_max(&buf, 0, 195);
        assert!(
            faded < 90,
            "the transient image should have faded, got {faded}"
        );

        // settled: the same screen committed does store, and survives any wait
        let mut tube = Tube::new(1024, 780);
        tube.clear_stored();
        stroke_screen(&screen, 20, 4, &mut tube, Write::Settle, false);
        let (_, stored) = tube.stats();
        assert!(stored > 0.0, "a settle write must store charge");
        tube.decay(600.0);
        tube.render_into(&mut buf, 1, 1);
        let held = band_max(&buf, 0, 195);
        assert!(held > 150, "the settled image should hold, got {held}");

        // A commit *replaces* the stored image, so content that moved away
        // leaves no ghost.  This is the crux of putting a terminal on a target
        // that cannot erase: merging would keep the old line for ever.
        let mut a = vt::Screen::new(20, 4);
        let mut pa = vt::Parser::new();
        pa.feed(b"AAAAAAAAAA", &mut a);
        let mut b = vt::Screen::new(20, 4);
        let mut pb = vt::Parser::new();
        pb.feed(b"          BBBBBBBBBB", &mut b);

        let mut tube = Tube::new(1024, 780);
        tube.clear_stored();
        stroke_screen(&a, 20, 4, &mut tube, Write::Settle, false);
        tube.clear_stored();
        stroke_screen(&b, 20, 4, &mut tube, Write::Settle, false);
        tube.decay(600.0);
        tube.render_into(&mut buf, 1, 1);
        // a cell is 1024/20 wide and the first row of a 4 row page sits about a
        // third of the way down the target
        let cell_max = |col: i32| {
            let x0 = col * 51;
            (100..160)
                .flat_map(|y| (x0..(x0 + 51)).map(move |x| (x, y)))
                .map(|(x, y)| (buf[(y * 1024 + x) as usize] >> 8) & 0xff)
                .max()
                .unwrap_or(0)
        };
        assert!(
            cell_max(10) > 150,
            "the new text should be there, got {}",
            cell_max(10)
        );
        assert!(
            cell_max(0) < 110,
            "text that moved away must not ghost, got {}",
            cell_max(0)
        );
    }

    #[test]
    fn a_wholesale_change_is_a_page_replacement_not_a_trace() {
        // a cell or two is a trace
        assert!(!wholesale(0, 40));
        assert!(!wholesale(1, 40));
        assert!(!wholesale(10, 40));
        // a scroll, a clear, an alt screen switch: the page moved, and tracing
        // it would be minutes of drawing at any watchable beam rate
        assert!(wholesale(11, 40));
        assert!(wholesale(40, 40));
        // and it is a share of the page, not a count: a quarter is the line
        assert!(wholesale(26, 100));
        assert!(!wholesale(25, 100));
    }

    /// The image is what the pen has drawn, never what the screen says.  Taking
    /// the character from the screen put a glyph into the stored image before
    /// the beam had drawn it, and the beam then drew it again over the top:
    /// that is the doubled drawing.
    #[test]
    fn a_settle_stores_what_the_pen_drew_not_what_the_screen_says() {
        let mut screen = vt::Screen::new(20, 4);
        let mut parser = vt::Parser::new();
        parser.feed(b"A", &mut screen);
        let mut tube = Tube::new(1024, 780);
        let mut pen = Pen::new(20 * 4);
        fn run(pen: &mut Pen, tube: &mut Tube, screen: &vt::Screen) {
            let mut guard = 0;
            while pen.busy(screen) && guard < 4000 {
                pen.advance(8.0, screen, tube, 20, 4);
                guard += 1;
            }
            assert!(guard < 4000, "the pen never finished");
        }

        // the screen already holds the letter, but nothing has been drawn, so
        // there is nothing to settle
        tube.clear_stored();
        pen.settle_drawn(&mut tube, &screen, 20, 4);
        assert_eq!(tube.stats().1, 0.0, "the screen is not the image");

        // draw it, and it settles
        run(&mut pen, &mut tube, &screen);
        tube.clear_stored();
        pen.settle_drawn(&mut tube, &screen, 20, 4);
        assert!(tube.stats().1 > 0.0, "what the pen drew must settle");

        // the screen goes blank: the drawn glyph is still the image, because a
        // tube that cannot erase holds it until the beam comes back for it
        let blank = vt::Screen::new(20, 4);
        tube.clear_stored();
        pen.settle_drawn(&mut tube, &blank, 20, 4);
        assert!(
            tube.stats().1 > 0.0,
            "a cell the pen has drawn is not blanked by the screen alone"
        );

        // once the beam has walked over the blank page the cell is gone
        run(&mut pen, &mut tube, &blank);
        tube.clear_stored();
        pen.settle_drawn(&mut tube, &blank, 20, 4);
        assert_eq!(
            tube.stats().1,
            0.0,
            "a cell the pen has erased leaves the image"
        );
    }

    /// Per screen row, is there any ink?  The same band test as the lit-pixel
    /// checks above, one band per screen row.
    fn row_ink(tube: &mut Tube, rows: usize) -> Vec<bool> {
        let (w, h) = (tube.w, tube.h);
        let mut buf = Vec::new();
        tube.render_into(&mut buf, 1, 1);
        let ch = (h / rows as i32).max(1);
        (0..rows)
            .map(|r| {
                (r as i32 * ch..(r as i32 + 1) * ch)
                    .any(|y| (0..w).any(|x| ((buf[(y * w + x) as usize] >> 8) & 0xff) > 140))
            })
            .collect()
    }

    /// A scroll *moves* the committed image: the rows that scrolled off are gone,
    /// the rest travel up together, and the line that scrolled in arrives at the
    /// bottom.  Nothing is retraced - which is what makes a listing that scrolls
    /// cost one page-wide settle instead of minutes of beam time.
    #[test]
    fn a_scroll_moves_the_image_up_instead_of_being_retraced() {
        let mut screen = vt::Screen::new(40, 6);
        let mut parser = vt::Parser::new();
        parser.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix", &mut screen);
        let mut tube = Tube::new(1024, 780);
        let mut pen = Pen::new(40 * 6);
        let mut guard = 0;
        while pen.busy(&screen) && guard < 4000 {
            pen.advance(200.0, &screen, &mut tube, 40, 6);
            guard += 1;
        }
        assert!(!pen.busy(&screen), "the first page must be drawn");
        pen.settle_drawn(&mut tube, &screen, 40, 6);
        tube.decay(600.0);
        let before = row_ink(&mut tube, 6);
        assert!(
            before[0] && before[5],
            "six lines fill the page: {before:?}"
        );

        // one more line scrolls the page, and the screen says how far
        parser.feed(b"\r\nseven", &mut screen);
        assert_eq!(screen.take_page_change(), PageChange::Scroll(1));

        // what the commit does with that: sync the record so nothing is
        // retraced, and settle the moved page in one go
        pen.sync(&screen);
        assert!(
            !pen.busy(&screen),
            "a moved page leaves the beam nothing to trace"
        );
        tube.clear_stored();
        pen.settle_drawn(&mut tube, &screen, 40, 6);
        tube.decay(600.0);

        let after = row_ink(&mut tube, 6);
        for r in 0..5 {
            assert_eq!(
                after[r],
                before[r + 1],
                "row {r} should hold what was on row {}",
                r + 1
            );
        }
        assert!(
            after[5],
            "the line that scrolled in must be there: {after:?}"
        );
    }

    /// Lit pixels in the rendered frame, the same band test the tube's own tests
    /// use for "is there ink here".
    fn lit_pixels(tube: &mut Tube) -> usize {
        let mut buf = Vec::new();
        tube.render_into(&mut buf, 1, 1);
        buf.iter().filter(|p| ((**p >> 8) & 0xff) > 140).count()
    }

    /// With `--ghost`, the page a scroll replaced keeps glowing where it was:
    /// the previous screenful still legible underneath the new one, which is
    /// what a raster terminal's phosphor does and what our storage tube does
    /// not - its charge is permanent, so the ghost is off unless asked for.
    #[test]
    fn a_ghost_lingers_where_the_old_page_was_and_then_fades() {
        let mut screen = vt::Screen::new(40, 6);
        let mut parser = vt::Parser::new();
        parser.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix", &mut screen);

        let mut tube = Tube::new(1024, 780);
        tube.ghost_tau = 1.0;
        // the page that is on the tube when the scroll happens
        stroke_screen(&screen, 40, 6, &mut tube, Write::Settle, false);
        tube.capture_ghost();

        // the page moves: the next six lines are stamped where the old ones were
        parser.feed(
            b"\r\nseven\r\neight\r\nnine\r\nten\r\neleven\r\ntwelve",
            &mut screen,
        );
        tube.clear_stored();
        stroke_screen(&screen, 40, 6, &mut tube, Write::Settle, false);

        let with_ghost = lit_pixels(&mut tube);
        tube.decay(20.0);
        let page_only = lit_pixels(&mut tube);
        assert!(
            with_ghost > page_only,
            "the ghost must add light: {with_ghost} against {page_only}"
        );

        // and what is left when it has faded is exactly the new page
        let mut plain = Tube::new(1024, 780);
        stroke_screen(&screen, 40, 6, &mut plain, Write::Settle, false);
        assert_eq!(
            page_only,
            lit_pixels(&mut plain),
            "the ghost must fade to nothing, leaving the page"
        );

        // with the ghost off there is nothing to capture in the first place
        let mut off = Tube::new(1024, 780);
        stroke_screen(&screen, 40, 6, &mut off, Write::Settle, false);
        off.capture_ghost();
        assert_eq!(
            lit_pixels(&mut off),
            page_only,
            "a storage tube's charge does not fade, so nothing is captured"
        );
    }

    /// An erase takes the charge and leaves the light: blanking a cell does not
    /// switch off the phosphor that was in it, which is why a page still shows
    /// through for a moment after a raster terminal has wiped it.
    #[test]
    fn an_erase_takes_the_charge_and_leaves_the_ghost_light() {
        let mut tube = Tube::new(1024, 780);
        tube.ghost_tau = 1.0;
        let mut screen = vt::Screen::new(40, 6);
        let mut parser = vt::Parser::new();
        parser.feed(b"one\r\ntwo", &mut screen);
        stroke_screen(&screen, 40, 6, &mut tube, Write::Settle, false);
        tube.capture_ghost();
        tube.erase();
        assert!(
            lit_pixels(&mut tube) > 0,
            "the light the page left behind is still there"
        );
        tube.decay(20.0);
        assert_eq!(lit_pixels(&mut tube), 0, "and it fades on its own");
    }

    /// The delta stroke writes the cells that changed and nothing else, which is
    /// what keeps an idle terminal from re-stroking the whole page for one byte.
    #[test]
    fn the_delta_stroke_writes_only_what_changed() {
        let mut screen = vt::Screen::new(20, 4);
        let mut parser = vt::Parser::new();
        parser.feed(b"one\r\ntwo\r\nthree", &mut screen);
        let before: Vec<(char, bool)> = (0..80).map(|i| Pen::drawn_want(&screen, i)).collect();
        let whole = {
            let mut t = Tube::new(512, 256);
            stroke_screen(&screen, 20, 4, &mut t, Write::Flash, false);
            t.beam_dots()
        };
        assert!(whole > 0.0, "the whole-page stroke must write something");

        // nothing has changed, so nothing is written
        let mut tube = Tube::new(512, 256);
        stroke_changed(&screen, &before, 20, 4, &mut tube, Write::Flash, false);
        assert_eq!(tube.beam_dots(), 0.0, "an unchanged page costs nothing");

        // one character changes, so one cell is written
        parser.feed(b"\x1b[1;1HX", &mut screen);
        stroke_changed(&screen, &before, 20, 4, &mut tube, Write::Flash, false);
        let one = tube.beam_dots();
        assert!(
            one > 0.0 && one < whole * 0.2,
            "one cell should cost one cell: {one} against {whole} for the page"
        );
    }

    /// A page replacement takes the previous page's *glow* with it, not just its
    /// stored charge.
    ///
    /// The shell erases the tail of the command line and writes a new prompt;
    /// the erased glyphs are still in the transient population, and with only
    /// the stored image cleared they stayed lit for the whole trail - which is
    /// what showed the old prompt with the old command still on it, twice over,
    /// after an `echo` or a `cls`.
    #[test]
    fn a_page_replacement_erases_the_previous_page_s_glow_too() {
        let mut tube = Tube::new(1024, 780);
        let mut screen = vt::Screen::new(74, 18);
        let mut parser = vt::Parser::new();
        parser.feed(b"PS C:\\Users\\kevin> echo hi", &mut screen);
        let mut pen = Pen::new(74 * 18);

        // the page the shell has written, and the erased tail of the command
        // line it has just taken away - which is in the transient population only
        stroke_screen(&screen, 74, 18, &mut tube, Write::Settle, false);
        let flash = Style {
            line: LineStyle::Solid,
            defocus: false,
            write: Write::Flash,
        };
        tube.character(b'e', 300.0, 700.0, 20, 30, flash, 1.0, false);
        assert!(
            glow_pixels(&mut tube) > 0,
            "the erased glyph should still be glowing"
        );

        // a clear of the display (or a scroll) is a page replacement
        parser.feed(b"\x1b[2J", &mut screen);
        commit_to(&mut tube, &screen, &mut pen, 74, 18, false);
        assert_eq!(
            glow_pixels(&mut tube),
            0,
            "a replaced page must not leave the old glyphs shining through"
        );
    }

    /// Pixels that come only from the transient population: render with the
    /// stored charge switched off, so nothing but the beam's own glow is left.
    /// The flood gun's field is dimmer than the threshold, as it is in the
    /// lit-pixel checks above.
    fn glow_pixels(tube: &mut Tube) -> usize {
        let stored_gain = tube.phosphor.stored_gain;
        tube.phosphor.stored_gain = 0.0;
        let mut buf = Vec::new();
        tube.render_into(&mut buf, 1, 1);
        tube.phosphor.stored_gain = stored_gain;
        buf.iter().filter(|p| ((**p >> 8) & 0xff) > 100).count()
    }

    /// The pen spends its budget in dots, which is what makes `--write-rate`
    /// mean dots per second and its default of 4000 mean about 65 letters a
    /// second.
    #[test]
    fn the_pen_spends_its_budget_in_dots() {
        let mut screen = vt::Screen::new(74, 35);
        let mut parser = vt::Parser::new();
        parser.feed(b"HHHH", &mut screen);
        let mut tube = Tube::new(1024, 780);
        let mut pen = Pen::new(74 * 35);
        let before = tube.beam_dots();
        pen.advance(30.0, &screen, &mut tube, 74, 35);
        let spent = tube.beam_dots() - before;
        // The budget is spent dot for dot up to the rounding of each segment's
        // endpoints onto the target grid - a dot or so per segment - plus one
        // dot for the hot spot at the beam tip.  What matters is the scale: a
        // budget in any other unit would come out at hundreds here, because four
        // letters carry a couple of hundred dots.
        assert!(
            (spent - 30.0).abs() <= 5.0,
            "a 30 dot budget must spend about 30 dots, spent {spent}"
        );
    }

    /// The point of the pen: a letter must appear *part* drawn after a small
    /// budget and complete after enough of them, which is what "you can watch it
    /// being written" means mechanically.
    /// The symptom this guards: with the pen running, nothing was ever
    /// committed, so drawn text decayed all the way back to the background
    /// instead of cooling from the bright flash to the stored green.  A commit
    /// during a slow trace must settle whatever has been drawn so far.
    #[test]
    fn a_commit_mid_trace_settles_what_has_been_drawn() {
        let mut screen = vt::Screen::new(20, 4);
        let mut parser = vt::Parser::new();
        parser.feed(b"AB", &mut screen);
        let mut tube = Tube::new(1024, 780);
        let mut pen = Pen::new(20 * 4);

        // part way through the first letter: nothing is complete, so there is
        // nothing to settle yet
        pen.advance(4.0, &screen, &mut tube, 20, 4);
        pen.settle_drawn(&mut tube, &screen, 20, 4);
        assert_eq!(tube.stats().1, 0.0, "an unfinished glyph must not settle");

        // finish it: now a settle during the trace *must* store something, even
        // though the pen is still busy with the rest of the screen
        let mut guard = 0;
        while pen.drawn.first().copied().unwrap_or((' ', false)) == (' ', false) && guard < 400 {
            pen.advance(3.0, &screen, &mut tube, 20, 4);
            guard += 1;
        }
        assert!(pen.busy(&screen), "the pen should still have work");
        tube.clear_stored();
        pen.settle_drawn(&mut tube, &screen, 20, 4);
        assert!(
            tube.stats().1 > 0.0,
            "a mid-trace commit must settle the drawn glyph"
        );
        tube.decay(600.0);
        let mut buf = Vec::new();
        tube.render_into(&mut buf, 1, 1);
        // the first row of a 4 row page sits about a fifth of the way down, and a
        // glyph's ink is well inside its cell, so scan a band not a row
        let lit = (60..200)
            .flat_map(|y| (0..1024).map(move |x| (x, y)))
            .map(|(x, y)| (buf[(y * 1024 + x) as usize] >> 8) & 0xff)
            .max()
            .unwrap();
        assert!(
            lit > 140,
            "and it must still be readable after settling, got {lit}"
        );
    }

    #[test]
    fn the_pen_draws_a_letter_progressively() {
        let mut screen = vt::Screen::new(74, 35);
        let mut parser = vt::Parser::new();
        parser.feed(b"H", &mut screen);
        let mut tube = Tube::new(1024, 780);
        let mut pen = Pen::new(74 * 35);
        assert!(pen.busy(&screen), "there is a letter to draw");

        let ink = |tube: &mut Tube| {
            let mut buf = Vec::new();
            tube.render_into(&mut buf, 1, 1);
            buf.iter().filter(|p| ((*p >> 8) & 0xff) > 110).count()
        };

        // a sixth of a second's worth of drawing, well short of a whole letter
        pen.advance(6.0, &screen, &mut tube, 74, 35);
        let part = ink(&mut tube);
        assert!(part > 0, "the pen should have drawn something");
        assert!(pen.busy(&screen), "and it should not be finished");

        // keep going until it is done
        let mut frames = 1;
        while pen.busy(&screen) && frames < 400 {
            pen.advance(10.0, &screen, &mut tube, 74, 35);
            frames += 1;
        }
        let whole = ink(&mut tube);
        assert!(!pen.busy(&screen), "a full pass leaves nothing to draw");
        assert!(
            whole > part,
            "the finished letter must carry more ink than the partial one: {whole} vs {part}"
        );
        assert!(
            frames > 1,
            "it must take more than one frame, took {frames}"
        );

        // an unchanged screen costs the pen nothing
        pen.advance(1000.0, &screen, &mut tube, 74, 35);
        assert!(!pen.busy(&screen), "an unchanged screen is not work");
        assert_eq!(ink(&mut tube), whole, "and it must not draw anything more");
    }

    #[test]
    fn the_parser_and_screen_drive_a_tube() {
        // the whole render path with no pseudoconsole: parse a stream, stroke it
        // as a commit, and check the tube lit up where the text is
        let mut tube = Tube::new(1024, 780);
        let mut screen = vt::Screen::new(74, 35);
        let mut parser = vt::Parser::new();
        parser.feed(b"hello\r\nworld", &mut screen);
        tube.clear_stored();
        stroke_screen(&screen, 74, 35, &mut tube, Write::Settle, false);
        let mut buf = Vec::new();
        tube.render_into(&mut buf, 1, 1);
        let green = |x: i32, y: i32| (buf[(y * 1024 + x) as usize] >> 8) & 0xff;
        // The first text row is drawn at the top of the target, and a glyph's
        // baseline sits about a quarter of the cell above the cell's bottom, so
        // scan a band rather than a single row.
        let band = |y0: i32, y1: i32| {
            (y0..y1)
                .flat_map(|y| (0..1024).map(move |x| (x, y)))
                .map(|(x, y)| green(x, y))
                .max()
                .unwrap_or(0)
        };
        let top_lit = band(0, 30);
        assert!(
            top_lit > 100,
            "the first text row should be lit, got {top_lit}"
        );
        let mid_lit = band(370, 410);
        assert!(
            mid_lit < top_lit,
            "the middle should be darker than the text row: {mid_lit} vs {top_lit}"
        );
    }

    /// The shell commands come from the setup's `shells.txt`, and the shells
    /// worth offering sit under `Program Files`, so the quoting has to survive a
    /// path with a space in it.
    #[test]
    fn a_command_line_splits_on_quotes_not_only_on_spaces() {
        let split = |s: &str| split_command(s);
        assert_eq!(split("powershell.exe"), vec!["powershell.exe"]);
        assert_eq!(
            split("pwsh.exe -NoLogo -NoProfile"),
            vec!["pwsh.exe", "-NoLogo", "-NoProfile"]
        );
        assert_eq!(
            split(r#""C:\Program Files\Git\bin\bash.exe" --login -i"#),
            vec![r"C:\Program Files\Git\bin\bash.exe", "--login", "-i"]
        );
        assert_eq!(
            split(r#""C:\Program Files\Git\bin\bash.exe""#),
            vec![r"C:\Program Files\Git\bin\bash.exe"]
        );
        assert_eq!(split("wsl.exe -d Ubuntu"), vec!["wsl.exe", "-d", "Ubuntu"]);
        // a token can be part quoted, and a doubled quote is a literal one
        assert_eq!(split(r#"--flag="a b" c"#), vec!["--flag=a b", "c"]);
        assert_eq!(split(r#"say "say ""hi""""#), vec!["say", r#"say "hi""#]);
        // an empty quoted argument is an argument, and an unclosed quote takes
        // the rest of the line rather than dropping it
        assert_eq!(split(r#"a "" b"#), vec!["a", "", "b"]);
        assert_eq!(split(r#"a "b c"#), vec!["a", "b c"]);
        assert_eq!(split("   "), Vec::<String>::new());
    }
}
