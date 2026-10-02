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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Attr {
    /// SGR colour index, mapped to a brightness for a monochrome green tube.
    Dim,
    Normal,
    Bright,
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
    pub title: String,
    scroll_top: usize,
    scroll_bottom: usize,
    saved: (usize, usize),
    attr_now: Attr,
    /// Set when the cursor is in the pending-wrap state after filling the line.
    pending_wrap: bool,
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
    /// Set when the page moved as a whole: a scroll, an erase of the display, or
    /// an alt-screen switch.  Taken by the renderer, which cannot trace one of
    /// those - every cell has changed, so the image is replaced instead.
    page_reset: bool,
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
            title: String::new(),
            scroll_top: 0,
            scroll_bottom: rows - 1,
            saved: (0, 0),
            attr_now: Attr::Normal,
            pending_wrap: false,
            history: std::collections::VecDeque::new(),
            view: 0,
            alt_saved: None,
            page_reset: false,
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
        self.page_reset = true;
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
            self.page_reset = true;
            self.dirty.iter_mut().for_each(|d| *d = true);
        }
    }

    /// Has the page moved as a whole since this was last asked?  Taken rather
    /// than read: one scroll is one replacement.
    pub fn take_page_reset(&mut self) -> bool {
        std::mem::take(&mut self.page_reset)
    }

    pub fn in_alt_screen(&self) -> bool {
        self.alt_saved.is_some()
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
        self.page_reset = true;
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
        // whatever the pen has drawn now describes the wrong rows
        self.page_reset = true;
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
        self.saved = (self.saved.0.min(cols - 1), self.saved.1.min(rows - 1));
        self.pending_wrap = false;
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
        self.page_reset = true;
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
}

/// Escape-sequence states.  Names follow the usual VT naming.
enum State {
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
}

pub struct Parser {
    state: State,
    params: Vec<u16>,
    /// Digits of the parameter being accumulated.
    cur: Option<u16>,
    private: Option<char>,
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
                s.cx = ((s.cx / 8) + 1) * 8;
                if s.cx >= s.cols {
                    s.cx = s.cols - 1;
                }
            }
            0x07 => self.bell_count += 1,
            0x00..=0x1f | 0x7f => {}
            _ => {
                // printable, possibly multi-byte UTF-8
                if b < 0x80 {
                    self.utf8.clear();
                    s.put(b as char);
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
                State::Csi
            }
            b']' => {
                s.title.clear();
                self.osc_past_prefix = false;
                State::Osc
            }
            b'7' => {
                s.saved = (s.cx, s.cy);
                State::Ground
            }
            b'8' => {
                s.cx = s.saved.0;
                s.cy = s.saved.1;
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
            0x20..=0x2f => {} // intermediate bytes
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
                s.cy = (row - 1).min(s.rows - 1);
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
            b'd' => s.cy = (self.raw(0).max(1) as usize - 1).min(s.rows - 1),
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
            b'T' => {}
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
                        22 => s.attr_now = Attr::Normal,
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
                        7 => s.autowrap = set,
                        25 => s.cursor_visible = set,
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
        assert!(
            s.take_page_reset(),
            "and the renderer is told to replace the page"
        );
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
}
