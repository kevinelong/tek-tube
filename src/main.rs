//! tek-tube - a Tektronix 4010/4014 storage tube display emulator.
//!
//! Plays back plot files (the Tektronix "PLOT10" / 4010 graphics protocol that
//! the 1970s mainframe and minicomputer plotting packages sent down a serial
//! line) onto a simulated direct-view bistable storage tube: vectors are
//! written by a hot write beam that flashes almost white and settles into the
//! acid green stored image, exactly like the real terminal.

mod font;
mod panel;
mod protocol;
mod terminal;
mod tube;
mod vt;

use font::Face;
use protocol::{Decoder, Device, Write};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::sync::LazyLock;
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
    /// Print the shells the setup configured (`shells.txt`) and exit.
    list_shells: bool,
    /// Print the version and exit.
    version: bool,
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
    /// Paint glyphs as a dot matrix this many dots tall, the way a terminal's
    /// character ROM is built.  Zero strokes them, which is what a storage
    /// tube's beam does.
    dots: Option<usize>,
    /// The phone camera the source footage was shot on: blur and grain only, no
    /// contrast curve.  What a filmed terminal needs.
    lens: Option<f32>,
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
            list_shells: false,
            version: false,
            trail: None,
            vt_replay: None,
            write_rate: None,
            ghost: None,
            slide: None,
            scan: None,
            dots: None,
            lens: None,
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
  --dots N                  paint glyphs as a dot matrix N dots tall, the way a
                            terminal's character ROM is built, instead of tracing
                            the strokes a storage tube writes (0, the default,
                            traces them; a ROM was about 8 wide by 15 tall).  The
                            grid is the cell's and the same for every glyph, so
                            capitals and baselines line up
  --wyse                    the raster-terminal look in one flag: a black face,
                            a 15 dot matrix face, the scan's stripes, the page
                            changed a screenful at a time with the one before it
                            left glowing, no beam writer, and a 19,200 baud link.
                            Equivalent to --write-rate 0 --slide 0 --ghost 0.1
                            --scan 0.5 --dots 15 --ambient 0 --trail 0.2
                            --baud 19200; a later flag overrides any of them
  --lens F                  the phone camera the source footage was shot on: F
                            is the lens blur and grain, 0 (the default) is the
                            sharp device.  It applies no contrast curve, unlike
                            --camera, so it is what a *filmed* terminal needs -
                            the tube as a camera saw it, without a lens model
                            calibrated through a different scene
  --shell CMD               run a shell under a pseudoconsole and render the
                            session on the tube instead of playing plot files,
                            e.g. --shell powershell.exe.  CMD may also be the
                            *name* of a shell the Windows setup offered
                            (shells.txt), which is what the Start Menu entries
                            pass, e.g. --shell \"Git Bash\"; a command line is
                            quoted the way Windows quotes it, so a program under
                            \"Program Files\" keeps its quotes.  The page is the
                            tube's own 74x35.  Ctrl+Shift+Q quits, or exit the
                            shell; ESC goes to the shell as ESC
  --list-shells             print the shells the setup configured, and which of
                            them a bare tek-tube.exe runs
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
  --version print the version and exit

Without --png/--png-seq a window opens (ESC quits).

With no file and no --shell the tube runs a terminal on the setup's default shell
(shells.txt), or on the platform's own shell when that file is not there.

In terminal mode the type advertised to the shell (and so to anything it ssh's to)
is vt100, forced rather than inherited; TEK_TUBE_TERM overrides it.
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
    if opts.version {
        // The version the binary was built from, not a file's metadata: this is
        // what the Windows setup reports and what a bug report wants.
        println!("tek-tube {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if opts.list_shells {
        list_shells();
        return Ok(());
    }
    run(opts)
}

/// One shell the setup offered: the name it is listed and referred to by, and the
/// command line that starts it.
#[derive(PartialEq, Debug)]
struct ShellProfile {
    name: String,
    command: String,
}

/// The shells `tools/setup.ps1` wrote into `shells.txt`, and which of them a bare
/// `tek-tube.exe` runs.
///
/// The file is the installer's own text:
///
/// ```text
/// # tek-tube shells: one per line, `name = command line`
/// default = Windows PowerShell
/// Windows PowerShell = C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe
/// Git Bash = "C:\Program Files\Git\bin\bash.exe" --login -i
/// WSL (Ubuntu) = wsl.exe -d Ubuntu
/// ```
///
/// Every line is `name = command line`, split on the *first* `=` so a command may
/// contain one, and `#` starts a comment.  Names are matched without case, so what
/// the setup capitalised is nothing a Start Menu entry or a typed `--shell bash`
/// has to know.  Anything unreadable is dropped rather than reported: a shell
/// profile is a convenience, and none of them is worth refusing to start over.
#[derive(Default, PartialEq, Debug)]
struct Shells {
    profiles: Vec<ShellProfile>,
    default: Option<String>,
}

impl Shells {
    fn parse(text: &str) -> Shells {
        let mut shells = Shells::default();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((name, command)) = line.split_once('=') else {
                continue;
            };
            let (name, command) = (name.trim(), command.trim());
            if name.is_empty() || command.is_empty() {
                continue;
            }
            if name.eq_ignore_ascii_case("default") {
                shells.default = Some(command.to_string());
            } else {
                shells.profiles.push(ShellProfile {
                    name: name.to_string(),
                    command: command.to_string(),
                });
            }
        }
        shells
    }

    /// The configured profiles, or none at all: a missing or unreadable file
    /// leaves the platform's own shell as the default, which is what the program
    /// did before this file existed.
    fn load() -> Shells {
        shells_path()
            .and_then(|path| fs::read_to_string(path).ok())
            .map(|text| Shells::parse(&text))
            .unwrap_or_default()
    }

    /// The command line a name starts, or `None` if no profile goes by that name.
    fn command_for(&self, name: &str) -> Option<&str> {
        self.profiles
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .map(|p| p.command.as_str())
    }

    /// What a bare `tek-tube.exe` runs: the shell the file names as `default`, else
    /// the first one it lists, else nothing - and then `default_shell` falls back
    /// to what the platform has.
    fn default_command(&self) -> Option<&str> {
        self.default
            .as_deref()
            .and_then(|name| self.command_for(name))
            .or_else(|| self.profiles.first().map(|p| p.command.as_str()))
    }
}

/// `--list-shells`: what the setup configured, and which of them a bare
/// `tek-tube.exe` runs - so "why does that Start Menu entry run *that*" is one
/// command, not a hunt through `%APPDATA%`.
fn list_shells() {
    let shells = Shells::load();
    let Some(path) = shells_path() else {
        println!("no config directory: no shells configured");
        return;
    };
    println!("{}", path.display());
    if shells.profiles.is_empty() {
        println!("no shells configured: a bare tek-tube.exe runs the platform's own");
        return;
    }
    let default = shells.default_command();
    for p in &shells.profiles {
        let mark = if Some(p.command.as_str()) == default {
            "*"
        } else {
            " "
        };
        println!("{mark} {} = {}", p.name, p.command);
    }
}

/// The shell to run when the command line names nothing: the setup's choice, or
/// the platform's own.
///
/// `shells.txt` comes first because it is what the Start Menu entries run and what
/// the user's own edit of that file expresses.  After that, an absolute path,
/// because a bare `powershell.exe` does not resolve when the program is launched
/// from Explorer or a shortcut - which is exactly when there are no arguments to
/// go on.  Nothing found means no default, and the usage is printed the way it
/// always was.
fn default_shell() -> Option<String> {
    if let Some(command) = Shells::load().default_command() {
        return Some(command.to_string());
    }
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    for candidate in [
        format!("{root}\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"),
        format!("{root}\\System32\\cmd.exe"),
    ] {
        if std::path::Path::new(&candidate).exists() {
            return Some(candidate);
        }
    }
    std::env::var("SHELL").ok().filter(|s| !s.is_empty())
}

