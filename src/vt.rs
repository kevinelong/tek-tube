//! A VT screen and parser, scoped to what the census actually found.
//!
//! This is M1 of docs/terminal-mode.md, built in the sandbox first so the real
//! repo stays dependency-free until the approach is proven.  The subset is
//! deliberately the measured one, not the spec: from a real 71 KB PowerShell
//! session the only sequences used were CUP (dominant), ECH, SGR, cursor
//! visibility, focus/Win32-input-mode private modes, OSC title, and the
//! `CSI 6n` cursor query.  ED/EL/scroll-region/alt-screen did not appear at all
//! and are implemented only where they are cheap, because a full-screen app
//! would use them.
//!
//! Two behaviours matter more than the rest:
//!
//! * `CSI 6n` must be answered *from the real cursor position*.  In the M0
//!   spike a fixed `1;1` answer let the session run but corrupted PSReadLine's
//!   redraw into overlapping text, because it positions relative to what we
//!   told it.
//! * ECH (`CSI nX`) is how PSReadLine erases as it redraws a line; get it wrong
//!   and the prompt accumulates garbage.

/// How the page moved as a whole since the renderer last asked.
///
/// A scroll is not a clear: the committed image can move with it and only the
/// lines that scrolled in are new, so the renderer slides the image instead of
/// rewriting the page.  Everything else - an erase of the display, an alt screen
/// switch, a resize - replaces the page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageChange {
    None,
    Scroll(usize),
    Wholesale,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Attr {
    /// SGR colour index, mapped to a brightness for a monochrome green tube.
    Dim,
    Normal,
    Bright,
    /// SGR 7 reverse video.  A storage tube has no second colour to swap in, so
    /// there is nothing to draw; the attribute is tracked anyway because SGR 27
    /// has to clear exactly this and not the rest of the state.
    Reverse,
}

/// The state `ESC 7` saves and `ESC 8` puts back (DECSC/DECRC).
#[derive(Clone, Copy)]
struct SavedState {
    cx: usize,
    cy: usize,
    attr: Attr,
    g0_acs: bool,
    g1_acs: bool,
    gl: u8,
}

pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    ch: Vec<char>,
    attr: Vec<Attr>,
    /// Rows above and below that have changed since the last take.
    dirty: Vec<bool>,
    pub cx: usize,
    pub cy: usize,
    pub cursor_visible: bool,
    pub autowrap: bool,
    /// The cursor keys send `ESC O A` rather than `ESC [ A` (DECCKM).  The key
    /// path reads this, so it is public.
    pub app_cursor: bool,
    /// Which of G0 and G1 is the special graphics set, and which is shifted in.
    /// Programs select it around line art (`ESC ( 0`) and put ASCII back with
    /// `ESC ( B`, usually without touching SO/SI.
    g0_acs: bool,
    g1_acs: bool,
    gl: u8,
    pub title: String,
    scroll_top: usize,
    scroll_bottom: usize,
    /// What `ESC 7` saved and `ESC 8` restores: the cursor, the attribute in
    /// force and the charset state, which is what DECSC/DECRC are defined to
    /// carry.
    saved: SavedState,
    attr_now: Attr,
    /// Set when the cursor is in the pending-wrap state after filling the line.
    pending_wrap: bool,
    /// Which columns a TAB stops on.  The default is every eighth column; `ESC
    /// H` adds a stop and `CSI g` removes one or all.  A program that lays out
    /// its own columns (a form, a table) relies on these, so the default grid
    /// alone is not enough.
    tabs: Vec<bool>,
    /// Origin mode (DECOM, `CSI ?6 h`): CUP and VPA rows count from the top of
    /// the scroll region rather than the page, which is how a full-screen
    /// program keeps its drawing inside its own region.
    origin: bool,
    /// Mouse tracking mode: 0 off, 1 = `?1000` (press/release), 2 = `?1002`
    /// (adds drag), 3 = `?1003` (adds bare motion).
    mouse_mode: u8,
    /// Report the mouse in SGR (`?1006`) rather than the legacy X10 bytes.
    mouse_sgr: bool,
    /// Bracketed paste (`?2004`): the key path wraps a paste in
    /// `ESC [ 200 ~` / `ESC [ 201 ~` when this is set.
    bracketed_paste_mode: bool,
    /// Per row: (double width, double height) from `ESC # 3/4/5/6`.  Recorded
    /// because the sequence must be consumed rather than printed; the renderer
    /// decides what to draw, and today it draws nothing special.
    double_line: Vec<(bool, bool)>,
    /// Lines that have scrolled off the top, oldest first.  A storage tube has
    /// no memory of these - showing them is a deliberate convenience, and the
    /// renderer is what decides whether to show the live page or the history.
    history: std::collections::VecDeque<(Vec<char>, Vec<Attr>)>,
    /// How many lines back the view is from the live page.  0 is live.
    view: usize,
    /// The screen saved by the alternate-screen modes (`CSI ?1049h` / `?47h`),
    /// which is how full-screen programs keep the shell's scrollback intact
    /// while they own the page.
    alt_saved: Option<Box<AltSaved>>,
    /// How the page moved as a whole: a scroll carries the committed image with
    /// it, anything else replaces it.
    change: PageChange,
}

/// How much scrollback to keep.  A storage tube holds one page; this is a
/// convenience bolted on top, so it is bounded hard.
const HISTORY_MAX: usize = 4000;

/// The primary screen, stashed while a full-screen program owns the page.
struct AltSaved {
    ch: Vec<char>,
    attr: Vec<Attr>,
    cx: usize,
    cy: usize,
    scroll_top: usize,
    scroll_bottom: usize,
}

impl Screen {
    /// Apply a charset designator: `0` selects the DEC special graphics set the
    /// line-drawing characters live in, `B` selects ASCII.  The other sets a
    /// designator can name (UK, the national variants) differ from ASCII in one
    /// or two glyphs this face does not draw, so they are treated as ASCII.  The
    /// designator is consumed for all four slots either way: leaving `ESC ( A`
    /// to print would put a stray "A" in the output.
    pub fn set_charset(&mut self, slot: u8, designator: u8) {
        let acs = designator == b'0';
        match slot {
            0 => self.g0_acs = acs,
            1 => self.g1_acs = acs,
            // G2/G3 are eaten here too; nothing shifts them into use.
            _ => {}
        }
    }

    /// Is the special graphics set selected right now?
    fn acs_in_force(&self) -> bool {
        if self.gl == 0 {
            self.g0_acs
        } else {
            self.g1_acs
        }
    }

    /// A printable ASCII byte as it should be *displayed*.
    ///
    /// With the special graphics set selected, `0x5F..=0x7E` is line art rather
    /// than text: `q` is a horizontal rule, `x` a vertical one, `l` `k` `m` `j`
    /// the corners.  Those are carried on `U+0080..=U+009F` - one code each, the
    /// same order - so the renderer can tell a drawn rule from a real letter
    /// without the parser and the font having to share a table.
    fn charset_char(&self, b: u8) -> char {
        if self.acs_in_force() && (0x5f..=0x7e).contains(&b) {
            char::from_u32(0x80 + (b - 0x5f) as u32).unwrap_or(b as char)
        } else {
            b as char
        }
    }

    pub fn new(cols: usize, rows: usize) -> Self {
        Screen {
            cols,
            rows,
            ch: vec![' '; cols * rows],
            attr: vec![Attr::Normal; cols * rows],
            dirty: vec![true; rows],
            cx: 0,
            cy: 0,
            cursor_visible: true,
            autowrap: true,
            app_cursor: false,
            g0_acs: false,
            g1_acs: false,
            gl: 0,
            title: String::new(),
            scroll_top: 0,
            scroll_bottom: rows - 1,
            saved: SavedState {
                cx: 0,
                cy: 0,
                attr: Attr::Normal,
                g0_acs: false,
                g1_acs: false,
                gl: 0,
            },
            attr_now: Attr::Normal,
            pending_wrap: false,
            tabs: (0..cols).map(|i| i % 8 == 0).collect(),
            origin: false,
            mouse_mode: 0,
            mouse_sgr: false,
            bracketed_paste_mode: false,
            double_line: vec![(false, false); rows],
            history: std::collections::VecDeque::new(),
            view: 0,
            alt_saved: None,
            change: PageChange::None,
        }
    }

    /// Switch to the alternate screen, remembering the primary one.  `clear` is
    /// what `?1049h` and `?47h` both do; `?47h` leaves the cursor alone.
    pub fn enter_alt_screen(&mut self, clear: bool, save_cursor: bool) {
        self.alt_saved = Some(Box::new(AltSaved {
            ch: std::mem::replace(&mut self.ch, vec![' '; self.cols * self.rows]),
            attr: std::mem::replace(&mut self.attr, vec![Attr::Normal; self.cols * self.rows]),
            cx: self.cx,
            cy: self.cy,
            scroll_top: self.scroll_top,
            scroll_bottom: self.scroll_bottom,
        }));
        if !save_cursor {
            // ?47h: the cursor is not saved, so leave it where it is
            self.cx = self.cx.min(self.cols - 1);
            self.cy = self.cy.min(self.rows - 1);
        } else {
            self.cx = 0;
            self.cy = 0;
        }
        let _ = clear;
        self.change = PageChange::Wholesale;
        self.dirty.iter_mut().for_each(|d| *d = true);
    }

