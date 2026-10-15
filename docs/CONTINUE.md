# tek-tube: continue here

Started 2026-10-05.  **The live state is the first section**, "Where it stands, 2026-10-15 -
restart here".  Everything below it is the record, newest session first, of what each one did
and measured; where an older section disagrees with the top section, the top section wins.

This note is **tracked in the repository** as `docs/CONTINUE.md`, so a fresh checkout carries
the record.  It used to live outside the checkout, in `C:\Users\kevin\tek\cls3\`, which now
holds a pointer to this file.  The older session archives it names - the 2026-10-05 running
record (`NIGHT-2026-10-05.md`), what is unresolved from it (`OPEN-QUESTIONS-2026-10-05.md`),
the system changes and their undo (`ELEVATED-SESSION.md`) and the 2026-10-12 licence sweep
(`LICENSE-SWEEP-2026-10-12.md`) - are still kept outside the checkout in that directory; the
sections below quote everything a reader of this repository needs from them.

## Where it stands, 2026-10-15 - restart here

A documentation and verification pass.  **No source and no test changed.**  The state note
moved into the repository as this file, `README.md` points at it, the verification the README
describes was re-run with the output recorded below, and the release-surface claims were
checked against the repository and against the published v0.3.4.

| | |
|---|---|
| Source | `tek-tube` at **`096e177`**, clean and in sync; the note became tracked here in that commit |
| Tests | `cargo test` **and** `cargo test --release`: **128 pass, 0 fail, 2 ignored** (123 in `src/main.rs`, 5 in `tests/release_tooling.rs`) |
| Releases | unchanged: `kevinelong/tek-tube-releases` latest **v0.3.4**, `tek-tube-0.3.4-win64.zip` 461,836 bytes plus `SHA256SUMS.txt`; nothing published this pass |
| CI | **none** - still no workflow in this repository, so the 128 tests run nowhere but here |

### The verification, re-run 2026-10-15, and what it printed

These are the commands the README's Build and Verification sections describe.  All of them ran
on this machine with the toolchain already in use (rustc 1.96.0); the text quoted is the
observed output, not a summary of intent.  The transient files went to
`C:\Users\kevin\tek\_scratch\verify-2026-10-15\` and are not part of the repository.

1. `cargo build --release`

   ```
   Finished `release` profile [optimized] target(s) in 1.54s
   ```

   exit 0.  (Incremental: the tree at `f30ac5f` was already built, so this proves the release
   profile builds clean, not that a cold build costs 1.5 s.)

2. The headless render, the README's own "useful verification flags" example -
   `target/release/tek-tube.exe samples/isco10.plt --png isco10.png --stats --dump-chars
   --probe 1600,1500`, exit 0, six lines of output:

   ```
   device: Tektronix 4014 (4096x3120 dots)
   stream: 4044 vectors, 0 points, 0 characters, max 10 bit address 817x767, 12 bit addressing used: false
   dot 1600,1500 displays #0EBD5A
   wrote C:\Users\kevin\tek\_scratch\verify-2026-10-15\isco10.png (1024x780), mean light 0.057, 9.8% of the target stored
   decoded 14386 of 14386 bytes: 4044 vectors, 0 points, 0 characters, 0 stray bytes, max address 817x767, 12 bit addressing used: false
   beam: 201481 target dots in 7.49 s of playback = 26890 dots/s, 6723 of the 4010's dots/s (spot 4)
   ```

   The file is a 1024x780 8-bit RGB PNG of 781,893 bytes; `--dump-chars` printed nothing extra
   because the file is pure graphics - 0 characters.  The probe reads `#0EBD5A` where the
   README's example text (written from an earlier session) says `#0EBD59`: one unit of blue,
   on a different dot of a different file, so the README's rounded colour claims are left as
   they are rather than rewritten around a single sample.

3. `cargo test --release` - exit 0, four lines:

   ```
   test result: ok. 123 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.42s
   test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
   ```

   (The first is `unittests src\main.rs`, the second `tests\release_tooling.rs`; 128 tests in
   total, 2 ignored.)

4. `cargo test` (debug) - the same two result lines with the same counts, exit 0.  So the
   README's Build and run block's `cargo test` is current too, once its count said 128 instead
   of 123.

