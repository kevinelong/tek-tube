# Terminal mode: scope

Goal: a mode of `tek-tube` that runs a real shell under a pseudoconsole, renders
it through the existing storage-tube optics, and scrolls the way a terminal
*could* have if its phosphor had persistence - smooth, smearing, resolving.

Status: **M0-M4 are implemented and measured** (below); M5 (scrollback, resize,
alternate screen) is not started.  Measured means measured on this machine.

It works: `tek-tube --shell powershell.exe` runs a real PowerShell on the tube,
74x35, with the smear-then-resolve model.  See the README for how to drive it.

## 0. Progress

**M0 - the PTY risk is retired.**  `portable-pty` 0.9 drives a real PowerShell
under a 74x35 pseudoconsole: 71,446 bytes of genuine VT in 11.6 s, 69,393 of them
plain text, 32 distinct CSI shapes.  It lives in `C:/Users/kevin/tek/pty-spike`
(deliberately outside this repo, so the repo stays dependency-free until M2).
Two traps were found the hard way and are worth more than the census:

* **`CSI 6n` blocks the session.**  The console host asks "terminal, where is
  your cursor?" and waits.  With no answer, a 25 second session produced exactly
  4 bytes and not one character of prompt.  A terminal *must* answer, and it must
  answer from its own real cursor position: answering with a fixed `1;1` lets the
  session run but corrupts PSReadLine's redraw into overlapping text, because it
  positions relative to what it was told.
* **Do not join the reader thread on shutdown.**  It is blocked in a pty read and
  the master is still open, so killing the child produces no EOF and the join
  hangs for ever - the spike timed out at 240 s for exactly that reason.

**M1, first slice - the parser, against the real corpus.**  A screen model and
parser covering the measured subset, replayed against the captured 71 KB with two
real invariants: feeding the stream whole and in 64 byte chunks must produce an
identical screen (this is what catches a state machine that cannot survive a
sequence split across reads), and the parser must request exactly as many cursor
reports as the corpus contains `CSI 6n`.  14 unit tests.  The replay caught one
genuine bug immediately - an escape handler that set the CSI state and then
clobbered it, so every sequence was printed as literal text (`93m`, `?25l`,
`35;1H` all appeared on screen) - and the tests caught a second: folding an
explicit `0` parameter into the default turned `CSI 0K` (erase to end of line)
into `CSI 1K` (erase to start), which erases from the wrong end.

**Measured churn, which is what M2 has to pay for:** across the corpus the screen
marks **34,085 dirty-row events**, and the worst 64-byte chunk dirties **all 35
rows**.  PSReadLine rewrites most of the page constantly, so the renderer cannot
assume small dirty regions - a full-page restroke is the common case, not the
worst case.  That is the number to design against.

**The terminal, on the tube (2026-10-02).**  `--shell` renders a live PowerShell
on the 4010 face with the write beam visible: a pen walks the page in reading
order and traces only the cells whose character changed, at `WRITE_RATE_DEFAULT`
= 2500 target dots/s, six times the 400 it was being run at.  That figure is
calibrated against the thing it sits beside - a plot file's own beam measures
4,587 to 15,193 of the 4010's dots per second on the sample vector plots at the
default 19,200 baud, and 401 dots/s on a point plot - so the pen now draws in the
band the plots draw in.  `--stats` prints the measured figure for whatever stream
is played.  `--write-rate 0` restores the instant page.

Fullscreen is the default, with no title bar, and F11 toggles it (`--windowed`
opts out).  A window cannot restyle itself, so F11 rebuilds the window and the
session carries on.  In terminal mode the tube *is* the window, so the page keeps
the face's own shape and the cell grows with the window - a 2560x1440 screen is a
93x32 page of 27x45 dot cells - and the stroke weight is derived from the cell
actually being shown rather than from `--cell`, or the text goes spindly as the
window grows.

**F2 opens the options panel**, drawn on the tube in the tube's own stroke font:
the pen rate (`baud`, `write-rate`, `slide`), the phosphor (`beam`, `glow`, the
two gains, `sat`, `ambient`, `mottle`, `bloom`, `bloom-wide`, `lens`, `camera`,
`scan`, the dot grid), `trail`, `ghost`, `settle`, `cell` and `exit-after`.  A
value is applied when the dragging has settled - 0.25 s after the last change -
rather than every frame, and only `cell`, which changes the page, needs it
written again.  The phosphor is reconfigured from the parameters the tube was
built with rather than over the top of the last pass, because `--sat` scales a
stored chromaticity and would otherwise compound on every drag.