/// Everything after the command line: choose a mode and go.
///
/// A function rather than the body of `main` so the terminal can also be started
/// from the test harness, which on this machine is the only kind of binary the
/// application control policy will run - see the `terminal` test at the end.
///
/// The options are owned and mutable, because the terminal's options panel edits
/// them while it runs.
fn run(mut opts: Options) -> Result<(), Box<dyn Error>> {
    if let Some(n) = opts.threads {
        tube::set_render_threads(n);
    }
    if let Some(command) = opts.shell.clone() {
        return run_shell(&command, &mut opts);
    }
    if let Some(path) = opts.vt_replay.clone() {
        return replay_terminal(&path, &opts);
    }
    if opts.files.is_empty() {
        // Nothing named: a terminal on the platform's shell.  This is the mode
        // the machine is for, and the one the harness has always defaulted to;
        // printing the usage and exiting was the only way to launch it that did
        // nothing at all.
        if let Some(command) = default_shell() {
            return run_shell(&command, &mut opts);
        }
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

    if run_window(&mut player, tw, th, &mut opts, fullscreen, screen)? {
        return relaunch_process(&opts);
    }
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
    if let Some(v) = opts.dots {
        tube.dot_rows = v;
    }
    tube.phosphor.camera = opts.camera;
    if opts.camera {
        // the capture model is the camera *and* its lens
        tube.phosphor.cam_blur = tube::CAM_BLUR;
        tube.phosphor.cam_noise = tube::CAM_NOISE;
    }
    if let Some(v) = opts.lens {
        tube.phosphor.cam_blur = v.max(0.0);
    }
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
        let mut player = Player {
            data,
            cursor: 0,
            decoder,
            tube,
            rate: 0.0,
            credit: 0.0,
            instant: true,
            done: false,
        };
        player.set_rate(opts);
        player
    }

    /// Re-derive the link rate from the options, so the panel's baud and speed
    /// move a running plot the way the command line moves a fresh one.  The
    /// credit already banked is left alone: the change takes effect from here.
    fn set_rate(&mut self, opts: &Options) {
        let (rate, instant) = if opts.baud <= 0.0 || opts.speed <= 0.0 {
            (0.0, true)
        } else {
            (opts.baud / 10.0 * opts.speed, false)
        };
        self.rate = rate;
        self.instant = instant;
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

fn slider(name: &'static str, min: f32, max: f32, value: f32, structural: bool) -> panel::Row {
    panel::Row {
        name,
        kind: panel::Kind::Slider { min, max },
        value,
        structural,
        launch: false,
    }
}

fn toggle(name: &'static str, value: f32, structural: bool) -> panel::Row {
    panel::Row {
        name,
        kind: panel::Kind::Toggle,
        value,
        structural,
        launch: false,
    }
}

/// A chooser: a row whose value is an index into a list of names.
///
/// The preset row is the only one the panel offers.  A preset is not a value of
/// its own - it is the *state* the other rows are in - so the index here is
/// derived from them (see `preset_index`) rather than remembered, and only the
/// press applies anything.
///
/// The list is owned because the preset chooser is the built-ins plus the names
/// the user has saved, which the compiler cannot know.
fn choice(name: &'static str, list: &[String], index: usize) -> panel::Row {
    panel::Row {
        name,
        kind: panel::Kind::Choice(list.to_vec()),
        value: index as f32,
        structural: false,
        launch: false,
    }
}

/// A chooser the running tube can honour by writing the page again in the new
/// rules: the face.
///
/// A face is not fixed when the process starts after all - the tube holds it, and
/// the terminal can clear its record and write the page again - so this is a
/// *structural* row rather than a relaunch one.  Changing it costs a page, not a
/// session: a storage tube cannot rewrite the glyphs already stored, so the page is
/// wiped and drawn again in the new face, and the shell underneath never notices.
fn structural_choice(name: &'static str, list: &[&str], index: usize) -> panel::Row {
    panel::Row {
        name,
        kind: panel::Kind::Choice(owned(list)),
        value: index as f32,
        structural: true,
        launch: false,
    }
}

/// A chooser whose value can only be honoured by relaunching the program: the
/// target and the geometry are fixed when the process starts, so the panel offers
/// them but the settle builds a fresh command line instead of applying the number
/// to a machine that cannot read it.  Only a *press* does that; a step with the
/// wheel browses.
fn launch_choice(name: &'static str, list: &[&str], index: usize) -> panel::Row {
    panel::Row {
        name,
        kind: panel::Kind::Choice(list.iter().map(|s| s.to_string()).collect()),
        value: index as f32,
        structural: false,
        launch: true,
    }
}

/// A compiled-in list of names as an owned one, for the choosers that need the
/// panel's owned-list form.
fn owned(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// The index of a name in a list, or one past the end when it is not there -
/// which the panel shows as `custom`.
fn index_of(list: &[&str], name: &str) -> usize {
    list.iter().position(|s| *s == name).unwrap_or(list.len())
}

/// The `--font` spelling of a face, the form the parser accepts.
fn face_name(face: Face) -> &'static str {
    match face {
        Face::Simplex => "simplex",
        Face::Mono => "mono",
        Face::Gothic => "gothic",
    }
}

/// The `--device` spelling of a target.  Written out rather than taken from
/// `Device::name`, which is prose for the startup banner and not a model number.
fn device_name(dev: Device) -> &'static str {
    match dev {
        Device::Tek4010 => "4010",
        Device::Tek4014 => "4014",
    }
}

/// The terminal panel's relaunch rows.
///
/// `--font` was left out of the panel while the panel could only change live
/// values: the face is fixed when the tube is built, so a row that moved an index
/// nothing read would have been a lie.  As a relaunch row it is honest - it
/// genuinely takes effect at launch - and it is the only way a running session can
/// change face at all.
const FONTS: &[&str] = &["simplex", "mono", "gothic"];

/// The slots a save writes into.  There is no text entry on the panel, so a saved
/// preset is named by the slot it went into rather than by anything the user
/// types.
const SAVE_SLOTS: &[&str] = &["user 1", "user 2", "user 3", "user 4"];

/// The plot panel's relaunch rows: a different target is a different tube and a
/// different decoder, and `--zoom` and `--shrink` size the window, which cannot
/// change once it is made.
const DEVICES: &[&str] = &["4010", "4014"];
const ZOOMS: &[&str] = &["1", "2", "3", "4"];
/// `auto` first: the fit decides, which is what an unset `--shrink` means.
const SHRINKS: &[&str] = &["auto", "1", "2", "3", "4", "5", "6", "8"];

/// The built-in presets the chooser offers, in the order it lists them.  The
/// user's saved presets follow them; one past the end of the whole list is
/// `custom`: the rows match none of them.
const PRESETS: &[&str] = &["wyse", "tek"];

/// One preset: a name and the panel rows it sets.  The row names are owned
/// because a saved one comes from the user's file, which may name a row this
/// build has not got - that row is ignored rather than fatal.
#[derive(Clone)]
struct Preset {
    name: String,
    rows: Vec<(String, f32)>,
}

/// Every preset the chooser can offer: the built-ins, then the user's saved ones.
///
/// Carried by the caller rather than read from disk on every settle - the file
/// changes only when a preset is saved, and the caller refreshes this then.
/// Nothing here can fail: a missing or corrupt file is an empty list of user
/// presets, and the built-ins are always there.
struct Presets {
    users: Vec<Preset>,
}

impl Presets {
    /// The built-ins alone: what the panel offers when there is no config
    /// directory, and what the tests use so the machine's own saved presets
    /// cannot change what they measure.
    fn builtins() -> Self {
        Presets { users: Vec::new() }
    }

    /// Read the user's presets from the config file, if the environment names a
    /// config directory at all.
    fn load() -> Self {
        Presets {
            users: presets_path().map(|p| read_presets(&p)).unwrap_or_default(),
        }
    }

    /// The chooser's entries, in the order it lists them: the built-ins first,
    /// then the user's.
    fn names(&self) -> Vec<String> {
        PRESETS
            .iter()
            .map(|s| s.to_string())
            .chain(self.users.iter().map(|p| p.name.clone()))
            .collect()
    }

    /// The rows a named preset sets, built-in or saved, or `None` if the name is
    /// no preset at all.
    fn bundle(&self, name: &str) -> Option<Vec<(String, f32)>> {
        if let Some(rows) = preset_rows(name) {
            return Some(rows.iter().map(|(n, v)| (n.to_string(), *v)).collect());
        }
        self.users
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.rows.clone())
    }
}

/// Where the user's own settings live: `%APPDATA%\tek-tube` on Windows and
/// `$XDG_CONFIG_HOME/tek-tube` (falling back to `~/.config/tek-tube`) elsewhere.
///
/// A preset or a shell profile belongs to the user, not to the repository and not
/// to whatever directory the program happened to be started in.  `None` when the
/// environment names no such directory: nothing user-owned is then read or
/// written, the panel offers the built-in presets as it always did, and a bare
/// `tek-tube.exe` runs the platform's own shell.
fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("tek-tube"))
    }
    #[cfg(not(windows))]
    {
        if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
            return Some(PathBuf::from(dir).join("tek-tube"));
        }
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config").join("tek-tube"))
    }
}

/// The user's saved panel presets: `presets.txt` in the config directory.
fn presets_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("presets.txt"))
}

/// The shells the Windows setup offered: `shells.txt` in the same directory,
/// written by `tools/setup.ps1` and read here, so a Start Menu entry can pass a
/// name and changing which shell it runs is an edit to one text file.
fn shells_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("shells.txt"))
}

/// Read the user's presets.  A missing file, or one that has been corrupted, is
/// an empty list: a saved preset is a convenience, and none of them is worth
/// refusing to start over.
fn read_presets(path: &std::path::Path) -> Vec<Preset> {
    fs::read_to_string(path)
        .map(|t| parse_presets(&t))
        .unwrap_or_default()
}

/// Write the user's presets, making the directory if it is not there yet.
fn write_presets(path: &std::path::Path, presets: &[Preset]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, format_presets(presets))
}

/// The file's text: one preset per line, the name then tab separated `row=value`
/// pairs, so a name with a space in it ("user 1") needs no quoting.  Lines
/// starting `#` are comments, which is also where the format documents itself.
fn format_presets(presets: &[Preset]) -> String {
    let mut s = String::from(
        "# tek-tube panel presets: one per line, a name then tab separated row=value pairs\n",
    );
    for p in presets {
        s.push_str(&p.name);
        for (name, value) in &p.rows {
            s.push('\t');
            s.push_str(name);
            s.push('=');
            s.push_str(&value.to_string());
        }
        s.push('\n');
    }
    s
}

/// Parse the format `format_presets` writes.  Anything malformed is dropped
/// rather than returned as an error: a value that is not a number is left out of
/// its preset, and a line with no usable rows at all is not a preset at all - an
/// empty bundle would match every machine.
fn parse_presets(text: &str) -> Vec<Preset> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let name = fields.next()?.trim();
            if name.is_empty() {
                return None;
            }
            let rows = fields
                .filter_map(|f| {
                    let (row, value) = f.split_once('=')?;
                    let value: f32 = value.trim().parse().ok()?;
                    Some((row.trim().to_string(), value))
                })
                .collect::<Vec<(String, f32)>>();
            (!rows.is_empty()).then(|| Preset {
                name: name.to_string(),
                rows,
            })
        })
        .collect()
}

/// Whether a row is a view or a gesture rather than state a preset can set.  The
/// preset chooser is derived from the other rows, the slot row names where a save
/// goes, and the save row is the act of saving: a preset that carried any of them
/// would be recording a gesture as a value.
fn is_gesture(name: &str) -> bool {
    matches!(name, "preset" | "slot" | "save")
}

/// Save the panel's current values as a user preset named `slot`, writing `path`.
///
/// Everything the panel sets except the chooser and the save row themselves -
/// those are a view and a gesture, not state - so applying the saved preset later
/// restores exactly what the machine was set to.  The file is written before the
/// in-memory list is updated, so a failed write leaves the chooser unchanged.  The
/// path is a parameter rather than resolved here so a test can point it at a file
/// of its own instead of the user's real one.
fn save_user_preset(
    path: &std::path::Path,
    slot: &str,
    rows: &[panel::Row],
    presets: &mut Presets,
) -> Result<(), String> {
    let bundle: Vec<(String, f32)> = rows
        .iter()
        .filter(|r| !is_gesture(r.name))
        .map(|r| (r.name.to_string(), r.value))
        .collect();
    let mut users = presets.users.clone();
    match users.iter_mut().find(|p| p.name == slot) {
        Some(p) => p.rows = bundle,
        None => users.push(Preset {
            name: slot.to_string(),
            rows: bundle,
        }),
    }
    write_presets(path, &users).map_err(|e| format!("{}: {e}", path.display()))?;
    presets.users = users;
    Ok(())
}

/// Rebuild the preset chooser after a save: the list is the built-ins plus the
/// saved names, so a freshly written preset must appear in it or the note would
/// claim a save the chooser cannot offer.
fn refresh_preset_choices(rows: &mut [panel::Row], presets: &Presets) {
    if let Some(row) = rows.iter_mut().find(|r| r.name == "preset") {
        row.kind = panel::Kind::Choice(presets.names());
    }
    sync_preset_row(rows, presets);
}

/// Which preset the rows currently match, or the list's length for "custom".
///
/// A preset is derived, never stored.  Only the rows a preset *names* are
/// compared, so a variable a preset says nothing about can never make the machine
/// read as custom, and a small tolerance absorbs the rounding of a dragged
/// slider.  A saved preset may name a row this build has not got; that row is
/// simply not compared, which is what "ignored for that option" means.  The first
/// match wins, which is the whole tie-break.  The order and length come from the
/// chooser row itself, so the index can never disagree with what is shown.
fn preset_index(rows: &[panel::Row], presets: &Presets) -> usize {
    let Some(row) = rows.iter().find(|r| r.name == "preset") else {
        return 0;
    };
    let panel::Kind::Choice(list) = &row.kind else {
        return 0;
    };
    list.iter()
        .position(|name| {
            presets.bundle(name).is_some_and(|bundle| {
                bundle.iter().all(|(row_name, want)| {
                    rows.iter()
                        .find(|r| r.name == row_name.as_str())
                        .is_some_and(|r| (r.value - want).abs() < 1e-3)
                })
            })
        })
        .unwrap_or(list.len())
}