5. The **shell/session check**.  A live window cannot be driven from here, and the README
   documents the harness for exactly that: `src/main.rs`'s `harness` module, whose
   `terminal_loop` test "drives a real shell through the terminal's own frame loop with **no
   window at all**".  It is `#[ignore]`d, so it is named explicitly; the README's Verification
   section lists it as the session check:

   ```sh
   TEK_SCRIPT="echo TEKMARK;ver" TEK_SECONDS=3 TEK_WAIT=1 TEK_PNG=session.png \
     cargo test --release -- --ignored --nocapture terminal_loop
   ```

   ```
   TLOG done: 1482 frames, 0.0 ms a frame, 7 commits, 835 bytes read, lit 134997
   test harness::terminal_loop ... ok
   test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 124 filtered out; finished in 7.08s
   ```

   exit 0, and the frame it wrote is a 1280x720 PNG.  A real shell started under the
   pseudoconsole, answered a typed script (7 commits, 835 bytes back) and the tube rendered it.

One environment note from this pass, because it will bite the next session too: in the agent
shell used here the release executable **cannot be started directly** -
`./target/release/tek-tube.exe --version` fails with `No such file or directory`, though the
file is a valid `PE32+ executable (console) x86-64` and `cmd.exe /c` runs it and prints
`tek-tube 0.3.4`.  The restriction is that shell's, not the program's; the render in step 2 was
run through `cmd.exe /c` for that reason.

### The release surface, checked 2026-10-15

- **`LICENSE.md` is present and is the FSL-1.1-MIT text**, as the README's Licence section says
  (it links `./LICENSE.md`), and `Cargo.toml` carries `license = "FSL-1.1-MIT"`.  Consistent.
- **`NOTICE.md` covers all three things the README claims**: the Hershey vector fonts
  (`assets/futural.jhf`, `assets/futuram.jhf`, `assets/gothgbt.jhf`), the sample plot files
  (`samples/*.plt`) and the reference photographs.  Its wording named `ref/` as if that were a
  directory of this repository - see the drift list below.
- **No binaries are committed.**  `git ls-files` comes to 8 `.rs`, 4 `.md`, 28 `.png`, 26 `.plt`,
  3 `.jhf`, 1 `.jpg`, 2 `.cmd`, 2 `.ps1`, 1 `.py`, `Cargo.toml`, `Cargo.lock`, `.gitignore` and
  `.gitattributes` - no `.exe`, `.zip`, `.dll` or `.pdb`.  `.gitignore` holds `/target` only, so
  an archive dropped in the tree by hand would show up in `git status`.
- The release the README points at is real and current: `gh release list -R
  kevinelong/tek-tube-releases` lists **v0.3.4** (2026-10-12) as latest, with
  `tek-tube-0.3.4-win64.zip` (461,836 bytes) and `SHA256SUMS.txt` as its assets, and v0.1.0 ..
  v0.3.3 behind it.

### Documentation drift found, and fixed in the docs rather than in the code

1. `README.md`, Build and run: `cargo test # 123 unit and end-to-end tests (2 ignored)`.  The
   count is **128** since 2026-10-14, when `tests/release_tooling.rs` added five.  Corrected.
2. `README.md`, Verification: "`RELEASING.md` and `.gitattributes` promised all of that and
   nothing checked it."  `RELEASING.md` is in the **releases repository**, not in this checkout
   (`git log --all -- RELEASING.md` is empty here), which the sentence did not say and a reader
   would go looking for.  It now names the repository.
3. `LICENSE.md` and `NOTICE.md` listed the reference photographs as "`ref/`, ..." - reading as a
   directory of this repository.  **It is not**: `ref/` is `C:\Users\kevin\tek\ref`, the parent
   directory of the checkout, holding `tek4010_screen.jpg`, `tek4052.jpg`, `tek611.jpg`,
   `tek4010.jpg` and `tek_t4002.jpg`, and it is untracked; only `docs/reference-real-4010.jpg`
   is in the repository (the README already said "in the parent directory").  Both files now
   say where the photographs live.
4. `README.md`, Download: the release was described as "`tek-tube.exe` ... with its SHA-256
   sums".  The archive also carries the Windows setup (`setup.cmd`, `setup.ps1`,
   `uninstall.cmd`, `uninstall.ps1`) and the licence/notices text - which is exactly what
   `tests/release_tooling.rs` pins and what `RELEASING.md` in the releases repository
   prescribes.  The sentence now names the payload.  No claim was false about the *source*
   repository: the binary, the scripts' siblings and the licences are all where they are said
   to be.

