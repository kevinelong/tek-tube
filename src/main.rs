//! tek-tube - a Tektronix 4010/4014 storage tube display emulator.
//!
//! Plays back plot files (the Tektronix "PLOT10" / 4010 graphics protocol that
//! the 1970s mainframe and minicomputer plotting packages sent down a serial
//! line) onto a simulated direct-view bistable storage tube: vectors are
//! written by a hot write beam that flashes almost white and settles into the
//! acid green stored image, exactly like the real terminal.

mod font;
mod protocol;
mod terminal;
mod tube;
mod vt;

use font::Face;
use protocol::{Decoder, Device, Write};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use tube::Tube;

struct Options {
    files: Vec<PathBuf>,
    device: Device,
    baud: f32,
    /// Whether `--baud` was actually given.  Plot playback has always had a
    /// default line speed; terminal mode only paces the link when asked, so the
    /// two meanings do not collide.
    baud_given: bool,
    speed: f32,
    glow: f32,
    beam: f32,
    settle: f32,
    png: Option<PathBuf>,
    png_at: Option<f32>,
    png_seq: Option<String>,
    frames: usize,
    frame_dt: f32,
    zoom: i32,
    shrink: Option<i32>,
    autoclear: bool,
    enhanced: bool,
    // None means "use the phosphor default", so the defaults live in one place
    store_gain: Option<f32>,
    fresh_gain: Option<f32>,
    sat: Option<f32>,
    ambient: Option<f32>,
    mottle: Option<f32>,
    bloom: Option<f32>,
    bloom_wide: Option<f32>,
    camera: bool,
    font: Face,
    text_scale: f32,
    stats: bool,
    dump_chars: bool,
    probe: Option<(i32, i32)>,
    exit_after: Option<f32>,
    /// Terminal mode: threads a render pass may use.  None asks the machine.
    threads: Option<usize>,
    /// Start in a window with a title bar instead of filling the screen.
    /// Fullscreen is the default - the tube is the whole point and a caption bar
    /// is furniture - and F11 toggles either way while running.
    windowed: bool,
    /// Run a shell under a pseudoconsole instead of playing plot files.
    shell: Option<String>,
    /// Terminal mode: how long the transient image lingers, in seconds.  This
    /// is the length of the smear; 0 makes it a plain storage tube.
    trail: Option<f32>,
    /// Terminal mode: cell size as a multiple of the 4010's own cell.
    cell: Option<f32>,
    /// Terminal mode: draw the screen with the beam at this many target dots
    /// per second, so the writing can be watched stroke by stroke.
    write_rate: Option<f32>,
    /// Terminal mode: seconds the page a scroll replaced keeps glowing where it
    /// was.  Zero - the default - has no ghost at all, which is what a storage
    /// tube does: its charge does not fade.
    ghost: Option<f32>,
    /// Terminal mode: rows a second the display slides a scroll at.  Zero makes
    /// a scroll jump, which is what a raster terminal does.
    slide: Option<f32>,
    /// The raster scan's own light structure: 0 is a flat face, and this is how
    /// deep the gaps between scan lines go.  Off by default, because a vector
    /// display has no scan lines at all.
    scan: Option<f32>,
    /// Replay a captured terminal byte stream instead of a plot file.  A
    /// diagnostic for the terminal renderer, which is otherwise only reachable
    /// by typing into a window.
    vt_replay: Option<PathBuf>,
    quiet: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            files: Vec::new(),
            device: Device::Tek4014,
            baud: 19200.0,
            baud_given: false,
            speed: 1.0,
            glow: 0.45,
            beam: 0.5,
            settle: 2.0,
            png: None,
            png_at: None,
            png_seq: None,
            frames: 8,
            frame_dt: 0.5,
            zoom: 1,
            shrink: None,
            autoclear: true,
            enhanced: true,
            store_gain: None,
            fresh_gain: None,
            sat: None,
            ambient: None,
            mottle: None,
            bloom: None,
            bloom_wide: None,
            camera: false,
            font: Face::Simplex,
            text_scale: 1.0,
            stats: false,
            dump_chars: false,
            probe: None,
            exit_after: None,
            threads: None,
            windowed: false,
            shell: None,
            trail: None,
            vt_replay: None,
            write_rate: None,
            ghost: None,
            slide: None,
            scan: None,
            cell: None,
            quiet: false,
        }
    }
}

const USAGE: &str = "\
tek-tube - Tektronix 4010/4014 storage tube plot file emulator

Usage: tek-tube [options] file.plt [more.plt ...]
       tek-tube [options] -            (read the stream from a pipe)

  --device 4010|4014        target display (default 4014, whose 4096x3120
                            target is a superset of the 4010's 1024x780)
  --baud N                  serial line speed, characters per second = N/10
                            (default 19200, 0 = draw as fast as possible).  In
                            terminal mode this paces the *link*: the screen is
                            fed at that rate, so a listing takes the time a
                            listing took, and it is unpaced unless given
  --speed F                 multiplier on the drawing rate (default 1.0)
  --glow F                  fresh stroke flash time constant in seconds
                            (default 0.45: strokes come in hot and settle)
  --beam F                  write beam radius in 4010 dots (default 0.5,
                            a fine glowing hairline; 0.8 is a heavier stroke)
  --settle F                seconds of settling before --png (default 2.0)
  --png PATH                render to PATH and exit
  --png-at F                with --png: freeze the drawing at F seconds of
                            playback instead of running the file to the end
  --png-seq PREFIX          write PREFIX_000.png ... while drawing
  --frames N                frames for --png-seq (default 8)
  --frame-dt F              simulated seconds per frame (default 0.5)
  --zoom N                  integer output magnification (default 1)
  --shrink N                output box filter divisor (default 1, 4 for 4014)
  --no-autoclear            do not erase the target when the page overflows
  --no-enhanced             ignore the Enhanced Graphics Module extra bits
  --store-gain F            emitted light of the stored image (default 0.44,
                            0.69 of full scale after the display transfer)
  --fresh-gain F            emitted light the write beam adds on top (default
                            0.56, which puts a just written stroke at full
                            scale, i.e. white)
  --sat F                   multiplier on the measured stored chromaticity
                            (default 1.0; the stored green measures b/g 0.47
                            and r/g 0.07 on hardware)
  --ambient F               flood gun glow level, in emitted light (default
                            0.0725: an unwritten target measures 0.30 of a
                            stored stroke on the screen, and a phone camera
                            crushes it to a few %)
  --mottle F                flood gun field contrast (default 0.12: the
                            blotchy dark green background)
  --bloom F                 halation: light spreading out of the strokes
                            (default 0.12 close halo, 0.05 wide)
  --bloom-wide F            the wide component of that halo
  --font simplex|gothic     stroke font for the terminal's own alpha text
                            (default simplex, measured against the video's
                            alpha label; gothic is a heavier face).  Most plot
                            files draw their lettering as vector strokes, which
                            are reproduced exactly and are not affected.
  --text-scale F            character cell multiplier (default 1.0 = the
                            4010/4014 74x35 page; the 4052 in the video draws
                            larger text, about 1.5)
  --camera                  push the finished frame through a lens/handling
                            model (pivot, shadow contrast, knee, channel
                            leak, blur, noise) fitted to the source
                            recording.  This is a diagnostic for comparing
                            against a phone video, NOT part of the tube
                            model, and it is not calibrated to the current
                            device pipeline
  --quiet                   suppress the device/stream summary
  --stats                   print stream statistics
  --dump-chars              print every decoded alpha character
  --probe X,Y               print the colour of one target dot after playing
  --exit-after F window: quit after F seconds of playback
  --beam F                  in terminal mode this is scaled by --cell and then
                            by 1.5, because a terminal glyph is much smaller
                            than a plot's lettering and a hairline reads as too
                            light beside it (default 0.5 gives about 4 px stems)
  --cell F                  terminal mode: cell size as a multiple of the
                            tube's own cell (default 2.0, which doubles the
                            glyphs and gives a 37x18 page; 1.0 is the 4010's own
                            74x35)
  --write-rate DOTS         terminal mode: how fast the beam draws, in target
                            dots per second.  About 60 dots is one letter, so
                            the default 2500 is about 42 letters a second and a
                            plot file's own beam measures 4000-15000 on the
                            sample plots.  0 draws the page instantly, as it
                            always did, and a scroll or a clear always does
  --vt-replay FILE          parse FILE as a captured terminal stream and render
                            it (needs --png): how the terminal renderer is
                            checked without a window
  --trail F                 terminal mode: seconds the transient image lingers,
                            i.e. the length of the smear (default 0.3, a quick
                            cool from the flash to the stored green; 0 gives a
                            plain storage tube with no trail at all)
  --ghost F                 terminal mode: seconds the page a scroll replaced
                            keeps glowing underneath the new one.  That is what a
                            raster terminal's phosphor does and the source of the
                            ghosting real terminals show; a storage tube's charge
                            does not fade, so the default is 0, no ghost
  --slide R                 terminal mode: rows a second the display slides a
                            scroll at (default 30).  0 makes a scroll land in one
                            step, the way a raster terminal's does when the
                            movement is carried by the residual image instead
  --scan D                  the raster scan's own light structure: D is how deep
                            the gaps between scan lines go, 0 (the default) is a
                            flat face.  A vector display emits no scan lines; a
                            raster one is striped, and that stripe is most of
                            what makes a photograph of one read as a CRT
  --shell CMD               run a shell under a pseudoconsole and render the
                            session on the tube instead of playing plot files,
                            e.g. --shell powershell.exe.  The page is the
                            tube's own 74x35.  Ctrl+Shift+Q quits, or exit the
                            shell; ESC goes to the shell as ESC
  --fullscreen start fullscreen with no title bar, the whole screen given to
                            the tube (this is the default; F11 toggles).  In
                            terminal mode the tube is the screen, so the cell
                            follows the window and the page keeps the face's own
                            shape; a plot file is fitted to the screen at an
                            integer shrink, so nothing is softened either way
  --windowed start in a window with a title bar instead
  --threads N               threads a render pass may use (default: the machine's
                            own count, capped at 8; 1 makes the render serial)
  -h, --help this text

