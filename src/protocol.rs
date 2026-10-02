//! Decoder for the Tektronix 4010/4014 terminal graphics protocol, which is
//! what PLOT10 and friends wrote to the serial line.
//!
//! Stream summary (4010/4014 User Manuals; see also README.md):
//!
//! * printable ASCII is alpha (text) mode
//! * `GS` enters graph mode; the next coordinate positions the beam with the
//!   beam blanked (nothing stored), further coordinates draw stored vectors
//! * `FS` switches to point plot mode, `ESC FS` to point plot with an
//!   intensity/focus character in front of every point
//! * `RS` switches to incremental (single byte direction) plot mode
//! * `US`, `CR`, `ESC`, `LF` and a few other codes act in every mode
//! * a coordinate is transmitted high-Y, low-Y, high-X, low-X, two ASCII
//!   characters per value, 5 bits each.  Y-high and X-high use `0x20..0x3F`,
//!   Y-low uses `0x60..0x7F` and X-low uses `0x40..0x5F`, so the range a byte
//!   falls into tells the terminal which field it belongs to - that is how
//!   coordinates may omit fields that did not change.
//! * with the Enhanced Graphics Module (4014) a byte in the Y-low range
//!   between high-Y and low-Y carries two extra bits per axis, giving quarter
//!   dot resolution.  The 4010's 1024x1024 (780 visible) address space and the
//!   4014's 4096x4096 (3120 visible) therefore describe the same tube face.

/// Line styles selectable with ESC `..w (Enhanced Graphics Module).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LineStyle {
    #[default]
    Solid,
    Dotted,
    DotDash,
    ShortDash,
    LongDash,
}

/// Which populations of the target a write lands in.
///
/// The storage tube has three, and a write chooses among them: `stored` is the
/// bistable charge that stays until the screen is erased, `fresh` is the write
/// beam's own flash which decays, and `under` is a write that never stores.
/// Terminal mode adds `Settle`, which writes charge without lighting the beam,
/// so a screen can be committed without a visible re-write.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Write {
    /// Ordinary plot writing: charge stores and the beam flashes with it.
    #[default]
    Store,
    /// The beam flash only.  Content that is still moving is written this way,
    /// so it smears and fades instead of being burnt in.
    Flash,
    /// Charge only, no flash: how a settled screen is committed.
    Settle,
    /// ESC p..w: drawn without storing, so it survives only while refreshed.
    Through,
}

/// How a stroke is drawn: its dash pattern, its focus, and where it lands.
#[derive(Clone, Copy, Debug, Default)]
pub struct Style {
    pub line: LineStyle,
    /// ESC h..o: beam defocussed - wider, dimmer stroke.
    pub defocus: bool,
    pub write: Write,
}

/// Everything the terminal draws goes through this.
pub trait Sink {
    fn erase(&mut self);
    fn move_to(&mut self, x: i32, y: i32);
    fn vector(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, st: Style, energy: f32);
    fn point(&mut self, x: i32, y: i32, st: Style, energy: f32);
    fn character(&mut self, ch: u8, x: i32, y: i32, cell_w: i32, cell_h: i32, st: Style);
    fn bell(&mut self) {}
}

/// Sink that throws everything away, used to measure a stream.
#[derive(Default)]
pub struct NullSink;

impl Sink for NullSink {
    fn erase(&mut self) {}
    fn move_to(&mut self, _x: i32, _y: i32) {}
    fn vector(&mut self, _x0: i32, _y0: i32, _x1: i32, _y1: i32, _st: Style, _e: f32) {}
    fn point(&mut self, _x: i32, _y: i32, _st: Style, _e: f32) {}
    fn character(&mut self, _ch: u8, _x: i32, _y: i32, _cw: i32, _chh: i32, _st: Style) {}
}

/// Display variants the decoder can address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Device {
    Tek4010,
    Tek4014,
}

impl Device {
    /// Visible part of the target in dots.  Both tubes address more than they
    /// show: 1024x1024 with 780 lines visible on the 4010, 4096x4096 with 3120
    /// visible on the 4014.
    pub fn size(self) -> (i32, i32) {
        match self {
            Device::Tek4010 => (1024, 780),
            Device::Tek4014 => (4096, 3120),
        }
    }