The plot-only options are deliberately **not** in the panel: `--device`,
`--font`, `--zoom`, `--shrink`, `--text-scale`, `--threads`, `--no-autoclear` and
`--no-enhanced` belong to decoding a file onto a 4014 face with an Enhanced
Graphics Module, and a terminal run honours none of them.  A control that moved a
number nothing reads would be worse than no control at all.

Two of the rows are buttons rather than values: **`wyse`** and **`tek`**.  A preset
is several variables at once, which is an action rather than a setting, so the row
carries no value of its own: the press writes the preset's values into the panel's
own rows, which are then applied through the same mapping the command line uses -
`--wyse` and the `wyse` button share one list, so they cannot drift.  `tek` is not
a table of numbers at all: it is "what a machine built with nothing given to it
reports", computed at the press by the same two calls a launch makes.

**What the terminal answers to.**  Cursor addressing, the erase and editing
families, scroll regions, the alternate screen, cursor save/restore (DECSC/DECRC,
which carries the attribute and the charset as well as the cursor) and intensity
SGR.  Line art arrives one of two ways and both are drawn as strokes on the cell's
own grid, so a frame's rules meet at the cell edges instead of stopping short:
the **DEC special graphics set** (`ESC ( 0`, which programs select around a frame
and put back with `ESC ( B`) and the **UTF-8 box-drawing characters** a modern
program sends instead - `vim`, `htop` and `tmux` mostly use the second.  Cursor
keys follow **DECCKM**, so `vim` and `less` get the `ESC O A` form they ask for.
Colour is swallowed by design (one phosphor), reverse video cannot be drawn on a
storage tube at all, and there is no mouse reporting.

The type advertised to the child is `vt100` when nothing in the environment says
otherwise, because that is what is true: over ssh it is what the far end believes
and acts on, and `xterm-256color` asked for 256 colours and a mouse that do not
exist here.

**Doubled drawing, found by recording the window.**  A `cls` followed by a long
listing was recorded frame by frame from the live window (grabber and analysis in
`cls3/`, findings in `cls3/FINDINGS.md`): the shell stopped at t=0.06 s and the
tube was still writing the page at t=7.5 s, filling it in from the top row down
and going back over rows it had already drawn.  The cause was that the committed
image took each cell's character from the *screen* rather than from what the pen
had *drawn*, so a scroll stored the whole new page before the beam had reached
it; the pen, whose record still described the old page, then set out to retrace
everything.  The image is now built from the pen's own record, and a scroll, a
clear or an alt-screen switch is a page replacement - `vt::Screen` sets a flag
the renderer takes - rather than something to trace, with a quarter of the cells
changing kept as a backstop.  Two regression tests cover it, both failing on the
old behaviour.

**Later, 2026-10-02: performance, and two live-path faults.**

* The render is split across cores.  Every pass is a per-pixel map or one
  direction of a separable blur, so it is split by *rows of the destination*:
  `chunks_mut` gives each thread its own rows, no `unsafe` anywhere, and no sum is
  reordered, so the result is bit for bit the serial one
  (`splitting_a_pass_does_not_change_a_single_value`).  The vertical half of a box
  blur was the hold-out - its running sum goes down a column - and is now two
  passes over independent rows with the column results copied into a transposed
  buffer.  The splitter measures 5.5x on 8 threads
  (`the_split_uses_more_than_one_thread` asserts it engages).  The terminal's exit
  summary reports the frame cost, which is the only figure that means anything:
  49.4 ms a frame in the debug profile at 1024x780, 20 fps while drawing, over
  1444 drawn frames.
* **Nothing serial or duplicated may be inferred from a running process.**  Two
  faults in a row were chased with theory when a measurement was available:
  the duplicate prompt after Enter was *not* the pen (a byte dump replayed through
  the parser came out clean) but the **resize**: the screen keeps no reflow, so a
  new geometry left stale rows and conhost's full repaint landed beside them.
  `Screen::clear_visible` blanks the page on a resize, keeping the scrollback.