Without --png/--png-seq a window opens (ESC quits).
";

/// A process that does not declare DPI awareness gets its window bitmap
/// stretched by the monitor's scale factor before anyone sees it: on a 200 %
/// display a 1024x780 buffer is blown up to 2048x1560 by the compositor, which
/// softens every stroke and every speck of phosphor grain.  Declaring per
/// monitor v2 awareness makes the window pixel exact, so the render resolution
/// is the resolution you actually look at.
#[cfg(windows)]
fn be_dpi_aware() {
    #[link(name = "user32")]
    extern "system" {
        fn SetProcessDpiAwarenessContext(value: isize) -> i32;
    }
    // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
    const PER_MONITOR_AWARE_V2: isize = -4;
    unsafe {
        // fails harmlessly on Windows versions that predate the API
        let _ = SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2);
    }
}

#[cfg(not(windows))]
fn be_dpi_aware() {}

/// Primary monitor size in physical pixels, or `None` where we cannot ask.
/// The process is DPI aware by the time this runs, so these are real pixels.
#[cfg(windows)]
fn primary_screen() -> Option<(usize, usize)> {
    #[link(name = "user32")]
    extern "system" {
        fn GetSystemMetrics(index: i32) -> i32;
    }
    const SM_CXSCREEN: i32 = 0;
    const SM_CYSCREEN: i32 = 1;
    unsafe {
        let (w, h) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        if w > 0 && h > 0 {
            Some((w as usize, h as usize))
        } else {
            None
        }
    }
}

#[cfg(not(windows))]
fn primary_screen() -> Option<(usize, usize)> {
    None
}

/// The largest integer down-sample of the target that still fits on the screen.
/// A full screen window therefore shows the face at the biggest size that needs
/// no scaling at all - scaling it up is what made it look like fog.
fn fit_shrink(tw: i32, th: i32, screen: (usize, usize)) -> i32 {
    let (sw, sh) = (screen.0.max(1) as i32, screen.1.max(1) as i32);
    let mut shrink = 1;
    while shrink < 64 && (tw / shrink > sw || th / shrink > sh) {
        shrink += 1;
    }
    shrink
}

fn main() -> Result<(), Box<dyn Error>> {
    be_dpi_aware();
    let opts = match parse_args()? {
        Some(o) => o,
        None => {
            print!("{}", USAGE);
            return Ok(());
        }
    };
    run(opts)
}