    /// Go back to the primary screen.
    pub fn leave_alt_screen(&mut self, restore_cursor: bool) {
        if let Some(prev) = self.alt_saved.take() {
            self.ch = prev.ch;
            self.attr = prev.attr;
            self.scroll_top = prev.scroll_top;
            self.scroll_bottom = prev.scroll_bottom.min(self.rows - 1);
            if restore_cursor {
                self.cx = prev.cx.min(self.cols - 1);
                self.cy = prev.cy.min(self.rows - 1);
            }
            self.change = PageChange::Wholesale;
            self.dirty.iter_mut().for_each(|d| *d = true);
        }
    }

    /// How the page moved as a whole since this was last asked?  Taken rather
    /// than read: one scroll is one move.
    pub fn take_page_change(&mut self) -> PageChange {
        std::mem::replace(&mut self.change, PageChange::None)
    }

    pub fn in_alt_screen(&self) -> bool {
        self.alt_saved.is_some()
    }

    /// Any mouse tracking mode currently on.  The key path asks before it turns
    /// mouse events into sequences.
    pub fn wants_mouse(&self) -> bool {
        self.mouse_mode != 0
    }

    /// Whether the tracking mode reports motion while a button is held (DECSET
    /// 1002 or 1003), as opposed to presses and releases alone (1000).  The
    /// caller needs this: `mouse_report` is handed an event, not a verdict on
    /// whether the mode wanted that event, so without it a drag would be
    /// reported in a mode that never asked for one.
    pub fn mouse_tracks_drag(&self) -> bool {
        self.mouse_mode >= 2
    }

    /// Whether bare motion - no button down - is reported too (DECSET 1003).
    pub fn mouse_tracks_motion(&self) -> bool {
        self.mouse_mode >= 3
    }