### Waiting on Kevin

1. **The CI decision is still open** (carried from 2026-10-14).  There is no workflow, so the
   128 tests run nowhere but on this machine.  A `windows-latest` job - checkout, toolchain,
   `cargo test` - would run them on every push; it was deliberately not added, because adding a
   workflow cannot be verified without pushing it to a public repository whose release pipeline
   matters.
2. **The three window checks** (carried from 2026-10-12) still need eyes on a running window:
   no second console window at Start-Menu launch, the cursor as a soft white-green stroke, and
   the preset row committing on a wheel step.
3. **`samples/*.plt` provenance** (carried from 2026-10-12): the collection is redistributed
   because the repository is public, while `NOTICE.md` records it as personal use.  Unresolved,
   and his call.
4. **Unchanged by this pass:** the release path beyond the payload scripts - the zip, its
   `SHA256SUMS.txt` and the version the archive's own `README.txt` claims - still has no check,
   and cannot have one from here: it lives in `tek-tube-releases` and needs a real release.
   `archive/README.txt` there is at 0.3.0, which `RELEASING.md` says to bump per release.

## Where it stands, 2026-10-14 - restart here

A test-and-audit pass. **No source changed** - one new integration test file, and the README's
Verification section now describes it. Both repositories are clean and still in sync, and the
release is still v0.3.4.

| | |
|---|---|
| Source | `tek-tube` at **`f30ac5f`**, clean and in sync (was `a99ae5a`; `3a9e3d0` added the release-surface tests, `f30ac5f` documents them) |
| Tests | `cargo test`: **128 pass, 0 fail, 2 ignored** (123 before, + 5 new) |
| Releases | unchanged: `kevinelong/tek-tube-releases` latest **v0.3.4**, nothing published |
| CI | **none** - this repository has no workflows at all, so the 128 tests run nowhere but here |

New: `tests/release_tooling.rs`, five tests over the Windows release surface rather than the tube.
The four scripts the archive ships must exist and be **CRLF** (`.gitattributes` pins it because a
batch file that arrives with LF endings "is a lottery", and `uninstall.cmd` deletes itself while
cmd is still reading it); each `.cmd` must reach its `.ps1` sibling through `%~dp0`, so the pair has
to travel together in the archive; `uninstall.cmd`'s `(goto) ... del /q "%~f0"` self-delete line
must still be present and still ordered before `exit /b` - that ordering is what makes deleting the
file while cmd reads it possible; and `LICENSE.md` / `NOTICE.md` must exist, because the archive
carries the licence as text rather than a link. **Falsified before it was trusted**: stripping the
CRs from `setup.cmd` fails the CRLF test, and reflowing the self-delete line fails the uninstaller
test.

Audit answers worth keeping:

- **The release path was not verified at all** before this pass; now the payload *scripts* are. The
  archive itself - the zip, `SHA256SUMS.txt`, the version its README claims - still has no check,
  and cannot have one from here: it lives in `tek-tube-releases` and needs a real release.
- **No test depends on a machine-specific path.** `SystemRoot` is read with a `C:\Windows` fallback
  and only used to *construct* the expected command line (`src/main.rs:2757/2853`).
- **No test depends on the pristine console window**, but note `src/main.rs:3395` is wrapped in
  `if console_processes() > 1`: on a machine with a single console it asserts nothing at all. It
  cannot fail for the wrong reason; it also cannot catch the owned-console branch there. That half
  stays the Start Menu check below.

### Waiting on Kevin

1. **There is no CI, and adding it is a one-line decision.** A `windows-latest` job - checkout,
   toolchain, `cargo test` - would run the 128 tests on every push. Not added: this pass's brief was
   to audit rather than rebuild, and a workflow cannot be verified without pushing it to a public
   repository whose release pipeline matters.
2. The three window checks from 2026-10-12 are unchanged and still need eyes on a running window:
   no second console window at Start-Menu launch, the cursor as a soft white-green stroke, and the
   preset row committing on a wheel step.
3. `samples/*.plt` provenance: still unresolved and still Kevin's call.

## Where it stands, 2026-10-12 evening - restart here

Everything from this session is committed, pushed and published, and nothing is left in
flight: both repositories are clean, `origin` has every commit, and the release the Start Menu
runs is the newest one.