/// Everything after the command line: choose a mode and go.
///
/// A function rather than the body of `main` so the terminal can also be started
/// from the test harness, which on this machine is the only kind of binary the
/// application control policy will run - see the `terminal` test at the end.
fn run(opts: Options) -> Result<(), Box<dyn Error>> {
    if let Some(n) = opts.threads {
        tube::set_render_threads(n);
    }
    if let Some(command) = opts.shell.clone() {
        return run_shell(&command, &opts);
    }
    if let Some(path) = opts.vt_replay.clone() {
        return replay_terminal(&path, &opts);
    }
    if opts.files.is_empty() {
        print!("{}", USAGE);
        return Ok(());
    }

    let mut data = Vec::new();
    for f in &opts.files {
        if f.as_os_str() == "-" {
            // a real 4010 is just a byte stream on a serial line, so let the
            // stream come from a pipe: `Get-Process | Out-String | tek-tube -`
            use std::io::Read;
            std::io::stdin().read_to_end(&mut data)?;
        } else {
            // name the file that could not be read: a bare "NotFound" says
            // nothing about which of several arguments was the problem
            data.extend_from_slice(&fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?);
        }
    }

    // Measure the stream for the report and pick the target: the 4014's
    // 4096x3120 (12 bit) target is a superset of the 4010's 1024x780, so it
    // renders either kind of stream correctly.
    let mut probe = Decoder::new(Device::Tek4014, opts.autoclear);
    {
        let mut sink = protocol::NullSink;
        probe.feed(&data, &mut sink);
    }
    let dev = opts.device;
    let (tw, th) = dev.size();
    if !opts.quiet {
        println!("device: {} ({}x{} dots)", dev.name(), tw, th);
        println!(
            "stream: {} vectors, {} points, {} characters, max 10 bit address {}x{}, \
             12 bit addressing used: {}",
            probe.vectors,
            probe.points,
            probe.chars,
            probe.max_x,
            probe.max_y,
            probe.extra_bits_used
        );
    }

    // A full screen window shows the face at the largest size that still needs
    // no scaling; an explicit --shrink always wins.  With --png/--png-seq there
    // is no window, so fullscreen has nothing to say about the output size.
    let has_window = opts.png.is_none() && opts.png_seq.is_none();
    // The monitor is asked for in every mode so F11 can reach real fullscreen
    // from a windowed start; --windowed decides only which mode we *start* in,
    // and the fit below still follows the start mode.
    let screen = if has_window { primary_screen() } else { None };
    let fullscreen = !opts.windowed && screen.is_some();
    let shrink = match (opts.shrink, if fullscreen { screen } else { None }) {
        (Some(s), _) => s,
        (None, Some((sw, sh))) => fit_shrink(tw, th, (sw, sh)),
        (None, None) => dev.scale(),
    };
    let (ow, oh) = (
        (tw / shrink * opts.zoom) as usize,
        (th / shrink * opts.zoom) as usize,
    );
    if !opts.quiet && fullscreen {
        let (sw, sh) = screen.unwrap();
        println!(
            "fullscreen: screen {}x{}, tube face {}x{} at --shrink {} (pixel exact, centred)",
            sw, sh, ow, oh, shrink
        );
    }

    let mut player = Player::new(data, dev, &opts);
    let mut buf: Vec<u32> = Vec::new();

    if let Some(path) = &opts.png {
        match opts.png_at {
            // Freeze mid-draw: step in small increments so the write beam's
            // flash is only aged by the last fraction of a frame.
            Some(t) => {
                let step = 1.0f32 / 120.0;
                let mut elapsed = 0.0;
                while elapsed < t {
                    let dt = step.min(t - elapsed);
                    player.advance(dt);
                    elapsed += dt;
                }
            }
            None => {
                player.run_to_end();
                player.tube.decay(opts.settle);
            }
        }
        if let Some((x, y)) = opts.probe {
            match player.tube.rgb_at(x, y) {
                Some((r, g, b)) => println!("dot {x},{y} displays #{r:02X}{g:02X}{b:02X}"),
                None => println!("dot {x},{y} is off the target"),
            }
        }
        player.tube.render_into(&mut buf, shrink, opts.zoom);
        write_png(path, &buf, ow, oh)?;
        if !opts.quiet {
            let (mean, written) = player.tube.stats();
            println!(
                "wrote {} ({}x{}), mean light {:.3}, {:.1}% of the target stored",
                path.display(),
                ow,
                oh,
                mean,
                written * 100.0
            );
        }
        report(&player, &opts);
        return Ok(());
    }

    if let Some(prefix) = &opts.png_seq {
        let steps = opts.frames;
        for i in 0..steps {
            // advance() already ages the phosphor for the elapsed time
            player.advance(opts.frame_dt);
            player.tube.render_into(&mut buf, shrink, opts.zoom);
            let path = PathBuf::from(format!("{}_{:03}.png", prefix, i));
            write_png(&path, &buf, ow, oh)?;
            if !opts.quiet {
                let (mean, written) = player.tube.stats();
                println!(
                    "t={:5.2}s bytes={:7} strokes={:6} mean light {:.3} stored {:.1}% -> {}",
                    (i + 1) as f32 * opts.frame_dt,
                    player.cursor,
                    player.decoder.vectors + player.decoder.points,
                    mean,
                    written * 100.0,
                    path.display()
                );
            }
        }
        report(&player, &opts);
        return Ok(());
    }

    run_window(&mut player, tw, th, &opts, fullscreen, screen)?;
    Ok(())
}

/// Everything the options say about how the target behaves, shared by plot
/// playback and terminal mode so the two cannot drift apart.
fn configure_tube(tube: &mut Tube, dev: Device, opts: &Options) {
    tube.fresh_tau = opts.glow;
    tube.sigma = opts.beam * dev.scale() as f32;
    if let Some(v) = opts.store_gain {
        tube.phosphor.stored_gain = v;
    }
    if let Some(v) = opts.fresh_gain {
        tube.phosphor.fresh_gain = v;
    }
    if let Some(v) = opts.sat {
        // scale the measured stored chromaticity, 1.0 = as measured
        tube.phosphor.stroke_r *= v;
        tube.phosphor.stroke_b *= v;
    }
    if let Some(v) = opts.ambient {
        tube.phosphor.ambient = v;
    }
    if let Some(v) = opts.mottle {
        tube.phosphor.mottle = v;
    }
    if let Some(v) = opts.bloom {
        tube.phosphor.bloom = v;
    }
    if let Some(v) = opts.bloom_wide {
        tube.phosphor.bloom_wide = v;
    }
    if let Some(v) = opts.scan {
        tube.phosphor.scan_depth = v.clamp(0.0, 0.95);
    }
    tube.phosphor.camera = opts.camera;
    tube.font = font::face(opts.font);
}

struct Player {
    data: Vec<u8>,
    cursor: usize,
    decoder: Decoder,
    tube: Tube,
    /// Bytes per second the terminal can accept.
    rate: f32,
    credit: f32,
    instant: bool,
    done: bool,
}

impl Player {
    fn new(data: Vec<u8>, dev: Device, opts: &Options) -> Self {
        let mut tube = Tube::new(dev.size().0, dev.size().1);
        configure_tube(&mut tube, dev, opts);
        let mut decoder = Decoder::new(dev, opts.autoclear);
        tube.sigma = opts.beam * dev.scale() as f32;
        if let Some(v) = opts.store_gain {
            tube.phosphor.stored_gain = v;
        }
        if let Some(v) = opts.fresh_gain {
            tube.phosphor.fresh_gain = v;
        }
        if let Some(v) = opts.sat {
            // scale the measured stored chromaticity, 1.0 = as measured
            tube.phosphor.stroke_r *= v;
            tube.phosphor.stroke_b *= v;
        }
        if let Some(v) = opts.ambient {
            tube.phosphor.ambient = v;
        }
        if let Some(v) = opts.mottle {
            tube.phosphor.mottle = v;
        }
        if let Some(v) = opts.bloom {
            tube.phosphor.bloom = v;
        }
        if let Some(v) = opts.bloom_wide {
            tube.phosphor.bloom_wide = v;
        }
        decoder.enhanced = opts.enhanced && dev == Device::Tek4014;
        decoder.record_chars = opts.dump_chars;
        decoder.text_scale = opts.text_scale;
        let (rate, instant) = if opts.baud <= 0.0 || opts.speed <= 0.0 {
            (0.0, true)
        } else {
            (opts.baud / 10.0 * opts.speed, false)
        };
        Player {
            data,
            cursor: 0,
            decoder,
            tube,
            rate,
            credit: 0.0,
            instant,
            done: false,
        }
    }