    /// Whether the application asked for pasted text to be bracketed (DECSET
    /// 2004), so a paste cannot be mistaken for a burst of typed input.
    ///
    /// Unused by the input path on purpose.  There is nothing to wrap: minifb has
    /// no clipboard support at all, so a paste never reaches this window, and the
    /// one signal that might suggest one - a burst of characters in a frame - is
    /// indistinguishable from key auto-repeat or an IME commit.  The state is
    /// still worth keeping: it is what an application asks for, and the day the
    /// window gains a real paste source this is what it consults.
    #[allow(dead_code)]
    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste_mode
    }

    /// Recorded from `ESC # 3/4/5/6` and not yet acted on: the renderer draws no
    /// special width or height for such a line, so this is the state a renderer
    /// would read rather than a feature that is finished.  Kept because the
    /// sequences are consumed rather than printed, and dropping the state would
    /// mean dropping the record of what the program asked for.
    #[allow(dead_code)]
    pub fn line_double(&self, y: usize) -> (bool, bool) {
        self.double_line.get(y).copied().unwrap_or((false, false))
    }

    /// One mouse event as the tracking mode asks for it, or `None` when no mode
    /// is on.
    ///
    /// The two encodings differ in more than syntax.  X10 (`ESC [ M`) has no
    /// room for a real release, so any release is button 3, and every byte is
    /// biased by 32 because the coordinates have to stay 7-bit.  SGR (`?1006`)
    /// writes the real button and separates press from release with the final
    /// `M`/`m`.
    ///
    /// `button` is 0 left, 1 middle, 2 right; 3 is the report with no button
    /// held, i.e. bare motion.  The frozen signature has no separate motion
    /// flag, so the only motion a mode can be asked to filter is that sentinel:
    /// a drag arrives looking exactly like a press and is accepted by every
    /// mode, and `?1002` and `?1003` therefore encode identically through this
    /// interface.  What `?1000` refuses is the held-nothing motion.
    pub fn mouse_report(
        &self,
        button: u8,
        x: usize,
        y: usize,
        release: bool,
        wheel: i32,
    ) -> Option<Vec<u8>> {
        let mode = self.mouse_mode;
        if mode == 0 {
            return None;
        }
        // a bare motion event: no button held, no release, no wheel
        if wheel == 0 && !release && button >= 3 && mode < 2 {
            return None;
        }
        let b: u8 = if wheel > 0 {
            64
        } else if wheel < 0 {
            65
        } else {
            button.min(3)
        };
        // coordinates leave here one-based; the caller works in cells
        let cx = (x + 1).min(223) as u8;
        let cy = (y + 1).min(223) as u8;
        if self.mouse_sgr {
            let mut out = Vec::with_capacity(16);
            out.extend_from_slice(b"\x1b[<");
            out.extend_from_slice(format!("{};{};{}", b, cx, cy).as_bytes());
            out.push(if release { b'm' } else { b'M' });
            Some(out)
        } else {
            // X10: a release is button 3 whatever button it was, and the button
            // and both coordinates all carry a +32 bias.
            let cb = if release { 3 } else { b };
            Some(vec![0x1b, b'[', b'M', 32 + cb, 32 + cx, 32 + cy])
        }
    }

    /// Map a row of the view to what it shows: `true` and a history index, or
    /// `false` and the row within the live page.
    ///
    /// The view shows `rows` consecutive lines ending `view` lines above the
    /// bottom of the live page, so part of the window is usually history and the
    /// rest is live - and the live part starts at a row that is *not* `y`.
    fn view_map(&self, y: usize) -> (bool, usize) {
        if self.view == 0 {
            return (false, y);
        }
        let total = self.history.len() + self.rows;
        let start = total.saturating_sub(self.rows + self.view);
        let line = start + y;
        if line < self.history.len() {
            (true, line)
        } else {
            (false, line - self.history.len())
        }
    }

    /// The character displayed at (x, y) for the current view, which may be
    /// scrolled back into the history.
    pub fn visible_char(&self, x: usize, y: usize) -> char {
        let (hist, i) = self.view_map(y);
        if hist {
            self.history.get(i).map(|(c, _)| c[x]).unwrap_or(' ')
        } else {
            self.char_at(x, i)
        }
    }

    pub fn visible_attr(&self, x: usize, y: usize) -> Attr {
        let (hist, i) = self.view_map(y);
        if hist {
            self.history
                .get(i)
                .map(|(_, a)| a[x])
                .unwrap_or(Attr::Normal)
        } else {
            self.attr_at(x, i)
        }
    }

    /// Blank the visible page but keep the scrollback.
    ///
    /// What a resize needs.  A screen model that keeps no reflow cannot rewrap
    /// what was already drawn, and the shell works out which line it is on from
    /// the geometry it has been given, so leaving the old rows in place makes its
    /// repaint land *beside* them.  That is how an F11 produced a duplicated
    /// block of output and a half-overwritten prompt.  A storage tube cannot
    /// reflow either, so the honest answer is a fresh page: the shell repaints
    /// its prompt immediately, and the old lines are still in the scrollback.
    pub fn clear_visible(&mut self) {
        self.ch.iter_mut().for_each(|c| *c = ' ');
        self.attr.iter_mut().for_each(|a| *a = Attr::Normal);
        self.cx = 0;
        self.cy = 0;
        self.pending_wrap = false;
        self.view = 0;
        self.change = PageChange::Wholesale;
        self.dirty.iter_mut().for_each(|d| *d = true);
    }

    /// Scroll the view back into the history; positive is further back.
    pub fn scroll_view(&mut self, lines: i32) {
        let max = self.history.len();
        let next = (self.view as i32 + lines).clamp(0, max as i32) as usize;
        if next != self.view {
            self.view = next;
            self.dirty.iter_mut().for_each(|d| *d = true);
        }
    }

    /// How many lines are kept in the scrollback (for tests and diagnostics).
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// How far back the view is, in lines (0 is live).
    pub fn view_offset(&self) -> usize {
        self.view
    }

    pub fn char_at(&self, x: usize, y: usize) -> char {
        self.ch[y * self.cols + x]
    }

    pub fn attr_at(&self, x: usize, y: usize) -> Attr {
        self.attr[y * self.cols + x]
    }

    pub fn take_dirty(&mut self) -> Vec<bool> {
        std::mem::replace(&mut self.dirty, vec![false; self.rows])
    }

    fn touch(&mut self, y: usize) {
        if y < self.rows {
            self.dirty[y] = true;
        }
    }

    fn put(&mut self, ch: char) {
        // New output takes the page back to live: on a storage tube you are
        // either looking at the page or at history, and the page wins.
        self.view = 0;
        // A full line does not wrap until the *next* character arrives - that is
        // what lets a program fill the last column and then move the cursor
        // without a spurious linefeed.  This has to live here rather than in the
        // printable-ASCII path, or a multi-byte character at the last column
        // overwrites the cell it should have wrapped past.
        if self.pending_wrap {
            self.pending_wrap = false;
            if self.autowrap {
                self.cx = 0;
                self.linefeed();
            }
        }
        let i = self.cy * self.cols + self.cx;
        self.ch[i] = ch;
        self.attr[i] = self.attr_now;
        self.touch(self.cy);
        if self.cx + 1 < self.cols {
            self.cx += 1;
        } else {
            self.pending_wrap = true;
        }
    }

    fn linefeed(&mut self) {
        if self.cy == self.scroll_bottom {
            self.scroll_up(1);
        } else if self.cy + 1 < self.rows {
            self.cy += 1;
        }
    }

    /// Scroll the region up by `n`, blanking the exposed lines.
    pub fn scroll_up(&mut self, n: usize) {
        // The image moves with the page: the rows that scroll off the top are
        // gone and only the lines that scrolled in are new.  A clear that arrives
        // with it wins - that is a replacement, not a move.
        self.change = match self.change {
            PageChange::Scroll(k) => PageChange::Scroll(k + n),
            PageChange::Wholesale => PageChange::Wholesale,
            PageChange::None => PageChange::Scroll(n),
        };
        for _ in 0..n {
            let (top, bottom) = (self.scroll_top, self.scroll_bottom);
            let width = self.cols;
            // only a whole-page scroll sends a line into the history; a scroll
            // region is a program's own furniture and must not
            if top == 0 && bottom == self.rows - 1 {
                let line: Vec<char> = self.ch[0..width].to_vec();
                let attrs: Vec<Attr> = self.attr[0..width].to_vec();
                self.history.push_back((line, attrs));
                while self.history.len() > HISTORY_MAX {
                    self.history.pop_front();
                }
                // the history grew, so if we are scrolled back, stay put rather
                // than sliding; callers reset the view when they write
                self.view = self.view.min(self.history.len());
            }
            for y in top..bottom {
                let (dst, src) = (y * width, (y + 1) * width);
                for x in 0..width {
                    self.ch[dst + x] = self.ch[src + x];
                    self.attr[dst + x] = self.attr[src + x];
                }
            }
            let last = bottom * width;
            for x in 0..width {
                self.ch[last + x] = ' ';
                self.attr[last + x] = Attr::Normal;
            }
            for y in top..=bottom {
                self.touch(y);
            }
        }
    }

    fn erase_chars(&mut self, n: usize) {
        for x in self.cx..(self.cx + n).min(self.cols) {
            let i = self.cy * self.cols + x;
            self.ch[i] = ' ';
            self.attr[i] = Attr::Normal;
        }
        self.touch(self.cy);
    }

    fn erase_line(&mut self, mode: u16) {
        let (from, to) = match mode {
            0 => (self.cx, self.cols),
            1 => (0, self.cx + 1),
            _ => (0, self.cols),
        };
        for x in from..to.min(self.cols) {
            let i = self.cy * self.cols + x;
            self.ch[i] = ' ';
            self.attr[i] = Attr::Normal;
        }
        self.touch(self.cy);
    }

    /// Resize the page, keeping the top-left of the old content and clamping
    /// the cursor.  Anchoring at the top is the predictable choice: the shell
    /// writes downward from row 0, so the rows that survive are the ones it
    /// wrote first, and the cursor is still inside them afterwards.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        let mut ch = vec![' '; cols * rows];
        let mut attr = vec![Attr::Normal; cols * rows];
        for y in 0..rows.min(self.rows) {
            for x in 0..cols.min(self.cols) {
                ch[y * cols + x] = self.ch[y * self.cols + x];
                attr[y * cols + x] = self.attr[y * self.cols + x];
            }
        }
        self.ch = ch;
        self.attr = attr;
        self.dirty = vec![true; rows];
        self.cols = cols;
        self.rows = rows;
        self.cx = self.cx.min(cols - 1);
        self.cy = self.cy.min(rows - 1);
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.saved.cx = self.saved.cx.min(cols - 1);
        self.saved.cy = self.saved.cy.min(rows - 1);
        self.pending_wrap = false;
        // the old stops belonged to the old width, so the grid is rebuilt at
        // the default; line doubling is keyed by row and starts over too
        self.tabs = (0..cols).map(|i| i % 8 == 0).collect();
        self.double_line = vec![(false, false); rows];
    }

    /// The whole screen as text, for verification in tests.
    #[cfg(test)]
    pub fn to_text(&self) -> String {
        let mut s = String::with_capacity((self.cols + 1) * self.rows);
        for y in 0..self.rows {
            let row: String = (0..self.cols).map(|x| self.char_at(x, y)).collect();
            s.push_str(row.trim_end());
            s.push('\n');
        }
        s
    }

    pub fn erase_display(&mut self, mode: u16) {
        // a clear takes the whole page away from whatever was drawn on it
        self.change = PageChange::Wholesale;
        if mode >= 2 {
            // a full clear discards the scrollback too, as it does on hardware
            self.history.clear();
            self.view = 0;
        }
        let all = mode >= 2;
        for y in 0..self.rows {
            let hit = all || (mode == 0 && y > self.cy) || (mode == 1 && y < self.cy);
            if hit {
                for x in 0..self.cols {
                    self.ch[y * self.cols + x] = ' ';
                    self.attr[y * self.cols + x] = Attr::Normal;
                }
                self.touch(y);
            }
        }
        if !all {
            self.erase_line(if mode == 1 { 1 } else { 0 });
        }
    }

    /// Set a tab stop at the cursor (`ESC H`).
    fn set_tab(&mut self) {
        if let Some(t) = self.tabs.get_mut(self.cx) {
            *t = true;
        }
    }

    /// The next tab stop strictly after `from`.  With none ahead a tab runs to
    /// the right margin, which is what the hardware does and what a program
    /// that cleared every stop is asking for.
    fn next_tab(&self, from: usize) -> usize {
        ((from + 1)..self.cols)
            .find(|&x| self.tabs[x])
            .unwrap_or(self.cols - 1)
    }

    /// The previous tab stop strictly before `from`, or column 0 when there is
    /// none - the left margin.
    fn prev_tab(&self, from: usize) -> usize {
        (0..from).rev().find(|&x| self.tabs[x]).unwrap_or(0)
    }

    /// `ESC # 3/4/5/6` on the cursor's line.
    fn set_line_double(&mut self, b: u8) {
        let v = match b {
            // DECDHL is double width and double height both; the frozen pair
            // cannot say which half of the line this is, so the top and the
            // bottom designators record the same thing.
            b'3' | b'4' => (true, true),
            b'5' => (false, false),
            b'6' => (true, false),
            _ => return,
        };
        if let Some(slot) = self.double_line.get_mut(self.cy) {
            *slot = v;
        }
    }

    /// Scroll the region down by `n`: the exact inverse of `scroll_up`, shifting
    /// content toward the bottom and blanking the exposed top line.  The line
    /// pushed off the bottom is discarded rather than saved, because the
    /// scrollback only ever grows at the bottom.
    pub fn scroll_down(&mut self, n: usize) {
        for _ in 0..n {
            let (top, bottom) = (self.scroll_top, self.scroll_bottom);
            let width = self.cols;
            for y in (top + 1..=bottom).rev() {
                let (dst, src) = (y * width, (y - 1) * width);
                for x in 0..width {
                    self.ch[dst + x] = self.ch[src + x];
                    self.attr[dst + x] = self.attr[src + x];
                }
            }
            for x in 0..width {
                self.ch[top * width + x] = ' ';
                self.attr[top * width + x] = Attr::Normal;
            }
            for y in top..=bottom {
                self.touch(y);
            }
        }
        // A downward move cannot ride the committed image - that only slides up
        // - and it undoes any upward scroll accumulated since the renderer last
        // asked, so the slide it was promised is taken back here rather than
        // leaving the image offset by a move that did not happen.
        self.change = match self.change {
            PageChange::Scroll(k) if k > n => PageChange::Scroll(k - n),
            PageChange::Scroll(_) => PageChange::None,
            other => other,
        };
    }

    /// `CSI ! p` (DECSTR): put the modes back where they started without
    /// touching the page.  A program sends it on exit, so a mode left behind
    /// would be inherited by the next one; the screen is left alone because the
    /// shell repaints its prompt from what it believes is already there, and
    /// erasing under it would make that repaint land on a blank page.
    fn soft_reset(&mut self) {
        self.autowrap = true;
        self.origin = false;
        self.cursor_visible = true;
        self.mouse_mode = 0;
        self.mouse_sgr = false;
        self.bracketed_paste_mode = false;
        self.attr_now = Attr::Normal;
        self.g0_acs = false;
        self.g1_acs = false;
        self.gl = 0;
        self.pending_wrap = false;
    }
}

/// Escape-sequence states.  Names follow the usual VT naming.
enum State {
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
    /// Collecting a charset designator - `ESC ( 0` and friends.  The payload is
    /// the slot it applies to: 0 = G0, 1 = G1, 2 = G2, 3 = G3.
    Charset(u8),
    /// The byte after `ESC #`: a line attribute (`# 3/4/5/6`).  It gets its own
    /// state so the digit is consumed rather than printed.
    Hash,
}

pub struct Parser {
    state: State,
    params: Vec<u16>,
    /// Digits of the parameter being accumulated.
    cur: Option<u16>,
    private: Option<char>,
    /// The intermediate byte of a CSI, e.g. the `!` of `CSI ! p` (DECSTR).
    /// Without it a soft reset is indistinguishable from a bare `CSI p`.
    intermediate: Option<u8>,
    utf8: Vec<u8>,
    /// True once the OSC command number and its ';' have been skipped.
    osc_past_prefix: bool,
    pub replies: Vec<u8>,
    pub dsr_count: usize,
    pub bell_count: usize,
}