| | |
|---|---|
| Source | `tek-tube` at `a99ae5a`, clean and in sync |
| Releases | `kevinelong/tek-tube-releases`: latest **v0.3.4**, with v0.1.0 .. v0.3.3 behind it |
| Installed here | `%LOCALAPPDATA%\Programs\tek-tube`, **0.3.4**, hash matched that release's `SHA256SUMS.txt` |
| Start Menu | five entries: `tek terminal` (default shell Windows PowerShell), plus PowerShell, Command Prompt, Git Bash, WSL (Ubuntu) |
| Settings | `%APPDATA%\tek-tube\shells.txt` (five shells, `default = Windows PowerShell`) and `presets.txt` |
| Tests | `cargo test`: 123 pass, 0 fail, 2 ignored |

Re-verify in four commands:

```sh
cd C:/Users/kevin/tek/tek-tube && cargo test
C:/Users/kevin/AppData/Local/Programs/tek-tube/tek-tube.exe --version    # 0.3.4
TEK_CURSOR=1 TEK_SECONDS=1 cargo test --release harness::terminal_loop -- --ignored --nocapture
gh release list -R kevinelong/tek-tube-releases
```

### Waiting on Kevin

1. **The console window (0.3.4).**  Launch from the Start Menu and check there is no second
   black window.  A brief flash at startup is possible, since conhost creates the window
   before the process runs.  If the window you saw contained a *shell prompt* rather than the
   program's own report, it is something else - nothing has been seen or reproduced for that.
2. **The cursor (0.3.3, 0.3.4).**  Look at the underline at the tube: it should be a soft
   white-green stroke, as thick as the glyphs and steady.  `..\_scratch\cursor-zoom3.png` is a
   6x crop of a rendered frame; `cursor-zoom-left.png` is the earlier flat bar for comparison.
3. **The preset row** applies a preset as you walk it - a wheel step applies, not only a
   press.  Deliberate (the row is a *view* of the machine's state, so it cannot show a preset
   the machine is not in), but it is the one chooser that still commits while browsing.  Make
   it press-only on request; that needs a pending-name state, or the row snaps back to
   `custom` on the next settle.
4. **`samples/*.plt`** are redistributed now that the repository is public, and the upstream
   collection documents them as personal use.  The download deliberately does not carry them.
   Unresolved, and Kevin's call.

### Scratch, and what may be deleted

`C:\Users\kevin\tek\_scratch` holds this session's throwaway: the release notes for
v0.3.0 .. v0.3.4 (`dist-v03N-notes.md`), the drivers that verified the setup
(`setup-forms-test.ps1`, `setup-shortcut-test.ps1`, `setup-sandbox.ps1`, `real-install.ps1`),
the window probe (`probe-windows.ps1`), the commit messages (`commit-*.txt`), the two cursor
crops, and the licence sweep's plan and log (`license-plan.json`, `license-log.json`,
`repos.json`).  All of it may be deleted except the three JSON files: they are the evidence
behind `LICENSE-SWEEP-2026-10-12.md`.

## The project

`C:\Users\kevin\tek\tek-tube` - a Tektronix 4010/4014 storage tube, as a terminal
and as a plot-file player, in Rust.  Remote: **public since 2026-10-12**,
`https://github.com/kevinelong/tek-tube` as `origin`, pushed and in sync.  Windows
builds are published from `github.com/kevinelong/tek-tube-releases` - see
"Published" below.

Three commits from this session:

- `169b9ef`  F2 options panel, presets, and the terminal work for full-screen programs
- `2767450`  VT coverage a full-screen program needs, and reverse video drawn
- `15b10c7`  Presets you can save, and rows that relaunch instead of lying

`cargo test` and `cargo test --release`: 112 tests, both green.  `rustfmt --check
--config skip_children=true` is clean for `src/main.rs`, `src/panel.rs` and
`src/vt.rs`.  Do **not** run `cargo fmt` across the crate: rustfmt 1.9 is not the
formatter this repository's other files were written with, and it re-wraps
`terminal.rs` and `tube.rs` wholesale.

## Published, 2026-10-12

A later session turned this into a published project.  The repository was private; it is
now **public** under **FSL-1.1-MIT**, and the Windows binaries are published from a second
repository, `github.com/kevinelong/tek-tube-releases` (the same shape as `fc` and
`fc-releases`).