    /// Consume input for `dt` seconds of terminal time and age the phosphor.
    /// Returns true if any input byte was actually fed, i.e. if the target
    /// changed.
    ///
    /// The input is fed in small chunks and the phosphor is aged between them,
    /// so a stroke written early in the interval has faded by the time the
    /// interval ends while one written at the end is still hot.  Feeding a
    /// whole frame and then ageing it by the whole frame - which is what this
    /// used to do - makes every stroke in the batch share the age of the last
    /// one, and wipes out the write beam's trail at any frame rate below
    /// ~1000 fps.
    fn advance(&mut self, dt: f32) -> bool {
        if dt <= 0.0 {
            return false;
        }
        if self.instant {
            let fed = self.cursor < self.data.len();
            if fed {
                let (data, cursor, decoder, tube) =
                    (&self.data, self.cursor, &mut self.decoder, &mut self.tube);
                decoder.feed(&data[cursor..], tube);
                self.cursor = self.data.len();
            }
            self.done = true;
            self.tube.decay(dt);
            return fed;
        }
        self.credit += dt * self.rate;
        let budget = self.credit as usize;
        self.credit -= budget as f32;
        let left = self.data.len() - self.cursor;
        let total = budget.min(left);
        if total == 0 {
            self.done = self.cursor >= self.data.len();
            self.tube.decay(dt);
            return false;
        }
        // ~16 sub-steps per interval, and never more than 64 bytes at a time
        let chunk = (total / 16).clamp(1, 64);
        let steps = total.div_ceil(chunk);
        let sub_dt = dt / steps as f32;
        let mut remaining = total;
        while remaining > 0 {
            let take = remaining.min(chunk);
            let (data, cursor, decoder, tube) =
                (&self.data, self.cursor, &mut self.decoder, &mut self.tube);
            decoder.feed(&data[cursor..cursor + take], tube);
            self.cursor += take;
            remaining -= take;
            self.tube.decay(sub_dt);
        }
        self.done = self.cursor >= self.data.len();
        true
    }

    /// Simulated seconds of input played so far, from the byte rate.  Zero when
    /// the stream is drawn as fast as possible, where a dots-per-second figure
    /// would be meaningless rather than large.
    fn elapsed(&self) -> f32 {
        if self.instant || self.rate <= 0.0 {
            0.0
        } else {
            self.cursor as f32 / self.rate
        }
    }

    fn run_to_end(&mut self) {
        while !self.done {
            self.advance(1.0 / 60.0);
        }
    }
}

/// Render a captured terminal byte stream, for checking the terminal renderer
/// without a window.
fn replay_terminal(path: &PathBuf, opts: &Options) -> Result<(), Box<dyn Error>> {
    let out = opts.png.clone().ok_or("--vt-replay needs --png")?;
    let dev = opts.device;
    let (tw, th) = dev.size();
    let shrink = opts.shrink.unwrap_or_else(|| dev.scale());
    let (ow, oh) = (
        (tw / shrink * opts.zoom) as usize,
        (th / shrink * opts.zoom) as usize,
    );

    let data = fs::read(path)?;
    let cell = opts.cell.unwrap_or(terminal::CELL_SCALE_DEFAULT);
    let (cols, rows) = terminal::page(cell);
    let mut screen = vt::Screen::new(cols, rows);
    let mut parser = vt::Parser::new();
    let replies = parser.feed(&data, &mut screen);

    let mut tube = Tube::new(tw, th);
    configure_terminal_tube(&mut tube, dev, opts, cell);
    terminal::stroke_screen(&screen, cols, rows, &mut tube, Write::Settle, true);
    let mut buf = Vec::new();
    tube.render_into(&mut buf, shrink, opts.zoom);
    write_png(&out, &buf, ow, oh)?;
    if !opts.quiet {
        println!(
            "replayed {} bytes: {} cursor reports requested, title {:?}",
            data.len(),
            parser.dsr_count,
            screen.title.trim()
        );
        println!("wrote {} ({ow}x{oh})", out.display());
    }
    let _ = replies;
    Ok(())
}

/// Terminal mode: how heavy the beam is for a terminal cell.
///
/// The beam is measured in 4010 dots, which says nothing about how big a glyph
/// is - so doubling the cell size halved the stroke weight relative to the
/// glyph and the text went spindly.  Scaling by the cell restores that, and the
/// extra factor is deliberate: a terminal glyph is far smaller than the lettering
/// inside a plot file, and at a hairline weight it reads as too light next to
/// one.  1.5 puts the stems at about 4 px, near the ~3 px double-stroked stems a
/// DISSPLA title draws.
const TERMINAL_STROKE_BOLD: f32 = 1.5;

/// Everything the terminal's target needs, so the two places that build one
/// (startup and resize) cannot drift.
fn configure_terminal_tube(tube: &mut Tube, dev: Device, opts: &Options, cell: f32) {
    configure_tube(tube, dev, opts);
    tube.sigma = opts.beam * cell * TERMINAL_STROKE_BOLD;
    // The flash cools quickly and the *stored* image is what holds, which is
    // the whole point of a storage tube: hot almost white in, medium acid green
    // held.  1.1 s of smear left a written character visibly fading for a second,
    // which at the frame rate a window can manage reads as slow, and the length
    // of the fade is the part that steps.  0.3 s is a quick cool - the hue is
    // settled in well under a second - and it leaves just enough smear that the
    // writing looks like a beam rather than a paste.
    tube.fresh_tau = opts.trail.unwrap_or(0.3);
    // A raster terminal's phosphor keeps the page it has just replaced lit for a
    // moment, so scrolling leaves the previous screenful readable underneath the
    // new one.  A storage tube's charge is permanent and does not fade, so this
    // is off unless asked for.
    tube.ghost_tau = opts.ghost.unwrap_or(0.0);
}