impl Parser {
    pub fn new() -> Self {
        Parser {
            state: State::Ground,
            params: Vec::new(),
            cur: None,
            private: None,
            intermediate: None,
            utf8: Vec::new(),
            osc_past_prefix: false,
            replies: Vec::new(),
            dsr_count: 0,
            bell_count: 0,
        }
    }

    fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    /// Raw parameter value; absent means 0, which is the documented default for
    /// the erase commands.  Commands whose default is 1 apply `.max(1)`
    /// themselves.
    ///
    /// Folding 0 into the default here silently turned `CSI 0K` (erase from the
    /// cursor to the end of the line) into `CSI 1K` (erase from the start to the
    /// cursor), which the erase-line test caught.  It would have shown up as
    /// lines erased from the wrong end once text was on screen.
    fn raw(&self, i: usize) -> u16 {
        self.params.get(i).copied().unwrap_or(0)
    }

    pub fn feed(&mut self, bytes: &[u8], s: &mut Screen) -> Vec<u8> {
        for &b in bytes {
            match self.state {
                State::Ground => self.ground(b, s),
                State::Esc => self.esc(b, s),
                State::Csi => self.csi(b, s),
                State::Osc => match b {
                    0x07 => {
                        self.state = State::Ground;
                        self.bell_count += 1;
                    }
                    0x1b => self.state = State::OscEsc,
                    // the body starts with the command number and a ';' -
                    // OSC 0 and 2 are both "set window title"
                    b';' if !self.osc_past_prefix => self.osc_past_prefix = true,
                    _ => {
                        if self.osc_past_prefix && s.title.len() < 200 {
                            s.title.push(b as char);
                        }
                    }
                },
                State::OscEsc => {
                    self.state = State::Ground;
                }
                State::Charset(slot) => {
                    s.set_charset(slot, b);
                    self.state = State::Ground;
                }
                State::Hash => {
                    s.set_line_double(b);
                    self.state = State::Ground;
                }
            }
        }
        self.take_replies()
    }

    fn ground(&mut self, b: u8, s: &mut Screen) {
        match b {
            0x1b => self.state = State::Esc,
            0x0d => {
                s.cx = 0;
                s.pending_wrap = false;
            }
            0x0a | 0x0b | 0x0c => {
                s.linefeed();
                s.pending_wrap = false;
            }
            0x08 => {
                s.cx = s.cx.saturating_sub(1);
                s.pending_wrap = false;
            }
            0x09 => {
                // a tab runs to the next stop the program set, or along the
                // default every-eighth-column grid when it set none
                s.cx = s.next_tab(s.cx);
                s.pending_wrap = false;
            }
            0x07 => self.bell_count += 1,
            // SO and SI shift between G1 and G0.  Programs usually select the
            // special graphics set with a designator instead, but both work.
            0x0e => s.gl = 1,
            0x0f => s.gl = 0,
            0x00..=0x1f | 0x7f => {}
            _ => {
                // printable, possibly multi-byte UTF-8
                if b < 0x80 {
                    self.utf8.clear();
                    s.put(s.charset_char(b));
                } else {
                    self.utf8.push(b);
                    if let Some(ch) = decode_utf8(&self.utf8) {
                        self.utf8.clear();
                        s.put(ch);
                    } else if self.utf8.len() > 3 {
                        self.utf8.clear();
                    }
                }
            }
        }
    }

    fn esc(&mut self, b: u8, s: &mut Screen) {
        // The next state must be decided here and assigned once at the end: an
        // earlier version set `State::Csi` inside the match and then clobbered
        // it with an unconditional `State::Ground`, so every escape sequence was
        // printed as literal text ("93m", "?25l", "35;1H" all appeared on the
        // screen).  The corpus replay is what caught it.
        let next = match b {
            b'[' => {
                self.params.clear();
                self.cur = None;
                self.private = None;
                self.intermediate = None;
                State::Csi
            }
            b']' => {
                s.title.clear();
                self.osc_past_prefix = false;
                State::Osc
            }
            // Charset designators: `(` `)` `*` `+` name G0..G3, and the byte
            // that follows names the set (`0` the special graphics set with the
            // line-drawing characters, `B` ASCII).  Consuming them matters as
            // much as acting on them: before this, `ESC ( B` - which `ls`, `git`
            // and `less` all emit when they finish drawing - printed its
            // designator as the text "(B" in the middle of the output.
            b'(' => State::Charset(0),
            b')' => State::Charset(1),
            b'*' => State::Charset(2),
            b'+' => State::Charset(3),
            // `ESC # 3/4/5/6` marks the line's width/height.  The designator is
            // consumed; before this `ESC # 6` printed its "6" into the line.
            b'#' => State::Hash,
            // DECSET/DECRST's tab-set sibling: `ESC H` puts a tab stop at the
            // cursor, which is the only way a program can lay out its own
            // columns.
            b'H' => {
                s.set_tab();
                State::Ground
            }
            b'7' => {
                s.saved = SavedState {
                    cx: s.cx,
                    cy: s.cy,
                    attr: s.attr_now,
                    g0_acs: s.g0_acs,
                    g1_acs: s.g1_acs,
                    gl: s.gl,
                };
                State::Ground
            }
            b'8' => {
                s.cx = s.saved.cx;
                s.cy = s.saved.cy;
                s.attr_now = s.saved.attr;
                s.g0_acs = s.saved.g0_acs;
                s.g1_acs = s.saved.g1_acs;
                s.gl = s.saved.gl;
                State::Ground
            }
            b'D' => {
                s.linefeed();
                State::Ground
            }
            b'E' => {
                s.cx = 0;
                s.linefeed();
                State::Ground
            }
            b'M' => {
                if s.cy == s.scroll_top {
                    // reverse index at the top of the region: shift down, blank
                    // the first line
                    let width = s.cols;
                    for y in (s.scroll_top + 1..=s.scroll_bottom).rev() {
                        for x in 0..width {
                            s.ch[y * width + x] = s.ch[(y - 1) * width + x];
                        }
                        s.touch(y);
                    }
                    for x in 0..width {
                        s.ch[s.scroll_top * width + x] = ' ';
                    }
                    s.touch(s.scroll_top);
                } else {
                    s.cy = s.cy.saturating_sub(1);
                }
                State::Ground
            }
            b'c' => {
                *s = Screen::new(s.cols, s.rows);
                State::Ground
            }
            _ => State::Ground,
        };
        self.state = next;
    }

    fn csi(&mut self, b: u8, s: &mut Screen) {
        match b {
            b'0'..=b'9' => {
                let d = (b - b'0') as u16;
                self.cur = Some(self.cur.unwrap_or(0).saturating_mul(10).saturating_add(d));
            }
            b';' => {
                self.params.push(self.cur.unwrap_or(0));
                self.cur = None;
            }
            b'?' | b'>' | b'=' => self.private = Some(b as char),
            0x20..=0x2f => self.intermediate = Some(b), // intermediate bytes
            0x40..=0x7e => {
                if let Some(c) = self.cur.take() {
                    self.params.push(c);
                }
                self.dispatch(b, s);
                self.state = State::Ground;
            }
            0x1b => {
                self.state = State::Esc;
            }
            _ => {}
        }
    }