* F11 could not reach fullscreen from a windowed start, because the monitor size
  was only queried when *starting* fullscreen.
* Pen defaults retuned by ear and by measurement: 4000 -> 2500 target dots/s
  (~42 letters a second) and the trail 0.5 -> 0.3 s (the cool measured 1.01 s to a
  third of the excess, now 0.48 s).

## 1. Where it stands today (measured)

The emulator is a 4010 protocol decoder with no process, no VT and no input.

| capability | state | evidence |
|---|---|---|
| draw text from a byte stream | works | `Get-Process \| Format-Table \| Out-String \| tek-tube -` renders with columns intact (`docs/pipe-text.png`) |
| read the stream from a pipe | works | `tek-tube -`, added for this |
| draw vectors from a program's output | works | `python tools/tekplot.py \| tek-tube -` - 436 vectors plus captions (`docs/pipe-plot.png`) |
| VT cursor positioning | **discarded** | `ESC[2J`, `ESC[H`, `ESC[5;10H` are all swallowed; text only flows to the next line |
| VT colour | discarded | intended, it is a monochrome green tube |
| page overflow | **erases everything** | 60 lines fed, only lines 036-060 survive; the first 35 are gone |
| keyboard | none | ESC quits and that is all |
| process / pty | none | - |

So the four things a shell needs are exactly the four things missing.

## 2. What is reused as it stands

This is the point: most of the hard, tuned work is already done and none of it
has to be rewritten.

* **The tube** (`tube.rs`) - `stored` / `fresh` / `under` populations, Gaussian
  beam deposition through `deposit()`, five-scale halation, the flood-gun field,
  the linear-light pipeline with a tabulated transfer, O(1) decay. The terminal
  renders into this; it does not replace it.
* **The font** (`font.rs`) is already a *stroked* glyph model -
  `Glyph { paths, width }` from Hershey data - and `Tube::character(ch, x, y,
  cell_w, cell_h, st)` already centres a glyph in a fixed cell and strokes it
  through the same beam as graphics. A monospaced terminal grid is therefore
  free: it is the same call with regular cell arguments. Glyphs glow, halo and
  store exactly like vectors, which is what the real hardware did.
* **The page geometry**: `FONT_SIZES[0] = (74, 35)`. 1024/74 by 780/35 gives
  cells of 13.8 x 22.3 dots - a 0.62 aspect, close to a classic 8x12 character
  cell, and it suits the 4:3 face. An 80x24 grid would give 12.8 x 32.5 dots
  (0.39) - unnaturally tall. **Advertise 74x35 to the pty** and the tube's own
  page becomes the terminal's page, with no distortion.
* **The window** (`main.rs`): DPI-exact presentation, borderless fullscreen,
  skip-recomposition when nothing moves, and the measured frame budget
  (38 ms at 1024x780, 26 fps: 11 ms target scan, 17 ms halation, 11 ms colour).
* **The transport**: the `-` pipe path is the same shape as a pty's output pipe.
* **Input**: `minifb::InputCallback` gives `add_char(u32)` (Unicode, driven by
  `WM_CHAR`, so keyboard layout and AltGr are handled for us) and
  `set_key_state(Key, bool)`, and `Key` covers F1-F15, arrows,
  Home/End/PgUp/PgDn, Insert/Delete, modifiers, numpad and the punctuation keys.
  No Win32 FFI is needed for typing.

## 3. What has to be built