/// Write the derived preset index into the `preset` row, so it reads the preset
/// the machine is set to, or `custom` once one variable is nudged away.
fn sync_preset_row(rows: &mut [panel::Row], presets: &Presets) {
    let index = preset_index(rows, presets);
    if let Some(row) = rows.iter_mut().find(|r| r.name == "preset") {
        row.value = index as f32;
    }
}

/// The Wyse-like preset - what `--wyse` sets, as one list, so the flag and the
/// panel's preset row cannot drift apart.
///
/// No beam writer (the glyphs just appear), a screenful per step, the page before
/// it fading, the scan's own stripes, a ROM-ish dot grid, a black face rather than
/// a lit flood field, a short glow, and a paced serial link.
const WYSE: &[(&str, f32)] = &[
    ("write-rate", 0.0),
    ("slide", 0.0),
    ("ghost", 0.1),
    ("scan", 0.5),
    ("dots", 15.0),
    ("ambient", 0.0),
    ("trail", 0.2),
    ("baud", 19200.0),
];

/// The `tek` preset: what a machine built with nothing given to it reports,
/// computed here by the same two calls a launch makes, so the preset cannot drift
/// from the defaults it means.  Built once, because the chooser reads it on every
/// settle, and the tube a launch builds is not cheap to make.
static TEK: LazyLock<Vec<(&'static str, f32)>> = LazyLock::new(|| {
    let fresh = Options::default();
    let cell = fresh.cell.unwrap_or(terminal::CELL_SCALE_DEFAULT);
    let mut tube = Tube::new(1024, 780);
    configure_terminal_tube(&mut tube, Device::Tek4010, &fresh, cell);
    raw_rows(&fresh, &tube, &Presets::builtins())
        .into_iter()
        // the chooser, the slot and the save row are views and gestures, not state
        // a preset can set: a preset naming any of them would be circular
        .filter(|r| !is_gesture(r.name))
        .map(|r| (r.name, r.value))
        .collect()
});

/// The rows a preset sets, in the panel's own names, or None if the name is not a
/// preset.
///
/// `wyse` is a list of numbers, written out.  `tek` is not: it is read off a fresh
/// machine, so it cannot drift from the defaults it means.
fn preset_rows(name: &str) -> Option<&'static [(&'static str, f32)]> {
    match name {
        "wyse" => Some(WYSE),
        "tek" => Some(&TEK),
        _ => None,
    }
}

/// Apply a preset to the panel's rows: everything a preset press means.  A saved
/// preset goes through the same path as a built-in, so it cannot mean something
/// different from one, and a row it names that this build has not got is left out
/// rather than fatal.  Returns true if the page has to be written again, which
/// only the cell size forces.
fn apply_preset_rows(rows: &mut [panel::Row], name: &str, presets: &Presets) -> bool {
    let Some(bundle) = presets.bundle(name) else {
        return false;
    };
    let cell_before = rows.iter().find(|r| r.name == "cell").map(|r| r.value);
    for (row_name, value) in &bundle {
        if let Some(row) = rows.iter_mut().find(|r| r.name == row_name.as_str()) {
            row.value = *value;
        }
    }
    let cell_after = rows.iter().find(|r| r.name == "cell").map(|r| r.value);
    cell_before != cell_after
}

/// The options panel's rows: the variables a terminal run honours.
///
/// Read from the options, and for the ones the tube is *holding* (the phosphor
/// gains, the scan, the ghost, the dot grid) from the tube, so the panel shows
/// what the machine is set to rather than a parallel table of defaults.
fn panel_rows(opts: &Options, tube: &Tube, presets: &Presets) -> Vec<panel::Row> {
    let mut rows = raw_rows(opts, tube, presets);
    // the chooser is a view of the rest, so it can only be filled in once they
    // have all been built
    sync_preset_row(&mut rows, presets);
    rows
}

/// The same rows, without the chooser's derived value filled in.
///
/// Split out because reading a preset off a fresh machine - which is how `tek` is
/// defined - must not come back through `panel_rows` and ask `preset_index`, which
/// asks *it*: that recursion overflowed the stack the moment the chooser was filled
/// in at construction.
fn raw_rows(opts: &Options, tube: &Tube, presets: &Presets) -> Vec<panel::Row> {
    let ph = &tube.phosphor;
    // Only the variables a *terminal* run honours.  --device, --zoom, --shrink,
    // --text-scale, --threads, --no-autoclear and --no-enhanced belong to the plot
    // path (there is no 12 bit coordinate stream and no file being decoded here),
    // so a control for them would move a number that nothing reads.  --font *is*
    // honoured here: the tube holds the face, so it is a structural row - the tube
    // is configured with the new face and the page is written again - and no part
    // of a session is lost by changing it.
    vec![
        // A preset is a *state*, not a value, so the rows start with one chooser
        // that shows which preset the values below it match.  It is filled in
        // once they exist, so the placeholder index here is never read.
        choice("preset", &presets.names(), 0),
        // Saving is a gesture the panel can express without a text entry: the
        // slot row picks where it goes and the save row performs it.  A press on
        // `save` writes the current rows into the shown slot, tells the note line
        // which one, and flips itself back off, so it reads as a press rather than
        // a state.
        choice("slot", &owned(SAVE_SLOTS), 0),
        toggle("save", 0.0, false),
        slider("baud", 0.0, 115200.0, opts.baud, false),
        slider("beam", 0.1, 2.0, opts.beam, false),
        slider("glow", 0.0, 2.0, opts.glow, false),
        slider("settle", 0.0, 8.0, opts.settle, false),
        slider("store-gain", 0.0, 2.0, ph.stored_gain, false),
        slider("fresh-gain", 0.0, 2.0, ph.fresh_gain, false),
        slider("sat", 0.0, 4.0, opts.sat.unwrap_or(1.0), false),
        slider("ambient", 0.0, 0.4, ph.ambient, false),
        slider("mottle", 0.0, 0.4, ph.mottle, false),
        slider("bloom", 0.0, 0.5, ph.bloom, false),
        slider("bloom-wide", 0.0, 0.3, ph.bloom_wide, false),
        slider("lens", 0.0, 1.2, ph.cam_blur, false),
        toggle("camera", opts.camera as u8 as f32, false),
        structural_choice("font", FONTS, index_of(FONTS, face_name(opts.font))),
        slider("scan", 0.0, 0.95, ph.scan_depth, false),
        slider("dots", 0.0, 32.0, tube.dot_rows as f32, false),
        slider("trail", 0.0, 2.0, opts.trail.unwrap_or(1.1), false),
        slider(
            "write-rate",
            0.0,
            8000.0,
            opts.write_rate.unwrap_or(terminal::WRITE_RATE_DEFAULT),
            false,
        ),
        slider("ghost", 0.0, 2.0, tube.ghost_tau, false),
        slider(
            "slide",
            0.0,
            120.0,
            opts.slide.unwrap_or(terminal::SLIDE_RATE),
            false,
        ),
        slider(
            "cell",
            0.5,
            3.0,
            opts.cell.unwrap_or(terminal::CELL_SCALE_DEFAULT),
            true,
        ),
        slider(
            "exit-after",
            0.0,
            600.0,
            opts.exit_after.unwrap_or(0.0),
            false,
        ),
    ]
}

/// Set one named option.  Returns false if this panel has no handler for the name.
///
/// The single mapping from a row name to an option: the panel applies its rows
/// through it, `--wyse` sets its options through it, and a preset goes through it
/// after its values have been written into the panel's rows.  One mapping means a
/// preset cannot mean one thing from the command line and another from the panel.
fn apply_value(opts: &mut Options, name: &str, v: f32) -> bool {
    match name {
        "baud" => {
            opts.baud = v;
            opts.baud_given = true;
        }
        "beam" => opts.beam = v,
        "glow" => opts.glow = v,
        "speed" => opts.speed = v,
        "settle" => opts.settle = v,
        "store-gain" => opts.store_gain = Some(v),
        "fresh-gain" => opts.fresh_gain = Some(v),
        "sat" => opts.sat = Some(v),
        "ambient" => opts.ambient = Some(v),
        "mottle" => opts.mottle = Some(v),
        "bloom" => opts.bloom = Some(v),
        "bloom-wide" => opts.bloom_wide = Some(v),
        "lens" => opts.lens = Some(v),
        "camera" => opts.camera = v > 0.5,
        "scan" => opts.scan = Some(v),
        "dots" => opts.dots = Some(v.round().max(0.0) as usize),
        "trail" => opts.trail = Some(v),
        // 0 is an instant page, as on the command line: the pace is cleared
        // rather than left alone
        "write-rate" => opts.write_rate = Some(v),
        "ghost" => opts.ghost = Some(v),
        "slide" => opts.slide = Some(v),
        "cell" => opts.cell = Some(v),
        "exit-after" => opts.exit_after = if v <= 0.0 { None } else { Some(v) },
        // The relaunch rows.  Their value is a chooser index - the target is a
        // `Device`, not a name - so the index becomes the option here, where every
        // other row name becomes its option.  An index past the end is `custom`,
        // which names no value and so leaves the option alone.
        "device" => {
            if let Some(name) = DEVICES.get(v.round().max(0.0) as usize) {
                opts.device = match *name {
                    "4010" => Device::Tek4010,
                    _ => Device::Tek4014,
                };
            }
        }
        "font" => {
            if let Some(name) = FONTS.get(v.round().max(0.0) as usize) {
                opts.font = match *name {
                    "mono" => Face::Mono,
                    "gothic" => Face::Gothic,
                    _ => Face::Simplex,
                };
            }
        }
        "zoom" => {
            if let Some(name) = ZOOMS.get(v.round().max(0.0) as usize) {
                opts.zoom = name.parse().unwrap_or(opts.zoom);
            }
        }
        "shrink" => {
            // `auto` is the entry that names no number: the option goes back to
            // letting the fit decide what fits, which is what an unset --shrink
            // means
            if let Some(name) = SHRINKS.get(v.round().max(0.0) as usize) {
                opts.shrink = name.parse().ok();
            }
        }
        // The chooser holds an index, not an option: pressing it applies a whole
        // preset, and the caller does that through the rows the preset names.
        // Handled so the mapping reports the name - the row is applied like any
        // other - but it moves nothing itself.  The slot and save rows are the same:
        // one names a slot and the other is the act of writing the file, and the
        // caller does that.
        "preset" | "slot" | "save" => {}
        _ => return false,
    }
    true
}