/// Terminal mode: run a shell under a pseudoconsole and render it on the tube.
fn run_shell(command: &str, opts: &Options) -> Result<(), Box<dyn Error>> {
    // The terminal always uses the 4010's face.  It is not addressed as a 4014
    // (there is no 12 bit coordinate stream to honour), and the tube *is* the
    // window here, so --device, --shrink and --zoom do not apply: the face is
    // 1024x780 and the cell size follows from that.
    let dev = Device::Tek4010;
    let (tw, th) = dev.size();
    // The screen size is asked for whatever mode we start in, because F11 has to
    // reach real fullscreen from a windowed start.  With `screen` left as None it
    // could only fall back to the remembered windowed size - a borderless window
    // the size of the one it replaced, which is what "F11 does not go fullscreen"
    // looks like.
    let screen = primary_screen();
    let fullscreen = !opts.windowed && screen.is_some();
    let start_screen = if fullscreen { screen } else { None };
    let cell = opts.cell.unwrap_or(terminal::CELL_SCALE_DEFAULT);

    // Fullscreen gives the whole screen to the tube, and the tube *is* the
    // window, so the page keeps the face's own shape while the cell grows with
    // the window.  Sizing the tube and the page here, rather than letting the
    // first frame's resize do it, means no flicker at startup and no stray pty
    // resize for the shell to re-lay out.
    let (cols, rows, tw, th) = match start_screen {
        Some((sw, sh)) => {
            let (c, r) = terminal::page_for_window(sw, sh, cell);
            (c, r, sw as i32, sh as i32)
        }
        None => {
            let (c, r) = terminal::page(cell);
            (c, r, tw, th)
        }
    };

    let mut tube = Tube::new(tw, th);
    let cell_now = terminal::cell_scale_of(tube.w, cols);
    configure_terminal_tube(&mut tube, dev, opts, cell_now);

    let mut term = terminal::Terminal::spawn(command, cols, rows)?;
    if let Some(rate) = opts.write_rate {
        term.set_write_rate(rate);
    }
    if let Some(rate) = opts.slide {
        term.set_slide_rate(rate);
    }
    if opts.baud_given {
        term.set_baud(opts.baud);
    }
    if !opts.quiet {
        println!(
            "terminal: {command} on a {cols}x{rows} page (cell x{:.2}) of a {}x{} target{}",
            terminal::cell_scale_of(tube.w, cols),
            tube.w,
            tube.h,
            if fullscreen { ", fullscreen" } else { "" }
        );
        println!(
            "Ctrl+Shift+Q quits, or exit the shell.  ESC goes to the shell.  F11 toggles fullscreen."
        );
    }
    run_terminal_window(
        &mut term,
        &mut tube,
        dev,
        opts,
        fullscreen,
        screen,
        opts.exit_after,
        opts.quiet,
        cell,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_terminal_window(
    term: &mut terminal::Terminal,
    tube: &mut Tube,
    dev: Device,
    opts: &Options,
    fullscreen: bool,
    screen: Option<(usize, usize)>,
    exit_after: Option<f32>,
    quiet: bool,
    cell_scale: f32,
) -> Result<(), Box<dyn Error>> {
    use minifb::{Key, ScaleMode, Window, WindowOptions};
    let started = Instant::now();
    // F11 out of a fullscreen start should land on the tube's own face rather
    // than on a window the size of the screen.
    let mut windowed_size = if fullscreen {
        let (fw, fh) = Device::Tek4010.size();
        (fw as usize, fh as usize)
    } else {
        (tube.w as usize, tube.h as usize)
    };
    let mut fullscreen = fullscreen;
    let mut dead_frames = 0;
    let mut was_viewing = false;
    // Frames actually presented, across any F11 rebuilds: the frame rate is what
    // decides whether a fade can be smooth, so it is reported rather than guessed.
    let mut frames: u64 = 0;
    // seconds actually spent drawing a frame, so the rate reported is the cost of
    // a frame and not a session average diluted by the idle time between bursts
    let mut drawn_secs = 0.0f32;
    // A window cannot change its own decorations, so F11 rebuilds it: one
    // window per state, and the shell and the tube simply carry on.
    loop {
        let (win_w, win_h) = if fullscreen {
            screen.unwrap_or(windowed_size)
        } else {
            windowed_size
        };
        let title = format!("tek-tube - {} [{}x{}]", term.title(), term.cols, term.rows);
        let mut window = Window::new(
            &title,
            win_w,
            win_h,
            WindowOptions {
                // the tube follows the window in terminal mode: the cell size is
                // what has to stay put, not the page, so resizing adds or removes
                // whole cells rather than stretching glyphs
                resize: !fullscreen,
                borderless: fullscreen,
                none: fullscreen,
                scale_mode: if fullscreen {
                    ScaleMode::Center
                } else {
                    ScaleMode::Stretch
                },
                ..WindowOptions::default()
            },
        )?;
        window.set_input_callback(term.input_handler());
        window.set_target_fps(60);

        let mut buf: Vec<u32> = Vec::new();
        let mut last = Instant::now();
        let mut dirty = true;
        let mut toggle = false;
        while window.is_open() {
            if term.take_fullscreen_toggle() {
                toggle = true;
                break;
            }
            // ESC has to reach the shell, so it can no longer be the quit key.
            if window.is_key_down(Key::LeftCtrl)
                && window.is_key_down(Key::LeftShift)
                && window.is_key_down(Key::Q)
            {
                break;
            }
            if !term.is_running() {
                // A command that exits immediately still has output in flight: keep
                // polling until the reader ends, with a short grace period in case
                // the pty outlives its client.
                dead_frames += 1;
                if term.eof() || dead_frames > 45 {
                    break;
                }
            } else {
                dead_frames = 0;
            }
            if let Some(limit) = exit_after {
                if started.elapsed().as_secs_f32() > limit {
                    break;
                }
            }
            let now = Instant::now();
            // The animation's clock is the wall clock.  Clamping this to a quarter of
            // a second does not smooth anything: on the frames that took longest it
            // makes the phosphor decay in slow motion, which is exactly when the fade
            // looks steppiest, and the rate then depends on the frame time - the
            // fade changing speed from slow to stopped to fast.  The decay is
            // exponential, so it is stable for any dt; the cap is only there to stop
            // a real *pause* (a dragged window, a suspended process) from resolving
            // the whole image in one step.
            let dt = (now - last).as_secs_f32().min(1.0);
            last = now;

            // Resize: keep the cell size and change how many cells there are.  The
            // page is 74x35 on a 1024x780 face, so the same ratio gives the page for
            // any window size, and the pty is told so the shell re-lays out.
            if window.get_size() != (tube.w as usize, tube.h as usize) {
                let (ww, wh) = window.get_size();
                let (cols, rows) = terminal::page_for_window(ww, wh, cell_scale);
                if ww >= 200 && wh >= 88 {
                    term.resize(cols, rows);
                    *tube = Tube::new(ww as i32, wh as i32);
                    // the cell in dots follows the window, and the stroke weight is
                    // measured in cells, so it has to be re-derived
                    let cell_now = terminal::cell_scale_of(tube.w, cols);
                    configure_terminal_tube(tube, dev, opts, cell_now);
                    // show the geometry, so a resize is visible without a debugger
                    window.set_title(&format!("tek-tube - {} [{}x{}]", term.title(), cols, rows));
                    dirty = true;
                }
            }

            // the wheel reads the scrollback; positive y is a scroll up in minifb
            if let Some((_, dy)) = window.get_scroll_wheel() {
                let step = if dy.abs() > 0.0 { (dy * 3.0) as i32 } else { 0 };
                if step != 0 {
                    term.scroll_view(step);
                }
            }

            // A full-screen program repaints every cell, so a long trail would only
            // smear it into mud; line-oriented output gets the full smear.
            tube.fresh_tau = if term.in_alt_screen() {
                0.25
            } else {
                opts.trail.unwrap_or(1.1)
            };

            let changed = term.poll(dt);
            tube.decay(dt);
            if term.commit_due() {
                term.commit(tube);
                dirty = true;
            }
            if changed {
                term.render(tube, dt);
                dirty = true;
            }
            // A storage tube has no scrollbar, so say it in the title instead: you
            // are either looking at the page or at history and must be able to tell.
            if (term.view_offset() > 0) != was_viewing {
                was_viewing = term.view_offset() > 0;
                window.set_title(&if was_viewing {
                    format!(
                        "tek-tube - {} [{}x{}] {} lines back of {}",
                        term.title(),
                        term.cols,
                        term.rows,
                        term.view_offset(),
                        term.scrollback_len()
                    )
                } else {
                    format!("tek-tube - {} [{}x{}]", term.title(), term.cols, term.rows)
                });
            }
            // keep presenting while the transient image is still fading, or the
            // settle would freeze half way
            if tube.transients_alive() {
                dirty = true;
            }
            if dirty {
                // one buffer pixel per window pixel: the tube is the window
                let drew = Instant::now();
                tube.render_into(&mut buf, 1, 1);
                window.update_with_buffer(&buf, tube.w as usize, tube.h as usize)?;
                drawn_secs += drew.elapsed().as_secs_f32();
                dirty = false;
                frames += 1;
            } else {
                window.update();
            }
        }
        // remember the windowed size, so F11 twice comes back to it
        if !fullscreen {
            windowed_size = window.get_size();
        }
        drop(window);
        if !toggle {
            break;
        }
        fullscreen = !fullscreen;
    }
    term.stop();
    if !quiet {
        let (mean, stored) = tube.stats();
        let secs = started.elapsed().as_secs_f32();
        println!(
            "terminal: {} bytes read, {} frames committed, {} bytes sent; tube mean light {:.4}, \
             {:.1}% stored; {} frames drawn, {:.1} ms each over {:.1}s of drawing \
             ({:.0} fps while drawing, {:.1} fps over the {:.0}s session)",
            term.bytes_in,
            term.commits,
            term.keys_in,
            mean,
            stored * 100.0,
            frames,
            drawn_secs * 1000.0 / frames.max(1) as f32,
            drawn_secs,
            frames as f32 / drawn_secs.max(1e-3),
            frames as f32 / secs.max(1e-3),
            secs
        );
    }
    Ok(())
}

fn run_window(
    player: &mut Player,
    tw: i32,
    th: i32,
    opts: &Options,
    start_fullscreen: bool,
    screen: Option<(usize, usize)>,
) -> Result<(), Box<dyn Error>> {
    use minifb::{Key, ScaleMode, Window, WindowOptions};
    let dev = player.decoder.dev;
    let title = format!(
        "tek-tube - {} {}x{}",
        dev.name(),
        player.tube.w,
        player.tube.h
    );
    let mut fullscreen = start_fullscreen;
    let mut f11_down = false;
    // The fit follows the mode: the monitor at fullscreen, `--shrink` or the
    // device's own scale in a window.  F11 rebuilds the window, as in the
    // terminal, and the fit is recomputed on the way.
    let fit = |fullscreen: bool| -> (i32, i32, usize, usize) {
        let shrink = match (opts.shrink, if fullscreen { screen } else { None }) {
            (Some(s), _) => s,
            (None, Some((sw, sh))) => fit_shrink(tw, th, (sw, sh)),
            (None, None) => dev.scale(),
        };
        (
            shrink,
            opts.zoom,
            (tw / shrink * opts.zoom) as usize,
            (th / shrink * opts.zoom) as usize,
        )
    };
    loop {
        let (shrink, zoom, ow, oh) = fit(fullscreen);
        // In fullscreen the window is the whole monitor and the tube face is placed
        // in the middle of it *without scaling* (ScaleMode::Center): upscaling the
        // buffer to fill a 16:9 screen would soften exactly what the pixel exact
        // rendering exists to keep sharp.
        let (win_w, win_h) = if fullscreen {
            screen.unwrap_or((ow, oh))
        } else {
            (ow, oh)
        };
        let mut window = Window::new(
            &title,
            win_w,
            win_h,
            WindowOptions {
                resize: false,
                // On Windows `borderless` only drops the resize frame and keeps the
                // caption; `none` is what actually asks for WS_POPUP, i.e. no title
                // bar at all, so the client area is the whole monitor.
                borderless: fullscreen,
                none: fullscreen,
                scale_mode: if fullscreen {
                    ScaleMode::Center
                } else {
                    ScaleMode::Stretch
                },
                ..WindowOptions::default()
            },
        )?;
        window.set_target_fps(60);
        let mut buf: Vec<u32> = Vec::new();
        let started = Instant::now();
        let mut last = Instant::now();
        // Recomposing is only needed while something is actually moving: new input
        // arriving, or a transient population still fading.  A settled screen is
        // static, so the loop just presents the buffer it already has.
        let mut dirty = true;
        let mut toggle = false;
        while window.is_open() && !window.is_key_down(Key::Escape) {
            let down = window.is_key_down(Key::F11);
            if down && !f11_down {
                toggle = true;
                break;
            }
            f11_down = down;
            let now = Instant::now();
            // The same wall clock as the terminal loop: a clamp here makes the
            // drawing run in slow motion on exactly the slowest frames.
            let dt = (now - last).as_secs_f32().min(1.0);
            last = now;
            if player.advance(dt) || player.tube.transients_alive() {
                dirty = true;
            }
            if dirty {
                player.tube.render_into(&mut buf, shrink, zoom);
                window.update_with_buffer(&buf, ow, oh)?;
                dirty = false;
            } else {
                window.update();
            }
            if let Some(limit) = opts.exit_after {
                if started.elapsed().as_secs_f32() > limit {
                    break;
                }
            }
        }
        if !toggle {
            break;
        }
        fullscreen = !fullscreen;
    }
    report(player, opts);
    Ok(())
}

fn report(player: &Player, opts: &Options) {
    if !opts.stats {
        return;
    }
    let d = &player.decoder;
    println!(
        "decoded {} of {} bytes: {} vectors, {} points, {} characters, {} stray bytes, \
         max address {}x{}, 12 bit addressing used: {}",
        player.cursor,
        player.data.len(),
        d.vectors,
        d.points,
        d.chars,
        d.stray,
        d.max_x,
        d.max_y,
        d.extra_bits_used
    );
    // How fast the beam actually swept, which is not the line speed: a byte of
    // plot file may command a stroke of any length.  In the 4010's own dots so a
    // 4014 stream can be compared with the 4010's face, and with the terminal
    // pen's rate, which is quoted in the same units.
    let dots = player.tube.beam_dots();
    let secs = player.elapsed();
    if secs > 0.0 {
        let spot = player.tube.spot.max(1) as f64;
        println!(
            "beam: {:.0} target dots in {:.2} s of playback = {:.0} dots/s, {:.0} of the \
             4010's dots/s (spot {})",
            dots,
            secs,
            dots / secs as f64,
            dots / secs as f64 / spot,
            player.tube.spot
        );
    }

    if opts.dump_chars && !player.decoder.char_log.is_empty() {
        println!("alpha characters (character, x, y, cell width, cell height):");
        let mut line = String::new();
        for (ch, x, y, cw, chh) in &player.decoder.char_log {
            line.push_str(&format!("{} @{},{} {}x{} ", *ch as char, x, y, cw, chh));
            if line.len() > 90 {
                println!("  {}", line.trim_end());
                line.clear();
            }
        }
        if !line.is_empty() {
            println!("  {}", line.trim_end());
        }
    }
}

fn write_png(path: &PathBuf, buf: &[u32], w: usize, h: usize) -> Result<(), Box<dyn Error>> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }
    let file = fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    let mut rgb = Vec::with_capacity(w * h * 3);
    for px in buf {
        rgb.push((px >> 16) as u8);
        rgb.push((px >> 8) as u8);
        rgb.push(*px as u8);
    }
    writer.write_image_data(&rgb)?;
    Ok(())
}