| # | component | size | notes |
|---|---|---|---|
| a | pty + child process | ~80 lines + 1 dep | use `portable-pty` 0.9 (wezterm's, used by real terminals) or `conpty` 0.7 (Windows Terminal team). Both are on crates.io and reachable from this machine |
| b | VT parser + screen model | ~1200-1800 lines | the bulk of the work, and the main correctness risk |
| c | key -> VT encoding | ~250 lines | arrows/F-keys/Ctrl/Alt/paste, plus Win32 input mode |
| d | terminal renderer | ~400 lines | dirty cells -> glyph strokes into the transient population, cursor, sub-line scroll offset, commit |
| e | smear / resolve effect | ~100 lines + tuning | see section 4 |
| f | scrollback ring + navigation | ~150 lines | wheel / Shift-PgUp |
| g | resize | ~100 lines | window resize -> new grid -> `ResizePseudoConsole` -> reflow |
| h | SGR colour | ~80 lines | map to the 4014 intensity table, staying green |

Roughly **2500-3200 lines of new Rust plus tests** - a project, not a patch.

### The VT subset that actually matters

Bound it by census, not by spec: capture a real session and implement what it
emits. The measured minimum from the ConPTY preamble alone is
`ESC[?9001h` (Win32 input mode), `ESC[?1004h` (focus reporting), `ESC[?25l/h`
(cursor visibility), `ESC[2J`, `ESC[H`, `ESC[m`, OSC 0, plus swallow-and-forget
for the mouse and bracketed-paste modes.

For a usable PowerShell prompt, add: `CUP/HVP`, `CUU/CUD/CUF/CUB`, `ED`, `EL`,
`SGR`, `DECSC/DECRC`, autowrap (`?7`), origin mode (`?6`), tab stops, `RI`, `IND`,
`NEL`, and the alternate screen (`?1049`). For `less`/`vim`, also `DECSTBM`
(scroll region) and `IL`/`DL`/`ICH`/`DCH`, which are the ones the emulator's
"no scrollback" nature makes awkward but not impossible.

UTF-8 decoding and double-width cells are required regardless (box drawing and
the PowerShell prompt glyphs).

## 4. The aesthetic: smear, then resolve

This is the interesting part and it falls almost entirely out of what the tube
already models.

`fresh` is the write beam's flash: it decays with a time constant and it does not
store. If the terminal redraws the *visible screen* every frame while the content
is moving, each frame lands on top of the previous frame's still-decaying flash,
so everything that moved leaves a trail whose colour follows the phosphor - the
"matrix" look, produced by `deposit()` and `decay()` exactly as they are, with
no new machinery.

`stored` is the bistable image that does not fade. So:

* **moving** (typing, output arriving, scrolling) -> draw into `fresh` only,
  repeatedly, so the motion smears;
* **still** (nothing changed for ~300 ms) -> *commit*: blend the current screen
  into `stored` and clear `fresh`, so the picture **resolves** to the crisp
  storage-tube image.

**Smooth scroll (Wyse-style)**: the pty reports whole-line scrolls. The renderer
keeps a sub-line offset animated 0 -> 1 over ~120 ms with easing, so content
slides instead of jumping, and the smear is longest in the middle of the slide.

The one honest caveat, which must be written in the code and the README: a real
storage tube *cannot* do this. Its charge is bistable and never fades, and it
cannot erase one cell. This effect is a deliberate hybrid - a phosphor
persistence layer over the storage model - and it is the one place where the
emulator is less faithful than the hardware on purpose. It is also why the
"resolve" step exists: without it the trail would be permanent, which is what a
real tube would do and not what anyone wants to read.

Knobs: `--trail F` (0 = pure storage-tube, no smear; 1 = heavy), `--smooth-scroll`,
`--scroll-ms`, and a key to toggle the trail at runtime.

## 5. Risks, ranked, each with its retirement step

1. **PTY plumbing - RETIRED.**  `portable-pty` 0.9 works here (see Progress).  Do
   not hand-roll it: my ctypes prototype had `CreatePseudoConsole` return `S_OK`,
   `UpdateProcThreadAttribute` and `CreateProcessW` return success, and still
   moved not one byte.  The crate also settles the shutdown question: do not join
   the reader thread, because a blocked pty read never returns while the master
   is open.
2. **VT completeness - BOUNDED by measurement, remaining risk is full-screen
   apps.**  The census replaces guesswork: a real 11 second PowerShell session used
   only 32 distinct CSI shapes, dominated by `CUP` (236 of them), plus `ECH`
   (`CSI nX`), `SGR` (including 256-colour `38;5;n`), cursor visibility `?25`,
   focus reporting `?1004`, Win32 input mode `?9001` and an OSC title.  `ED`,
   `EL`, scroll regions, insert/delete-line and the alternate screen did **not**
   appear at all; they are implemented where cheap but they are unverified
   against real use, and `less`/`vim` would exercise them. *Retirement:
   this is now a parser test corpus plus a census, not a survey of the spec.*
3. **Readability versus smear (MEDIUM, one unresolved cosmetic).** A live
   capture of the debug build shows a faint dotted line down the left edge that
   is not in the screen (the screen dump is clean), so it lives in the tube's
   populations rather than the parser.  The likeliest explanation is the
   transient trail: at roughly one frame per second the simulated clock advances
   at best 0.25 s per frame, so a 1.1 s trail barely decays within the life of a
   run.

   This is *not* settled, because the attempt to settle it was confounded:
   comparing debug against release by screenshotting the live window is
   unreliable, since a screen grab composites whatever window is on top at those
   coordinates - one "measurement" that reported 337 rows of ink was actually
   reading a browser sitting over the terminal.  The trustworthy visual evidence
   is `docs/terminal-session.png`, which is a *replay* of a captured stream and
   therefore deterministic, plus the one live capture that visibly did show
   tek-tube.  Before changing anything, capture a *stream* the way M1 did and
   replay it, rather than trusting a screenshot. *Retirement: trail is a knob
   defaulted low, the current frame is always drawn at full brightness, and
   `--trail 0` must reproduce today's crisp behaviour exactly.*
4. **Performance - now quantified (MEDIUM).** During a scroll the whole screen
   restrokes each frame, and M1 measured how real that is: 34,085 dirty-row
   events over the corpus and the worst 64-byte chunk dirtying all 35 rows, so a
   full-page restroke is the *common* case.  Estimate +2-5 ms on top of the
   current 38 ms at 1024x780 for ~1900 stroked glyphs, which must be measured
   before it is believed. *Retirement: measure a full-screen restroke of 74x35
   glyphs through the existing beam; if it is too slow, cache the stroked row
   between frames.*
5. **Scrollback (LOW).** A ring buffer plus reflow on resize; the only wrinkle is
   that scrolling back should also render through the smear.
6. **Input latency (LOW-MEDIUM).** pty round trip plus a 26 fps renderer. Mitigate
   by drawing the *echo locally* (the tube writes the character on keypress,
   before the pty echoes it) - which is what a real terminal did anyway.

## 6. Milestones

| | deliverable | retires | status |
|---|---|---|---|
| M0 | spike: `portable-pty` -> print the shell's bytes to stdout | risk 1 | **done** (71 KB captured, 2 traps found) |
| M1 | VT screen + parser, headless, against a captured corpus | risk 2 | **first slice done** (14 tests, corpus replay; full-screen apps unverified) |
| M2 | wire to the tube: text appears, cursor | risk 4 | **done** |
| M3 | keyboard and echo | - | **done** (character input via minifb's Unicode callback, no FFI) |
| M4 | smooth scroll + smear/resolve + knobs | risks 3, 4 | **done** (`--trail`) |
| M5 | scrollback, resize, alternate screen | risks 5, 6 | not started |

The code now lives in this repo: `src/vt.rs` (screen and parser, with its test
suite) and `src/terminal.rs` (the pty, the input mapping and the render model).
It costs one dependency, `portable-pty`.  The spike that proved the pty works is
still at `C:/Users/kevin/tek/pty-spike` and can be deleted.

Two traps cost real time and are worth keeping in mind for M5:

* the pty **master must be held for as long as the terminal is**.  Dropping it
  closes the pseudoconsole and the shell dies within milliseconds - it exits
  after 14 ms with zero bytes, which looks exactly like the spawn having failed.
  The spike only worked because its master lived in `main`'s scope by accident.
* a command that exits immediately still has output in flight, so the loop must
  drain after the child exits rather than break on it.  Breaking on exit lost
  5,475 bytes down to 4 for `cmd /c dir`.

## 7. Explicit non-goals

* Not a general VT emulator for arbitrary TUIs in v1.
* Not making a 4010 speak VT on the wire. The two modes stay separate: **plot
  mode** keeps the 4010 protocol for files and pipes (that is the authentic
  hardware), **terminal mode** speaks VT to the shell and renders internally.
  Pretending an emulated 4010 was driven by ANSI would be a fiction.
* Not colour. SGR maps to intensity on a green tube, not to RGB.