    fn dispatch(&mut self, fin: u8, s: &mut Screen) {
        // count-like parameters default to 1; the erase modes default to 0 and
        // are read raw (see `raw`)
        let a = self.raw(0);
        let n = a.max(1) as usize;
        match fin {
            // cursor position: rows and columns are one-based, and 0 or absent
            // both mean 1
            b'H' | b'f' => {
                let row = self.raw(0).max(1) as usize;
                let col = self.raw(1).max(1) as usize;
                // with origin mode on, row 1 is the top of the scroll region
                // and the cursor cannot leave it; the column always counts from
                // the left edge
                s.cy = if s.origin {
                    (s.scroll_top + row - 1).min(s.scroll_bottom)
                } else {
                    (row - 1).min(s.rows - 1)
                };
                s.cx = (col - 1).min(s.cols - 1);
                s.pending_wrap = false;
            }
            b'A' => {
                s.cy = s.cy.saturating_sub(n);
                s.pending_wrap = false;
            }
            b'B' => {
                s.cy = (s.cy + n).min(s.rows - 1);
                s.pending_wrap = false;
            }
            b'C' => {
                s.cx = (s.cx + n).min(s.cols - 1);
                s.pending_wrap = false;
            }
            b'D' => {
                s.cx = s.cx.saturating_sub(n);
                s.pending_wrap = false;
            }
            b'G' => s.cx = (self.raw(0).max(1) as usize - 1).min(s.cols - 1),
            b'd' => {
                let row = self.raw(0).max(1) as usize;
                s.cy = if s.origin {
                    (s.scroll_top + row - 1).min(s.scroll_bottom)
                } else {
                    (row - 1).min(s.rows - 1)
                };
            }
            b'I' => {
                // CHT: forward n tab stops
                for _ in 0..n {
                    s.cx = s.next_tab(s.cx);
                }
                s.pending_wrap = false;
            }
            b'Z' => {
                // CBT: back n tab stops
                for _ in 0..n {
                    s.cx = s.prev_tab(s.cx);
                }
                s.pending_wrap = false;
            }
            b'g' => match a {
                // 0: clear the stop under the cursor.  3: clear every stop.
                // (1 and 2 are the VT52 variants and are not used.)
                0 => s.tabs[s.cx] = false,
                3 => s.tabs.iter_mut().for_each(|t| *t = false),
                _ => {}
            },
            b'E' => {
                s.cx = 0;
                for _ in 0..n {
                    s.linefeed();
                }
            }
            b'F' => {
                s.cx = 0;
                s.cy = s.cy.saturating_sub(n);
            }
            b'J' => s.erase_display(a),
            b'K' => s.erase_line(a),
            b'X' => s.erase_chars(n),
            b'L' => {
                // insert lines at the cursor: shift the region down
                for _ in 0..n {
                    let width = s.cols;
                    let bottom = s.scroll_bottom;
                    for y in (s.cy + 1..=bottom).rev() {
                        for x in 0..width {
                            s.ch[y * width + x] = s.ch[(y - 1) * width + x];
                        }
                        s.touch(y);
                    }
                    for x in 0..width {
                        s.ch[s.cy * width + x] = ' ';
                    }
                    s.touch(s.cy);
                }
            }
            b'M' => {
                let saved_top = s.scroll_top;
                s.scroll_top = s.cy;
                s.scroll_up(n);
                s.scroll_top = saved_top;
            }
            b'P' => {
                // delete characters: pull the rest of the line left
                for _ in 0..n {
                    for x in s.cx..s.cols.saturating_sub(1) {
                        s.ch[s.cy * s.cols + x] = s.ch[s.cy * s.cols + x + 1];
                    }
                    s.ch[s.cy * s.cols + s.cols - 1] = ' ';
                }
                s.touch(s.cy);
            }
            b'@' => {
                for _ in 0..n {
                    for x in (s.cx + 1..s.cols).rev() {
                        s.ch[s.cy * s.cols + x] = s.ch[s.cy * s.cols + x - 1];
                    }
                    s.ch[s.cy * s.cols + s.cx] = ' ';
                }
                s.touch(s.cy);
            }
            b'S' => s.scroll_up(n),
            b'T' => s.scroll_down(n),
            b'r' => {
                if self.params.is_empty() {
                    // bare `CSI r` resets the region to the whole page
                    s.scroll_top = 0;
                    s.scroll_bottom = s.rows - 1;
                } else {
                    s.scroll_top = (self.raw(0).max(1) as usize - 1).min(s.rows - 1);
                    s.scroll_bottom =
                        (self.raw(1).max(1) as usize - 1).clamp(s.scroll_top, s.rows - 1);
                }
            }
            b'm' => {
                // SGR: map the common intensities onto the tube's brightness and
                // ignore the rest, including 38;5;n
                let mut i = 0;
                let n = self.params.len();
                if n == 0 {
                    s.attr_now = Attr::Normal;
                }
                while i < n {
                    match self.params[i] {
                        0 => s.attr_now = Attr::Normal,
                        1 => s.attr_now = Attr::Bright,
                        2 => s.attr_now = Attr::Dim,
                        // 4 (underline) and 5 (blink) are swallowed on purpose:
                        // a storage tube can hold neither, and this model has no
                        // state for them to turn off again.
                        4 | 5 => {}
                        // 7/27 toggle reverse video.  A storage tube cannot show
                        // it, but the attribute must be tracked so 27 clears the
                        // right thing and not a brightness the program set.
                        7 => s.attr_now = Attr::Reverse,
                        22 => s.attr_now = Attr::Normal,
                        // reverse off only, so a brightness set before it
                        // survives - the attribute is single-valued, so 27 has
                        // to be careful not to flatten everything
                        27 => {
                            if matches!(s.attr_now, Attr::Reverse) {
                                s.attr_now = Attr::Normal;
                            }
                        }
                        // 38/48 must come before the 30..=49 range that contains
                        // them, or this arm is unreachable
                        38 | 48 => i += 2, // 38;5;n or 38;2;r;g;b
                        30..=49 => {}
                        90..=97 => s.attr_now = Attr::Bright,
                        _ => {}
                    }
                    i += 1;
                }
            }
            b'h' | b'l' => {
                let set = fin == b'h';
                if self.private == Some('?') {
                    match a {
                        1 => s.app_cursor = set,
                        7 => s.autowrap = set,
                        25 => s.cursor_visible = set,
                        3 => {
                            // DECCOLM asks for 132 columns.  The page shape is
                            // the face's own, so the geometry deliberately does
                            // not change; consuming the sequence is the point,
                            // because a silent no-op that still printed its "3"
                            // would be worse than either.
                        }
                        6 => s.origin = set,
                        1000 => s.mouse_mode = if set { 1 } else { 0 },
                        1002 => s.mouse_mode = if set { 2 } else { 0 },
                        1003 => s.mouse_mode = if set { 3 } else { 0 },
                        1006 => s.mouse_sgr = set,
                        2004 => s.bracketed_paste_mode = set,
                        47 => {
                            // alternate screen, cursor untouched
                            if set {
                                s.enter_alt_screen(true, false);
                            } else {
                                s.leave_alt_screen(false);
                            }
                        }
                        1049 => {
                            // alternate screen with the cursor saved and restored
                            if set {
                                s.enter_alt_screen(true, true);
                            } else {
                                s.leave_alt_screen(true);
                            }
                        }
                        1047 => {
                            if set {
                                s.enter_alt_screen(true, false);
                            } else {
                                s.leave_alt_screen(false);
                            }
                        }
                        _ => {}
                    }
                }
            }
            b'n' => match a {
                6 => {
                    // report cursor position: one-based, from the REAL cursor
                    self.dsr_count += 1;
                    let reply = format!("\x1b[{};{}R", s.cy + 1, s.cx + 1);
                    self.replies.extend_from_slice(reply.as_bytes());
                }
                5 => self.replies.extend_from_slice(b"\x1b[0n"),
                _ => {}
            },
            b'c' => self.replies.extend_from_slice(b"\x1b[?1;2c"),
            // `CSI ! p` is DECSTR.  The `!` arrives as an intermediate byte; a
            // bare `CSI p` is not a command we answer, so the guard matters.
            b'p' if self.intermediate == Some(b'!') => s.soft_reset(),
            _ => {}
        }
    }
}