    /// Dots per 4010 dot: the 4014 resolves four times finer.
    pub fn scale(self) -> i32 {
        match self {
            Device::Tek4010 => 1,
            Device::Tek4014 => 4,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Device::Tek4010 => "Tektronix 4010",
            Device::Tek4014 => "Tektronix 4014",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Alpha,
    /// Four coordinate bytes positioning the beam with the beam blanked.
    GraphMove,
    /// Four coordinate bytes ending in a stored vector (or a point).
    GraphVector,
    Escape,
    /// ANSI CSI sequence, ignored.
    Csi,
    /// ANSI OSC sequence, ignored.
    Osc,
    /// RS: one byte per direction step.
    Incremental,
    /// ESC FS: one intensity/focus character per point.
    Intensity,
}

/// Intensity/focus table for special point plot mode (ESC FS), from the
/// Tektronix 4014 User Manual page F-9.  Indexed by `ch - 32` for `ch` in
/// 32..=125 as stored here.
const INTENSITY: [u8; 94] = [
    14, 16, 17, 19, 20, 22, 23, 25, 28, 31, 34, 38, 41, 33, 47, 50, 56, 62, 69, 75, 81, 88, 94,
    100, 56, 62, 69, 75, 81, 88, 96, 100, 0, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4,
    5, 5, 5, 6, 6, 7, 8, 9, 10, 11, 12, 12, 13, 14, 16, 17, 19, 20, 22, 23, 25, 28, 31, 34, 38, 41,
    44, 47, 50, 56, 62, 69, 75, 81, 88, 94, 100, 56, 63, 69, 75, 81, 88,
];

/// Character cell counts for the four alpha font sizes (ESC 8/9/:/;).
const FONT_SIZES: [(i32, i32); 4] = [(74, 35), (81, 38), (121, 58), (133, 64)];

pub struct Decoder {
    mode: Mode,
    saved: Mode,
    step: u8,
    /// Beam / cursor position in target dots, origin bottom left.
    pub pos: (i32, i32),
    xh: i32,
    xl: i32,
    yh: i32,
    yl: i32,
    extra: u8,

    pub dev: Device,
    /// Enhanced Graphics Module: 12 bit addressing (quarter dot resolution).
    pub enhanced: bool,
    pub style: Style,
    /// Point plot (FS) mode: coordinates plot points instead of vectors.
    pub point_mode: bool,
    special: bool,
    intensity: f32,

    pub cell_w: i32,
    pub cell_h: i32,
    /// Multiplier on the character cell (1.0 = the 4010/4014 74x35 page; the
    /// 4052 in the source video prints larger text, about 1.5).
    pub text_scale: f32,
    left_margin: i32,
    pub autoclear: bool,
    pen_down: bool,

    /// Stream measurement, used for the report and to check decode health.
    pub vectors: u64,
    pub points: u64,
    pub chars: u64,
    pub extra_bits_used: bool,
    pub max_x: i32,
    pub max_y: i32,
    pub min_x: i32,
    pub min_y: i32,
    pub stray: u32,
    /// When set, every alpha character is recorded with its position.
    pub record_chars: bool,
    pub char_log: Vec<(u8, i32, i32, i32, i32)>,
}

impl Decoder {
    pub fn new(dev: Device, autoclear: bool) -> Self {
        let (w, h) = dev.size();
        let (cols, rows) = FONT_SIZES[0];
        Decoder {
            mode: Mode::Alpha,
            saved: Mode::Alpha,
            step: 1,
            pos: (0, h - h / FONT_SIZES[0].1),
            xh: 0,
            xl: 0,
            yh: 0,
            yl: 0,
            extra: 0,
            dev,
            enhanced: dev == Device::Tek4014,
            style: Style::default(),
            point_mode: false,
            special: false,
            intensity: 1.0,
            cell_w: (w as f32 / cols as f32) as i32,
            cell_h: (h as f32 / rows as f32) as i32,
            left_margin: 0,
            autoclear,
            pen_down: true,
            vectors: 0,
            points: 0,
            chars: 0,
            extra_bits_used: false,
            max_x: 0,
            max_y: 0,
            min_x: i32::MAX,
            min_y: i32::MAX,
            stray: 0,
            text_scale: 1.0,
            record_chars: false,
            char_log: Vec::new(),
        }
    }

    /// Ten bit value to target dot, adding the Enhanced Graphics Module's
    /// quarter dot bits when present.
    fn horizontal(&self, coarse: i32) -> i32 {
        if self.enhanced {
            (coarse << 2) | (self.extra as i32 & 3)
        } else {
            coarse
        }
    }

    fn vertical(&self, coarse: i32) -> i32 {
        if self.enhanced {
            (coarse << 2) | ((self.extra as i32 >> 2) & 3)
        } else {
            coarse
        }
    }

    pub fn feed(&mut self, bytes: &[u8], sink: &mut dyn Sink) {
        for &b in bytes {
            self.byte(b & 0x7F, sink);
        }
    }

    pub fn byte(&mut self, ch: u8, sink: &mut dyn Sink) {
        // Codes with the same meaning in every mode.
        match ch {
            7 => {
                sink.bell();
                return;
            }
            10 => {
                self.pos.1 -= self.cell_h;
                self.check_limits(sink, self.mode == Mode::Alpha);
                return;
            }
            27 => {
                self.saved = self.mode;
                self.mode = Mode::Escape;
                return;
            }
            31 => {
                self.to_alpha();
                return;
            }
            13 => {
                if self.mode != Mode::Escape {
                    self.to_alpha();
                    self.pos.0 = self.left_margin;
                    return;
                }
            }
            29 => {
                self.mode = Mode::GraphMove;
                self.point_mode = false;
                self.special = false;
                self.step = 1;
                return;
            }
            28 => {
                if self.mode != Mode::Escape {
                    self.point_mode = true;
                    self.special = false;
                    self.mode = Mode::GraphVector;
                    self.step = 1;
                    return;
                }
            }
            30 => {
                self.pen_down = true;
                self.mode = Mode::Incremental;
                return;
            }
            _ => {}
        }

        match self.mode {
            Mode::Alpha => self.alpha(ch, sink),
            Mode::GraphMove | Mode::GraphVector => self.coordinate(ch, sink),
            Mode::Escape => self.escape(ch, sink),
            Mode::Csi => {
                if (0x40..=0x7E).contains(&ch) {
                    self.mode = Mode::Alpha;
                }
            }
            Mode::Osc => {
                if ch == 7 {
                    self.mode = Mode::Alpha;
                }
            }
            Mode::Incremental => self.incremental(ch, sink),
            Mode::Intensity => self.intensity_char(ch),
        }
    }

    fn to_alpha(&mut self) {
        self.mode = Mode::Alpha;
        self.point_mode = false;
        self.special = false;
    }

    fn alpha(&mut self, ch: u8, sink: &mut dyn Sink) {
        match ch {
            8 => {
                self.pos.0 -= self.cell_w;
                self.check_limits(sink, true);
            }
            9 => {
                // advance to the next 8 character tab stop
                self.pos.0 = (self.pos.0 / (8 * self.cell_w) + 1) * 8 * self.cell_w;
                self.check_limits(sink, true);
            }
            11 => {
                self.pos.1 += self.cell_h;
                self.check_limits(sink, true);
            }
            32..=126 => {
                self.check_limits(sink, true);
                if self.record_chars {
                    self.char_log
                        .push((ch, self.pos.0, self.pos.1, self.cell_w, self.cell_h));
                }
                self.chars += 1;
                sink.character(
                    ch,
                    self.pos.0,
                    self.pos.1,
                    self.cell_w,
                    self.cell_h,
                    self.style,
                );
                self.pos.0 += self.cell_w;
            }
            _ => {}
        }
    }

    /// Text page overflow handling: wrap right to next line, bottom to the
    /// next page (the other margin) and erase the target on a real terminal.
    ///
    /// `text_page` is false while graphics are being drawn: there the line feed
    /// is just a beam move (point plot files use it as a row separator) and
    /// must not roll the alpha page over.
    fn check_limits(&mut self, sink: &mut dyn Sink, text_page: bool) {
        let (w, h) = self.dev.size();
        if self.pos.0 < 0 {
            self.pos.0 = 0;
        }
        if self.pos.0 > w - self.cell_w {
            self.pos.0 = self.left_margin;
            self.pos.1 -= self.cell_h;
        }
        if self.pos.1 < 4 {
            if text_page {
                self.pos.1 = h - self.cell_h;
                self.left_margin = if self.left_margin == 0 { w / 2 } else { 0 };
                if self.autoclear {
                    sink.erase();
                    self.left_margin = 0;
                    self.pos.1 = h - self.cell_h;
                }
                if self.pos.0 < self.left_margin {
                    self.pos.0 = self.left_margin;
                }
            } else {
                self.pos.1 = 0;
            }
        }
        if self.pos.1 > h - self.cell_h {
            self.pos.1 = h - self.cell_h;
        }
    }

    /// Coordinate bytes: the ASCII range of each byte tells the terminal which
    /// field it is, so unchanged fields may simply be left out.
    fn coordinate(&mut self, ch: u8, sink: &mut dyn Sink) {
        let tag = (ch >> 5) & 3;
        if tag == 0 {
            self.stray += 1;
            return;
        }
        let mut step = self.step;
        if step == 1 && tag != 1 {
            step = 2;
        }
        if step == 3 && tag == 3 {
            // Enhanced Graphics Module: the extra byte that sits between
            // high-Y and low-Y and carries the quarter dot bits.
            step = 2;
            self.extra = self.yl as u8;
            if self.yl & 15 != 0 {
                self.extra_bits_used = true;
            }
        }
        if step == 2 && tag != 3 {
            step = 3;
        }
        if step == 3 && tag != 1 {
            step = 4;
        }
        self.step = step;
        match step {
            1 => {
                self.yh = 32 * (ch & 31) as i32;
                self.step = 2;
            }
            2 => {
                self.yl = (ch & 31) as i32;
                self.step = 3;
            }
            3 => {
                self.xh = 32 * (ch & 31) as i32;
                self.step = 4;
            }
            _ => {
                self.xl = (ch & 31) as i32;
                self.complete(sink);
            }
        }
    }

    fn complete(&mut self, sink: &mut dyn Sink) {
        let coarse_x = self.xh + self.xl;
        let coarse_y = self.yh + self.yl;
        self.max_x = self.max_x.max(coarse_x);
        self.max_y = self.max_y.max(coarse_y);
        self.min_x = self.min_x.min(coarse_x);
        self.min_y = self.min_y.min(coarse_y);
        let end = (self.horizontal(coarse_x), self.vertical(coarse_y));
        let was_move = self.mode == Mode::GraphMove;
        let (x0, y0) = self.pos;
        self.pos = end;
        if was_move {
            sink.move_to(end.0, end.1);
            self.mode = Mode::GraphVector;
        } else if self.point_mode {
            self.points += 1;
            sink.point(end.0, end.1, self.style, self.intensity);
            // in special point plot mode an intensity character follows
            self.mode = if self.special {
                Mode::Intensity
            } else {
                Mode::GraphVector
            };
        } else {
            self.vectors += 1;
            sink.vector(x0, y0, end.0, end.1, self.style, 1.0);
            self.mode = Mode::GraphVector;
        }
        self.step = 1;
    }

    fn intensity_char(&mut self, ch: u8) {
        if (32..=125).contains(&ch) {
            let v = INTENSITY[(ch - 32) as usize] as f32 / 100.0;
            self.intensity = v.max(0.02);
            self.style.defocus = (ch >> 5) == 1;
        }
        self.mode = Mode::GraphVector;
        self.step = 1;
    }

    /// RS incremental plot: one byte per step, four direction flags in the low
    /// nibble, `SP` lifts and `P` lowers the pen.
    fn incremental(&mut self, ch: u8, sink: &mut dyn Sink) {
        let (w, h) = self.dev.size();
        match ch {
            32 => self.pen_down = false,
            80 => self.pen_down = true,
            _ if (ch & 0x70) == 0x40 => {
                let (mut x, mut y) = self.pos;
                if ch & 0x01 != 0 {
                    x += 1;
                }
                if ch & 0x02 != 0 {
                    x -= 1;
                }
                if ch & 0x04 != 0 {
                    y += 1;
                }
                if ch & 0x08 != 0 {
                    y -= 1;
                }
                x = x.clamp(0, w - 1);
                y = y.clamp(0, h - 1);
                if self.pen_down {
                    self.vectors += 1;
                    sink.vector(self.pos.0, self.pos.1, x, y, self.style, 1.0);
                }
                self.pos = (x, y);
            }
            _ => self.stray += 1,
        }
    }

    fn escape(&mut self, ch: u8, sink: &mut dyn Sink) {
        let (_, h) = self.dev.size();
        let text_page = self.saved == Mode::Alpha;
        let next = match ch {
            8 => {
                self.pos.0 -= self.cell_w;
                self.check_limits(sink, text_page);
                Mode::Alpha
            }
            9 => {
                self.pos.0 = (self.pos.0 / (8 * self.cell_w) + 1) * 8 * self.cell_w;
                self.check_limits(sink, text_page);
                Mode::Alpha
            }
            11 => {
                self.pos.1 += self.cell_h;
                self.check_limits(sink, text_page);
                Mode::Alpha
            }
            12 => {
                // form feed: erase the target and restore the default font
                sink.erase();
                self.set_font(0);
                self.pos = (0, h - self.cell_h);
                self.left_margin = 0;
                Mode::Alpha
            }
            28 => {
                // special point plot: coordinate, then intensity character
                self.point_mode = true;
                self.special = true;
                Mode::Intensity
            }
            b'8' => {
                self.set_font(0);
                Mode::Alpha
            }
            b'9' => {
                self.set_font(1);
                Mode::Alpha
            }
            b':' => {
                self.set_font(2);
                Mode::Alpha
            }
            b';' => {
                self.set_font(3);
                Mode::Alpha
            }
            b'[' => Mode::Csi,
            b']' => Mode::Osc,
            b'`'..=b'w' => {
                // three rows (normal / defocussed / write-through) of eight
                // line types each
                let idx = (ch - b'`') as usize;
                let row = idx / 8;
                let col = idx % 8;
                self.style.line = match col {
                    1 => LineStyle::Dotted,
                    2 => LineStyle::DotDash,
                    3 => LineStyle::ShortDash,
                    4 => LineStyle::LongDash,
                    _ => LineStyle::Solid,
                };
                self.style.write = if row == 2 {
                    Write::Through
                } else {
                    Write::Store
                };
                self.style.defocus = row == 1;
                self.saved
            }
            _ => Mode::Alpha,
        };
        self.mode = next;
    }

    fn set_font(&mut self, idx: usize) {
        let (w, h) = self.dev.size();
        let (cols, rows) = FONT_SIZES[idx];
        let s = self.text_scale.max(0.1);
        self.cell_w = ((w as f32 / cols as f32 * s) as i32).max(1);
        self.cell_h = ((h as f32 / rows as f32 * s) as i32).max(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records the drawing calls the decoder produces.
    #[derive(Default)]
    pub struct Recorder {
        pub ops: Vec<String>,
    }

    impl Sink for Recorder {
        fn erase(&mut self) {
            self.ops.push("erase".into());
        }
        fn move_to(&mut self, x: i32, y: i32) {
            self.ops.push(format!("move {} {}", x, y));
        }
        fn vector(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, _st: Style, _e: f32) {
            self.ops.push(format!("line {} {} {} {}", x0, y0, x1, y1));
        }
        fn point(&mut self, x: i32, y: i32, _st: Style, e: f32) {
            self.ops.push(format!("point {} {} {:.2}", x, y, e));
        }
        fn character(&mut self, ch: u8, x: i32, y: i32, cw: i32, _chh: i32, _st: Style) {
            self.ops
                .push(format!("char {} {} {} {}", ch as char, x, y, cw));
        }
    }

    fn run(dev: Device, bytes: &[u8]) -> Recorder {
        let mut d = Decoder::new(dev, true);
        let mut r = Recorder::default();
        d.feed(bytes, &mut r);
        r
    }

    #[test]
    fn coordinate_encoding_matches_the_manual() {
        // (23,142) is "$n W" in high-Y, low-Y, high-X, low-X order.
        let mut dev_bytes = vec![0x1d];
        dev_bytes.extend_from_slice(b"$n W");
        let r = run(Device::Tek4010, &dev_bytes);
        assert_eq!(r.ops, vec!["move 23 142".to_string()]);
    }

    #[test]
    fn first_coordinate_moves_and_the_next_draws() {
        // GS + (0,0), then a second coordinate one dot to the right
        let bytes = [
            0x1d, 0x20, 0x60, 0x20, 0x40, // move to 0,0
            0x20, 0x60, 0x20, 0x41, // vector to 1,0
            0x1f,
        ];
        let r = run(Device::Tek4010, &bytes);
        assert_eq!(
            r.ops,
            vec!["move 0 0".to_string(), "line 0 0 1 0".to_string()]
        );
    }

    #[test]
    fn form_feed_erases_and_resets_the_cursor() {
        let r = run(Device::Tek4010, &[0x1b, 0x0c, b'A']);
        assert_eq!(r.ops[0], "erase");
        // default 74x35 cell on a 1024x780 target is 13x22 dots
        assert_eq!(r.ops[1], "char A 0 758 13");
    }

    #[test]
    fn quad_dot_addressing_takes_the_extra_byte() {
        // 4014: coarse X = 1, coarse Y = 20, extra byte 9 gives xb = 1, yb = 2
        let bytes = [0x1d, 0x20, b'i', b't', 0x20, b'A'];
        let r = run(Device::Tek4014, &bytes);
        assert_eq!(r.ops, vec!["move 5 82".to_string()]);
        // the same stream on a 4010 ignores the extra byte
        let r4010 = run(Device::Tek4010, &bytes);
        assert_eq!(r4010.ops, vec!["move 1 20".to_string()]);
    }

    #[test]
    fn escape_line_style_table() {
        let mut d = Decoder::new(Device::Tek4010, true);
        let mut r = Recorder::default();
        d.feed(&[0x1b, b'a'], &mut r); // dotted, stored
        assert_eq!(d.style.line, LineStyle::Dotted);
        assert_eq!(d.style.write, Write::Store);
        d.feed(&[0x1b, b'r'], &mut r); // write-through, dot-dash
        assert_eq!(d.style.line, LineStyle::DotDash);
        assert_eq!(d.style.write, Write::Through);
        d.feed(&[0x1b, b'i'], &mut r); // defocussed, dotted
        assert!(d.style.defocus);
        assert_eq!(d.style.write, Write::Store);
    }

    #[test]
    fn point_plot_mode_plots_points_not_vectors() {
        // FS then a coordinate
        let r = run(Device::Tek4010, &[0x1c, 0x20, 0x60, 0x20, 0x41]);
        assert_eq!(r.ops, vec!["point 1 0 1.00".to_string()]);
    }

    #[test]
    fn special_point_plot_reads_the_intensity_character() {
        // ESC FS, then intensity character '5' (index 21 -> 88%) followed by
        // the coordinate, twice
        let bytes = [
            0x1b, 0x1c, b'5', 0x20, 0x60, 0x20, 0x41, b'5', 0x20, 0x60, 0x20, 0x61,
        ];
        let r = run(Device::Tek4010, &bytes);
        assert_eq!(
            r.ops,
            vec!["point 1 0 0.88".to_string(), "point 1 0 0.88".to_string()]
        );
    }

    /// End to end on a real demo file: the Snoopy as the Red Baron plot that
    /// the Tektronix 4052 demo plays must land on the target and, once its
    /// flash has settled, be a saturated green stored image.
    #[test]
    fn sample_plot_draws_and_stores() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/samples/05_snoopy.plt");
        let Ok(data) = std::fs::read(path) else {
            return; // sample collection is optional
        };
        let mut t = crate::tube::Tube::new(4096, 3120);
        let mut d = Decoder::new(Device::Tek4014, true);
        d.feed(&data, &mut t);

        assert!(
            d.vectors > 300,
            "expected the snoopy outline, got {}",
            d.vectors
        );
        assert_eq!(d.stray, 0, "stream should decode without stray bytes");
        // the outline lives around x 200..800, y 150..750 of the 4010 grid
        assert!(d.max_x > 700 && d.max_x <= 1023, "max x {}", d.max_x);
        assert!(d.max_y > 600 && d.max_y <= 1023, "max y {}", d.max_y);

        t.decay(3.0);
        let (_, stored) = t.stats();
        assert!(stored > 0.005, "only {:.4} of the target stored", stored);

        // and the stored charge is that medium acid green, not a white flash
        let lit = (0..t.len())
            .map(|i| (i % t.w as usize, i / t.w as usize))
            .find_map(|(x, y)| {
                let c = t.rgb_at(x as i32, y as i32)?;
                (c.1 > 150).then_some(c)
            })
            .expect("the outline should leave stored charge behind");
        assert!(
            lit.0 < 120 && lit.2 < 120,
            "stored image should be green, got {:?}",
            lit
        );
    }

    #[test]
    fn incremental_plot_moves_one_dot_per_byte() {
        let mut d = Decoder::new(Device::Tek4010, true);
        let mut r = Recorder::default();
        // park the beam at 0,0
        d.feed(&[0x1d, 0x20, 0x60, 0x20, 0x40], &mut r);
        r.ops.clear();
        // RS then up, right, pen lift (which moves without drawing), up again
        d.feed(&[0x1e, 0x44, 0x41, 0x20, 0x44], &mut r);
        assert_eq!(
            r.ops,
            vec!["line 0 0 0 1".to_string(), "line 0 1 1 1".to_string()]
        );
    }
}
