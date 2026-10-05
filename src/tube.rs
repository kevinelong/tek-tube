//! Direct-view bistable storage tube model.
//!
//! Four things make a 1970s Tektronix 4010/4014 look the way it does, and all
//! four are modelled here:
//!
//! * `stored` - charge left by the write gun.  This is the image that stays on
//!   the screen until the whole screen is erased, and it burns in the
//!   characteristic green of the flood-gun illuminated target.
//! * `fresh`  - the hot flash of a vector that has *just* been written.  The
//!   write beam is far brighter than the stored image, so freshly painted
//!   strokes come in almost white and settle back to the stored level.
//! * `under`  - write-through ("dark mode") vectors, which never store and
//!   therefore only exist while the host keeps re-sending them.
//! * the flood gun itself - it floods the whole target, so unwritten areas are
//!   not black but a dark, uneven green field.  On real hardware (museum 4010
//!   photos, the 4052 recordings) that field is visibly blotchy, darker in the
//!   corners, and it brightens slightly around dense stored areas as light
//!   halates through the phosphor and glass.
//!
//! The beam is a Gaussian current density and strokes deposit *coverage*
//! (`exp(-d^2 / 2 sigma^2)` of the distance to the stroke) instead of lighting
//! whole pixels, so the target behaves like the vector device it is.  For the
//! screen the populations are area averaged in light space, halation is added,
//! and only then is the phosphor curve applied.

use crate::font;
use crate::protocol::{LineStyle, Sink, Style, Write};

/// How far the Gaussian beam profile is evaluated, in sigma.
const PROFILE_REACH: f32 = 2.6;
/// Coverage below this is dropped.
const PROFILE_CUTOFF: f32 = 0.015;

/// The display transfer is a power curve, so it is tabulated: the colour pass
/// used to pay for three `powf` per output pixel, which at 1024x780 was a
/// measurable slice of every frame.  Light at or above full scale encodes to
/// full scale (the curve reaches 1 at exactly 1), so the table covers `0..=1`
/// and everything brighter clamps to the top entry.
const RAMP_MAX: f32 = 1.0;
const RAMP_STEPS: usize = 2048;

/// Exponent of the display transfer, `display = light^(1/DISPLAY_GAMMA)`.
/// 2.2 is the phosphor/CRT/recording chain that the references were captured
/// through; it is baked into the table above so the hot loop only does lookups.
const DISPLAY_GAMMA: f32 = 2.2;

/// How the tube turns stored/fresh/under charge into emitted light.
///
/// The chain is linear until the very last step:
///
/// 1. the three populations are summed as *emitted light* (arbitrary but
///    linear units),
/// 2. halation is added there - scattering is linear in light,
/// 3. the flood gun's own glow is summed in,
/// 4. and only then is the display transfer applied, per channel, to get the
///    byte a display needs in order to emit that light.
///
/// Chromaticity therefore lives in linear light: every emitter has a fixed
/// linear red:green:blue ratio, and the transfer's exponent turns it into the
/// ratio a recording shows.  That is why the stored image (about 0.005:1:0.20
/// in light) reads as b/g ~ 0.48 while the flood gun's much bluer glow
/// (0.011:1:0.95) reads as b/g ~ 0.97 - both measured, and only explicable if
/// the two emitters have different linear chromaticities.
#[derive(Clone)]
pub struct Phosphor {
    /// Relative contribution of each population to the emitted light.
    pub stored_gain: f32,
    pub fresh_gain: f32,
    pub under_gain: f32,
    /// Linear-light chromaticity of the stored image, green normalised to 1.
    pub stroke_r: f32,
    pub stroke_b: f32,
    /// ... and of the write beam's flash, which blows out towards white.
    pub hot_r: f32,
    pub hot_b: f32,
    /// How strongly the flash's freshness shows as that white.
    pub fresh_whiten: f32,
    /// Flood gun: glow level (in emitted-light units) and its chromaticity.
    pub ambient: f32,
    pub ambient_r: f32,
    pub ambient_b: f32,
    /// Flood gun field variation: blotch contrast, per pixel grain, and how
    /// much darker the corners of the face are.
    pub mottle: f32,
    pub grain: f32,
    pub vignette: f32,
    /// Halation: light scattering through phosphor and glass.  Five scales are
    /// summed to approximate the heavy-tailed scatter of the real thing.  Each
    /// weight is the fraction of a stroke's light that scale contributes,
    /// measured on museum photos: about 5 % five dots out, 1.7 % at 37 dots,
    /// 0.9 % at 70 dots and 0.3 % at 98 dots.  Radii are quoted in 4010 dots,
    /// i.e. as a physical distance on the face, so they are the same size on a
    /// 4014 target (see `render_into`).
    pub bloom: f32,
    pub bloom_radius: f32,
    pub bloom_mid: f32,
    pub bloom_mid_radius: f32,
    pub bloom_wide: f32,
    pub bloom_wide_radius: f32,
    pub bloom_far: f32,
    pub bloom_far_radius: f32,
    pub bloom_farther: f32,
    pub bloom_farther_radius: f32,
    /// The slow, large scale unevenness of the flood gun across the face: the
    /// museum photo's background varies by about +-18 % on top of the fine
    /// blotches, brightest lower left.
    pub gradient: f32,
    /// Sparse brighter specks in the field (measured: ~2 % of background
    /// pixels, about 26 % above the local field, one to three dots across).
    pub speck: f32,
    /// The raster scan's own banding: a raster terminal lights the screen a line
    /// at a time, so a lit area is striped rather than flat, which is most of
    /// what makes a photograph of one read as a CRT.  Zero - the default - has
    /// no structure at all, which is what a vector display does.
    pub scan_depth: f32,
    /// Scan lines a display row: one every three rows unless told otherwise.
    pub scan_freq: f32,
    /// `light^(1/DISPLAY_GAMMA)` tabulated over `0..=RAMP_MAX`, linearly
    /// interpolated.  The table *is* the transfer: there is no live exponent to
    /// get out of step with it.
    ramp: Vec<f32>,
    /// Camera-like capture (off by default: this models the *device*; the
    /// source video is the device seen through a phone camera).  A diagnostic
    /// post-process, not part of the tube model: the curve is calibrated so
    /// that enabling it reproduces the source video's measured values from the
    /// same scene.
    pub camera: bool,
    /// The curve pivots on the stored stroke level and applies a contrast
    /// gamma: with the measured stroke and background levels of the source
    /// video (0.69 and 0.31 of full scale recorded as 0.69 and 0.035) the
    /// gamma comes out at ~3.7.
    pub cam_pivot: f32,
    pub cam_gamma: f32,
    /// Soft knee for the highlights, so a bright flash rolls off instead of
    /// clipping to a flat white.
    pub cam_knee: f32,
    /// Channel leak of a real camera on a green screen: the source video
    /// records 0.22-0.26 of the green in its red channel.
    pub cam_leak: f32,
    /// The same leak into the blue channel: the video records blue/green 0.57
    /// where the leak-free curve gives 0.29.
    pub cam_leak_b: f32,
    /// The phone's lens/compression blur: the video's strokes measure 3.7 dots
    /// across where the sharp museum photo measures 2.2.
    pub cam_blur: f32,
    /// Sensor/compression noise, in 0..1 units per pixel.
    pub cam_noise: f32,
}

impl Default for Phosphor {
    fn default() -> Self {
        let mut ramp = Vec::with_capacity(RAMP_STEPS + 1);
        for i in 0..=RAMP_STEPS {
            let light = i as f32 / RAMP_STEPS as f32 * RAMP_MAX;
            ramp.push(light.powf(1.0 / DISPLAY_GAMMA));
        }
        Phosphor {
            // A stored stroke carries 0.44 of emitted light; the display
            // transfer below turns that into the 0.69 of full scale that both
            // the museum photo and the source video measure.
            stored_gain: 0.44,
            // A just-written stroke adds the beam's own flash on top, landing
            // the sum at full scale: the flash is white.
            fresh_gain: 0.56,
            under_gain: 0.50,
            // measured on hardware: the stored green reads as b/g ~0.46 and
            // r/g ~0.02 - much *greener* than the flood gun's own glow, which
            // is why a single "phosphor colour" cannot produce both
            stroke_r: 0.002,
            stroke_b: 0.06,
            // the beam's flash saturates towards white with a cyan cast
            hot_r: 0.91,
            hot_b: 0.96,
            fresh_whiten: 1.70,
            // measured on real hardware: an unwritten target glows at 0.0725 of
            // a stored stroke's light, and it is much bluer than the strokes
            // (b/g 0.97 against the strokes' 0.48)
            ambient: 0.0725,
            ambient_r: 0.011,
            ambient_b: 0.95,
            // fitted to the de-vignetted field of the museum photo: a blank
            // target varies by about 4 % overall, of which only ~1 % is at
            // 8-16 dot blocks, so the mottle must stay fine and faint
            mottle: 0.05,
            grain: 0.04,
            vignette: 0.15,
            bloom: 0.12,
            bloom_radius: 5.0,
            bloom_mid: 0.05,
            bloom_mid_radius: 24.0,
            bloom_wide: 0.03,
            bloom_wide_radius: 90.0,
            bloom_far: 0.012,
            bloom_far_radius: 220.0,
            // the tail of the measured halo: still visible 300+ dots out
            bloom_farther: 0.006,
            bloom_farther_radius: 420.0,
            gradient: 0.10,
            speck: 0.20,
            scan_depth: 0.0,
            scan_freq: 1.0 / 3.0,
            ramp,
            camera: false,
            cam_pivot: 0.69,
            cam_gamma: 3.70,
            cam_knee: 2.20,
            cam_leak: 0.24,
            cam_leak_b: 0.28,
            cam_blur: 0.45,
            cam_noise: 0.006,
        }
    }
}