- `LICENSE` became `LICENSE.md`, and the body is now the canonical FSL-1.1-MIT text
  verbatim - diffed against `https://fsl.software/FSL-1.1-MIT.template.md` with the
  placeholders filled - behind a short preamble saying what the licence covers and, more
  to the point, what it does not: the Hershey fonts in `assets/`, `samples/`, the
  reference photographs.  The old preamble described a different project altogether, which
  is the one thing a licence preamble must not do.
- `Cargo.toml`: `license = "MIT"` became `FSL-1.1-MIT` (a valid SPDX identifier).  No MIT
  copy of 0.1.0 was ever published - the crate has no crates.io entry - so no
  previous-licence conversion clause is needed.
- `README.md`: a Download section pointing at tek-tube-releases, a Licence section, and the
  Hershey claim corrected from "public-domain" to the conditional terms `NOTICE.md` already
  records.  `NOTICE.md` now names all three `.jhf` files, not only `futural`.
- Release `v0.1.0` of tek-tube-releases: `tek-tube-0.1.0-win64.zip` (447,946 bytes, sha256
  `888bab977a47c98aced762920fcc8552a6c016042bcd83e9ab8403129238316d`), `tek-tube.exe`
  931,840 bytes sha256
  `cbc8b1e6cf784da414855af6a854c0e46bf88df1f3f19baebcf3f18174ca9e10`, built from commit
  `3d4ae7a` with rustc 1.96.0, 112 tests green.  Checked the way a user gets it: downloaded
  anonymously, extracted into an empty directory, rendered `samples/05_snoopy.plt` to a PNG
  with nothing beside it - the fonts are `include_str!`, so the executable is the whole
  program.  The licence and the third-party notices are inside the archive, not only
  linked.
- The sample plot files are **not** in the download: `NOTICE.md` records them as a
  historical collection provided for personal use.  Publishing the repository does
  redistribute them, which was Kevin's call on 2026-10-12; it is the one item here worth
  revisiting if that collection's provenance is ever questioned.

## The console window, 2026-10-12 (later session)

Kevin: *"why is there a visible separate terminal window when we run tek in addition to the one
we created?"*.  Measured, not guessed: the executable's PE subsystem is 3 (`WINDOWS_CUI`), and
Windows gives a console program a console of its own whenever it is started *without* one -
the Start Menu, a shortcut, Explorer, which is how the tube is normally launched - and conhost
puts that console on the screen.  It was never the shell's: the shell runs under a
pseudoconsole, which has no window at all, and the only thing that console ever showed was the
program's own closing report.

The subsystem stays, because a windows-subsystem build would not be *waited for* by a shell
running `tek-tube --png out.png` (the file would not exist yet when the next command ran) and
its report would go nowhere.  Instead `hide_owned_console()` hides the console when
`GetConsoleProcessList` says it is ours alone - one entry - and leaves a shell's exactly as it
is.  Hidden rather than freed: `FreeConsole` destroys the window but invalidates the standard
handles, and the closing report would then fail rather than go to a console nobody can see.
Test `a_console_shared_with_a_shell_is_left_alone` (123 tests in total).

The *owned* branch cannot be measured from a harness session: processes started there are not
on the interactive window station, so the window probe written for it found no windows at all
for any process it started, not even the tube's.  So that half is Kevin's five-second check -
from the Start Menu there should now be no console window.  A brief flash at startup is
possible: conhost creates the window before the process runs, and the hide is the first
statement of `main`.  Release `v0.3.4` (zip 461,836 bytes sha256 `88a8842c...`, exe 945,664
bytes sha256 `9536ee85...`, source `a99ae5a`).

## The cursor stays, 2026-10-12 (later session)

Kevin: *"cursor still fades away completely perhaps it should redraw after each fade"*.  The
cursor was drawn as a `Write::Flash` transient, and only when something else changed, so it
decayed to the flood-gun floor within a second of the shell going quiet and the frame the
window kept was the last, dim one.

His suggested fix was built and measured first, then rejected on cost: a flash rewritten on a
clock every `0.69` of the glow's time constant never lets `fresh_alive` clear (`decay` clears
it only below `ALIVE_SCALE`), so the tube would never go idle again and a 1024x780 frame
would be recomposed ~58 times a second, forever, for one underline - exactly the waste the
aliveness check was added to prevent.  Instead `Tube::cursor` holds the underline's dots and
the render adds their light at stored-ink level in pass 1: a *steady* mark, which is what the
hardware's hosts produced (a DVST cursor was stored ink, redrawn with the page to move it),
not charge (an erase does not blink it, moving it leaves no trail) and not a transient (an
idle tube still rests, and the mark is in whatever frame stays up).