/// Copy the panel's values back into the options.  Returns true if anything changed
/// that needs the tube reconfigured and the page written again, rather than just
/// recomposed.
fn apply_rows(opts: &mut Options, rows: &[panel::Row]) -> bool {
    let mut structural = false;
    for row in rows {
        // The call has to be *outside* the assertion.  Written as
        // `debug_assert!(apply_value(...))` - which is how this started - the
        // whole expression is compiled away in release, so nothing was applied:
        // the panel moved its rows and the machine never changed, which cost an
        // afternoon of chasing a live "the settle does nothing" that only the
        // release build had.
        let applied = apply_value(opts, row.name, row.value);
        // A control the panel offers but nothing applies is a slider that moves a
        // number nothing reads, which is worse than no slider at all.
        debug_assert!(applied, "panel row `{}` has no handler", row.name);
        structural |= row.structural;
    }
    structural
}

/// The options panel's rows for a running plot file.
///
/// A plot is not a terminal, so this is not the terminal's row set: it is the
/// variables the *plot* path reads, or can be made to read, while it plays.  The
/// tube's own appearance is live because the render works from the tube rather
/// than from the options, and the settle reconfigures the tube from the rows: the
/// beam, the glow, the phosphor gains, the scan, the dot grid, the camera and its
/// lens all take effect on the next frame.  The link (`baud`, `speed`) is a
/// property of the player, which the settle re-derives, and `exit-after` is read
/// every frame.
///
/// The target, the zoom and the shrink are here as *relaunch* rows: each is a
/// different tube, a different window or a different buffer, all decided when the
/// process starts, so the panel offers them but a press builds a fresh command
/// line instead of moving a number the running machine cannot read.
///
/// What is deliberately *not* here:
///
/// * `font` and `text-scale`, `autoclear`, `enhanced` - they change how *future*
///   bytes are drawn or decoded, so on a page already drawn they do nothing.
/// * `png`, `png-at`, `png-seq`, `settle`, `frames`, `frame-dt`, `probe`,
///   `stats`, `dump-chars` - there is no windowless render here at all.
/// * the terminal's own rows (`trail`, `ghost`, `slide`, `write-rate`, `cell`) -
///   the plot path never reads them, so a control for them would move a number
///   nothing reads.
/// * the preset chooser - the presets are bundles of *terminal* rows, and the
///   plot path honours none of them as a bundle.
fn plot_rows(opts: &Options, tube: &Tube) -> Vec<panel::Row> {
    let ph = &tube.phosphor;
    vec![
        launch_choice(
            "device",
            DEVICES,
            index_of(DEVICES, device_name(opts.device)),
        ),
        launch_choice("zoom", ZOOMS, index_of(ZOOMS, &opts.zoom.to_string())),
        launch_choice("shrink", SHRINKS, shrink_index(opts.shrink)),
        slider("baud", 0.0, 115200.0, opts.baud, false),
        slider("speed", 0.0, 8.0, opts.speed, false),
        slider("beam", 0.1, 2.0, opts.beam, false),
        slider("glow", 0.0, 2.0, opts.glow, false),
        slider("store-gain", 0.0, 2.0, ph.stored_gain, false),
        slider("fresh-gain", 0.0, 2.0, ph.fresh_gain, false),
        slider("sat", 0.0, 4.0, opts.sat.unwrap_or(1.0), false),
        slider("ambient", 0.0, 0.4, ph.ambient, false),
        slider("mottle", 0.0, 0.4, ph.mottle, false),
        slider("bloom", 0.0, 0.5, ph.bloom, false),
        slider("bloom-wide", 0.0, 0.3, ph.bloom_wide, false),
        slider("scan", 0.0, 0.95, ph.scan_depth, false),
        slider("dots", 0.0, 32.0, tube.dot_rows as f32, false),
        slider("lens", 0.0, 1.2, ph.cam_blur, false),
        toggle("camera", opts.camera as u8 as f32, false),
        slider(
            "exit-after",
            0.0,
            600.0,
            opts.exit_after.unwrap_or(0.0),
            false,
        ),
    ]
}

/// The shrink chooser's index: `auto` (0) when the fit is left to decide, else
/// the entry for that number, or one past the end - `custom` - when the number is
/// not one the list offers.
fn shrink_index(shrink: Option<i32>) -> usize {
    match shrink {
        None => 0,
        Some(n) => 1 + index_of(&SHRINKS[1..], &n.to_string()),
    }
}

/// The command line that reproduces these options, for a relaunch.
///
/// Read from the options, not from the line this process was started with: a
/// value set by hand in the panel exists only in the options, and rebuilding from
/// the original line would silently discard it, making the panel a thing you
/// could use once.  Only options that are *set* are written out, so an option left
/// at its default is left to the default rather than frozen at the value this
/// build happens to have.
fn program_args(opts: &Options) -> Vec<String> {
    fn val<T: ToString>(a: &mut Vec<String>, flag: &str, v: T) {
        a.push(flag.to_string());
        a.push(v.to_string());
    }
    fn opt<T: ToString>(a: &mut Vec<String>, flag: &str, v: Option<T>) {
        if let Some(v) = v {
            val(a, flag, v);
        }
    }
    fn path_arg(a: &mut Vec<String>, flag: &str, v: Option<&PathBuf>) {
        if let Some(p) = v {
            a.push(flag.to_string());
            a.push(p.display().to_string());
        }
    }

    let mut a: Vec<String> = Vec::new();
    val(&mut a, "--device", device_name(opts.device));
    // only when it was given: an unpaced terminal is not the same as one paced at
    // the default, so a baud that was never named must stay unnamed
    if opts.baud_given {
        val(&mut a, "--baud", opts.baud);
    }
    val(&mut a, "--speed", opts.speed);
    val(&mut a, "--glow", opts.glow);
    val(&mut a, "--beam", opts.beam);
    val(&mut a, "--settle", opts.settle);
    path_arg(&mut a, "--png", opts.png.as_ref());
    opt(&mut a, "--png-at", opts.png_at);
    opt(&mut a, "--png-seq", opts.png_seq.clone());
    val(&mut a, "--frames", opts.frames);
    val(&mut a, "--frame-dt", opts.frame_dt);
    val(&mut a, "--zoom", opts.zoom);
    opt(&mut a, "--shrink", opts.shrink);
    if !opts.autoclear {
        a.push("--no-autoclear".to_string());
    }
    if !opts.enhanced {
        a.push("--no-enhanced".to_string());
    }
    opt(&mut a, "--store-gain", opts.store_gain);
    opt(&mut a, "--fresh-gain", opts.fresh_gain);
    opt(&mut a, "--sat", opts.sat);
    opt(&mut a, "--ambient", opts.ambient);
    opt(&mut a, "--mottle", opts.mottle);
    opt(&mut a, "--bloom", opts.bloom);
    opt(&mut a, "--bloom-wide", opts.bloom_wide);
    if opts.camera {
        a.push("--camera".to_string());
    }
    val(&mut a, "--font", face_name(opts.font));
    val(&mut a, "--text-scale", opts.text_scale);
    if opts.stats {
        a.push("--stats".to_string());
    }
    if opts.dump_chars {
        a.push("--dump-chars".to_string());
    }
    if let Some((x, y)) = opts.probe {
        val(&mut a, "--probe", format!("{x},{y}"));
    }
    opt(&mut a, "--exit-after", opts.exit_after);
    opt(&mut a, "--threads", opts.threads);
    opt(&mut a, "--trail", opts.trail);
    opt(&mut a, "--write-rate", opts.write_rate);
    opt(&mut a, "--ghost", opts.ghost);
    opt(&mut a, "--slide", opts.slide);
    opt(&mut a, "--scan", opts.scan);
    opt(&mut a, "--dots", opts.dots);
    opt(&mut a, "--lens", opts.lens);
    opt(&mut a, "--cell", opts.cell);
    if opts.quiet {
        a.push("--quiet".to_string());
    }
    a.push(
        if opts.windowed {
            "--windowed"
        } else {
            "--fullscreen"
        }
        .to_string(),
    );

    // The mode goes last, and the files after every flag, so a file whose name
    // happens to look like an option cannot be taken for one.
    if let Some(cmd) = &opts.shell {
        a.push("--shell".to_string());
        a.push(cmd.clone());
    } else if let Some(p) = &opts.vt_replay {
        a.push("--vt-replay".to_string());
        a.push(p.display().to_string());
    } else {
        for f in &opts.files {
            a.push(f.display().to_string());
        }
    }
    a
}

/// Replace this program with a fresh one carrying the options as they stand.
///
/// The panel's relaunch rows - the target, the zoom, the shrink, the face - are
/// fixed when the process starts: a different target is a different tube and a
/// different decoder, and the window's size is decided before it is made.  Rather
/// than offer a control that moves a number nothing reads, pressing one of those
/// rows builds the command line it now means (see `program_args`) and starts the
/// program again with it.
///
/// The session being replaced does not come back, which is why the panel says so
/// on its note line before this runs.  On Unix the process image is replaced
/// outright, so it is the same pid and no second console; on Windows there is no
/// exec, so a new process is started and this one exits - as close as the platform
/// allows.
fn relaunch_process(opts: &Options) -> Result<(), Box<dyn Error>> {
    let exe = std::env::current_exe()?;
    let args = program_args(opts);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&exe).args(&args).exec();
        // exec only returns on failure, so reaching here is the error case
        Err(format!("could not relaunch {}: {err}", exe.display()).into())
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new(&exe)
            .args(&args)
            .spawn()
            .map_err(|e| format!("could not relaunch {}: {e}", exe.display()))?;
        std::process::exit(0);
    }
}