impl Phosphor {
    /// How much light a display row carries from the scan itself: 1.0 with no
    /// structure, dipping by `scan_depth` between lines.  A cosine, so the
    /// bands have no hard edges to alias against the display's own grid.
    pub fn scan_at(&self, row: usize) -> f32 {
        if self.scan_depth <= 0.0 {
            return 1.0;
        }
        let phase = (row as f32 * self.scan_freq) % 1.0;
        let band = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
        1.0 - self.scan_depth * band
    }

    /// Display level for a linear light value: the phosphor's own
    /// non-linearity and the recording chain, as one exponent.  Table driven,
    /// because this runs three times per output pixel.
    fn encode(&self, light: f32) -> f32 {
        if light <= 0.0 {
            return 0.0;
        }
        if light >= RAMP_MAX {
            return 1.0;
        }
        let x = light * (RAMP_STEPS as f32 / RAMP_MAX);
        let i = x as usize;
        let f = x - i as f32;
        self.ramp[i] * (1.0 - f) + self.ramp[i + 1] * f
    }

    /// Colour from an *area averaged* light level, the share of it emitted by
    /// the decaying write beam, and the flood gun's glow at this spot.  All
    /// three are linear light, and the display transfer is applied exactly
    /// once, at the end, to the chromaticity each emitter contributes.
    pub fn rgb_from_light(&self, light: f32, fresh_light: f32, ambient: f32) -> (f32, f32, f32) {
        let f = if light > 1e-6 {
            ((fresh_light / light) * self.fresh_whiten).min(1.0)
        } else {
            0.0
        };
        let mix = |a: f32, b: f32| a + (b - a) * f;
        // green carries every population; red and blue follow the
        // chromaticity of whichever emitter lights this spot
        let g = light + ambient;
        let r = light * mix(self.stroke_r, self.hot_r) + ambient * self.ambient_r;
        let b = light * mix(self.stroke_b, self.hot_b) + ambient * self.ambient_b;
        (self.encode(r), self.encode(g), self.encode(b))
    }

    /// Linear RGB in 0..1 for one spot of the target (halation excluded - that
    /// is a property of the whole glass, applied when the screen is composed).
    pub fn rgb_flat(&self, stored: f32, fresh: f32, under: f32, ambient: f32) -> (f32, f32, f32) {
        let light = stored * self.stored_gain + fresh * self.fresh_gain + under * self.under_gain;
        // a write-through stroke is not stored, but it is still the beam, so it
        // shows the same flash as a fresh one
        let flash = fresh * self.fresh_gain + under * self.under_gain;
        self.rgb_from_light(light, flash, ambient)
    }
}

/// What a cached flood field was built for: the output geometry and the field
/// constants.  Exact float comparison is enough, because the constants only
/// change when the command line is parsed.
#[derive(Clone, Copy, PartialEq)]
struct FieldKey {
    w: i32,
    h: i32,
    shrink: i32,
    mottle: f32,
    grain: f32,
    vignette: f32,
    speck: f32,
    gradient: f32,
    ambient: f32,
}

pub struct Tube {
    pub w: i32,
    pub h: i32,
    stored: Vec<f32>,
    fresh: Vec<f32>,
    under: Vec<f32>,

    /// `fresh` and `under` hold their values *at* these scales, and a decay is
    /// applied to the scale instead of to every element.  An exponential decay
    /// of a whole population is one multiply, and sweeping a megapixel array
    /// ~16 times per frame - which sub-stepped drawing would otherwise need -
    /// is what makes this emulator crawl.  Deposits divide by the scale so the
    /// "keep the largest value" rule still holds across decay steps.
    fresh_scale: f32,
    under_scale: f32,

    /// Target dots per 4010 dot (1 on the 4010, 4 on the 4014).  Also the unit
    /// the dash patterns are measured in.
    pub spot: i32,
    /// Gaussian radius of the write beam, in target dots.
    pub sigma: f32,
    pub phosphor: Phosphor,
    /// Stroke font used for alpha text.
    pub font: &'static font::Font,

    /// Time constants of the transient populations, in seconds.
    pub fresh_tau: f32,
    pub under_tau: f32,

    /// The distance the write beam has swept, in target dots.  It is a length,
    /// so divide by `spot` to put it in the 4010's own dots and compare a 4014
    /// stream against the 4010's face.  This is what says how fast a stream is
    /// really drawn, which the byte rate alone does not: one byte of plot file
    /// can command a stroke of any length, so the beam's speed is not the line
    /// speed and has to be measured.
    beam_dots: f64,

    /// Target dots the image is shown displaced *down* the face by, for the
    /// slide a scroll animates.  Zero is where the image really is.
    pub render_offset: i32,

    /// The image the page replaced, still glowing, and how much of it is left.
    /// A raster terminal leaves the previous screenful lit underneath the new
    /// one; a storage tube does not, so this is empty unless `ghost_tau` is set.
    ghost: Vec<f32>,
    ghost_scale: f32,
    /// Seconds a captured page keeps glowing.  Zero turns the ghost off, which
    /// is what the tube does on its own: charge does not fade.
    pub ghost_tau: f32,
    /// Cells whose old light has been kept for the ghost: a counter, so a
    /// headless run can be shown to have taken one.
    pub ghosted_cells: usize,

    /// Scratch for composing a frame (kept to avoid per frame allocation).
    light_buf: Vec<f32>,
    fresh_buf: Vec<f32>,
    bloom_light: Vec<f32>,
    bloom_fresh: Vec<f32>,
    /// The vertical half of a box blur, held the other way round (see `box_blur`).
    blur_cols: Vec<f32>,
    blur_small: Vec<f32>,
    blur_tmp: Vec<f32>,
    blur_acc: Vec<f32>,

    /// `ambient_at()` for every output pixel, plus the geometry and field
    /// parameters it was built for.  Evaluating the flood field costs a dozen
    /// hashes per pixel, which made it the most expensive part of a frame; it
    /// only changes when the output geometry or the field constants do.
    field: Vec<f32>,
    field_key: Option<FieldKey>,
}

impl Tube {
    pub fn new(w: i32, h: i32) -> Self {
        let n = (w * h) as usize;
        let unit = (w / 1024).max(1);
        Tube {
            w,
            h,
            stored: vec![0.0; n],
            fresh: vec![0.0; n],
            under: vec![0.0; n],
            spot: unit,
            sigma: 0.5 * unit as f32,
            phosphor: Phosphor::default(),
            font: font::embedded(),
            fresh_tau: 0.45,
            under_tau: 0.18,
            beam_dots: 0.0,
            fresh_scale: 1.0,
            under_scale: 1.0,
            render_offset: 0,
            ghost: Vec::new(),
            ghost_scale: 0.0,
            ghost_tau: 0.0,
            ghosted_cells: 0,
            light_buf: Vec::new(),
            fresh_buf: Vec::new(),
            bloom_light: Vec::new(),
            bloom_fresh: Vec::new(),
            blur_cols: Vec::new(),
            blur_small: Vec::new(),
            blur_tmp: Vec::new(),
            blur_acc: Vec::new(),
            field: Vec::new(),
            field_key: None,
        }
    }

    pub fn len(&self) -> usize {
        (self.w * self.h) as usize
    }

    pub fn erase(&mut self) {
        self.stored.iter_mut().for_each(|v| *v = 0.0);
        self.fresh.iter_mut().for_each(|v| *v = 0.0);
        self.under.iter_mut().for_each(|v| *v = 0.0);
        self.fresh_scale = 1.0;
        self.under_scale = 1.0;
        // The ghost is light that has already left the phosphor, and erasing the
        // charge does not switch it off - it fades on its own.  That is why a
        // raster terminal's page still glows for a moment after a clear.
    }

    /// Keep the image that is on the tube now as a fading ghost.
    ///
    /// A raster terminal writes the new screen over the old one and the old
    /// one's light is still on the face, which is what the ghosting in a real
    /// terminal looks like: the previous screenful, one or more lines out of
    /// place, still legible underneath.  A storage tube has no such thing - its
    /// stored charge is permanent and does not fade - so this does nothing
    /// unless `ghost_tau` has been set.
    pub fn capture_ghost(&mut self) {
        if self.ghost_tau <= 0.0 {
            return;
        }
        self.ghost.clear();
        self.ghost.extend_from_slice(&self.stored);
        self.ghost_scale = 1.0;
    }

    /// Keep the charge already in one cell, for the cells a commit is about to
    /// write over.
    ///
    /// The whole-page capture above is the page-at-a-time case; this is the other
    /// half of a raster terminal's behaviour, and the one you see while *typing*:
    /// a character written over one that was there leaves the old one's light
    /// behind for a moment.  The value stored is divided by the current scale,
    /// the same way a beam deposit is, so the cell restarts its fade from here
    /// rather than inheriting however long the last capture has been decaying.
    pub fn ghost_cell(&mut self, x: i32, y: i32, w: i32, h: i32) {
        if self.ghost_tau <= 0.0 {
            return;
        }
        self.ghosted_cells += 1;
        if self.ghost.len() != self.stored.len() {
            self.ghost.clear();
            self.ghost.resize(self.stored.len(), 0.0);
        }
        if self.ghost_scale <= 0.0 {
            self.ghost_scale = 1.0;
        }
        let scale = self.ghost_scale;
        let (tw, th) = (self.w, self.h);
        for yy in y.max(0)..(y + h).min(th) {
            for xx in x.max(0)..(x + w).min(tw) {
                let i = (yy * tw + xx) as usize;
                let keep = self.stored[i] / scale;
                if keep > self.ghost[i] {
                    self.ghost[i] = keep;
                }
            }
        }
    }