Release `v0.3.2` (zip 461,312 bytes sha256 `2ef9e0e2...`, exe 945,152 bytes sha256
`18b4f36f...`, source `2e8ccce`, 122 tests).

- Harness, idle tube, `TEK_CURSOR=1`, the rendered frame's green channel ~1400 times over 6 s:
  **steady 188** against a blank-cell floor of 76; before, the same measurement read dimmest
  77 (the floor - the cursor gone) and brightest 206.
- Idle cost, real window against the installed v0.3.1 on the same idle shell, 25 s: 797 frames
  drawn against 873, both stopping once the trail's light has decayed (~13 s in).
- Two traps that cost a debug cycle here: the rendered frame is in image orientation (tube y is
  output row `h-1-y`), and the mark lives in the *frame*, so `Tube::rgb_at` - which samples the
  populations - does not see it.  The cursor tests read the rendered buffer.

### ...and drawn as a stroke, 0.3.3

Kevin: *"cursor seem too thin and too crisp and no at all like a rendered font underscore"*.
The v0.3.2 mark was a two-dot rectangle of light in the settled green.  It is `CursorMark`
now - the segment plus the terminal's own stroke width (`tube.sigma`) - deposited through the
same Gaussian profile `stroke` deposits charge with, so the underline is as thick and as soft
as the glyphs beside it, and drawn at a *fresh* stroke's level **in both channels**: the total
`light` and the `fl` the colour pass reads for the hot chromaticity.  That second channel is
the part that is easy to miss - at full level in the settled channel the bar was still green,
because the whiteness of fresh ink on this tube comes from the fresh channel, not from the
level - and a host keeping its cursor written leaves it hot anyway.

Measured across the underline (green channel, y-6..y+6): `89, 90, 123, 164, 211, 250, 255,
251, 212, 164, 124, 88, 89`, against 76 for a blank cell.  Release `v0.3.3` (zip 461,804
bytes sha256 `462aad1c...`, exe 945,664 bytes sha256 `f1a98a1b...`, source `09357df`, 122
tests).  The 6x crop of a rendered frame is kept at `..\_scratch\cursor-zoom3.png` when the
look wants checking again; the harness prints the same profile under `TEK_CURSOR=1`.

## One click is one step, 2026-10-12 (later session)

Kevin: *"clicking arrows on multiple choice seems to keyrepeat immediately effectively making
the choice random instead of the next choice"*.  Measured in the code, not guessed:
`Panel::handle` is called once a frame with the mouse's **state**, and `Choice` and `Toggle`
acted on that state - so a click lasting N frames stepped N times (on a three entry list that
reads as random: `mono` -> `gothic` -> `simplex` -> `mono`), and a box flipped once per frame,
so its release decided the value by the parity of the click's length.  Held on a launch row it
was worse: `zoom` re-executed the program every frame the button was down.

Fixed by taking a press from the button's **edge**: `Panel` keeps `down_last`, `handle`
computes `pressed = down && !down_last` once per frame, and the lists and boxes use that.  A
slider keeps the state, because following the pointer while it is down is what a drag is.  The
**keyboard** was already right - the window loop compares each panel key against the previous
frame's key states (`nav_was`), so a held arrow or a held Enter is not a repeat - which is why
this only ever showed up with the mouse.

Release `v0.3.1` (zip 461,160 bytes sha256 `0f702606...`, exe 945,152 bytes sha256
`25dc2e7a...`, source `920d54e`).  Tests 119, two added: a held button steps a list once and
arms the next press on release, and a held button flips a box once.  One old test pressed
twice without releasing between the presses; it models real input now.

## The panel's face row, 2026-10-12 (later session)

Kevin: *"changing font is refreshing too immediately and not allowing navigating and making a
choice"*.  Measured in the code: `font` was a `launch` row and the panel's settle fires on
*any* value change - a wheel notch steps a Choice row just as a click does - so one notch
re-executed the program 0.25 s later and the shell died with it.  The list could not be
walked at all.

Two changes, shipped as release `v0.3.0` of tek-tube-releases (zip 461,122 bytes, sha256
`1eebfd07...`, exe 944,640 bytes sha256 `2acd716c...`, source `93bfd48`):