/// Terminal mode: run a shell under a pseudoconsole and render it on the tube.
fn run_shell(command: &str, opts: &mut Options) -> Result<(), Box<dyn Error>> {
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
    let relaunch = run_terminal_window(
        &mut term, &mut tube, dev, opts, fullscreen, screen, opts.quiet, cell,
    )?;
    // The shell has been stopped and the report printed by the time the window
    // loop returns, so a relaunch here starts the fresh process with a clean
    // exit behind it rather than orphaning a pty in a running console.
    if relaunch {
        return relaunch_process(opts);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_terminal_window(
    term: &mut terminal::Terminal,
    tube: &mut Tube,
    dev: Device,
    opts: &mut Options,
    fullscreen: bool,
    screen: Option<(usize, usize)>,
    quiet: bool,
    cell_scale: f32,
) -> Result<bool, Box<dyn Error>> {
    use minifb::{Key, MouseButton, MouseMode, ScaleMode, Window, WindowOptions};
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
    // The presets the chooser offers, read once before the F11 loop: the file
    // changes only when a preset is saved, and a save refreshes this, so a save
    // survives a rebuild of the window.
    let mut presets = Presets::load();
    // Set when a relaunch row settled.  The session ends and a fresh process is
    // started with the options, but not from here: the shell is stopped and the
    // report printed first, so the ordinary shutdown path runs rather than being
    // skipped by an exec in the middle of a frame.
    let mut relaunch = false;
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
        // The composed frame is kept apart from the presented buffer: while the
        // options panel is open it is drawn over a *copy* of the frame every
        // frame, so the panel follows the mouse without recomposing the tube.
        let mut frame: Vec<u32> = Vec::new();
        let mut panel = panel::Panel::new(panel_rows(opts, tube, &presets));
        // the parameters the tube was built with, so a reconfigure from the panel
        // is a fresh configuration rather than a second coat of it
        let base_phosphor = tube.phosphor.clone();
        let mut f2_was = false;
        // The four keys that drive the panel while it is open, one `_was` edge
        // each so a held key acts once - the same edge F2 uses.  `panel_takes_keys`
        // is what the pump is told: the keyboard belongs to the panel, which is
        // also true for as long as the ESC that closed it is held down (see
        // below).
        let mut nav_was = [false; 4];
        let mut panel_takes_keys = false;
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
            if let Some(limit) = opts.exit_after {
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

            // The options panel: F2 toggles it, the mouse drives it, and the tube
            // is only reconfigured and recomposed once a drag has *settled* - a
            // fullscreen frame is ~35 ms, and recomposing on every mouse move
            // would spend a core showing the same picture with one number
            // different.  The panel's own drawing is over the frame that is
            // already there, so it still follows the mouse at full rate.
            let f2 = window.is_key_down(Key::F2);
            if f2 && !f2_was {
                panel.toggle();
                // the chooser is a view of the rows, so opening the panel is the
                // moment to take a fresh reading of what the machine is set to
                if panel.open {
                    refresh_preset_choices(&mut panel.rows, &presets);
                }
                dirty = true;
            }
            f2_was = f2;

            // The keyboard's side of the panel.  The pump turns the four
            // navigation keys into panel keys while it is open, and is told to do
            // so here: the shell beneath must never see an arrow that moved the
            // selection.  Their own down state is read here too, because a held
            // key is reported down again on every OS repeat and only the window's
            // key *state* can tell a repeat from a press.
            let live = [
                window.is_key_down(Key::Up),
                window.is_key_down(Key::Down),
                window.is_key_down(Key::Enter),
                window.is_key_down(Key::Escape),
            ];
            // The ESC that closed the panel keeps the keyboard to the panel until
            // it comes back up: those repeats would otherwise reach the shell as
            // escapes the user meant for the panel.
            panel_takes_keys = panel.open || (panel_takes_keys && live[3]);
            term.panel_open = panel_takes_keys;
            if panel.open {
                let pos = window
                    .get_mouse_pos(MouseMode::Discard)
                    .unwrap_or((0.0, 0.0));
                let down = window.get_mouse_down(MouseButton::Left);
                panel.handle(tube.w as usize, tube.h as usize, pos, down, 0.0);
                if let Some((row, press)) = panel.settle_due(Instant::now()) {
                    let name = panel.rows[row].name;
                    if panel.rows[row].launch && press {
                        // A relaunch row moves nothing the running tube can read: the
                        // target is fixed when the tube is built.  The panel has
                        // already said on its note line that the session ends.
                        // Everything set by hand is in the rows, so they are written
                        // into the options and the caller builds the fresh command
                        // line from those, not from the line this process was started
                        // with.
                        apply_rows(opts, &panel.rows);
                        relaunch = true;
                        break;
                    }
                    if panel.rows[row].launch {
                        // A *step* through such a row moved the number the panel shows
                        // and nothing else.  The note line says a press is what
                        // restarts with it; here the session carries on.
                        dirty = true;
                        continue;
                    }
                    // The chooser names the preset through what it *shows*, not
                    // through its own name, and `custom` - the value past the end
                    // of the list - names no preset at all, so it applies nothing.
                    let shown = panel.rows[row].shown();
                    let preset = if name == "preset" && presets.bundle(&shown).is_some() {
                        Some(shown)
                    } else {
                        None
                    };
                    // a preset is several variables at once, so it writes the
                    // bundle into the panel's own rows and the one mapping below
                    // applies them - which leaves the panel showing what is set
                    let rewrite = match &preset {
                        Some(chosen) => apply_preset_rows(&mut panel.rows, chosen, &presets),
                        None => false,
                    };
                    apply_rows(opts, &panel.rows);
                    // Saving is a gesture, not a value: the row names the slot and
                    // pressing it writes the current rows there.  The note says which
                    // slot and whether it worked, because the file is the only place
                    // the result can be seen from the panel.
                    if name == "save" && panel.rows[row].value > 0.5 {
                        let slot_row = panel
                            .rows
                            .iter()
                            .find(|r| r.name == "slot")
                            .map(|r| r.value)
                            .unwrap_or(0.0);
                        let slot = SAVE_SLOTS[(slot_row as usize).min(SAVE_SLOTS.len() - 1)];
                        panel.note = match presets_path() {
                            Some(path) => {
                                match save_user_preset(&path, slot, &panel.rows, &mut presets) {
                                    Ok(()) => {
                                        refresh_preset_choices(&mut panel.rows, &presets);
                                        format!("saved as {slot}")
                                    }
                                    Err(e) => format!("save failed: {e}"),
                                }
                            }
                            None => "no config directory to save into".to_string(),
                        };
                        // the button is a press, not a state: leave it off so the
                        // next settle of any row does not save again
                        if let Some(row) = panel.rows.iter_mut().find(|r| r.name == "save") {
                            row.value = 0.0;
                        }
                    }
                    // the pen and the slide rates live in the terminal rather than
                    // in the options.  A preset names them, so they are pushed;
                    // a single row is only pushed when it is the row that moved, or
                    // applying the default baud to a terminal started with an
                    // explicit write rate would quietly change the speed the moment
                    // the panel was first used.
                    if preset.is_some() {
                        if let Some(rate) = opts.write_rate {
                            term.set_write_rate(rate);
                        }
                        term.set_baud(opts.baud);
                        term.set_slide_rate(opts.slide.unwrap_or(terminal::SLIDE_RATE));
                        panel.needs_rebuild |= rewrite;
                    } else {
                        match name {
                            "baud" => term.set_baud(opts.baud),
                            "write-rate" => {
                                if let Some(rate) = opts.write_rate {
                                    term.set_write_rate(rate);
                                }
                            }
                            "slide" => {
                                term.set_slide_rate(opts.slide.unwrap_or(terminal::SLIDE_RATE))
                            }
                            _ => {}
                        }
                    }
                    // The chooser is a view of the other rows, so it is recomputed
                    // after every settle: nudging one variable away from a preset
                    // is what makes it read `custom`.
                    sync_preset_row(&mut panel.rows, &presets);
                    // the page follows from the options, so it is rebuilt through
                    // the same helpers the resize path uses
                    let cell_now = opts.cell.unwrap_or(terminal::CELL_SCALE_DEFAULT);
                    let (ww, wh) = window.get_size();
                    let (cols, rows) = terminal::page_for_window(ww, wh, cell_now);
                    term.resize(cols, rows);
                    // The phosphor is configured from the options and one of them,
                    // --sat, scales a stored chromaticity rather than replacing it,
                    // so configuring the same tube twice would compound it: every
                    // pass starts from the parameters the tube was built with.
                    tube.phosphor = base_phosphor.clone();
                    configure_terminal_tube(tube, dev, opts, terminal::cell_scale_of(tube.w, cols));
                    if panel.needs_rebuild {
                        // A structural row changed what the rendering *means* - the
                        // face, or the cell size - so the page is written again with
                        // the new rules rather than left as the old ones drew it.  The
                        // cells hold the same characters, so nothing is stale and a
                        // plain commit would draw nothing at all: `restroke` clears
                        // the record and asks for the page to be replaced.
                        term.restroke();
                        panel.needs_rebuild = false;
                    }
                    dirty = true;
                }
            }

            // The window's mouse.  The shell hears about it only when a program
            // has asked for it (DECSET 1000/1002/1003), which `report_mouse`
            // decides from the screen; while the panel is open the mouse is the
            // panel's and nothing is reported.  The wheel is read once here and
            // used twice: it is the shell's when a tracking mode owns the mouse,
            // and the scrollback's when it does not.
            let wheel = window.get_scroll_wheel().map(|(_, dy)| dy).unwrap_or(0.0);
            let mouse = terminal::MouseState {
                pos: window.get_mouse_pos(MouseMode::Discard),
                left: window.get_mouse_down(MouseButton::Left),
                middle: window.get_mouse_down(MouseButton::Middle),
                right: window.get_mouse_down(MouseButton::Right),
                wheel: terminal::wheel_notches(wheel),
            };
            term.report_mouse(mouse, tube.w, tube.h, panel.open);
            // the wheel reads the scrollback; positive y is a scroll up in minifb
            if !panel.open && !term.screen.wants_mouse() {
                let step = if wheel.abs() > 0.0 {
                    (wheel * 3.0) as i32
                } else {
                    0
                };
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
            // The panel keys the pump translated out of this frame's input, acted
            // on where they are *new*.  A held key arrives again on every OS
            // repeat, so the edge is the only thing that separates a press from a
            // key the user is still holding; `nav_was` is that edge.
            let open_before_keys = panel.open;
            for k in term.take_panel_keys() {
                let i = match k {
                    panel::PanelKey::Up => 0,
                    panel::PanelKey::Down => 1,
                    panel::PanelKey::Enter => 2,
                    panel::PanelKey::Escape => 3,
                };
                // `key` reports whether a *row* changed; a selection move needs no
                // recompose, because the panel is drawn over the frame every frame
                // while it is open, and a changed value is applied by the settle
                if !nav_was[i] && panel.key(k, tube.w as usize, tube.h as usize) {
                    dirty = true;
                }
            }
            nav_was = live;
            if panel.open != open_before_keys {
                // Escape closed it, and the buffer the window is showing still has
                // the panel drawn over the frame: a recompose is what takes it off
                dirty = true;
            }
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
            let recomposed = dirty;
            if dirty {
                // one buffer pixel per window pixel: the tube is the window
                let drew = Instant::now();
                tube.render_into(&mut frame, 1, 1);
                drawn_secs += drew.elapsed().as_secs_f32();
                frames += 1;
                dirty = false;
            }
            if panel.open {
                // the panel is drawn over a *copy* of the frame and the copy is
                // uploaded every frame: that is cheap next to a recompose, which
                // is the half that waits for the dragging to settle
                buf.clear();
                buf.extend_from_slice(&frame);
                panel.draw(&mut buf, tube.w as usize, tube.h as usize, tube);
                window.update_with_buffer(&buf, tube.w as usize, tube.h as usize)?;
            } else if recomposed {
                window.update_with_buffer(&frame, tube.w as usize, tube.h as usize)?;
            } else {
                window.update();
            }
        }
        // remember the windowed size, so F11 twice comes back to it
        if !fullscreen {
            windowed_size = window.get_size();
        }
        drop(window);
        if !toggle || relaunch {
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
    Ok(relaunch)
}

fn run_window(
    player: &mut Player,
    tw: i32,
    th: i32,
    opts: &mut Options,
    start_fullscreen: bool,
    screen: Option<(usize, usize)>,
) -> Result<bool, Box<dyn Error>> {
    use minifb::{Key, MouseButton, MouseMode, ScaleMode, Window, WindowOptions};
    let dev = player.decoder.dev;
    let title = format!(
        "tek-tube - {} {}x{}",
        dev.name(),
        player.tube.w,
        player.tube.h
    );
    let mut fullscreen = start_fullscreen;
    let mut f11_down = false;
    // The phosphor the tube was built with, taken once.  `--sat` scales a stored
    // chromaticity rather than replacing it, so a reconfigure has to start from
    // these parameters or every pass would compound the last one.
    let base_phosphor = player.tube.phosphor.clone();
    // The fit follows the mode: the monitor at fullscreen, `--shrink` or the
    // device's own scale in a window.  F11 rebuilds the window, as in the
    // terminal, and the fit is recomputed on the way.  The options are a
    // parameter rather than captured, because the panel edits them while this
    // runs.
    let fit = |fullscreen: bool, opts: &Options| -> (i32, i32, usize, usize) {
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
    // Set when a relaunch row settled, so the window loop ends and the process is
    // replaced once the report has been printed.
    let mut relaunch = false;
    loop {
        let (shrink, zoom, ow, oh) = fit(fullscreen, opts);
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
        // The composed frame is kept apart from the presented buffer, as in the
        // terminal: while the panel is open it is drawn over a *copy* of the frame
        // every frame, so the panel follows the mouse and the keyboard without
        // recomposing the tube.
        let mut frame: Vec<u32> = Vec::new();
        let started = Instant::now();
        let mut last = Instant::now();
        // Recomposing is only needed while something is actually moving: new input
        // arriving, or a transient population still fading.  A settled screen is
        // static, so the loop just presents the buffer it already has.
        let mut dirty = true;
        let mut toggle = false;
        let mut panel = panel::Panel::new(plot_rows(opts, &player.tube));
        let mut f2_was = false;
        // one edge per navigation key, so a held key acts once
        let mut nav_was = [false; 4];
        while window.is_open() {
            let down = window.is_key_down(Key::F11);
            if down && !f11_down {
                toggle = true;
                break;
            }
            f11_down = down;

            // The options panel, behind F2 as in the terminal.  The keys are read
            // here rather than through `window_input` because this loop has no
            // terminal to translate them; each acts on the frame it goes down, so
            // a held key does not repeat.
            let f2 = window.is_key_down(Key::F2);
            if f2 && !f2_was {
                panel.toggle();
                dirty = true;
            }
            f2_was = f2;
            let up = window.is_key_down(Key::Up);
            let arrow_down = window.is_key_down(Key::Down);
            let enter = window.is_key_down(Key::Enter);
            let escape = window.is_key_down(Key::Escape);
            if panel.open {
                if up && !nav_was[0] && panel.key(panel::PanelKey::Up, ow, oh) {
                    dirty = true;
                }
                if arrow_down && !nav_was[1] && panel.key(panel::PanelKey::Down, ow, oh) {
                    dirty = true;
                }
                if enter && !nav_was[2] && panel.key(panel::PanelKey::Enter, ow, oh) {
                    dirty = true;
                }
            }
            // ESC quits the window, except when the panel is open: there it closes
            // the panel, and one press must not do both.
            if escape && !nav_was[3] {
                if panel.open {
                    panel.key(panel::PanelKey::Escape, ow, oh);
                    dirty = true;
                } else {
                    break;
                }
            }
            nav_was = [up, arrow_down, enter, escape];
            if panel.open {
                // In fullscreen the buffer is centred in the monitor at 1:1, so the
                // pointer has to be moved into the buffer's own coordinates before
                // the panel hit-tests it; in a window the two coincide.
                let off = (
                    (win_w as f32 - ow as f32) / 2.0,
                    (win_h as f32 - oh as f32) / 2.0,
                );
                let pos = window
                    .get_mouse_pos(MouseMode::Discard)
                    .unwrap_or((0.0, 0.0));
                let pos = (pos.0 - off.0, pos.1 - off.1);
                let held = window.get_mouse_down(MouseButton::Left);
                panel.handle(ow, oh, pos, held, 0.0);
                if let Some((row, press)) = panel.settle_due(Instant::now()) {
                    if panel.rows[row].launch && !press {
                        // a step through a relaunch row: the number moved, nothing
                        // else did, and the note line says a press is what restarts
                        dirty = true;
                        continue;
                    }
                    // the panel's rows are the source; the one mapping writes them
                    // into the options and the tube is configured from those
                    apply_rows(opts, &panel.rows);
                    if panel.rows[row].launch {
                        // A relaunch row - the target, the zoom, the shrink - is
                        // fixed when the window is made, so there is nothing to
                        // configure: the options now carry it and the caller builds
                        // the fresh command line from them.  The panel has already
                        // said on its note line that the session ends.
                        relaunch = true;
                        break;
                    }
                    player.tube.phosphor = base_phosphor.clone();
                    configure_tube(&mut player.tube, dev, opts);
                    player.set_rate(opts);
                    dirty = true;
                }
            }

            let now = Instant::now();
            // The same wall clock as the terminal loop: a clamp here makes the
            // drawing run in slow motion on exactly the slowest frames.
            let dt = (now - last).as_secs_f32().min(1.0);
            last = now;
            if player.advance(dt) || player.tube.transients_alive() {
                dirty = true;
            }
            let recomposed = dirty;
            if dirty {
                player.tube.render_into(&mut frame, shrink, zoom);
                dirty = false;
            }
            if panel.open {
                // the panel is drawn over a copy of the frame and the copy is
                // uploaded every frame: cheap next to a recompose
                buf.clear();
                buf.extend_from_slice(&frame);
                panel.draw(&mut buf, ow, oh, &player.tube);
                window.update_with_buffer(&buf, ow, oh)?;
            } else if recomposed {
                window.update_with_buffer(&frame, ow, oh)?;
            } else {
                window.update();
            }
            if let Some(limit) = opts.exit_after {
                if started.elapsed().as_secs_f32() > limit {
                    break;
                }
            }
        }
        if !toggle || relaunch {
            break;
        }
        fullscreen = !fullscreen;
    }
    report(player, opts);
    Ok(relaunch)
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
    ///   TEK_REFONT   change the face this way, the way the panel's font row does -
    ///                the tube configured again and the page written again - and
    ///                report the ink before and after and whether the session
    ///                answered a command typed after the change (default: none)
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
        let mut opts = parse_args_from(argv.into_iter().skip(1))
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

        // The live face change the panel's `font` row makes: the tube is
        // configured with the new face and the page is written again, and the shell
        // underneath is never touched.  `TEK_REFONT=gothic` measures it - the ink
        // before and after, and whether the session still answers a command typed
        // afterwards - which is the claim the row rests on.  A comma separated list
        // cycles through them in one session, so the numbers are comparable: the
        // same page, the same shell, one face after another.
        if let Ok(want) = std::env::var("TEK_REFONT") {
            let mark = "REFONT_SESSION_ALIVE";
            let mut told = false;
            for want in want.split(',').map(str::trim).filter(|w| !w.is_empty()) {
                let face = match want {
                    "simplex" => Face::Simplex,
                    "gothic" => Face::Gothic,
                    _ => Face::Mono,
                };
                let before = lit(&mut tube);
                opts.font = face;
                configure_terminal_tube(&mut tube, opts.device, &opts, cell);
                term.restroke();
                pump!(2.0);
                println!(
                    "REFONT {want}: lit {before} -> {}, page replaced without the shell{}",
                    lit(&mut tube),
                    if told { " (still the same session)" } else { "" }
                );
                told = true;
            }
            for c in format!("echo {mark}").chars() {
                cb.add_char(c as u32);
                std::thread::sleep(std::time::Duration::from_millis(6));
            }
            cb.set_key_state(Key::Enter, true);
            std::thread::sleep(std::time::Duration::from_millis(20));
            cb.set_key_state(Key::Enter, false);
            pump!(2.0);
            let text: String = (0..term.cols * term.rows)
                .map(|i| term.screen.visible_char(i % term.cols, i / term.cols))
                .collect();
            println!(
                "REFONT session answered after the changes: {}",
                text.contains(mark)
            );
        }

        let seq = std::env::var("TEK_SEQ").ok();
        let seq_max = env_f32("TEK_SEQ_MAX", 120.0) as usize;
        let trace = std::env::var("TEK_TRACE").is_ok();
        let until =
            Instant::now() + std::time::Duration::from_secs_f32(env_f32("TEK_SECONDS", 6.0));
        let mut n = 0usize;
        while Instant::now() < until {
            frame!();
            if trace {
                println!(
                    "TLOG t={:.2} frames={frames} lit={} commits={} bytes_in={} shown={} ghosted={} dots={}",
                    start.elapsed().as_secs_f32(),
                    lit(&mut tube),
                    term.commits,
                    term.bytes_in,
                    term.bytes_shown,
                    tube.ghosted_cells,
                    tube.beam_dots()
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
            write_png(
                &PathBuf::from(&path),
                &buf,
                tube.w as usize,
                tube.h as usize,
            )
            .expect("png");
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
            // A name from shells.txt is resolved here rather than where the shell
            // is spawned, so every path that starts one - the command line, a
            // Start Menu entry, a panel relaunch - agrees on what
            // `--shell "Git Bash"` means; a command line that names a program is
            // passed through exactly as it was.
            "--shell" => {
                let given = value("--shell")?;
                let shells = Shells::load();
                o.shell = Some(shells.command_for(&given).unwrap_or(&given).to_string());
            }
            "--list-shells" => o.list_shells = true,
            "--version" | "-V" => o.version = true,
            "--trail" => o.trail = Some(value("--trail")?.parse()?),
            "--vt-replay" => o.vt_replay = Some(PathBuf::from(value("--vt-replay")?)),
            "--write-rate" => o.write_rate = Some(value("--write-rate")?.parse()?),
            "--ghost" => o.ghost = Some(value("--ghost")?.parse()?),
            "--slide" => o.slide = Some(value("--slide")?.parse()?),
            "--scan" => o.scan = Some(value("--scan")?.parse()?),
            "--dots" => o.dots = Some(value("--dots")?.parse()?),
            "--lens" => o.lens = Some(value("--lens")?.parse()?),
            // The raster-terminal look, in one flag.  Every value here is one
            // the measurements settled on (see the README), and any of them can
            // be overridden by a later flag on the same command line.
            "--wyse" => {
                // one list, shared with the panel's preset row
                for (name, v) in WYSE {
                    apply_value(&mut o, name, *v);
                }
            }
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

    /// Every control the panel offers must have a handler and must survive a
    /// settle unchanged.  Eight of them started out as sliders that moved a number
    /// nothing in the terminal read, and a settle that shifted a value on its own
    /// would make the panel drift the moment it was opened.
    #[test]
    fn the_panel_lists_live_variables_only() {
        let mut opts = Options::default();
        let tube = Tube::new(1024, 780);
        let rows = panel_rows(&opts, &tube, &Presets::builtins());
        assert!(rows.len() >= 20, "the panel lists the terminal's variables");
        assert_eq!(
            rows.iter().filter(|r| r.launch).count(),
            0,
            "no terminal row needs a relaunch: even the face is written again, not started over"
        );
        let mut structural: Vec<&str> = rows
            .iter()
            .filter(|r| r.structural)
            .map(|r| r.name)
            .collect();
        structural.sort_unstable();
        assert_eq!(
            structural,
            vec!["cell", "font"],
            "the cell size and the face are the rows the page has to be written again for"
        );
        let structural = apply_rows(&mut opts, &rows);
        assert!(structural, "the cell row is structural");
        let again = panel_rows(&opts, &tube, &Presets::builtins());
        for (a, b) in rows.iter().zip(again.iter()) {
            assert_eq!(a.name, b.name);
            assert!(
                (a.value - b.value).abs() < 1e-3,
                "{} drifts on a settle: {} -> {}",
                a.name,
                a.value,
                b.value
            );
        }
    }

    /// `--write-rate 0` is an instant page, and the panel has to be able to
    /// express that: a preset written on the command line - `--wyse` asks for
    /// exactly this - must be reproducible from the panel, or the panel is a
    /// second, weaker command line.
    #[test]
    fn an_instant_pen_survives_the_panel() {
        let tube = Tube::new(1024, 780);

        let mut opts = Options::default();
        opts.write_rate = Some(0.0);
        let rows = panel_rows(&opts, &tube, &Presets::builtins());
        let row = rows
            .iter()
            .find(|r| r.name == "write-rate")
            .expect("the row");
        assert_eq!(row.value, 0.0, "an instant pen reads as zero");
        apply_rows(&mut opts, &rows);
        assert_eq!(opts.write_rate, Some(0.0), "and stays instant");

        // with nothing given, the row shows the rate the pen is really running at
        // rather than a zero that would read as instant
        let mut fresh = Options::default();
        let rows = panel_rows(&fresh, &tube, &Presets::builtins());
        let row = rows
            .iter()
            .find(|r| r.name == "write-rate")
            .expect("the row");
        assert_eq!(row.value, terminal::WRITE_RATE_DEFAULT);
        apply_rows(&mut fresh, &rows);
        assert_eq!(fresh.write_rate, Some(terminal::WRITE_RATE_DEFAULT));
    }

    /// The whole chain the live harness could not reach - a press, the settle,
    /// and the options changing - so what is left to a hand test is only whether
    /// the window delivers a real press at all.  The trace that started this
    /// showed the settle naming a row the pointer was not over, which is exactly
    /// what made the two ends worth testing together.
    #[test]
    fn a_press_reaches_the_options_through_the_settle() {
        let mut opts = Options::default();
        let tube = Tube::new(1024, 780);
        let mut panel = panel::Panel::new(panel_rows(&opts, &tube, &Presets::builtins()));
        panel.open = true;
        let (w, h) = (1024usize, 780usize);

        let scan = panel.row_index("scan").expect("the scan row");
        let (_, y) = panel.row_y(w, h, scan);
        let x = panel.control_x(h) + panel.track_width(h) * 3 / 4;
        assert!(panel.handle(w, h, (x as f32, (y + 4) as f32), true, 0.0));

        // nothing is applied while the dragging is fresh
        assert!(opts.scan.is_none(), "not before the settle");
        let settled = panel.settle_due(Instant::now() + std::time::Duration::from_millis(400));
        let (row, press) = settled.expect("the settle names the row that moved");
        assert_eq!(panel.rows[row].name, "scan");
        assert!(press, "a mouse press is a press");
        apply_rows(&mut opts, &panel.rows);
        let applied = opts.scan.expect("the option was set");
        assert!(
            (applied - 0.71).abs() < 0.05,
            "three quarters along the 0..0.95 track: {applied}"
        );
    }

    /// The face is a *rendering* change, so the row that sets it is structural and
    /// not a launch: the tube is configured again with the new face and the page is
    /// written again, and the session survives.  The rows that really are fixed when
    /// the process starts stay launch rows, and those are in the plot panel.
    #[test]
    fn the_face_is_live_and_the_geometry_is_not() {
        let mut opts = Options::default();
        opts.font = Face::Mono;
        let tube = configured(Tube::new(1024, 780), &opts);
        let rows = panel_rows(&opts, &tube, &Presets::builtins());
        let font = rows.iter().find(|r| r.name == "font").expect("the font row");
        assert!(font.structural, "the page has to be written again");
        assert!(!font.launch, "and no session has to end for it");
        assert_eq!(font.shown(), "mono", "the row shows the face in use");

        let plot = plot_rows(&opts, &tube);
        for name in ["device", "zoom", "shrink"] {
            let row = plot.iter().find(|r| r.name == name).expect(name);
            assert!(row.launch, "{name} is fixed when the process starts");
        }
    }

    /// Build the tube the way a launch does, so the rows that are read from the
    /// tube (the phosphor, the scan, the ghost, the dot grid) report what a
    /// configured machine reports rather than a bare `Tube::new`.
    fn configured(mut tube: Tube, opts: &Options) -> Tube {
        let cell = opts.cell.unwrap_or(terminal::CELL_SCALE_DEFAULT);
        configure_terminal_tube(&mut tube, Device::Tek4010, opts, cell);
        tube
    }

    /// A row's value by name, for the tests that compare a machine against a
    /// saved preset.
    fn row_value(rows: &[panel::Row], name: &str) -> f32 {
        rows.iter().find(|r| r.name == name).expect("the row").value
    }

    /// A preset press sets the rows it names, and the values that follow from
    /// them.  `wyse` asks for an instant page *and* a paced link, which sounds
    /// contradictory until the terminal's two rates are told apart.
    #[test]
    fn a_preset_sets_the_rows_it_names() {
        let mut opts = Options::default();
        let mut tube = configured(Tube::new(1024, 780), &opts);
        let mut rows = panel_rows(&opts, &tube, &Presets::builtins());

        assert!(
            !apply_preset_rows(&mut rows, "wyse", &Presets::builtins()),
            "wyse leaves the cell alone, so the page stands"
        );
        apply_rows(&mut opts, &rows);
        tube = configured(tube, &opts);
        // the settle recomputes the chooser after applying, and so must the test
        // if it is to compare against rows the machine is really set to
        sync_preset_row(&mut rows, &Presets::builtins());

        let row = |name: &str| rows.iter().find(|r| r.name == name).expect("the row").value;
        assert_eq!(row("write-rate"), 0.0, "no beam writer");
        assert_eq!(row("scan"), 0.5);
        assert_eq!(row("dots"), 15.0);
        assert_eq!(row("ambient"), 0.0);
        assert_eq!(row("ghost"), 0.1);
        assert_eq!(row("trail"), 0.2);
        assert_eq!(row("slide"), 0.0);
        assert_eq!(row("baud"), 19200.0);
        assert_eq!(opts.write_rate, Some(0.0), "the page appears, not paced");
        assert!(
            opts.baud_given,
            "and the link is paced, which --wyse has to ask for"
        );

        // the panel is now showing what the reconfigured machine is set to
        for r in panel_rows(&opts, &tube, &Presets::builtins()) {
            assert!(
                (r.value - row(r.name)).abs() < 1e-6,
                "{} reads {} not {}",
                r.name,
                r.value,
                row(r.name)
            );
        }
    }

    /// The chooser is derived from the rows, never stored: a fresh machine is the
    /// `tek` defaults, a machine set to either preset reads that preset, and one
    /// value nudged away from a preset reads `custom`.  The whole point is that
    /// the row cannot lie about what the machine is set to.
    #[test]
    fn the_preset_chooser_reads_the_rows_it_is_derived_from() {
        let shown = |rows: &[panel::Row]| {
            rows.iter()
                .find(|r| r.name == "preset")
                .expect("the row")
                .shown()
        };
        let mut opts = Options::default();
        let mut tube = configured(Tube::new(1024, 780), &opts);
        let mut rows = panel_rows(&opts, &tube, &Presets::builtins());

        assert_eq!(
            preset_index(&rows, &Presets::builtins()),
            1,
            "a fresh machine is the tek defaults"
        );
        assert_eq!(shown(&rows), "tek");

        // set to each preset in turn
        apply_preset_rows(&mut rows, "wyse", &Presets::builtins());
        apply_rows(&mut opts, &rows);
        tube = configured(tube, &opts);
        rows = panel_rows(&opts, &tube, &Presets::builtins());
        assert_eq!(preset_index(&rows, &Presets::builtins()), 0);
        assert_eq!(shown(&rows), "wyse");

        apply_preset_rows(&mut rows, "tek", &Presets::builtins());
        apply_rows(&mut opts, &rows);
        tube = configured(tube, &opts);
        rows = panel_rows(&opts, &tube, &Presets::builtins());
        assert_eq!(preset_index(&rows, &Presets::builtins()), 1);
        assert_eq!(shown(&rows), "tek");

        // one value off a preset and the machine matches neither of them
        rows.iter_mut()
            .find(|r| r.name == "beam")
            .expect("the beam row")
            .value = 1.4;
        assert_eq!(
            preset_index(&rows, &Presets::builtins()),
            PRESETS.len(),
            "custom"
        );
        sync_preset_row(&mut rows, &Presets::builtins());
        assert_eq!(shown(&rows), "custom");
    }

    /// The main-side half of a press on the chooser: the preset applied is the one
    /// the row *shows*, read through the same `shown` name the settle uses, and
    /// `custom` - the value past the end of the list - is not a preset, so it
    /// applies nothing.  (The other half, the value moving under the click or the
    /// Enter key, belongs to the panel.)
    #[test]
    fn a_press_on_the_preset_row_applies_that_preset() {
        let mut opts = Options::default();
        let tube = configured(Tube::new(1024, 780), &opts);
        let mut rows = panel_rows(&opts, &tube, &Presets::builtins());
        assert_eq!(rows[0].name, "preset");

        // the chooser has moved to wyse, so that is the preset that applies
        rows[0].value = 0.0;
        let chosen = rows[0].shown();
        assert_eq!(chosen, "wyse");
        assert!(
            !apply_preset_rows(&mut rows, &chosen, &Presets::builtins()),
            "wyse leaves the cell alone, so the page stands"
        );
        apply_rows(&mut opts, &rows);
        assert_eq!(opts.write_rate, Some(0.0), "no beam writer");
        assert_eq!(opts.scan, Some(0.5));
        assert!(opts.baud_given, "and the link is paced");
        // the chooser's own name must be handled, or the settle's guard fires
        assert!(apply_value(&mut opts, "preset", 1.0), "the name is handled");
        // and after the settle the chooser reads the machine it produced
        sync_preset_row(&mut rows, &Presets::builtins());
        assert_eq!(rows[0].shown(), "wyse");

        // `custom` names no preset, so it applies none of them
        rows[0].value = PRESETS.len() as f32;
        assert_eq!(rows[0].shown(), "custom");
        let chosen = rows[0].shown();
        let before: Vec<f32> = rows.iter().map(|r| r.value).collect();
        assert!(!apply_preset_rows(&mut rows, &chosen, &Presets::builtins()));
        assert_eq!(before, rows.iter().map(|r| r.value).collect::<Vec<f32>>());
    }

    /// `tek` is the field defaults.  The safe way to know it stays that way is to
    /// compare it against a machine built with nothing given to it.
    #[test]
    fn the_tek_preset_is_a_fresh_machine() {
        let mut opts = Options::default();
        let tube = configured(Tube::new(1024, 780), &opts);
        let mut rows = panel_rows(&opts, &tube, &Presets::builtins());
        // move several rows away from the defaults first
        for r in rows.iter_mut() {
            match r.name {
                "beam" => r.value = 1.4,
                "scan" => r.value = 0.6,
                "write-rate" => r.value = 0.0,
                "cell" => r.value = 3.0,
                _ => {}
            }
        }

        assert!(
            apply_preset_rows(&mut rows, "tek", &Presets::builtins()),
            "the cell came back from 3.0, so the page must be written again"
        );
        apply_rows(&mut opts, &rows);

        let fresh_opts = Options::default();
        let fresh_tube = configured(Tube::new(1024, 780), &fresh_opts);
        for (r, f) in rows
            .iter()
            .zip(panel_rows(&fresh_opts, &fresh_tube, &Presets::builtins()).iter())
        {
            assert_eq!(r.name, f.name);
            assert!(
                (r.value - f.value).abs() < 1e-6,
                "{} is {} where a fresh machine reports {}",
                r.name,
                r.value,
                f.value
            );
        }

        // a name that is not a preset does nothing at all
        let before: Vec<f32> = rows.iter().map(|r| r.value).collect();
        assert!(!apply_preset_rows(&mut rows, "nope", &Presets::builtins()));
        assert_eq!(before, rows.iter().map(|r| r.value).collect::<Vec<f32>>());
    }

    /// A relaunch is a fresh command line built from the options as they stand,
    /// never from the line the process was started with: a value set by hand in
    /// the panel exists nowhere else, and losing it would make the panel a thing
    /// you can use once.  The strongest statement that the rebuilt line means what
    /// it says is to hand it back to the program's own parser and compare.
    #[test]
    fn a_relaunch_command_line_round_trips_the_live_options() {
        let mut opts = Options::default();
        opts.files = vec![PathBuf::from("plot.plt")];
        // as a panel would leave them: a new target and face, a zoom, a hand-set
        // beam, and a baud that was given
        apply_value(&mut opts, "device", 0.0); // the 4010 entry
        apply_value(&mut opts, "font", 2.0); // the gothic entry
        apply_value(&mut opts, "zoom", 3.0); // the "4" entry
        opts.beam = 0.8;
        opts.baud = 9600.0;
        opts.baud_given = true;

        let args = program_args(&opts);
        let at = args
            .iter()
            .position(|a| a == "--device")
            .expect("--device is written out");
        assert_eq!(args[at + 1], "4010", "the target the panel set");
        assert_eq!(args.last().map(String::as_str), Some("plot.plt"));

        let back = parse_args_from(args.into_iter())
            .expect("the program's own parser accepts it")
            .expect("not a help request");
        assert_eq!(back.device, Device::Tek4010);
        assert_eq!(back.font, Face::Gothic);
        assert_eq!(back.zoom, 4);
        assert_eq!(back.beam, 0.8);
        assert_eq!(back.baud, 9600.0);
        assert!(back.baud_given, "a paced link stays paced");
        assert_eq!(back.files, vec![PathBuf::from("plot.plt")]);
    }

    /// A terminal relaunch names the shell and the face and no files, so the fresh
    /// process comes back as a terminal rather than trying to play a plot.
    #[test]
    fn a_terminal_relaunch_keeps_the_shell_and_the_face() {
        let mut opts = Options::default();
        opts.shell = Some("powershell.exe".to_string());
        apply_value(&mut opts, "font", 2.0); // gothic

        let back = parse_args_from(program_args(&opts).into_iter())
            .expect("accepted")
            .expect("not a help request");
        assert_eq!(back.shell.as_deref(), Some("powershell.exe"));
        assert_eq!(back.font, Face::Gothic);
        assert!(back.files.is_empty());
    }

    /// A preset saved from the panel is written in a plain text file and read back
    /// as a preset of the same name, applied through the very path a built-in
    /// uses - so a saved preset cannot mean something different from a built-in.
    #[test]
    fn a_saved_preset_reads_back_and_applies_like_a_built_in() {
        // a file of its own, so the machine's real presets are never touched
        let dir = std::env::temp_dir().join(format!("tek-tube-saved-{}", std::process::id()));
        let path = dir.join("presets.txt");
        let _ = fs::remove_dir_all(&dir);

        let opts = Options::default();
        let tube = configured(Tube::new(1024, 780), &opts);
        let mut rows = panel_rows(&opts, &tube, &Presets::builtins());
        // the user nudges two rows and saves them
        rows.iter_mut()
            .find(|r| r.name == "beam")
            .expect("beam")
            .value = 0.9;
        rows.iter_mut()
            .find(|r| r.name == "scan")
            .expect("scan")
            .value = 0.4;
        let mut presets = Presets::builtins();
        save_user_preset(&path, "user 1", &rows, &mut presets).expect("the file is writable");

        // a fresh process reads the file back
        let presets = Presets {
            users: read_presets(&path),
        };
        assert_eq!(
            presets.names(),
            vec!["wyse".to_string(), "tek".to_string(), "user 1".to_string()],
            "the saved preset joins the chooser after the built-ins"
        );

        // apply it to a machine at the defaults, through the same rows path
        let opts2 = Options::default();
        let tube2 = configured(Tube::new(1024, 780), &opts2);
        let mut rows2 = panel_rows(&opts2, &tube2, &presets);
        apply_preset_rows(&mut rows2, "user 1", &presets);
        assert_eq!(row_value(&rows2, "beam"), 0.9);
        assert_eq!(row_value(&rows2, "scan"), 0.4);
        // and the chooser now reads the saved preset
        sync_preset_row(&mut rows2, &presets);
        assert_eq!(
            rows2
                .iter()
                .find(|r| r.name == "preset")
                .expect("the row")
                .shown(),
            "user 1"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A saved preset from an older build may name a row this one has not got.
    /// The row is dropped and the rest of the preset still applies: a stale file
    /// must not be fatal, or a build that renames a row would take every saved
    /// preset with it.
    #[test]
    fn a_saved_preset_naming_a_row_this_build_lacks_still_applies() {
        let presets = Presets {
            users: vec![Preset {
                name: "user 9".to_string(),
                rows: vec![
                    ("beam".to_string(), 0.7),
                    ("a row from another build".to_string(), 1.0),
                ],
            }],
        };
        let opts = Options::default();
        let tube = configured(Tube::new(1024, 780), &opts);
        let mut rows = panel_rows(&opts, &tube, &presets);
        apply_preset_rows(&mut rows, "user 9", &presets);
        assert_eq!(row_value(&rows, "beam"), 0.7, "the known row still applies");
    }

    /// A missing or corrupt preset file is an empty list rather than a crash: the
    /// panel is a convenience, and no saved preset is worth refusing to start
    /// over.  A line that is not a preset, a value that is not a number and a
    /// preset with no usable rows are all dropped.
    #[test]
    fn a_missing_or_corrupt_preset_file_is_handled() {
        let missing = std::env::temp_dir().join("tek-tube-presets-that-are-not-there.txt");
        let _ = fs::remove_file(&missing);
        assert!(read_presets(&missing).is_empty(), "a missing file is empty");

        let text = "# a comment\n\
                    user 1\tbeam=0.9\tscan=nonsense\n\
                    \n\
                    not a preset\t\n\
                    user 2\tglow=0.2\n";
        let presets = parse_presets(text);
        assert_eq!(presets.len(), 2, "the two usable lines survive");
        assert_eq!(presets[0].name, "user 1");
        assert_eq!(
            presets[0].rows,
            vec![("beam".to_string(), 0.9f32)],
            "the value that is not a number is dropped, the good pair stays"
        );
        assert_eq!(presets[1].name, "user 2");
        assert_eq!(presets[1].rows, vec![("glow".to_string(), 0.2f32)]);

        // and what is written back is what is read, so a save survives the round
        // trip through the file
        let round = parse_presets(&format_presets(&presets));
        assert_eq!(round.len(), presets.len());
        assert_eq!(round[0].name, presets[0].name);
        assert_eq!(round[0].rows, presets[0].rows);
    }

    /// The setup writes `shells.txt` and the Start Menu entries name what it
    /// wrote, so the parse has to agree with `setup.ps1` exactly: split on the
    /// first `=`, names matched without case, and a `default` that falls back to
    /// the first profile rather than to nothing.
    #[test]
    fn shell_profiles_parse_the_way_the_setup_writes_them() {
        let text = "\
# tek-tube shells: one per line, `name = command line`
default = Git Bash

Windows PowerShell = C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe
Git Bash = \"C:\\Program Files\\Git\\bin\\bash.exe\" --login -i
WSL (Ubuntu) = wsl.exe -d Ubuntu
odd = cmd.exe /c echo a=b
";
        let shells = Shells::parse(text);
        assert_eq!(
            shells.profiles.len(),
            4,
            "a comment and a blank line are not profiles"
        );
        assert_eq!(
            shells.command_for("git bash"),
            Some("\"C:\\Program Files\\Git\\bin\\bash.exe\" --login -i"),
            "a name resolves whatever its case, and the command keeps its text"
        );
        assert_eq!(shells.command_for("not a shell"), None);
        assert_eq!(
            shells.default_command(),
            shells.command_for("Git Bash"),
            "`default` names a profile, and that is what a bare tek-tube.exe runs"
        );
        assert_eq!(
            shells.command_for("odd"),
            Some("cmd.exe /c echo a=b"),
            "only the first `=` separates, so a command may contain one"
        );

        // No `default` line is the first profile; a `default` naming nothing
        // falls through to it too, rather than leaving no shell at all.
        assert_eq!(
            Shells::parse("a = one.exe\nb = two.exe\n").default_command(),
            Some("one.exe")
        );
        assert_eq!(
            Shells::parse("default = gone\na = one.exe\n").default_command(),
            Some("one.exe")
        );
        // Nothing configured - no file, an empty file - is no choice at all, and
        // the platform's own shell is used.
        assert_eq!(Shells::default().default_command(), None);
        assert!(Shells::parse("").profiles.is_empty());
    }
}