    /// Exponential decay of the transient populations.  Costs one multiply:
    /// the populations are stored at `fresh_scale` / `under_scale`, which is
    /// what actually decays.  The arrays are only touched when the scale has
    /// underflowed far enough that the values in them no longer mean anything.
    pub fn decay(&mut self, dt: f32) {
        if dt <= 0.0 {
            return;
        }
        const MIN_SCALE: f32 = 1e-20;
        self.fresh_scale *= (-dt / self.fresh_tau.max(1e-3)).exp();
        self.under_scale *= (-dt / self.under_tau.max(1e-3)).exp();
        // once the scale has underflowed the values in the array no longer
        // describe anything (they were written at a much larger scale), so drop
        // them and start again.  This is the only path that touches the array
        // and it runs at most once per ~20 s of decaying.
        if self.fresh_scale < MIN_SCALE {
            self.fresh.iter_mut().for_each(|v| *v = 0.0);
            self.fresh_scale = 1.0;
        }
        if self.under_scale < MIN_SCALE {
            self.under.iter_mut().for_each(|v| *v = 0.0);
            self.under_scale = 1.0;
        }
        if self.ghost_scale > 0.0 {
            self.ghost_scale *= (-dt / self.ghost_tau.max(1e-3)).exp();
            if self.ghost_scale < MIN_SCALE {
                self.ghost.clear();
                self.ghost_scale = 0.0;
            }
        }
    }