- `font` is a **structural** row now, not a launch one.  The panel has carried that flag all
  along - "the tube has to be reconfigured and the page written again (the face, the device,
  the geometry)" - and the terminal does exactly that: `configure_terminal_tube` with the new
  face, then `Terminal::restroke` (clear the pen record and the last/flash screens, set
  `page_replaced` - the recipe `resize` uses).  A plain commit would have drawn nothing,
  because the cells hold the same characters and nothing is stale.
- A **launch** row - `device`, `zoom`, `shrink` in the plot path - is honoured only by a
  *press*: Enter, or a click on the row.  A step says what a press would do and changes
  nothing else.  `Panel::settle_due` returns `(row, press)` so the caller can tell the two
  apart, and `touched(row, press)` is where the flag comes from.

Measured through the terminal harness with no window, one page and one session
(`TEK_REFONT="gothic,simplex,mono,mono" TEK_SCRIPT="dir"`):

    gothic   lit 66694 -> 19266
    simplex  lit 19266 -> 37525
    mono     lit 37525 -> 21594
    mono     lit 21594 -> 20704      the same face twice, 4% apart
    REFONT session answered after the changes: true

The number follows the face, not the rewrite, and the shell answered a command typed after
four face changes.  `TEK_REFONT` is a new knob on the existing harness, kept because that
module exists for exactly this kind of live-path question.

Also fixed on the way: the distro README no longer embeds the archive version in its
examples (the file name carries it); the zip's own README.txt still does, because there the
version identifies the build.

## Start Menu setup, 2026-10-12 (later session)

`tools\setup.cmd` installs the Windows build and puts a **tek terminal** in the Start Menu,
with a shell choice like Windows Terminal's.  Shipped as release `v0.2.0` of
tek-tube-releases (archive 460,490 bytes, sha256 `ecc4bcc3...`; `tek-tube.exe` unchanged at
942,592 bytes, sha256 `ff1c36d0...`, source `e07faa8`).

- Installs to `%LOCALAPPDATA%\Programs\tek-tube` with no administrator; the entries are
  `tek terminal*.lnk` in `%APPDATA%\Microsoft\Windows\Start Menu\Programs`, and the settings
  live in `%APPDATA%\tek-tube\shells.txt` (`name = command line`, plus `default = <name>`).
- The entries pass a *name*: `tek terminal (Git Bash)` runs `--shell "Git Bash"`, and the
  plain entry passes nothing, so it follows `default` in that file.  `--list-shells` prints
  what is configured, `--version` prints the build, and `uninstall.cmd` reverses the install
  (`-Purge` also removes the settings).
- Three defects were found by testing, not by reading, and all three are fixed:
  1. `--shell` split its command line on whitespace, so no shell under `Program Files` could
     ever be started - the program name was `"C:\Program`.  `split_command` now quotes the
     way Windows does, proved by running Git Bash under the terminal harness and getting
     `BASHMARK 5.3.15(1)-release` back off the pty.
  2. `wsl -l -q` writes UTF-16 and PowerShell 5.1 decodes one byte per character, so the
     distribution name carried five NULs: invisible in every print, and fatal to
     WScript.Shell, which refuses a shortcut path containing one with "The shortcut pathname
     must end with .lnk or .url".  The NULs are stripped where the name is read.
  3. An array passed through `setup.cmd` reaches PowerShell as separate words, so
     `-Shells 'a','b'` refused to run at all; the list is split on commas now, which is the
     form that works from cmd.exe and from PowerShell alike.
- Verification: sandbox runs with `%APPDATA%` redirected (the shortcuts land in a sandbox
  Start Menu) covered install, uninstall, `-Purge` and every argument form; the real install
  came from the *published* archive - download, unpack, its own `setup.cmd` - and a
  four-second windowed run of the installed program on `--shell 'Git Bash'` reported 143
  bytes read, 3 commits, 143 bytes, exit 0.
- The batch files are pinned to CRLF in `.gitattributes`: cmd.exe reads a batch file block by
  block and `uninstall.cmd` deletes itself, which is exactly where LF endings would be a
  lottery.
- The sample plot files are still not redistributed; the archive carries the program, the
  setup and the licences, and its README says to clone the source repository for the files.