/// Run the terminal for real, from the test harness.
///
/// Smart App Control refuses freshly built *program* binaries on this machine:
/// the verdict is keyed on the file's flat SHA-256, so it is per build, and every
/// hash produced while this was written was refused - release and debug alike,
/// with different codegen flags, twelve in a row - while test binaries are
/// exempt.  That makes this the one way to run the terminal here:
///
/// ```sh
/// cargo test -- --ignored --nocapture terminal
/// TEK_ARGS="--windowed --cell 1.5" cargo test -- --ignored --nocapture terminal
/// ```
///
/// `TEK_ARGS` is the program's own command line, parsed by the program's own
/// parser; with nothing given it runs a PowerShell terminal on the tube.
#[cfg(test)]
mod harness {
    use super::*;

    #[test]
    #[ignore = "runs a shell and a window until you quit; run it deliberately"]
    fn terminal() {
        let extra: Vec<String> = std::env::var("TEK_ARGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let mut argv: Vec<String> = vec!["tek-tube".to_string()];
        argv.extend(extra.iter().cloned());
        // Decide whether a mode is wanted by *parsing*, not by the shape of the
        // arguments: `TEK_ARGS="--windowed"` means a shell on the tube, while
        // `TEK_ARGS="samples/05_snoopy.plt"` means play the plot file, and an
        // option value like `--trail 0.5` is neither.
        let named = matches!(
            parse_args_from(extra.clone().into_iter()).expect("TEK_ARGS should parse"),
            Some(o) if o.shell.is_some() || o.vt_replay.is_some() || !o.files.is_empty()
        );
        // Unless the arguments already name a mode, this is a shell on the tube:
        // `TEK_ARGS="--windowed"` should do what it says, and so should
        // `TEK_ARGS="--trail 0.5"`.
        //
        // The shell is named by absolute path because a bare `powershell.exe`
        // does not resolve when this test binary is started directly rather than
        // through cargo.  The failure then surfaces as
        // `the terminal should run: Os { code: 2, NotFound }`, which reads like a
        // missing shell rather than a missing PATH entry, and cost an hour.
        if !named {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
            argv.push("--shell".to_string());
            argv.push(format!(
                "{root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"
            ));
        }
        be_dpi_aware();
        let opts = parse_args_from(argv.into_iter().skip(1))
            .expect("TEK_ARGS should parse")
            .expect("TEK_ARGS asked for --help, so there is nothing to run");
        run(opts).expect("the terminal should run");
    }

    /// "1280x720" -> (1280, 720), or the given default.
    fn size_of(s: &str, dw: usize, dh: usize) -> (usize, usize) {
        match s.split_once('x') {
            Some((a, b)) => match (a.trim().parse(), b.trim().parse()) {
                (Ok(w), Ok(h)) => (w, h),
                _ => (dw, dh),
            },
            None => (dw, dh),
        }
    }

    fn env_f32(key: &str, default: f32) -> f32 {
        std::env::var(key)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// Lit pixels in the rendered frame: the threshold the tube's own tests use.
    fn lit(tube: &mut Tube) -> usize {
        let mut buf = Vec::new();
        tube.render_into(&mut buf, 1, 1);
        buf.iter().filter(|p| ((**p >> 8) & 0xff) > 120).count()
    }

    /// Drive a real shell through the terminal's own frame loop with **no window
    /// at all**, so what the terminal does can be measured and photographed from
    /// a script instead of by eye and by hand.
    ///
    /// Every option is parsed by the program's own parser, so anything the real
    /// program can be asked for, this can be asked for the same way:
    ///
    /// ```sh
    /// TEK_ARGS="--ghost 0.1 --write-rate 0" TEK_SCRIPT="dir;cls" \
    ///   cargo test --release -- --ignored --nocapture terminal_loop
    /// ```
    ///
    /// Environment:
    ///   TEK_ARGS     the program's own options (a shell is added if none is named)
    ///   TEK_SCRIPT   commands to type, separated by ';' (default: none)
    ///   TEK_WAIT     seconds to run after each command (default 2)
    ///   TEK_SECONDS  seconds to run after the script has been typed (default 6)
    ///   TEK_SIZE     the tube, WxH (default 1280x720)
    ///   TEK_PAGE     the shell's page, COLSxROWS (default: the cell scale's page)
    ///   TEK_PNG      write the last frame to this path
    ///   TEK_SEQ      write every frame after the script to PREFIX_%04d.png
    ///   TEK_SEQ_MAX  frames to write for TEK_SEQ (default 120)
    ///   TEK_TRACE    set to print a line a frame: lit pixels and commits
    ///
    /// This exists because the terminal's defects live in the *live* path - a
    /// doubled Enter from the window's key handling, a glow that outlived its
    /// erase - and both were found by a throwaway version of this driver after
    /// they had survived sessions of reading the code.  It is kept for that.
    #[test]
    #[ignore = "spawns a real shell and runs unattended; run it deliberately"]
    fn terminal_loop() {
        use minifb::Key;
        let (tw, th) = size_of(
            &std::env::var("TEK_SIZE").unwrap_or_else(|_| "1280x720".into()),
            1280,
            720,
        );
        let extra: Vec<String> = std::env::var("TEK_ARGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let mut argv: Vec<String> = vec!["tek-tube".to_string()];
        argv.extend(extra.iter().cloned());
        let named = matches!(
            parse_args_from(extra.clone().into_iter()).expect("TEK_ARGS should parse"),
            Some(o) if o.shell.is_some() || o.vt_replay.is_some() || !o.files.is_empty()
        );
        if !named {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
            argv.push("--shell".to_string());
            argv.push(format!(
                "{root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"
            ));
        }
        let opts = parse_args_from(argv.into_iter().skip(1))
            .expect("TEK_ARGS should parse")
            .expect("TEK_ARGS asked for --help, so there is nothing to run");
        let shell = opts.shell.clone().expect("the driver needs a shell");
        let (cols, rows) = match std::env::var("TEK_PAGE") {
            Ok(v) => size_of(&v, 80, 24),
            Err(_) => terminal::page(opts.cell.unwrap_or(terminal::CELL_SCALE_DEFAULT)),
        };
        let mut tube = Tube::new(tw as i32, th as i32);
        let cell = terminal::cell_scale_of(tube.w, cols);
        configure_terminal_tube(&mut tube, opts.device, &opts, cell);
        let mut term =
            terminal::Terminal::spawn(&shell, cols, rows).expect("the shell should spawn");
        if let Some(rate) = opts.write_rate {
            term.set_write_rate(rate);
        }
        if let Some(rate) = opts.slide {
            term.set_slide_rate(rate);
        }
        if opts.baud_given {
            term.set_baud(opts.baud);
        }
        let mut cb = term.input_handler();
        let start = Instant::now();
        let mut last = Instant::now();
        let (mut frames, mut ms) = (0usize, 0f64);
        let mut buf: Vec<u32> = Vec::new();

        macro_rules! frame {
            () => {{
                let now = Instant::now();
                let dt = (now - last).as_secs_f32().min(0.25);
                last = now;
                let t0 = Instant::now();
                let changed = term.poll(dt);
                // the per-frame work `run_shell` does, in the same order
                tube.fresh_tau = if term.in_alt_screen() {
                    0.25
                } else {
                    opts.trail.unwrap_or(1.1)
                };
                tube.decay(dt);
                if term.commit_due() {
                    term.commit(&mut tube);
                }
                if changed {
                    term.render(&mut tube, dt);
                }
                ms += t0.elapsed().as_secs_f64() * 1e3;
                frames += 1;
                std::thread::sleep(std::time::Duration::from_millis(4));
            }};
        }
        macro_rules! pump {
            ($secs:expr) => {{
                let until = Instant::now() + std::time::Duration::from_secs_f32($secs);
                while Instant::now() < until {
                    frame!();
                }
            }};
        }

        pump!(1.5); // let the shell come up
        let script = std::env::var("TEK_SCRIPT").unwrap_or_default();
        let wait = env_f32("TEK_WAIT", 2.0);
        for cmd in script.split(';').map(str::trim).filter(|c| !c.is_empty()) {
            for c in cmd.chars() {
                cb.add_char(c as u32);
                std::thread::sleep(std::time::Duration::from_millis(6));
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
            cb.set_key_state(Key::Enter, true);
            std::thread::sleep(std::time::Duration::from_millis(20));
            cb.set_key_state(Key::Enter, false);
            pump!(wait);
        }

        let seq = std::env::var("TEK_SEQ").ok();
        let seq_max = env_f32("TEK_SEQ_MAX", 120.0) as usize;
        let trace = std::env::var("TEK_TRACE").is_ok();
        let until = Instant::now() + std::time::Duration::from_secs_f32(env_f32("TEK_SECONDS", 6.0));
        let mut n = 0usize;
        while Instant::now() < until {
            frame!();
            if trace {
                println!(
                    "TLOG t={:.2} frames={frames} lit={} commits={} bytes_in={} shown={}",
                    start.elapsed().as_secs_f32(),
                    lit(&mut tube),
                    term.commits,
                    term.bytes_in,
                    term.bytes_shown
                );
            }
            if let (Some(prefix), true) = (&seq, n < seq_max) {
                tube.render_into(&mut buf, 1, 1);
                let path = PathBuf::from(format!("{prefix}_{n:04}.png"));
                write_png(&path, &buf, tube.w as usize, tube.h as usize).expect("png");
                n += 1;
            }
        }

        tube.render_into(&mut buf, 1, 1);
        if let Ok(path) = std::env::var("TEK_PNG") {
            write_png(&PathBuf::from(&path), &buf, tube.w as usize, tube.h as usize).expect("png");
        }
        println!(
            "TLOG done: {frames} frames, {:.1} ms a frame, {} commits, {} bytes read, lit {}",
            ms / frames.max(1) as f64,
            term.commits,
            term.bytes_in,
            lit(&mut tube)
        );
        term.stop();
    }
}

fn parse_args() -> Result<Option<Options>, Box<dyn Error>> {
    parse_args_from(std::env::args().skip(1))
}

/// The same parse over an explicit argument list, so the program's own command
/// line handling can be driven without a process command line.
fn parse_args_from<I>(args: I) -> Result<Option<Options>, Box<dyn Error>>
where
    I: Iterator<Item = String>,
{
    let mut o = Options::default();
    let mut args = args;
    while let Some(a) = args.next() {
        let mut value = |name: &str| -> Result<String, Box<dyn Error>> {
            args.next()
                .ok_or_else(|| format!("{} needs a value", name).into())
        };
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--device" => {
                o.device = match value("--device")?.as_str() {
                    "4010" | "tek4010" => Device::Tek4010,
                    "4014" | "tek4014" => Device::Tek4014,
                    other => return Err(format!("unknown device {}", other).into()),
                }
            }
            "--baud" => {
                o.baud = value("--baud")?.parse()?;
                o.baud_given = true;
            }
            "--speed" => o.speed = value("--speed")?.parse()?,
            "--glow" => o.glow = value("--glow")?.parse()?,
            "--beam" => o.beam = value("--beam")?.parse()?,
            "--settle" => o.settle = value("--settle")?.parse()?,
            "--png" => o.png = Some(PathBuf::from(value("--png")?)),
            "--png-at" => o.png_at = Some(value("--png-at")?.parse()?),
            "--png-seq" => o.png_seq = Some(value("--png-seq")?),
            "--frames" => o.frames = value("--frames")?.parse()?,
            "--frame-dt" => o.frame_dt = value("--frame-dt")?.parse()?,
            "--zoom" => o.zoom = value("--zoom")?.parse()?,
            "--shrink" => o.shrink = Some(value("--shrink")?.parse()?),
            "--store-gain" => o.store_gain = Some(value("--store-gain")?.parse()?),
            "--fresh-gain" => o.fresh_gain = Some(value("--fresh-gain")?.parse()?),
            "--sat" => o.sat = Some(value("--sat")?.parse()?),
            "--ambient" => o.ambient = Some(value("--ambient")?.parse()?),
            "--mottle" => o.mottle = Some(value("--mottle")?.parse()?),
            "--bloom" => o.bloom = Some(value("--bloom")?.parse()?),
            "--bloom-wide" => o.bloom_wide = Some(value("--bloom-wide")?.parse()?),
            "--camera" => o.camera = true,
            "--font" => {
                o.font = match value("--font")?.as_str() {
                    "gothic" | "bold" => Face::Gothic,
                    "simplex" | "light" => Face::Simplex,
                    "mono" | "monospace" => Face::Mono,
                    other => return Err(format!("unknown font {}", other).into()),
                }
            }
            "--text-scale" => o.text_scale = value("--text-scale")?.parse()?,
            "--exit-after" => o.exit_after = Some(value("--exit-after")?.parse()?),
            "--fullscreen" => o.windowed = false,
            "--windowed" => o.windowed = true,
            "--threads" => o.threads = Some(value("--threads")?.parse()?),
            "--shell" => o.shell = Some(value("--shell")?),
            "--trail" => o.trail = Some(value("--trail")?.parse()?),
            "--vt-replay" => o.vt_replay = Some(PathBuf::from(value("--vt-replay")?)),
            "--write-rate" => o.write_rate = Some(value("--write-rate")?.parse()?),
            "--ghost" => o.ghost = Some(value("--ghost")?.parse()?),
            "--slide" => o.slide = Some(value("--slide")?.parse()?),
            "--scan" => o.scan = Some(value("--scan")?.parse()?),
            "--cell" => o.cell = Some(value("--cell")?.parse()?),
            "--no-autoclear" => o.autoclear = false,
            "--no-enhanced" => o.enhanced = false,
            "--stats" => o.stats = true,
            "--dump-chars" => o.dump_chars = true,
            "--probe" => {
                let v = value("--probe")?;
                let (x, y) = v
                    .split_once(',')
                    .ok_or_else(|| format!("--probe wants X,Y, got {}", v))?;
                o.probe = Some((x.trim().parse()?, y.trim().parse()?));
            }
            "--quiet" => o.quiet = true,
            other if other.starts_with("--") => {
                return Err(format!("unknown option {}", other).into())
            }
            other => o.files.push(PathBuf::from(other)),
        }
    }
    Ok(Some(o))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4010 coordinate on the wire: high-Y, low-Y, high-X, low-X.
    fn coord(out: &mut Vec<u8>, x: i32, y: i32) {
        out.push(0x20 + (y >> 5) as u8);
        out.push(0x60 + (y & 31) as u8);
        out.push(0x20 + (x >> 5) as u8);
        out.push(0x40 + (x & 31) as u8);
    }

    /// The phosphor is aged *between* the strokes of one interval, so a stroke
    /// written at the end of a frame is still hot while one written at the
    /// start of the same frame has cooled.  Feeding the whole batch and then
    /// ageing it by the whole interval - which is what `advance` used to do -
    /// leaves every stroke in the batch equally cold and wipes out the write
    /// beam's trail at any frame rate below about 1000 fps.
    #[test]
    fn strokes_written_later_in_a_frame_are_hotter() {
        let mut data = vec![0x1d]; // GS: graph mode
        coord(&mut data, 100, 400);
        for i in 0..40 {
            coord(&mut data, 100 + i * 8, 400);
        }

        let mut opts = Options::default();
        opts.baud = 1920.0; // 192 characters per second
        let mut p = Player::new(data, Device::Tek4010, &opts);
        // 0.25 s of line time is 48 bytes, i.e. twelve coordinates, so the
        // second vector has been cooling for three quarters of a flash time
        // constant by the time the eleventh is written
        p.advance(0.25);
        let first = p.tube.rgb_at(104, 400).unwrap();
        let last = p.tube.rgb_at(172, 400).unwrap();
        assert!(
            last.0 > first.0 + 20,
            "the trailing strokes must still be hot: first {:?}, last {:?}",
            first,
            last
        );
        assert!(
            last.1 >= first.1,
            "the flash must not dim the stroke: first {:?}, last {:?}",
            first,
            last
        );
    }
}