    fn idx(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            None
        } else {
            Some((y * self.w + x) as usize)
        }
    }

    fn deposit(&mut self, x: i32, y: i32, energy: f32, st: Style) {
        let Some(i) = self.idx(x, y) else { return };
        match st.write {
            Write::Through => {
                let s = self.under_scale;
                if self.under[i] * s < energy {
                    self.under[i] = energy / s;
                }
            }
            Write::Flash => {
                let s = self.fresh_scale;
                if self.fresh[i] * s < energy {
                    self.fresh[i] = energy / s;
                }
            }
            Write::Settle => {
                if self.stored[i] < energy {
                    self.stored[i] = energy;
                }
            }
            Write::Store => {
                if self.stored[i] < energy {
                    self.stored[i] = energy;
                }
                let s = self.fresh_scale;
                if self.fresh[i] * s < energy {
                    self.fresh[i] = energy / s;
                }
            }
        }
    }

    /// Drop the stored image without touching the transient populations.
    ///
    /// Terminal mode commits a settled screen by erasing the stored image and
    /// re-stroking the current screen with `Write::Settle`.  Merging instead of
    /// replacing would leave permanent ghosts of whatever had moved away, which
    /// is exactly the problem a terminal has to solve and a plot file does not.
    pub fn clear_stored(&mut self) {
        self.stored.iter_mut().for_each(|v| *v = 0.0);
    }

    /// Dash pattern in beam units: one entry means "always on", otherwise
    /// alternating on/off run lengths.
    fn dash_pattern(style: LineStyle) -> &'static [i32] {
        match style {
            LineStyle::Solid => &[1],
            LineStyle::Dotted => &[1, 5],
            LineStyle::DotDash => &[6, 3, 1, 3],
            LineStyle::ShortDash => &[4, 3],
            LineStyle::LongDash => &[9, 4],
        }
    }

    fn dash_on(pattern: &[i32], unit: f32, dist: f32) -> bool {
        if pattern.len() == 1 {
            return true;
        }
        let cycle: f32 = pattern.iter().sum::<i32>() as f32 * unit;
        let mut d = dist.rem_euclid(cycle);
        let mut on = true;
        for p in pattern {
            let l = *p as f32 * unit;
            if d < l {
                return on;
            }
            d -= l;
            on = !on;
        }
        false
    }

    /// Stroke one segment with the beam profile, in continuous target
    /// coordinates.  Coverage comes from the distance to the segment, so the
    /// result is smooth at any angle, at any sub-dot position and for any
    /// beam width.
    pub fn stroke(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, st: Style, energy: f32) {
        // A defocussed beam (ESC h..o) is wider and correspondingly dimmer.
        let (sigma, peak) = if st.defocus {
            (self.sigma * 2.6, energy * 0.42)
        } else {
            (self.sigma, energy)
        };
        let reach = PROFILE_REACH * sigma;
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len2 = dx * dx + dy * dy;
        let len = len2.sqrt();
        self.beam_dots += len as f64;
        let inv2s2 = 1.0 / (2.0 * sigma * sigma);
        let pattern = Self::dash_pattern(st.line);
        let unit = self.spot as f32;

        // Walk the scan lines (or columns) the stroke can touch.
        let scan_y = dy.abs() > dx.abs();
        if scan_y {
            let half = if dy.abs() < 1e-6 {
                reach
            } else {
                reach * len / dy.abs()
            };
            let a0 = (y0.min(y1) - reach).floor() as i32;
            let a1 = (y0.max(y1) + reach).ceil() as i32;
            for y in a0..=a1 {
                let ty = if dy.abs() < 1e-6 {
                    0.0
                } else {
                    ((y as f32 - y0) / dy).clamp(0.0, 1.0)
                };
                let xc = x0 + ty * dx;
                let b0 = (xc - half).floor() as i32;
                let b1 = (xc + half).ceil() as i32;
                for x in b0..=b1 {
                    self.cover(
                        x, y, x0, y0, dx, dy, len2, len, inv2s2, peak, st, pattern, unit,
                    );
                }
            }
        } else {
            let half = if dx.abs() < 1e-6 {
                reach
            } else {
                reach * len / dx.abs()
            };
            let a0 = (x0.min(x1) - reach).floor() as i32;
            let a1 = (x0.max(x1) + reach).ceil() as i32;
            for x in a0..=a1 {
                let tx = if dx.abs() < 1e-6 {
                    0.0
                } else {
                    ((x as f32 - x0) / dx).clamp(0.0, 1.0)
                };
                let yc = y0 + tx * dy;
                let b0 = (yc - half).floor() as i32;
                let b1 = (yc + half).ceil() as i32;
                for y in b0..=b1 {
                    self.cover(
                        x, y, x0, y0, dx, dy, len2, len, inv2s2, peak, st, pattern, unit,
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn cover(
        &mut self,
        x: i32,
        y: i32,
        x0: f32,
        y0: f32,
        dx: f32,
        dy: f32,
        len2: f32,
        len: f32,
        inv2s2: f32,
        peak: f32,
        st: Style,
        pattern: &[i32],
        unit: f32,
    ) {
        let (fx, fy) = (x as f32, y as f32);
        let t = if len2 > 0.0 {
            (((fx - x0) * dx + (fy - y0) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let d2 = {
            let px = fx - (x0 + t * dx);
            let py = fy - (y0 + t * dy);
            px * px + py * py
        };
        let cov = (-d2 * inv2s2).exp();
        if cov < PROFILE_CUTOFF {
            return;
        }
        if pattern.len() > 1 && !Self::dash_on(pattern, unit, t * len) {
            return;
        }
        self.deposit(x, y, peak * cov, st);
    }

    /// Integer coordinate convenience wrapper (the protocol's addresses).
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, st: Style, energy: f32) {
        self.stroke(x0 as f32, y0 as f32, x1 as f32, y1 as f32, st, energy);
    }

    pub fn dot(&mut self, x: i32, y: i32, energy: f32, st: Style) {
        self.beam_dots += 1.0;
        let (fx, fy) = (x as f32, y as f32);
        let sigma = if st.defocus {
            self.sigma * 2.6
        } else {
            self.sigma
        };
        let peak = if st.defocus { energy * 0.42 } else { energy };
        let reach = PROFILE_REACH * sigma;
        let inv2s2 = 1.0 / (2.0 * sigma * sigma);
        let x0 = (fx - reach).floor() as i32;
        let x1 = (fx + reach).ceil() as i32;
        let y0 = (fy - reach).floor() as i32;
        let y1 = (fy + reach).ceil() as i32;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d2 = (x as f32 - fx) * (x as f32 - fx) + (y as f32 - fy) * (y as f32 - fy);
                let cov = (-d2 * inv2s2).exp();
                if cov >= PROFILE_CUTOFF {
                    self.deposit(x, y, peak * cov, st);
                }
            }
        }
    }

    /// Draw one alpha character, bottom-left of its cell at (x, y).
    /// The pen strokes of `ch` as placed in a cell, in target coordinates, in
    /// the order the beam would draw them.
    ///
    /// This is the same placement `character` uses, so a partly drawn glyph
    /// lands exactly where a whole one would - which is what lets the writing be
    /// watched stroke by stroke without the geometry drifting between the two
    /// paths.
    pub fn glyph_strokes(
        &self,
        ch: u8,
        x: f32,
        y: f32,
        cell_w: i32,
        cell_h: i32,
        mut f: impl FnMut(f32, f32, f32, f32),
    ) {
        let Some(glyph) = self.font.glyph(ch) else {
            return;
        };
        // Hershey capitals are 21 units tall and sit on y = 0; leave room for
        // the 7 unit descender and keep wide glyphs inside the cell.
        // How much of the cell the glyph fills.  These are what set the gaps
        // between letters: the advance is the cell and stays fixed, so filling
        // more of the cell brings the letters closer together without breaking
        // the fixed pitch.  A monospaced face has one width for every glyph, so
        // 0.95 leaves a consistent 5% gap.
        const FILL_H: f32 = 0.85;
        const FILL_W: f32 = 0.95;
        let scale = ((cell_h as f32 * FILL_H) / 21.0)
            .min(cell_w as f32 * FILL_W / self.font.max_width().max(1.0));
        let baseline = y + cell_h as f32 * 0.24;
        // Condense the widest glyphs to the cap, so the width range shrinks and
        // ordinary letters are not spaced out by the widest one.  The centre of
        // the glyph stays put, so the pitch is still exactly one cell.
        let squeeze = self.font.squeeze(glyph.width);
        let glyph_w = glyph.width * scale * squeeze;
        let x_off = x + (cell_w as f32 - glyph_w).max(0.0) * 0.5;
        for path in &glyph.paths {
            let mut prev: Option<(f32, f32)> = None;
            for &(gx, gy) in path {
                let px = x_off + gx * scale * squeeze;
                let py = baseline + gy * scale;
                if let Some((ox, oy)) = prev {
                    f(ox, oy, px, py);
                }
                prev = Some((px, py));
            }
        }
    }

    pub fn character(
        &mut self,
        ch: u8,
        x: f32,
        y: f32,
        cell_w: i32,
        cell_h: i32,
        st: Style,
        energy: f32,
    ) {
        // draws through the same placement helper, so the two cannot drift
        let mut segs: Vec<(f32, f32, f32, f32)> = Vec::new();
        self.glyph_strokes(ch, x, y, cell_w, cell_h, |x0, y0, x1, y1| {
            segs.push((x0, y0, x1, y1));
        });
        for (x0, y0, x1, y1) in segs {
            self.stroke(x0, y0, x1, y1, st, energy);
        }
    }

    /// Flood gun illumination at a point of the target: a slow large scale
    /// gradient, blotchy clouds, a corner falloff, sparse specks and a little
    /// per pixel grain.  Returns a multiplier around 1.
    pub fn flood_field(&self, x: f32, y: f32) -> f32 {
        let unit = self.spot as f32; // 4014 dots are four times finer
                                     // big smooth clouds, medium blotches, and a faint fine speckle: the
                                     // spectrum fitted to the de-vignetted background of the museum photo
                                     // cells and amplitudes fitted to the trend-free contrast curve of the
                                     // museum photo.  The cells are deliberately *fine*: at 8 and 18 dots
                                     // the field read as visible clouds whose block contrast was three times
                                     // what the photograph shows, which is what "blotchy" was.
                                     // The weights sum to 1 and the result is renormalised to unit standard
                                     // deviation so `mottle` is the field's relative contrast.
        let n = 0.40 * value_noise(x, y, 6.0 * unit, 3) + 0.60 * value_noise(x, y, 2.0 * unit, 4);
        const NOISE_STD: f32 = 0.208; // std of the weighted sum above
        let blotch = (n - 0.5) / NOISE_STD;
        // the photo's field is flat to about 60 % of the radius, then steps
        // down by ~15 % towards the corners
        let nx = x / self.w as f32 * 2.0 - 1.0;
        let ny = y / self.h as f32 * 2.0 - 1.0;
        let r = ((nx * nx + ny * ny) * 0.5).sqrt();
        let shelf = smoothstep(((r - 0.45) / 0.40).clamp(0.0, 1.0));
        let vignette = 1.0 - self.phosphor.vignette * shelf;
        // the measured slow gradient (fitted to the museum photo, flipped into
        // this y-up space): brightest lower left, about +-20 %
        let u = x / self.w as f32 - 0.5;
        let v = y / self.h as f32 - 0.5;
        let trend = self.phosphor.gradient
            * (0.30 * u - 1.20 * v + 0.25 * u * u - 0.90 * v * v + 0.30 * u * v);
        // sparse specks: a few percent of the dots are noticeably brighter
        let grain = (lattice(x as i32, y as i32, 7) - 0.5) * 2.0 * self.phosphor.grain;
        let speck_hash = lattice(x as i32, y as i32, 11);
        let speck = if speck_hash > 0.978 {
            self.phosphor.speck * (speck_hash - 0.978) / 0.022
        } else {
            0.0
        };
        (1.0 + trend + self.phosphor.mottle * blotch + grain + speck).max(0.0) * vignette
    }

    /// Flood gun glow, in emitted-light units, at a point of the target.
    pub fn ambient_at(&self, x: f32, y: f32) -> f32 {
        self.phosphor.ambient * self.flood_field(x, y)
    }

    /// Emitted colour of one spot, as the display would show it (halation not
    /// included, since it depends on the whole neighbourhood).
    pub fn rgb_at(&self, x: i32, y: i32) -> Option<(u8, u8, u8)> {
        let i = self.idx(x, y)?;
        let amb = self.ambient_at(x as f32, y as f32);
        let (r, g, b) = self.phosphor.rgb_flat(
            self.stored[i],
            self.fresh[i] * self.fresh_scale,
            self.under[i] * self.under_scale,
            amb,
        );
        Some((to8(r) as u8, to8(g) as u8, to8(b) as u8))
    }

    /// Convert the target to a 0x00RRGGBB buffer, shrinking by `shrink` and
    /// then repeating each output pixel `zoom` times.
    ///
    /// The shrink averages the emitted light over the covered area (energy
    /// conserving, and therefore smooth), then halation is added, and only
    /// then is the phosphor curve applied.
    pub fn render_into(&mut self, out: &mut Vec<u32>, shrink: i32, zoom: i32) {
        let shrink = shrink.max(1);
        let zoom = zoom.max(1) as usize;
        let ow = (self.w / shrink) as usize;
        let oh = (self.h / shrink) as usize;
        let stride = ow * zoom;
        out.clear();
        out.resize(stride * oh * zoom, 0);

        let (gs, gf, gu) = (
            self.phosphor.stored_gain,
            self.phosphor.fresh_gain,
            self.phosphor.under_gain,
        );
        // fold the decay scales into the gains: a deposit divided by the scale
        // times the scale is the energy it was written with
        let gf = gf * self.fresh_scale;
        let gu = gu * self.under_scale;
        let n = ow * oh;
        self.light_buf.clear();
        self.light_buf.resize(n, 0.0);
        self.fresh_buf.clear();
        self.fresh_buf.resize(n, 0.0);

        // pass 1: area averaged light per output pixel.
        //
        // Split by output row.  The sources are borrowed as shared references
        // separately from the two destinations, which is what lets a row of each
        // destination be handed to a thread without touching the others.
        {
            let Self {
                stored,
                fresh,
                under,
                light_buf,
                fresh_buf,
                w: tw,
                h: th,
                render_offset,
                ghost,
                ghost_scale,
                ..
            } = self;
            let (stored, fresh, under) = (&*stored, &*fresh, &*under);
            let (tw, th) = (*tw as usize, *th as usize);
            // the ghost of the page before this one, if there is one: it fades
            // where it was, like the light a raster terminal left behind
            let ghost: &[f32] = if ghost.len() == tw * th { ghost } else { &[] };
            let ghost_scale = *ghost_scale;
            // a scroll slides: the image is already where it belongs, and the
            // offset shows it displaced down the face until the slide finishes
            let off = *render_offset as i64;
            par_rows2(
                &mut light_buf[..],
                &mut fresh_buf[..],
                ow,
                |oy, lrow, frow| {
                    // the tube's origin is bottom left, the image's is top left
                    let ytop = th as i64 - 1 - (oy * shrink as usize) as i64 + off;
                    for ox in 0..ow {
                        let x0 = (ox * shrink as usize).min(tw - 1);
                        let mut light = 0.0f32;
                        let mut fl = 0.0f32;
                        let mut count = 0.0f32;
                        for dy in 0..shrink as usize {
                            let y = ytop - dy as i64;
                            if y < 0 || y >= th as i64 {
                                continue;
                            }
                            let row = y as usize * tw;
                            for dx in 0..shrink as usize {
                                let x = x0 + dx;
                                if x >= tw {
                                    break;
                                }
                                let i = row + x;
                                light += stored[i] * gs + fresh[i] * gf + under[i] * gu;
                                if !ghost.is_empty() {
                                    light += ghost[i] * gs * ghost_scale;
                                }
                                fl += fresh[i] * gf;
                                count += 1.0;
                            }
                        }
                        lrow[ox] = light / count.max(1.0);
                        frow[ox] = fl / count.max(1.0);
                    }
                },
            );
        }

        // pass 2: halation - light spreading through phosphor and glass
        // ... and the flood gun's illumination, which is static for a given
        // output geometry: build it once instead of a dozen hashes per pixel
        // per frame.
        let key = FieldKey {
            w: self.w,
            h: self.h,
            shrink,
            mottle: self.phosphor.mottle,
            grain: self.phosphor.grain,
            vignette: self.phosphor.vignette,
            speck: self.phosphor.speck,
            gradient: self.phosphor.gradient,
            ambient: self.phosphor.ambient,
        };
        if self.field_key != Some(key) {
            self.field.clear();
            self.field.resize(ow * oh, 0.0);
            for oy in 0..oh {
                let yc = self.h as f32 - (oy as f32 + 0.5) * shrink as f32;
                for ox in 0..ow {
                    let xc = (ox as f32 + 0.5) * shrink as f32;
                    self.field[oy * ow + ox] = self.ambient_at(xc, yc);
                }
            }
            self.field_key = Some(key);
        }

        let ph = &self.phosphor;
        let scales = [
            (ph.bloom, ph.bloom_radius),
            (ph.bloom_mid, ph.bloom_mid_radius),
            (ph.bloom_wide, ph.bloom_wide_radius),
            (ph.bloom_far, ph.bloom_far_radius),
            (ph.bloom_farther, ph.bloom_farther_radius),
        ];
        let blooming = scales.iter().any(|(w, _)| *w > 0.0);
        if blooming {
            // The radii are quoted in 4010 dots, i.e. as a physical distance on
            // the face, and one output pixel is `spot / shrink` of those.  So
            // the halo comes out the same physical size on a 4010 target (a
            // 1024 dot face) and on a 4014 one (the same face addressed with 4x
            // the dots), instead of shrinking with the target resolution.
            let px_per_4010_dot = self.spot as f32 / shrink as f32;
            let cells: Vec<(usize, f32)> = scales
                .iter()
                .filter(|(w, _)| *w > 0.0)
                .map(|(w, r)| ((r * px_per_4010_dot).round().max(1.0) as usize, *w))
                .collect();
            bloom_sum(
                &self.light_buf,
                &mut self.bloom_light,
                &mut self.blur_small,
                &mut self.blur_tmp,
                &mut self.blur_acc,
                &mut self.blur_cols,
                ow,
                oh,
                &cells,
            );
            bloom_sum(
                &self.fresh_buf,
                &mut self.bloom_fresh,
                &mut self.blur_small,
                &mut self.blur_tmp,
                &mut self.blur_acc,
                &mut self.blur_cols,
                ow,
                oh,
                &cells,
            );
        }

        // pass 3: colour.  Split by output row, which is one buffer again.
        let Self {
            field,
            light_buf,
            fresh_buf,
            bloom_light,
            bloom_fresh,
            phosphor: ph,
            ..
        } = self;
        let (field, light_buf, fresh_buf, bloom_light, bloom_fresh) = (
            &*field,
            &*light_buf,
            &*fresh_buf,
            &*bloom_light,
            &*bloom_fresh,
        );
        let ph: &Phosphor = &*ph;
        par_rows(&mut out[..], stride * zoom, |oy, row| {
            // the scan's own banding, measured in unzoomed display rows
            let scan = ph.scan_at(oy / zoom);
            for ox in 0..ow {
                let i = oy * ow + ox;
                let amb = field[i];
                let mut light = light_buf[i];
                let mut fl = fresh_buf[i];
                if blooming {
                    // bloom_sum() has already applied the per-scale weights, so
                    // these are the halo itself, not a second gain on it
                    light += bloom_light[i];
                    fl += bloom_fresh[i];
                }
                // light a raster terminal never emits flatly: 1.0 when there is
                // no scan structure at all
                light *= scan;
                fl *= scan;
                let (r, g, b) = ph.rgb_from_light(light, fl, amb);
                let cam = ph;
                let (r, g, b) = if cam.camera {
                    let p = cam.cam_pivot;
                    let curve = |v: f32| {
                        if v <= p {
                            p * (v.max(0.0) / p).powf(cam.cam_gamma)
                        } else {
                            p + (1.0 - p) * (1.0 - (-cam.cam_knee * (v - p) / (1.0 - p)).exp())
                        }
                    };
                    let (gc, bc) = (curve(g), curve(b));
                    (
                        (curve(r) + cam.cam_leak * gc).min(1.0),
                        gc,
                        (bc + cam.cam_leak_b * gc).min(1.0),
                    )
                } else {
                    (r, g, b)
                };
                let px = pack(r, g, b);
                let xbase = ox * zoom;
                for zy in 0..zoom {
                    for zx in 0..zoom {
                        row[zy * stride + xbase + zx] = px;
                    }
                }
            }
        });

        // a phone camera softens and grains the picture
        if self.phosphor.camera && (self.phosphor.cam_blur > 0.0 || self.phosphor.cam_noise > 0.0) {
            camera_finish(
                out,
                ow * zoom,
                oh * zoom,
                self.phosphor.cam_blur,
                self.phosphor.cam_noise,
            );
        }
    }

    /// Are the transient populations still emitting?  When this is false and
    /// no new input has arrived, the target is not changing, and a window can
    /// skip recomposing the frame entirely.
    pub fn transients_alive(&self) -> bool {
        self.fresh_scale > 1e-3 || self.under_scale > 1e-3
    }

    /// Mean emitted light of the whole target, plus the fraction of the target
    /// that carries stored charge - used to verify fade behaviour.
    /// Target dots the beam has swept since this tube was made.
    pub fn beam_dots(&self) -> f64 {
        self.beam_dots
    }

    pub fn stats(&self) -> (f32, f32) {
        let mut sum = 0.0f64;
        let mut written = 0u64;
        for i in 0..self.len() {
            sum += (self.stored[i] + self.fresh[i] * self.fresh_scale) as f64;
            if self.stored[i] > 0.05 {
                written += 1;
            }
        }
        (
            sum as f32 / self.len() as f32,
            written as f32 / self.len() as f32,
        )
    }
}

/// Halation: `out = sum_i weight_i * blur(src, radius_i)`.
///
/// Every scale is very much wider than it is detailed - the narrowest kernel is
/// six output pixels across and the widest covers the whole face - so the whole
/// sum is evaluated on one half resolution grid: a single box down-sample, each
/// scale as a sliding window box blur there, the weighted sum accumulated in
/// place, and a single bilinear up-sample at the end.  Doing it at full
/// resolution needed five separate down/up-sample pairs per channel, which cost
/// 32 ms of a 57 ms frame; this costs about a quarter of that, and because
/// every term is a low pass the two are visually the same.
/// Most threads a single pass will use.
///
/// Deliberately not "all of them": a pass spawns its threads, and on Windows a
/// spawn costs tens of microseconds, so a frame's worth of passes spread over
/// twenty-four cores costs more in thread creation than the extra cores return.
/// Eight is where that trade stops paying here.
const MAX_RENDER_THREADS: usize = 8;

/// Least rows a thread is given.
///
/// Spawning costs more than a couple of rows cost to process, and `--threads`
/// can ask for anything: 700 threads on a 1440-row pass would have asked for 480
/// chunks - 480 threads a frame - so the count is capped by the work rather than
/// taken at face value.
const MIN_ROWS_PER_CHUNK: usize = 16;

/// The threads a pass of `rows` rows may actually use.
fn effective_threads(rows: usize, threads: usize) -> usize {
    threads.min((rows / MIN_ROWS_PER_CHUNK).max(1))
}

/// Threads a pass may use, or 0 for "ask the machine".  A process-wide knob
/// because the passes are free functions: `--threads N` sets it, and a machine
/// where the render should not take the cores for itself can dial it down.
static RENDER_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Set the render's thread budget: 0 restores the automatic choice.
pub fn set_render_threads(threads: usize) {
    RENDER_THREADS.store(threads, std::sync::atomic::Ordering::Relaxed);
}

fn auto_threads() -> usize {
    let asked = RENDER_THREADS.load(std::sync::atomic::Ordering::Relaxed);
    if asked > 0 {
        return asked;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(MAX_RENDER_THREADS)
}

/// Split a pass over the rows of a buffer between the machine's cores.
///
/// Every pass in the render is either a per-pixel map or one direction of a
/// separable blur, so the work divides exactly: a row of the destination is
/// written only from data its own row of the source carries, no sum is
/// reordered, and the result is bit for bit what the serial code produced.  That
/// matters more than the speed - the model is tuned against measured hardware
/// behaviour, and a version that rounded differently would quietly invalidate
/// that.  It is also why the split is by *rows* of the destination, which is
/// what lets `chunks_mut` hand each thread its own rows with no `unsafe`.
fn par_rows_with<T, F>(dst: &mut [T], w: usize, threads: usize, f: F)
where
    T: Send,
    F: Fn(usize, &mut [T]) + Sync,
{
    if w == 0 || dst.is_empty() {
        return;
    }
    let rows = dst.len() / w;
    let threads = effective_threads(rows, threads);
    if threads <= 1 {
        for (y, row) in dst.chunks_mut(w).enumerate() {
            f(y, row);
        }
        return;
    }
    let per = rows.div_ceil(threads);
    std::thread::scope(|scope| {
        for (block, chunk) in dst.chunks_mut(per * w).enumerate() {
            let f = &f;
            scope.spawn(move || {
                for (k, row) in chunk.chunks_mut(w).enumerate() {
                    f(block * per + k, row);
                }
            });
        }
    });
}

fn par_rows<T, F>(dst: &mut [T], w: usize, f: F)
where
    T: Send,
    F: Fn(usize, &mut [T]) + Sync,
{
    par_rows_with(dst, w, auto_threads(), f);
}

/// The same split for a pass that fills two buffers at once, which pass 1 of the
/// render does: it reads one set of populations and produces both the light and
/// the fresh light.
fn par_rows2_with<T, U, F>(a: &mut [T], b: &mut [U], w: usize, threads: usize, f: F)
where
    T: Send,
    U: Send,
    F: Fn(usize, &mut [T], &mut [U]) + Sync,
{
    if w == 0 || a.is_empty() || b.is_empty() {
        return;
    }
    let rows = (a.len() / w).min(b.len() / w);
    let threads = effective_threads(rows, threads);
    if threads <= 1 {
        for ((y, ra), rb) in a.chunks_mut(w).enumerate().zip(b.chunks_mut(w)) {
            f(y, ra, rb);
        }
        return;
    }
    let per = rows.div_ceil(threads);
    std::thread::scope(|scope| {
        for ((block, ca), cb) in a.chunks_mut(per * w).enumerate().zip(b.chunks_mut(per * w)) {
            let f = &f;
            scope.spawn(move || {
                for ((k, ra), rb) in ca.chunks_mut(w).enumerate().zip(cb.chunks_mut(w)) {
                    f(block * per + k, ra, rb);
                }
            });
        }
    });
}

fn par_rows2<T, U, F>(a: &mut [T], b: &mut [U], w: usize, f: F)
where
    T: Send,
    U: Send,
    F: Fn(usize, &mut [T], &mut [U]) + Sync,
{
    par_rows2_with(a, b, w, auto_threads(), f);
}

#[allow(clippy::too_many_arguments)]
fn bloom_sum(
    src: &[f32],
    out: &mut Vec<f32>,
    small: &mut Vec<f32>,
    tmp: &mut Vec<f32>,
    acc: &mut Vec<f32>,
    columns: &mut Vec<f32>,
    w: usize,
    h: usize,
    scales: &[(usize, f32)],
) {
    let dw = w.div_ceil(2);
    let dh = h.div_ceil(2);
    // box down-sample by two; the sum of a scale is preserved exactly, which is
    // what a low pass over halation needs
    small.clear();
    small.resize(dw * dh, 0.0);
    par_rows(&mut small[..], dw, |y, row| {
        for x in 0..dw {
            let mut s = 0.0f32;
            let mut n = 0.0f32;
            for yy in 0..2 {
                let sy = y * 2 + yy;
                if sy >= h {
                    break;
                }
                for xx in 0..2 {
                    let sx = x * 2 + xx;
                    if sx >= w {
                        break;
                    }
                    s += src[sy * w + sx];
                    n += 1.0;
                }
            }
            row[x] = s / n.max(1.0);
        }
    });
    acc.clear();
    acc.resize(dw * dh, 0.0);
    for (bs, weight) in scales {
        // The old full resolution kernel - a box of `bs` plus three [1,2,1]
        // passes - has a standard deviation of about 1.26 * bs output pixels.  A
        // plain box of width W has sigma W/sqrt(12), so matching the spread
        // needs W = 4.36 * bs output pixels, i.e. a radius of 1.09 * bs in the
        // half resolution grid.
        let r = ((*bs as f32 * 1.09).round() as usize).max(1);
        box_blur(small, acc, tmp, columns, dw, dh, r, *weight);
    }
    // one bilinear up-sample, with the half resolution grid's cell centres
    // lining up with the full resolution picture
    out.clear();
    out.resize(w * h, 0.0);
    par_rows(&mut out[..], w, |y, row| {
        let fy = ((y as f32 + 0.5) * 0.5 - 0.5).clamp(0.0, dh as f32 - 1.0);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(dh - 1);
        let ty = fy - y0 as f32;
        for x in 0..w {
            let fx = ((x as f32 + 0.5) * 0.5 - 0.5).clamp(0.0, dw as f32 - 1.0);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(dw - 1);
            let tx = fx - x0 as f32;
            let a = acc[y0 * dw + x0];
            let b = acc[y0 * dw + x1];
            let c = acc[y1 * dw + x0];
            let d = acc[y1 * dw + x1];
            let top = a + (b - a) * tx;
            let bot = c + (d - c) * tx;
            row[x] = top + (bot - top) * ty;
        }
    });
}

/// Separable box blur of radius `r`, and `dst[j] += weight * blurred[j]`.
/// O(1) per output pixel via a running sum.
///
/// The window is *truncated* at the edges and divided by how many samples it
/// actually covered, so a radius wider than the image degenerates to the mean -
/// which is what the two widest halation scales are.  Clamping the index
/// instead (repeating the edge value) would make those scales read the border
/// rather than the picture, which dimmed the halo everywhere by about 4 %.
#[allow(clippy::too_many_arguments)]
fn box_blur(
    src: &[f32],
    dst: &mut [f32],
    scratch: &mut Vec<f32>,
    columns: &mut Vec<f32>,
    w: usize,
    h: usize,
    r: usize,
    weight: f32,
) {
    scratch.clear();
    scratch.resize(w * h, 0.0);
    // horizontal
    par_rows(&mut scratch[..], w, |y, out_row| {
        let base = y * w;
        let last = r.min(w - 1);
        let mut sum: f32 = src[base..base + last + 1].iter().sum();
        let mut count = (last + 1) as f32;
        for x in 0..w {
            out_row[x] = sum / count;
            if x >= r {
                sum -= src[base + x - r];
                count -= 1.0;
            }
            if x + r + 1 < w {
                sum += src[base + x + r + 1];
                count += 1.0;
            }
        }
    });
    // vertical, accumulating the weighted result into `dst`.
    //
    // The sum runs *down* a column, so the natural unit of work is a column -
    // but the destination is written a row at a time, and one thread's writes
    // would interleave with the next's.  Copying the column results into a
    // buffer laid out the other way round turns it into two passes over
    // independent rows, which `par_rows` can split with no `unsafe` at all.  It
    // costs an extra buffer and one more read of the data, and it is what takes
    // this half of the blur off one core.
    // no clear: every element is written below, and clearing first would add a
    // full-buffer memset to each of the ten vertical passes a frame
    columns.resize(w * h, 0.0);
    par_rows(&mut columns[..], h, |x, col| {
        let last = r.min(h - 1);
        let mut sum: f32 = (0..=last).map(|i| scratch[i * w + x]).sum();
        let mut count = (last + 1) as f32;
        for y in 0..h {
            col[y] = weight * sum / count;
            if y >= r {
                sum -= scratch[(y - r) * w + x];
                count -= 1.0;
            }
            if y + r + 1 < h {
                sum += scratch[(y + r + 1) * w + x];
                count += 1.0;
            }
        }
    });
    par_rows(&mut dst[..], w, |y, row| {
        for x in 0..w {
            row[x] += columns[x * h + y];
        }
    });
}

/// Soften and grain a finished frame the way a phone camera would.  Two
/// [1,2,1] passes give the ~1 px spread the recording shows: its settled
/// strokes measure 3.0 px across where the sharp museum photo measures 2.0.
fn camera_finish(buf: &mut [u32], w: usize, h: usize, blur: f32, noise: f32) {
    if blur > 0.0 && w > 2 && h > 2 {
        let mix = blur.clamp(0.0, 0.95);
        let original: Vec<u32> = buf.to_vec();
        let mut work = original.clone();
        let rgb = |p: u32| {
            (
                ((p >> 16) & 0xff) as f32,
                ((p >> 8) & 0xff) as f32,
                (p & 0xff) as f32,
            )
        };
        for _ in 0..2 {
            let src = work.clone();
            // horizontal then vertical [1,2,1]
            let mut pass = vec![0.0f32; w * h * 3];
            for y in 0..h {
                for x in 0..w {
                    let a = rgb(src[y * w + x.saturating_sub(1)]);
                    let b = rgb(src[y * w + x]);
                    let c = rgb(src[y * w + (x + 1).min(w - 1)]);
                    let i = (y * w + x) * 3;
                    pass[i] = (a.0 + 2.0 * b.0 + c.0) * 0.25;
                    pass[i + 1] = (a.1 + 2.0 * b.1 + c.1) * 0.25;
                    pass[i + 2] = (a.2 + 2.0 * b.2 + c.2) * 0.25;
                }
            }
            for y in 0..h {
                for x in 0..w {
                    let u = (y.saturating_sub(1) * w + x) * 3;
                    let m = (y * w + x) * 3;
                    let d = ((y + 1).min(h - 1) * w + x) * 3;
                    let r = (pass[u] + 2.0 * pass[m] + pass[d]) * 0.25;
                    let g = (pass[u + 1] + 2.0 * pass[m + 1] + pass[d + 1]) * 0.25;
                    let b = (pass[u + 2] + 2.0 * pass[m + 2] + pass[d + 2]) * 0.25;
                    work[y * w + x] = pack(r / 255.0, g / 255.0, b / 255.0);
                }
            }
        }
        for i in 0..w * h {
            let (sr, sg, sb) = rgb(original[i]);
            let (br, bg, bb) = rgb(work[i]);
            let blend = |a: f32, b: f32| (a * (1.0 - mix) + b * mix).clamp(0.0, 255.0) as u32;
            buf[i] = (blend(sr, br) << 16) | (blend(sg, bg) << 8) | blend(sb, bb);
        }
    }
    if noise > 0.0 {
        for (i, p) in buf.iter_mut().enumerate() {
            let x = (i % w) as i32;
            let y = (i / w) as i32;
            let n = (lattice(x, y, 23) - 0.5) * 2.0 * noise * 255.0;
            let clamp = |v: f32| v.clamp(0.0, 255.0) as u32;
            let r = clamp(((*p >> 16) & 0xff) as f32 + n);
            let g = clamp(((*p >> 8) & 0xff) as f32 + n);
            let b = clamp((*p & 0xff) as f32 + n);
            *p = (r << 16) | (g << 8) | b;
        }
    }
}

fn hash_u32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

/// Deterministic pseudo random value in 0..1 for a lattice point.
fn lattice(ix: i32, iy: i32, seed: u32) -> f32 {
    let h = hash_u32(
        (ix as u32).wrapping_mul(374_761_393)
            ^ (iy as u32).wrapping_mul(668_265_263)
            ^ seed.wrapping_mul(362_437),
    );
    (h >> 8) as f32 / 16_777_216.0
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Smooth value noise in 0..1 with cells of `cell` dots.
fn value_noise(x: f32, y: f32, cell: f32, seed: u32) -> f32 {
    let (fx, fy) = (x / cell, y / cell);
    let (ix, iy) = (fx.floor(), fy.floor());
    let (tx, ty) = (smoothstep(fx - ix), smoothstep(fy - iy));
    let (ix, iy) = (ix as i32, iy as i32);
    let a = lattice(ix, iy, seed);
    let b = lattice(ix + 1, iy, seed);
    let c = lattice(ix, iy + 1, seed);
    let d = lattice(ix + 1, iy + 1, seed);
    let top = a + (b - a) * tx;
    let bot = c + (d - c) * tx;
    top + (bot - top) * ty
}

fn pack(r: f32, g: f32, b: f32) -> u32 {
    (to8(r) << 16) | (to8(g) << 8) | to8(b)
}

fn to8(v: f32) -> u32 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32
}

impl Sink for Tube {
    fn erase(&mut self) {
        Tube::erase(self);
    }

    fn move_to(&mut self, _x: i32, _y: i32) {
        // dark-mode move: the beam is blanked, nothing is stored
    }

    fn vector(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, st: Style, energy: f32) {
        self.line(x0, y0, x1, y1, st, energy);
    }

    fn point(&mut self, x: i32, y: i32, st: Style, energy: f32) {
        self.dot(x, y, energy, st);
    }

    fn character(&mut self, ch: u8, x: i32, y: i32, cell_w: i32, cell_h: i32, st: Style) {
        Tube::character(self, ch, x as f32, y as f32, cell_w, cell_h, st, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Style;

    /// A vector must arrive hot (almost white) and settle into the medium acid
    /// green stored image - this is the whole point of the tube model.
    #[test]
    fn fresh_strokes_cool_to_the_stored_green() {
        let mut t = Tube::new(1024, 780);
        // flat flood field: this test is about the phosphor, not the field
        t.phosphor.mottle = 0.0;
        t.phosphor.grain = 0.0;
        t.line(150, 400, 450, 400, Style::default(), 1.0);
        let hot = t.rgb_at(300, 400).unwrap();
        assert!(hot.1 > 230, "fresh stroke should be bright, got {:?}", hot);
        assert!(
            hot.0 > 150 && hot.2 > 150,
            "fresh stroke should be desaturated towards white, got {:?}",
            hot
        );

        // settle for a couple of time constants
        t.decay(2.0);
        let settled = t.rgb_at(300, 400).unwrap();
        // measured on hardware: the photo's stored strokes are (0,174,84) and
        // the recording's are (4,192,85), both about b/g 0.46
        assert!(
            settled.0 < 90,
            "stored image should be a saturated green, got {:?}",
            settled
        );
        assert!(
            settled.2 < 150,
            "stored image should be green, got {:?}",
            settled
        );
        assert!(
            (150..210).contains(&settled.1),
            "stored green should land on the measured level, got {:?}",
            settled
        );
        // measured on real hardware: the stored green has more blue than red
        assert!(
            settled.2 > settled.0,
            "stored hue should lean teal, got {:?}",
            settled
        );
        assert!(
            (settled.2 as f32 / settled.1 as f32) < 0.55,
            "the stored green is greener than the flood field, got {:?}",
            settled
        );
        assert!(
            settled.1 < hot.1,
            "settled image must be dimmer than the flash"
        );
        // the flash is gone but the image is stored: it stays after any wait
        t.decay(600.0);
        let final_colour = t.rgb_at(300, 400).unwrap();
        assert!(
            final_colour.1 > 150,
            "stored image must persist, got {:?}",
            final_colour
        );
        assert!(
            final_colour.0 <= settled.0,
            "the last of the flash must fade away: {:?} -> {:?}",
            settled,
            final_colour
        );
    }

    #[test]
    fn the_scan_structure_bands_the_light() {
        // a flat, lit face with the flood gun's own texture switched off, so the
        // only thing that can make two rows differ is the scan
        let mut t = Tube::new(64, 64);
        for v in t.stored.iter_mut() {
            *v = 0.5;
        }
        let ph = &mut t.phosphor;
        ph.ambient = 0.0;
        ph.mottle = 0.0;
        ph.grain = 0.0;
        ph.vignette = 0.0;
        ph.gradient = 0.0;
        ph.speck = 0.0;
        let rows = |t: &mut Tube| {
            let (w, h) = (t.w as usize, t.h as usize);
            let mut buf = Vec::new();
            t.render_into(&mut buf, 1, 1);
            (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| ((buf[y * w + x] >> 8) & 0xff) as u32)
                        .sum::<u32>()
                })
                .collect::<Vec<_>>()
        };
        // the middle rows, so the blur's edges do not enter into it
        let flat = rows(&mut t)[16..48].to_vec();
        for w in flat.windows(2) {
            assert!(
                w[0].abs_diff(w[1]) <= 1,
                "a face with no scan structure is flat: {w:?}"
            );
        }
        t.phosphor.scan_depth = 0.6;
        let striped = rows(&mut t)[16..48].to_vec();
        let worst = striped
            .windows(2)
            .map(|w| w[0].abs_diff(w[1]))
            .max()
            .unwrap_or(0);
        assert!(
            worst > 20,
            "the scan must band the light, worst row step was {worst}"
        );
    }

    #[test]
    fn a_ghost_can_be_taken_of_one_cell() {
        let lit = |t: &mut Tube| {
            let mut buf = Vec::new();
            t.render_into(&mut buf, 1, 1);
            buf.iter().filter(|p| ((**p >> 8) & 0xff) > 100).count()
        };
        let mut t = Tube::new(64, 64);
        t.ghost_tau = 1.0;
        for y in 24..40 {
            for x in 24..40 {
                t.stored[(y * 64 + x) as usize] = 0.6;
            }
        }
        t.ghost_cell(24, 24, 16, 16);
        // the charge is gone; only the light it left is still on the face
        t.clear_stored();
        assert!(lit(&mut t) > 0, "the cell's own light is still there");
        t.decay(20.0);
        assert_eq!(lit(&mut t), 0, "and it fades on its own");

        // and with no ghost asked for, nothing is kept at all
        let mut off = Tube::new(64, 64);
        for y in 24..40 {
            for x in 24..40 {
                off.stored[(y * 64 + x) as usize] = 0.6;
            }
        }
        off.ghost_cell(24, 24, 16, 16);
        off.clear_stored();
        assert_eq!(lit(&mut off), 0, "a storage tube keeps nothing");
    }

    #[test]
    fn erase_clears_the_target() {
        let mut t = Tube::new(1024, 780);
        t.phosphor.mottle = 0.0;
        t.phosphor.grain = 0.0;
        t.line(10, 10, 20, 20, Style::default(), 1.0);
        t.decay(5.0);
        assert!(t.rgb_at(15, 15).unwrap().1 > 100);
        t.erase();
        assert_eq!(t.stats().1, 0.0, "nothing may be stored after an erase");
        assert!(
            t.rgb_at(15, 15).unwrap().1 < 90,
            "only the flood gun is left"
        );
    }

    /// Write-through (dark mode) vectors are never stored, so they vanish.
    #[test]
    fn write_through_strokes_do_not_persist() {
        let mut t = Tube::new(1024, 780);
        t.phosphor.mottle = 0.0;
        t.phosphor.grain = 0.0;
        let st = Style {
            write: Write::Through,
            ..Style::default()
        };
        t.vector(0, 0, 100, 100, st, 1.0);
        assert!(t.rgb_at(50, 50).unwrap().1 > 100);
        assert_eq!(t.stats().1, 0.0, "nothing may be stored");
        t.decay(2.0);
        assert!(t.rgb_at(50, 50).unwrap().1 < 90, "write-through fades away");
    }

    /// The tube's origin is bottom left, the image's top left: a stroke high on
    /// the target must come out near the top of the rendered buffer.
    #[test]
    fn output_rows_are_flipped_to_image_orientation() {
        let mut t = Tube::new(1024, 780);
        t.phosphor.mottle = 0.0;
        t.phosphor.grain = 0.0;
        t.line(100, 700, 200, 700, Style::default(), 1.0);
        t.decay(3.0);
        let mut buf = Vec::new();
        t.render_into(&mut buf, 1, 1);
        let lit_row = (0..780)
            .find(|r| (buf[r * 1024 + 150] >> 8) as u8 > 100)
            .expect("the stroke must be rendered");
        // y = 700 is 79 dots below the top of a 780 dot target
        assert!(
            (70..95).contains(&lit_row),
            "stroke at y=700 rendered on row {} instead of ~79",
            lit_row
        );
    }

    #[test]
    fn dashing_leaves_gaps() {
        let mut t = Tube::new(1024, 780);
        t.phosphor.mottle = 0.0;
        t.phosphor.grain = 0.0;
        let st = Style {
            line: LineStyle::Dotted,
            ..Style::default()
        };
        t.line(0, 400, 200, 400, st, 1.0);
        let on = (0..200)
            .filter(|x| t.rgb_at(*x, 400).unwrap().1 > 100)
            .count();
        assert!(
            on > 10 && on < 150,
            "dotted line covered {} of 200 dots",
            on
        );
    }

    /// Anti-aliasing: a diagonal stroke must fade across the edge pixels
    /// instead of switching on and off, and a stroke between two dots must
    /// light both of them.
    #[test]
    fn strokes_are_anti_aliased() {
        let mut t = Tube::new(1024, 780);
        t.phosphor.mottle = 0.0;
        t.phosphor.grain = 0.0;
        t.line(100, 100, 600, 600, Style::default(), 1.0);
        t.decay(3.0); // examine the stored image: a fresh flash is clipped white
                      // the ideal line passes through (500,500); the pixel next to it has to
                      // be partially lit, not black and not fully on
        let on = t.rgb_at(500, 500).unwrap().1;
        let beside = t.rgb_at(500, 501).unwrap().1;
        let further = t.rgb_at(500, 503).unwrap().1;
        assert!(on > 165, "stroke centre should be bright, got {}", on);
        assert!(beside > 25, "edge pixel should be lit, got {}", beside);
        assert!(
            beside < on,
            "edge pixel must be dimmer than the centre: {} {}",
            beside,
            on
        );
        assert!(
            further < beside,
            "profile must keep falling: {} {}",
            further,
            beside
        );

        // sub-dot placement: a horizontal stroke at y = 400.5 lights both rows
        let mut t2 = Tube::new(1024, 780);
        t2.phosphor.mottle = 0.0;
        t2.phosphor.grain = 0.0;
        t2.stroke(100.0, 400.5, 300.0, 400.5, Style::default(), 1.0);
        t2.decay(3.0);
        let above = t2.rgb_at(200, 401).unwrap().1;
        let below = t2.rgb_at(200, 400).unwrap().1;
        assert!(
            above > 100 && below > 100,
            "both rows should be lit: {} {}",
            above,
            below
        );
        assert!(
            (above as i32 - below as i32).abs() <= 6,
            "a centred stroke must be symmetric: {} {}",
            above,
            below
        );
    }

    /// The flood gun must light the target unevenly, the way it does on real
    /// hardware: a slow gradient across the face, blotchy mottling, a corner
    /// falloff and sparse bright specks.  Measured on museum 4010/4052 photos:
    /// background contrast 0.24 at 5 dot blocks, corner falloff about 15 %,
    /// gradient about +-10 % from corner to corner, specks ~2 % of dots.
    #[test]
    fn flood_gun_field_matches_the_hardware() {
        let plain = |f: fn(&mut Tube)| {
            let mut t = Tube::new(1024, 780);
            t.phosphor.mottle = 0.0;
            t.phosphor.grain = 0.0;
            t.phosphor.gradient = 0.0;
            t.phosphor.speck = 0.0;
            f(&mut t);
            t
        };
        // vignette alone
        let t = plain(|_| {});
        let centre = t.flood_field(512.0, 390.0);
        assert!(
            t.flood_field(1016.0, 772.0) < centre * 0.95,
            "far corner should be darker: {}",
            t.flood_field(1016.0, 772.0)
        );
        assert!(
            (centre - 1.0).abs() < 1e-6,
            "centre of the face is unvignetted"
        );

        // gradient alone: brightest lower left
        let t = plain(|t| {
            t.phosphor.gradient = 0.20;
        });
        let mean = |t: &Tube, x0: f32, x1: f32, y0: f32, y1: f32| {
            let mut acc = 0.0;
            let mut n = 0.0;
            for i in 0..12 {
                for j in 0..12 {
                    acc += t.flood_field(
                        x0 + (x1 - x0) * i as f32 / 11.0,
                        y0 + (y1 - y0) * j as f32 / 11.0,
                    );
                    n += 1.0;
                }
            }
            acc / n
        };
        let low_left = mean(&t, 20.0, 240.0, 20.0, 200.0);
        let top_right = mean(&t, 800.0, 1010.0, 600.0, 760.0);
        assert!(
            low_left > top_right * 1.15,
            "field gradient should brighten the lower left: {} vs {}",
            low_left,
            top_right
        );

        // mottling alone: the field varies, but only modestly
        let t = plain(|t| {
            t.phosphor.mottle = 0.26;
        });
        let mut lo = f32::INFINITY;
        let mut hi: f32 = 0.0;
        for i in 0..200 {
            for j in 0..200 {
                let f = t.flood_field(5.0 * i as f32, 3.9 * j as f32);
                lo = lo.min(f);
                hi = hi.max(f);
            }
        }
        assert!(hi - lo > 0.5, "field must be blotchy, got {:?}", (lo, hi));
        assert!(
            lo > 0.3 && hi < 1.7,
            "mottling must stay modest: {:?}",
            (lo, hi)
        );

        // specks: a few percent of dots sit clearly above their neighbour
        let t = plain(|t| {
            t.phosphor.speck = 0.26;
        });
        let mut bright = 0;
        let mut total = 0;
        for i in 0..200 {
            for j in 0..200 {
                total += 1;
                if t.flood_field(2.5 * i as f32, 3.9 * j as f32) > 1.15 {
                    bright += 1;
                }
            }
        }
        let fraction = bright as f32 / total as f32;
        assert!(
            (0.004..0.06).contains(&fraction),
            "specks should be sparse but present, got {:.3}",
            fraction
        );
    }

    /// The display transfer is a lookup table for speed; it has to agree with
    /// the curve it replaces, or the calibration in the README is a lie.
    #[test]
    fn the_transfer_table_matches_the_curve() {
        let p = Phosphor::default();
        for i in 0..=200 {
            let light = i as f32 / 100.0;
            let want = light.powf(1.0 / DISPLAY_GAMMA).min(1.0);
            let got = p.encode(light);
            assert!(
                (got - want).abs() < 0.004,
                "encode({light}) = {got}, the curve gives {want}"
            );
        }
        assert_eq!(p.encode(0.0), 0.0);
        assert_eq!(p.encode(-1.0), 0.0);
        assert_eq!(p.encode(RAMP_MAX * 2.0), 1.0);
    }

    /// Halation is light spreading through the glass, so it is a physical
    /// distance on the face: the halo around a stroke has to come out the same
    /// size in output pixels whether the face is addressed as a 4010 (1024
    /// dots) or as a 4014 (4096 dots, the default device).  The radii used to
    /// be divided by the target's scale factor as well, which made every halo
    /// four times too small on the 4014 and left the wide glow that the source
    /// recording shows to be faked by the camera stage.  The halo is
    /// deliberately amplified here so the test measures its geometry rather
    /// than its level.
    #[test]
    fn halation_is_the_same_size_on_a_4010_and_a_4014() {
        let profile = |w: i32, h: i32| {
            let mut t = Tube::new(w, h);
            t.phosphor.mottle = 0.0;
            t.phosphor.grain = 0.0;
            t.phosphor.speck = 0.0;
            t.phosphor.gradient = 0.0;
            t.phosphor.vignette = 0.0;
            t.phosphor.bloom = 0.5;
            t.phosphor.bloom_mid = 0.5;
            t.phosphor.bloom_wide = 0.5;
            t.phosphor.bloom_far = 0.5;
            t.phosphor.bloom_farther = 0.5;
            // a half plane of stored charge with a sharp vertical edge
            let st = Style::default();
            for y in 0..h {
                for x in 0..w / 2 {
                    t.deposit(x, y, 1.0, st);
                }
            }
            let scale = t.spot;
            let mut buf = Vec::new();
            t.render_into(&mut buf, scale, 1);
            let ow = (w / scale) as usize;
            let row = ((h - 1 - h / 2) / scale) as usize;
            let cx = (w / 2 / scale) as usize;
            (0..=130)
                .step_by(10)
                .map(|d| ((buf[row * ow + cx + d] >> 8) & 0xff) as i32)
                .collect::<Vec<_>>()
        };
        let a = profile(1024, 780);
        let b = profile(4096, 3120);
        for (i, (x, y)) in a.iter().zip(&b).enumerate() {
            let d = i * 10;
            assert!(
                (x - y).abs() <= 6,
                "the halo {d} dots past the edge reads {y} on a 4014 against {x} on a 4010"
            );
        }
    }
}

#[cfg(test)]
mod render_split {
    use super::*;

    /// The render is split between threads by *row*, and the whole point of
    /// splitting it that way is that nothing is reordered: a row of the
    /// destination is filled from its own rows of the source, so the arithmetic
    /// - and therefore the rounding - is identical to the serial version.  The
    /// model is calibrated against measured hardware, so a version that came out
    /// subtly different would quietly invalidate that; this is the test that says
    /// it does not.
    #[test]
    fn splitting_a_pass_does_not_change_a_single_value() {
        const W: usize = 37;
        const ROWS: usize = 100;
        let seed: Vec<f32> = (0..W * ROWS).map(|i| (i % 13) as f32 * 0.25).collect();

        fn work(y: usize, row: &mut [f32]) {
            for (x, v) in row.iter_mut().enumerate() {
                *v = ((y * 31 + x) as f32).sqrt() / (y as f32 + 3.0);
            }
        }
        let mut serial = seed.clone();
        let mut split = seed.clone();
        par_rows_with(&mut serial, W, 1, work);
        par_rows_with(&mut split, W, 8, work);
        assert_eq!(
            serial, split,
            "a split pass must be bit for bit the serial one"
        );

        fn work2(y: usize, a: &mut [f32], b: &mut [f32]) {
            for x in 0..a.len() {
                a[x] = (y * x) as f32 / 7.0;
                b[x] = y as f32 / (x as f32 + 1.0);
            }
        }
        let (mut a1, mut b1) = (seed.clone(), seed.clone());
        let (mut a8, mut b8) = (seed.clone(), seed);
        par_rows2_with(&mut a1, &mut b1, W, 1, work2);
        par_rows2_with(&mut a8, &mut b8, W, 8, work2);
        assert_eq!((a1, b1), (a8, b8), "the two buffer split must match too");
    }
}

#[cfg(test)]
mod split_speed {
    use super::*;
    use std::time::Instant;

    /// The split has to *engage*, not just be correct.  This is the assertion
    /// that would have caught a splitter that quietly ran everything on one
    /// thread: it is deliberately loose (1.4x on a machine with 24 cores) so it
    /// cannot flake on a loaded one.
    #[test]
    fn a_thread_count_is_capped_by_the_work() {
        // 1440 rows cannot use 10000 threads, and must not try: at a minimum of
        // 16 rows a chunk the most it can use is 90.
        assert_eq!(effective_threads(1440, 10000), 90);
        assert_eq!(effective_threads(1440, 700), 90);
        // a sane count is left alone, and too little work still means serial
        assert_eq!(effective_threads(1440, 8), 8);
        assert_eq!(effective_threads(10, 8), 1);
        assert_eq!(effective_threads(0, 8), 1);
    }

    #[test]
    fn the_split_uses_more_than_one_thread() {
        println!(
            "SPLIT available_parallelism = {:?}, auto_threads = {}",
            std::thread::available_parallelism(),
            auto_threads()
        );
        fn work(y: usize, row: &mut [f32]) {
            for (x, v) in row.iter_mut().enumerate() {
                *v = ((y * 7919 + x) as f32).sqrt();
            }
        }
        fn time(buf: &mut [f32], threads: usize) -> f64 {
            let t0 = Instant::now();
            par_rows_with(buf, 2560, threads, work);
            t0.elapsed().as_secs_f64()
        }
        let mut buf = vec![0.0f32; 2560 * 1440];
        time(&mut buf, 1); // warm
        let one = time(&mut buf, 1);
        let eight = time(&mut buf, 8);
        println!(
            "SPLIT 1 thread {:.1} ms, 8 threads {:.1} ms = {:.2}x",
            one * 1e3,
            eight * 1e3,
            one / eight
        );
        assert!(
            eight < one * 0.7,
            "the split must actually run in parallel: {:.1} ms vs {:.1} ms",
            one * 1e3,
            eight * 1e3
        );
    }
}