fn decode_utf8(bytes: &[u8]) -> Option<char> {
    let b0 = bytes[0];
    let need = if b0 >= 0xf0 {
        4
    } else if b0 >= 0xe0 {
        3
    } else if b0 >= 0xc0 {
        2
    } else {
        return None;
    };
    if bytes.len() < need {
        return None;
    }
    let mut cp = (b0 as u32) & (0x7f >> need);
    for &b in &bytes[1..need] {
        cp = (cp << 6) | ((b as u32) & 0x3f);
    }
    char::from_u32(cp)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed the whole stream and return the screen, the parser and the bytes the
    /// parser wants written back.  The replies must be taken from `feed`'s
    /// return value: it *drains* them, so `parser.replies` is empty afterwards
    /// and comparing that would pass vacuously.
    fn run(bytes: &[u8], cols: usize, rows: usize) -> (Screen, Parser, Vec<u8>) {
        let mut s = Screen::new(cols, rows);
        let mut p = Parser::new();
        let replies = p.feed(bytes, &mut s);
        (s, p, replies)
    }

    /// The row as displayed, i.e. through the current view, so a test can see
    /// the scrollback (view 0 is the live page and reads the same either way).
    fn row_text(s: &Screen, y: usize) -> String {
        (0..s.cols)
            .map(|x| s.visible_char(x, y))
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// The special graphics set: selected by a designator, consumed rather than
    /// printed, and mapped to the codes the renderer draws line art from.
    #[test]
    fn the_graphics_set_is_selected_and_consumed() {
        // `q` in the graphics set is a horizontal rule: code 0x80 + (0x71 - 0x5F)
        let (s, _, _) = run(b"\x1b(0q", 4, 1);
        assert_eq!(row_text(&s, 0), "\u{92}");
        // `ESC ( B` puts ASCII back, and neither designator reaches the screen.
        // Before this they printed as text, which is how a coloured `ls` grew a
        // stray "(B" in the middle of its output.
        let (s, _, _) = run(b"a\x1b(Bb", 4, 1);
        assert_eq!(row_text(&s, 0), "ab");
        let (s, _, _) = run(b"\x1b(0\x1b(Bl", 4, 1);
        assert_eq!(row_text(&s, 0), "l");
        // every byte in 0x5F..=0x7E is line art while the set is selected -
        // lower case included, which is why programs toggle it carefully - and
        // anything outside the range stays text
        let (s, _, _) = run(b"\x1b(0A", 4, 1);
        assert_eq!(row_text(&s, 0), "A");
    }

    /// DECSC/DECRC carry the attribute and the charset, not only the cursor.
    #[test]
    fn saving_restores_the_charset_and_the_attribute() {
        let (s, _, _) = run(b"\x1b[2m\x1b(0\x1b7\x1b[0m\x1b(B\x1b8q", 4, 1);
        assert_eq!(row_text(&s, 0), "\u{92}", "the graphics set came back");
        assert!(
            s.visible_attr(0, 0) == Attr::Dim,
            "and so did the attribute"
        );
    }

    /// DECCKM is a mode; the key path reads it.
    #[test]
    fn decckm_is_a_mode() {
        let (s, _, _) = run(b"\x1b[?1h", 4, 1);
        assert!(s.app_cursor);
        let (s, _, _) = run(b"\x1b[?1l", 4, 1);
        assert!(!s.app_cursor);
    }

    /// The box-drawing characters a program speaking UTF-8 sends have to arrive
    /// as themselves: they are what `vim`, `htop` and `tmux` use instead of the
    /// special graphics set, and before this they came out as `?`.
    #[test]
    fn a_utf8_box_character_survives_the_parser() {
        let (s, _, _) = run("─│┌┐".as_bytes(), 8, 1);
        assert_eq!(row_text(&s, 0), "─│┌┐");
    }

    #[test]
    fn cup_positions_the_cursor_one_based() {
        let (s, _, _) = run(b"\x1b[5;10H", 74, 35);
        assert_eq!((s.cx, s.cy), (9, 4));
        // missing or zero parameters mean one
        let (s, _, _) = run(b"\x1b[H", 74, 35);
        assert_eq!((s.cx, s.cy), (0, 0));
        let (s, _, _) = run(b"\x1b[0;0H", 74, 35);
        assert_eq!((s.cx, s.cy), (0, 0));
    }

    #[test]
    fn cup_clamps_to_the_page() {
        let (s, _, _) = run(b"\x1b[999;999H", 74, 35);
        assert_eq!((s.cx, s.cy), (73, 34));
    }

    #[test]
    fn ech_erases_exactly_n_characters() {
        let (s, _, _) = run(b"abcdefghij\x1b[1;3H\x1b[4X", 74, 35);
        assert_eq!(row_text(&s, 0), "ab    ghij");
    }

    #[test]
    fn text_wraps_on_the_character_after_a_full_line() {
        let mut s = Screen::new(10, 4);
        let mut p = Parser::new();
        p.feed(b"0123456789", &mut s);
        // the tenth character fills the line; the cursor has not moved yet
        assert_eq!(p.feed(b"", &mut s).len(), 0);
        assert_eq!(row_text(&s, 0), "0123456789");
        p.feed(b"A", &mut s);
        assert_eq!(row_text(&s, 0), "0123456789");
        assert_eq!(row_text(&s, 1), "A");
    }

    #[test]
    fn a_multibyte_character_at_the_last_column_wraps_too() {
        let mut s = Screen::new(4, 3);
        let mut p = Parser::new();
        p.feed(b"abcd", &mut s);
        p.feed("é".as_bytes(), &mut s); // 2 bytes, lands on row 1
        assert_eq!(row_text(&s, 0), "abcd");
        assert_eq!(row_text(&s, 1), "é");
    }

    #[test]
    fn erase_line_honours_its_mode() {
        // 0: cursor to end, inclusive.  1: start to cursor, inclusive - so with
        // the cursor on column 4 the a,b,c,d are cleared and four spaces remain.
        // 2: the whole line.
        let (s, _, _) = run(b"abcdef\x1b[1;4H\x1b[0K", 74, 35);
        assert_eq!(row_text(&s, 0), "abc");
        let (s, _, _) = run(b"abcdef\x1b[1;4H\x1b[1K", 74, 35);
        assert_eq!(row_text(&s, 0), "    ef");
        let (s, _, _) = run(b"abcdef\x1b[1;4H\x1b[2K", 74, 35);
        assert_eq!(row_text(&s, 0), "");
        // and the mode parameter must be read raw, not folded into a default
        let (s, _, _) = run(b"abcdef\x1b[1;4H\x1b[K", 74, 35);
        assert_eq!(row_text(&s, 0), "abc");
    }

    #[test]
    fn scrolling_at_the_bottom_moves_everything_up() {
        let mut s = Screen::new(6, 3);
        let mut p = Parser::new();
        p.feed(b"one\r\ntwo\r\nthree", &mut s);
        assert_eq!(row_text(&s, 2), "three");
        p.feed(b"\r\nfour", &mut s);
        assert_eq!(row_text(&s, 0), "two");
        assert_eq!(row_text(&s, 1), "three");
        assert_eq!(row_text(&s, 2), "four");
    }

    #[test]
    fn a_scroll_region_confines_scrolling() {
        let mut s = Screen::new(6, 6);
        let mut p = Parser::new();
        p.feed(b"aaa\r\nbbb\r\nccc\r\nddd\r\neee\r\nfff", &mut s);
        p.feed(b"\x1b[3;5r", &mut s); // region = rows 3..5
        p.feed(b"\x1b[5;1H\r\n", &mut s); // newline at the bottom of it
        assert_eq!(row_text(&s, 0), "aaa");
        assert_eq!(row_text(&s, 1), "bbb");
        assert_eq!(row_text(&s, 2), "ddd");
        assert_eq!(row_text(&s, 3), "eee");
        assert_eq!(row_text(&s, 5), "fff");
    }

    #[test]
    fn the_cursor_report_uses_the_real_position() {
        // The M0 spike answered with a fixed 1;1 and PSReadLine's redraw came out
        // as overlapping text, so this is the behaviour that matters most.
        let (_, p, replies) = run(b"\x1b[7;3H\x1b[6n", 74, 35);
        assert_eq!(p.dsr_count, 1);
        assert_eq!(replies, b"\x1b[7;3R");
    }

    #[test]
    fn sgr_maps_to_intensity() {
        let (s, _, _) = run(b"\x1b[1mX", 10, 2);
        assert!(matches!(s.attr_at(0, 0), Attr::Bright));
        let (s, _, _) = run(b"\x1b[90mX", 10, 2);
        assert!(matches!(s.attr_at(0, 0), Attr::Bright));
        let (s, _, _) = run(b"\x1b[2mX", 10, 2);
        assert!(matches!(s.attr_at(0, 0), Attr::Dim));
        let (s, _, _) = run(b"\x1b[1m\x1b[0mX", 10, 2);
        assert!(matches!(s.attr_at(0, 0), Attr::Normal));
        // 38;5;n must not be mistaken for a brightness
        let (s, _, _) = run(b"\x1b[38;5;9mX", 10, 2);
        assert!(matches!(s.attr_at(0, 0), Attr::Normal));
    }

    #[test]
    fn the_osc_title_loses_its_command_prefix() {
        let (s, _, _) = run(b"\x1b]0;hello world\x07", 10, 2);
        assert_eq!(s.title, "hello world");
    }

    #[test]
    fn scrolled_off_lines_can_be_read_back_until_something_writes() {
        let mut s = Screen::new(12, 3);
        let mut p = Parser::new();
        for i in 1..=5 {
            p.feed(format!("line{i}\r\n").as_bytes(), &mut s);
        }
        // Three rows, five lines written top down.  Every linefeed at the bottom
        // scrolls, so the live page ends up holding lines 4 and 5 with the
        // cursor's blank line under them, and lines 1-3 have gone into history.
        assert_eq!(row_text(&s, 0), "line4");
        assert_eq!(row_text(&s, 1), "line5");
        assert_eq!(row_text(&s, 2), "");
        assert_eq!(s.history_len(), 3);
        assert_eq!(s.view_offset(), 0);

        // Two lines back shows history[1..3] followed by the first live row.
        s.scroll_view(2);
        assert!(s.view_offset() > 0);
        assert_eq!(row_text(&s, 0), "line2");
        assert_eq!(row_text(&s, 1), "line3");
        assert_eq!(row_text(&s, 2), "line4");
        assert!(
            s.take_dirty().iter().all(|d| *d),
            "scrolling the view redraws"
        );

        s.scroll_view(99);
        assert_eq!(row_text(&s, 0), "line1", "the oldest kept line");
        assert_eq!(row_text(&s, 2), "line3");

        // New output takes the page back to live, and the line it scrolls away
        // becomes readable.
        p.feed(b"line6\r\n", &mut s);
        assert_eq!(s.view_offset(), 0, "writing returns to the live page");
        assert_eq!(s.history_len(), 4, "line4 scrolled off");
        s.scroll_view(2);
        assert_eq!(row_text(&s, 0), "line3");

        // a full clear drops the history, as the hardware's erase does
        p.feed(b"\x1b[2J", &mut s);
        assert_eq!(s.history_len(), 0);
        s.scroll_view(5);
        assert_eq!(s.view_offset(), 0, "nothing left to scroll back to");
    }

    #[test]
    fn the_alternate_screen_stashes_and_restores_the_primary_one() {
        let mut s = Screen::new(20, 4);
        let mut p = Parser::new();
        p.feed(b"primary text", &mut s);
        assert_eq!(row_text(&s, 0), "primary text");

        p.feed(b"\x1b[?1049h", &mut s); // a full-screen program takes over
        assert!(s.in_alt_screen());
        assert_eq!(row_text(&s, 0), "", "the alternate screen starts blank");
        p.feed(b"\x1b[2;1Hmenu", &mut s);
        assert_eq!(row_text(&s, 1), "menu");

        p.feed(b"\x1b[?1049l", &mut s); // and gives the page back
        assert!(!s.in_alt_screen());
        assert_eq!(row_text(&s, 0), "primary text");
        assert_eq!(row_text(&s, 1), "", "the program's output must not survive");
        assert!(s.take_dirty().iter().all(|d| *d), "the restore must redraw");

        // ?47h switches without moving the cursor, ?1048 only saves it
        p.feed(b"\x1b[1;5H\x1b[?47h", &mut s);
        assert!(s.in_alt_screen());
        assert_eq!(s.cx, 4, "?47h leaves the cursor alone");
        p.feed(b"\x1b[?47l", &mut s);
        assert_eq!(row_text(&s, 0), "primary text");
    }

    #[test]
    fn a_resize_blanks_the_page_but_keeps_the_scrollback() {
        let mut s = Screen::new(10, 3);
        let mut p = Parser::new();
        // enough lines to push some of them into the history
        p.feed(b"one\r\ntwo\r\nthree\r\nfour", &mut s);
        let history = s.history_len();
        assert!(history > 0, "the test needs something in the scrollback");

        s.clear_visible();
        for row in 0..3 {
            let line: String = (0..10).map(|c| s.visible_char(c, row)).collect();
            assert_eq!(line, " ".repeat(10), "the visible page must be blank");
        }
        assert_eq!(s.history_len(), history, "the scrollback must survive");
        assert_eq!((s.cx, s.cy), (0, 0), "and the cursor goes home");
        assert_eq!(
            s.take_page_change(),
            PageChange::Wholesale,
            "and the renderer is told to replace the page"
        );
    }

    /// A scroll says *how far* it moved, because the image moves with it; a
    /// clear says nothing but "replace".
    #[test]
    fn a_scroll_reports_how_far_the_page_moved() {
        let mut s = Screen::new(12, 3);
        let mut p = Parser::new();
        p.feed(b"one\r\ntwo\r\nthree", &mut s);
        assert_eq!(s.take_page_change(), PageChange::None, "three rows fit");

        // the next line scrolls the page once
        p.feed(b"\r\nfour", &mut s);
        assert_eq!(s.take_page_change(), PageChange::Scroll(1));
        assert_eq!(s.take_page_change(), PageChange::None, "and only once");

        // two more lines add up rather than being lost
        p.feed(b"\r\nfive\r\nsix", &mut s);
        assert_eq!(s.take_page_change(), PageChange::Scroll(2));

        // a clear is not a move, and it wins if the two arrive together
        p.feed(b"\r\nseven", &mut s);
        p.feed(b"\x1b[2J", &mut s);
        assert_eq!(s.take_page_change(), PageChange::Wholesale);
    }

    #[test]
    fn resizing_keeps_the_top_left_and_clamps_the_cursor() {
        let mut s = Screen::new(10, 4);
        let mut p = Parser::new();
        p.feed(b"one\r\ntwo\r\nthree\r\nfour", &mut s);
        p.feed(b"\x1b[4;9H", &mut s); // cursor to the bottom right
        assert_eq!((s.cx, s.cy), (8, 3));

        s.resize(20, 6); // grow: everything survives
        assert_eq!(row_text(&s, 0), "one");
        assert_eq!(row_text(&s, 3), "four");
        assert_eq!(
            (s.cx, s.cy),
            (8, 3),
            "the cursor should not move when growing"
        );
        assert!(
            s.take_dirty().iter().all(|d| *d),
            "a resize must redraw everything"
        );

        s.resize(10, 2); // shrink: keep the top, clamp
        assert_eq!(row_text(&s, 0), "one");
        assert_eq!(row_text(&s, 1), "two");
        assert_eq!(s.rows, 2);
        assert!(
            s.cy < 2,
            "the cursor must stay inside the page, got {}",
            s.cy
        );

        s.resize(5, 2);
        assert_eq!(s.cols, 5);
        assert!(s.cx < 5, "the cursor must stay inside the narrower page");
        assert_eq!(row_text(&s, 0), "one");
        // and the parser still drives it afterwards
        p.feed(b"\x1b[1;1HX", &mut s);
        assert_eq!(row_text(&s, 0), "Xne");
    }

    #[test]
    fn feeding_is_independent_of_chunk_boundaries() {
        // the whole point of a state machine: a sequence split across two reads
        let stream = b"ab\x1b[2;3Hcd\x1b[4X\x1b[1m\x1b]0;t\x07\x1b[?25lef\x1b[6n\x1b[10;1Hg";
        let (whole, pw, whole_replies) = run(stream, 20, 6);
        let mut chunked = Screen::new(20, 6);
        let mut p = Parser::new();
        let mut chunked_replies = Vec::new();
        for b in stream.iter() {
            chunked_replies.extend_from_slice(&p.feed(&[*b], &mut chunked));
        }
        assert_eq!(whole.to_text(), chunked.to_text());
        assert_eq!(pw.dsr_count, p.dsr_count);
        assert!(
            !whole_replies.is_empty(),
            "the stream must actually ask for a report"
        );
        assert_eq!(whole_replies, chunked_replies);
    }

    #[test]
    fn cursor_visibility_and_autowrap_are_private_modes() {
        let (s, _, _) = run(b"\x1b[?25l", 10, 2);
        assert!(!s.cursor_visible);
        let (s, _, _) = run(b"\x1b[?25h", 10, 2);
        assert!(s.cursor_visible);
        let (s, _, _) = run(b"\x1b[?7l", 10, 2);
        assert!(!s.autowrap);
    }

    #[test]
    fn a_full_utf8_character_across_three_reads_survives() {
        let mut s = Screen::new(10, 2);
        let mut p = Parser::new();
        for b in "→".as_bytes() {
            p.feed(&[*b], &mut s);
        }
        assert_eq!(row_text(&s, 0), "→");
    }

    /// SGR 7/27 toggle reverse video.  Nothing can be drawn on a storage tube,
    /// but if the attribute were not tracked, 27 could not clear it and a later
    /// drawing would inherit a state the program had finished with.
    #[test]
    fn sgr_reverse_video_is_its_own_attribute() {
        let (s, _, _) = run(b"\x1b[7mX", 10, 2);
        assert!(matches!(s.attr_at(0, 0), Attr::Reverse));
        let (s, _, _) = run(b"\x1b[7m\x1b[27mX", 10, 2);
        assert!(matches!(s.attr_at(0, 0), Attr::Normal));
        // 27 removes reverse and nothing else, so a brightness under it stays
        let (s, _, _) = run(b"\x1b[1m\x1b[27mX", 10, 2);
        assert!(matches!(s.attr_at(0, 0), Attr::Bright));
        // and 0 still clears everything, reverse included
        let (s, _, _) = run(b"\x1b[7m\x1b[0mX", 10, 2);
        assert!(matches!(s.attr_at(0, 0), Attr::Normal));
    }

    /// A program that lays out its own columns sets stops with `ESC H`; a TAB
    /// that always snapped to the eighth-column grid would ignore them and its
    /// table would drift.
    #[test]
    fn a_tab_stop_set_by_the_program_beats_the_default_grid() {
        let (s, _, _) = run(b"\x1b[1;4H\x1bH\x1b[1;1H\t", 20, 2);
        assert_eq!(s.cx, 3, "TAB lands on the stop set at column 3, not 8");
        // clearing the stop at column 8 sends the next TAB on to 16
        let (s, _, _) = run(b"\x1b[1;9H\x1b[g\x1b[1;1H\t", 20, 2);
        assert_eq!(s.cx, 16);
    }

    /// CHT (`CSI I`) and CBT (`CSI Z`) walk several stops at once, forward and
    /// back; before this the cursor stayed put and the program's alignment was
    /// lost.
    #[test]
    fn cht_and_cbt_walk_whole_stops() {
        let (s, _, _) = run(b"\x1b[1;1H\x1b[2I", 20, 2);
        assert_eq!(s.cx, 16, "two stops forward from column 0");
        let (s, _, _) = run(b"\x1b[1;17H\x1b[2Z", 20, 2);
        assert_eq!(s.cx, 0, "two stops back from column 16");
        let (s, _, _) = run(b"\x1b[1;17H\x1b[Z", 20, 2);
        assert_eq!(s.cx, 8, "one stop back");
    }

    /// DECOM (`CSI ?6 h`) makes CUP and VPA count rows from the scroll region's
    /// top.  Without it a full-screen program's `CUP 1;1` lands on the page top
    /// and draws over its own status line.
    #[test]
    fn origin_mode_makes_cup_relative_to_the_scroll_region() {
        let (s, _, _) = run(b"\x1b[3;6r\x1b[?6h\x1b[1;1H", 20, 8);
        assert_eq!(s.cy, 2, "row 1 of the region is page row 3");
        assert_eq!(s.cx, 0);
        let (s, _, _) = run(b"\x1b[3;6r\x1b[?6h\x1b[99;1H", 20, 8);
        assert_eq!(s.cy, 5, "clamped to the region's bottom, not the page's");
        let (s, _, _) = run(b"\x1b[3;6r\x1b[?6h\x1b[2d", 20, 8);
        assert_eq!(s.cy, 3, "VPA counts from the region too");
        // with origin off the same CUP is absolute again
        let (s, _, _) = run(b"\x1b[3;6r\x1b[?6l\x1b[1;1H", 20, 8);
        assert_eq!(s.cy, 0);
    }

    /// DECSTR (`CSI ! p`) is what a program sends on its way out.  Every mode
    /// it leaves behind would be inherited by the next one, and it must not
    /// touch the page, because the shell repaints from what it believes is
    /// already on screen.
    #[test]
    fn a_soft_reset_clears_the_modes_without_clearing_the_page() {
        let (s, _, _) = run(
            b"hello\x1b[?7l\x1b[?6h\x1b[?25l\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[2m\x1b(0\x1b[!p",
            20,
            4,
        );
        assert!(s.autowrap, "autowrap comes back on");
        assert!(!s.origin, "origin mode goes off");
        assert!(s.cursor_visible, "the cursor comes back");
        assert!(!s.wants_mouse(), "no tracking");
        assert!(!s.bracketed_paste(), "no bracketed paste");
        assert!(matches!(s.attr_now, Attr::Normal), "attributes normal");
        assert!(!s.g0_acs && !s.g1_acs, "both charsets are ASCII again");
        assert_eq!(s.gl, 0);
        assert_eq!(row_text(&s, 0), "hello", "and the page is untouched");
    }

    /// `CSI T` scrolls down, blanking the top line: the exact inverse of the
    /// scroll-up.  Before this it was swallowed, so a program that scrolled
    /// down (a menu repositioning, `less` at the top of a file) left the page
    /// where it was instead of opening the blank line it asked for.
    #[test]
    fn scroll_down_is_the_inverse_of_scroll_up() {
        let mut s = Screen::new(6, 3);
        let mut p = Parser::new();
        p.feed(b"one\r\ntwo\r\nthree", &mut s);
        p.feed(b"\x1b[T", &mut s);
        assert_eq!(row_text(&s, 0), "", "the exposed top line is blank");
        assert_eq!(row_text(&s, 1), "one");
        assert_eq!(row_text(&s, 2), "two", "everything moved down one");

        // the scroll region confines it, as it does for an upward scroll
        let mut s = Screen::new(6, 4);
        let mut p = Parser::new();
        p.feed(b"aaa\r\nbbb\r\nccc\r\nddd", &mut s);
        p.feed(b"\x1b[2;3r\x1b[T", &mut s);
        assert_eq!(row_text(&s, 0), "aaa", "outside the region is untouched");
        assert_eq!(row_text(&s, 1), "");
        assert_eq!(row_text(&s, 2), "bbb");
        assert_eq!(row_text(&s, 3), "ddd");
    }

    /// A scroll down undoes an accumulated scroll up rather than letting the
    /// renderer slide the committed image for a move that did not happen.
    #[test]
    fn a_scroll_down_takes_back_a_scroll_up() {
        let mut s = Screen::new(6, 3);
        let mut p = Parser::new();
        p.feed(b"one\r\ntwo\r\nthree\r\nfour", &mut s);
        assert_eq!(s.take_page_change(), PageChange::Scroll(1));
        p.feed(b"\x1b[T", &mut s);
        assert_eq!(s.take_page_change(), PageChange::None, "the move is undone");
        assert_eq!(row_text(&s, 0), "");
        assert_eq!(row_text(&s, 1), "two");
    }

    /// DECCOLM (`CSI ?3 h`) asks for 132 columns, but the page shape belongs to
    /// the face.  The geometry must not move - and the sequence must not print
    /// its "3", which is the bug this guards.
    #[test]
    fn the_132_column_mode_is_consumed_without_resizing() {
        let (s, _, _) = run(b"\x1b[?3h", 20, 4);
        assert_eq!((s.cols, s.rows), (20, 4), "the page shape is unchanged");
        assert_eq!(row_text(&s, 0), "", "the parameter must not print");
        let (s, _, _) = run(b"\x1b[?3l", 20, 4);
        assert_eq!((s.cols, s.rows), (20, 4));
        assert_eq!(row_text(&s, 0), "");
    }

    /// `ESC # 6` marks the line double width.  The renderer may draw nothing
    /// special, but the designator must never print as a "6" in the line.
    #[test]
    fn a_double_width_line_attribute_is_consumed_not_printed() {
        let (s, _, _) = run(b"ab\x1b#6cd", 10, 2);
        assert_eq!(row_text(&s, 0), "abcd");
        assert_eq!(s.line_double(0), (true, false), "double width recorded");
        // `ESC # 5` puts the line back to normal
        let (s, _, _) = run(b"\x1b#6\x1b#5", 10, 2);
        assert_eq!(s.line_double(0), (false, false));
        // and it is per line: only the cursor's line changes
        let (s, _, _) = run(b"\x1b#6\x1b[2;1H", 10, 2);
        assert_eq!(s.line_double(0), (true, false));
        assert_eq!(s.line_double(1), (false, false));
    }

    /// The mouse encoder's two formats.  X10 has no release of its own, so it
    /// says button 3, and biases every byte by 32; SGR keeps the real button
    /// and uses `m` for the release.  Getting either wrong sends the child a
    /// click it cannot place.
    #[test]
    fn the_mouse_reports_the_bytes_each_encoding_asks_for() {
        let (s, _, _) = run(b"\x1b[?1000h", 80, 24);
        assert!(s.wants_mouse());
        // left press at cell (3,7): Cb 0, Cx 4, Cy 8, each +32
        assert_eq!(
            s.mouse_report(0, 3, 7, false, 0).unwrap(),
            vec![0x1b, b'[', b'M', 32, 36, 40]
        );
        // a release in X10 is button 3 whatever button it was
        assert_eq!(
            s.mouse_report(0, 3, 7, true, 0).unwrap(),
            vec![0x1b, b'[', b'M', 35, 36, 40]
        );
        // the wheel is buttons 64 (up) and 65 (down)
        assert_eq!(
            s.mouse_report(0, 3, 7, false, 1).unwrap(),
            vec![0x1b, b'[', b'M', 96, 36, 40]
        );
        assert_eq!(
            s.mouse_report(0, 3, 7, false, -1).unwrap(),
            vec![0x1b, b'[', b'M', 97, 36, 40]
        );
        // SGR: `ESC [ < b ; x ; y M` for a press, `m` for a release
        let (s, _, _) = run(b"\x1b[?1000h\x1b[?1006h", 80, 24);
        assert_eq!(
            s.mouse_report(0, 3, 7, false, 0).unwrap(),
            b"\x1b[<0;4;8M".to_vec()
        );
        assert_eq!(
            s.mouse_report(0, 3, 7, true, 0).unwrap(),
            b"\x1b[<0;4;8m".to_vec()
        );
        assert_eq!(
            s.mouse_report(0, 3, 7, false, -1).unwrap(),
            b"\x1b[<65;4;8M".to_vec()
        );
    }

    /// Nothing is reported when no tracking mode is on, or after the one that
    /// was on is turned off; a terminal that kept sending mouse bytes would
    /// spray escapes into a shell that never asked for them.
    #[test]
    fn the_mouse_is_silent_until_a_mode_turns_it_on() {
        let (s, _, _) = run(b"", 80, 24);
        assert!(!s.wants_mouse());
        assert!(s.mouse_report(0, 3, 7, false, 0).is_none());
        assert!(s.mouse_report(0, 3, 7, false, 1).is_none());
        let (s, _, _) = run(b"\x1b[?1000h\x1b[?1000l", 80, 24);
        assert!(!s.wants_mouse());
        assert!(s.mouse_report(0, 3, 7, false, 0).is_none());
    }

    /// `?1000` reports presses and releases but not bare motion; `?1002` and
    /// `?1003` add it.  Motion reaches the encoder as button 3 with no release,
    /// the only shape the frozen signature can use for it.
    #[test]
    fn the_tracking_modes_gate_motion_differently() {
        let (s, _, _) = run(b"\x1b[?1000h", 80, 24);
        assert!(s.mouse_report(3, 5, 5, false, 0).is_none(), "no motion");
        assert!(s.mouse_report(0, 5, 5, false, 0).is_some(), "but a press");
        assert!(s.mouse_report(0, 5, 5, true, 0).is_some(), "and a release");
        let (s, _, _) = run(b"\x1b[?1002h", 80, 24);
        assert!(s.mouse_report(3, 5, 5, false, 0).is_some(), "?1002 adds it");
        let (s, _, _) = run(b"\x1b[?1003h", 80, 24);
        assert!(s.mouse_report(3, 5, 5, false, 0).is_some(), "?1003 too");
    }

    /// `?2004` is bracketed paste: the key path reads it to wrap a paste so a
    /// shell cannot mistake pasted newlines for typed commands.
    #[test]
    fn bracketed_paste_is_a_mode_that_round_trips() {
        let (s, _, _) = run(b"\x1b[?2004h", 10, 2);
        assert!(s.bracketed_paste());
        let (s, _, _) = run(b"\x1b[?2004l", 10, 2);
        assert!(!s.bracketed_paste());
        // and it is its own mode: it does not turn the mouse on
        let (s, _, _) = run(b"\x1b[?2004h", 10, 2);
        assert!(!s.wants_mouse());
    }

    /// Every charset slot's designator is consumed, not printed.  Only the
    /// graphics/ASCII distinction is acted on - the national sets differ by a
    /// glyph or two this face does not draw - but `ESC ( A` must not leave an
    /// "A" in the output.
    #[test]
    fn every_charset_designator_is_consumed_not_printed() {
        let (s, _, _) = run(b"\x1b(A\x1b)B\x1b*A\x1b+Bx", 10, 2);
        assert_eq!(row_text(&s, 0), "x");
    }
}