- The verification drivers are beside this note in `..\_scratch\`: `setup-forms-test.ps1`,
  `setup-shortcut-test.ps1`, `setup-sandbox.ps1`, `real-install.ps1`.

## What exists now

- **F2 options panel**, drawn on the tube with its own stroke font, driven by mouse
  or keyboard (arrows walk, Enter presses, Escape closes).  Values apply a quarter
  of a second after the dragging settles, not every frame; only `cell` rewrites the
  page.
- **Presets**: `wyse` and `tek` as bundles of terminal rows, showing `custom` when
  the machine matches neither, plus the user's own saved into four numbered slots
  (`slot` names one, `save` writes the current rows into it).  Stored as plain text
  under the user's config directory - `%APPDATA%\tek-tube\presets.txt` on Windows,
  `$XDG_CONFIG_HOME` or `~/.config` on Unix - never in the repository.
- **Launch rows**: `font` in the terminal panel; `device`, `zoom` and `shrink` in
  the plot panel.  They re-execute the program with argv rebuilt from the live
  options and end the session.  `device` is deliberately absent from the terminal
  panel, where `run_shell` hardcodes the 4010 face.
- **The plot path has a panel too**, offering only rows a change takes effect on.
- **VT**: charset designators consumed; the DEC special graphics set and the UTF-8
  box characters drawn as line art on the cell's edges so frames join; DECSC/DECRC
  carrying the attribute and charset; DECCKM; tab stops; DECOM; DECSTR; scroll-down;
  `?3` consumed without resizing; `ESC #` double width/height recorded but not drawn
  at double size; mouse tracking `?1000/?1002/?1003` with SGR `?1006`.
- **Reverse video drawn as a stencil** - the cell filled with the pen, the glyph cut
  out of it - with its cost in the comment.  Colour is swallowed; there is no
  clipboard, so bracketed paste is advertised but not implemented.
- **`TERM` is forced to `vt100`** for the child, with `TEK_TUBE_TERM` as the
  override.  Not inherited: an inherited `xterm-256color` asks a remote for 256
  colours it cannot have, and an inherited `dumb` makes a program refuse to draw.

## Verified live, and verified only by test

Measured in the real window, from inside the tube:

- the F2 panel by mouse and by keyboard; the `wyse` preset applied from the
  keyboard, face going black (mean green 1.24 against 76);
- the plot panel's sixteen rows over a real plot file;
- ssh out to WSL Ubuntu and into this machine's own OpenSSH server, both reporting
  `TERM=vt100`, with `stty size` confirming the pty is the page (18x37 - htop's
  cut-off columns are htop being cramped at 37, not a bug here);
- htop over ssh drawing its meter bars as stencilled reverse video.

**Not verified live**: pressing `save`, and pressing a launch row.  Both are covered
by unit tests; the live attempts produced no preset file and an unchanged process
command line, with a black capture of the panel area - most likely the panel did not
open in those attempts, since synthetic input has been unreliable throughout.  A
thirty-second hand test settles it: F2, press `save` (the note should say "saved as
user 1", the file should appear), then step to `font` and press it (the window
closes and reopens, session gone).

## The machine, and what this session changed

- `~/.ssh` had an ACL that stopped Windows' ssh from running at all; fixed and
  verified (see `ELEVATED-SESSION.md`).  The same ACE is **still on the profile
  root** - see open question 1.
- The **Windows OpenSSH Server** is installed (StartType **Manual**, so nothing at
  boot) and its firewall rule exists.  The service is stopped on exit:
  `Start-Service sshd` to use it.
- **WSL Ubuntu** was started for the tests and is stopped on exit.  Its sshd on
  `:22` is a manual instance - re-run `wsl-sshd.sh` after a distro restart - and the
  throwaway key is authorised in its `/root/.ssh/authorized_keys`.
- Left in this directory: `sshd_probe.py`/`sshvenv` (a paramiko loopback sshd),
  `vtprobe.ps1`, `acsprobe.ps1`, `wsl-pty.sh`, `wsl-tui-demo.sh`, `win-sshd-demo.cmd`,
  `sshprobe_key`/`.pub` (a throwaway, unencrypted key), the session logs, and the
  screenshots named in the running record.  Delete any of it freely.

## Open

Questions 1-5 in `OPEN-QUESTIONS-2026-10-05.md` are unanswered (the profile-root ACE,
what created that SID, `CodexSandboxUsers`, and whether to keep or revert the ssh-test
machine changes).  Questions 12-17 are decided but deferred to another session, so
the startup banner still does not mention F2, and the plot panel's `zoom`/`shrink`
are launch rows rather than live ones by design.
