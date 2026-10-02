//! Stroke font used for the alpha (text) mode of the terminal.
//!
//! The Tektronix 4010/4014 alpha characters were not rasterised: the write
//! beam stroked the glyph outlines straight onto the storage target, so text
//! is drawn here with the very same vector machinery as graphics (and it
//! therefore glows and stores identically).
//!
//! Glyph data: Hershey vector fonts (Dr. A. V. Hershey, U.S. National Bureau
//! of Standards), `.jhf` encoding by James Hurt.  See assets/futural.jhf.

use std::sync::LazyLock;

/// One glyph: a set of pen-stroked polylines plus its nominal advance width.
pub struct Glyph {
    pub paths: Vec<Vec<(f32, f32)>>,
    pub width: f32,
}

impl Glyph {
    fn empty() -> Self {
        Glyph {
            paths: Vec::new(),
            width: 0.0,
        }
    }
}

/// Baseline (in raw Hershey units, y increasing downwards) of the font.
/// Capitals and digits sit on this line; lowercase descenders go below it.
const BASELINE: f32 = 9.0;

pub struct Font {
    glyphs: Vec<Glyph>,
    /// The widest a glyph is *allowed* to be: the width the fit is computed
    /// from, and the cap that wide glyphs are squeezed down to.
    ///
    /// A plotter font like Hershey has a huge width range - `i` is 2 units,
    /// `W` is 20, `@` is 28 - and since every glyph shares one scale, the widest
    /// one sets it and everything ordinary is left filling half its cell.  The
    /// gap between letters is `1 - 0.95 * W / W_max`, so it is fixed by the
    /// font's range alone and no page geometry can close it: the only fix is to
    /// narrow the range.  Condensing the widest glyphs to a cap does that, and
    /// it is what a real terminal font looks like anyway.
    fit_width: f32,
}

/// Which of the embedded stroke fonts to draw alpha text with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Face {
    /// Hershey Simplex *Monospaced*: the same letterforms as Simplex with a
    /// fixed advance, which is what a terminal is.  The default.
    Mono,
    /// Hershey Simplex: real alpha text is a light single stroke font.
    /// Against the only *valid* reference for the terminal's own character
    /// generator (the GPLANET label the 4052 program prints in the source
    /// video) Simplex scores IoU 0.371 while Gothic Bold scores 0.233 with
    /// twice the ink, so Simplex is the default.
    Gothic,
    /// Hershey Simplex: lighter, the classic plotter face.
    Simplex,
}

/// The embedded fonts, parsed once.
pub fn face(face: Face) -> &'static Font {
    static GOTHIC: LazyLock<Font> =
        LazyLock::new(|| Font::parse(include_str!("../assets/gothgbt.jhf")));
    static SIMPLEX: LazyLock<Font> =
        LazyLock::new(|| Font::parse(include_str!("../assets/futural.jhf")));
    static MONO: LazyLock<Font> =
        LazyLock::new(|| Font::parse(include_str!("../assets/futuram.jhf")));
    match face {
        Face::Gothic => &GOTHIC,
        Face::Simplex => &SIMPLEX,
        Face::Mono => &MONO,
    }
}

/// The default face.
pub fn embedded() -> &'static Font {
    face(Face::Mono)
}

impl Font {
    /// Parse a `.jhf` database.  Line order maps to ASCII 32..=126.
    pub fn parse(src: &str) -> Self {
        let mut glyphs = Vec::new();
        for line in src.lines() {
            if line.trim().is_empty() {
                continue;
            }
            glyphs.push(parse_line(line));
        }
        // The cap is what every glyph is fitted into, so a plotter font's
        // huge width range cannot space ordinary letters out.
        let fit_width = (max_width(&glyphs) * CONDENSE).max(1.0);
        Font { glyphs, fit_width }
    }

    pub fn glyph(&self, ch: u8) -> Option<&Glyph> {
        if ch < 32 {
            return None;
        }
        self.glyphs.get((ch - 32) as usize)
    }
}

/// The widest glyph in a parsed set.
fn max_width(glyphs: &[Glyph]) -> f32 {
    glyphs.iter().map(|g| g.width).fold(1.0_f32, f32::max)
}

/// How much of the widest glyph to keep.  Everything wider than this is
/// condensed to it, everything narrower is untouched; the fit scale is then
/// computed from the cap, so ordinary letters fill far more of their cell.
const CONDENSE: f32 = 0.70;

impl Font {
    /// The width every glyph is fitted into, in raw Hershey units.  This is the
    /// cap, not the widest glyph, so ordinary letters are not left swimming in
    /// their cell.
    pub fn max_width(&self) -> f32 {
        self.fit_width
    }

    /// How much to condense a glyph horizontally so it fits the cap.  Never
    /// stretches: a narrow `i` stays narrow.
    pub fn squeeze(&self, glyph_width: f32) -> f32 {
        (self.fit_width / glyph_width.max(0.001)).min(1.0)
    }
}

fn parse_line(line: &str) -> Glyph {
    let bytes = line.as_bytes();
    if bytes.len() < 10 {
        return Glyph::empty();
    }
    let nverts: usize = line[5..8].trim().parse().unwrap_or(0);
    let left = bytes[8] as i32 - 'R' as i32;
    let right = bytes[9] as i32 - 'R' as i32;

    let mut glyph = Glyph {
        paths: Vec::new(),
        width: (right - left) as f32,
    };
    let coords = &bytes[10..];
    let mut path: Vec<(f32, f32)> = Vec::new();
    // The left/right hand pair counts as the first of the `nverts` pairs, so
    // the vertices are the remaining `nverts - 1` pairs.
    for i in 0..nverts.saturating_sub(1) {
        let a = *coords.get(i * 2).unwrap_or(&b'R');
        let b = *coords.get(i * 2 + 1).unwrap_or(&b'R');
        if a == b' ' && b == b'R' {
            // pen up
            if path.len() > 1 {
                glyph.paths.push(std::mem::take(&mut path));
            } else {
                path.clear();
            }
            continue;
        }
        let x = (a as i32 - 'R' as i32) as f32 - left as f32;
        // Raw data has y growing downwards; flip it and put the origin on the
        // baseline so all glyphs of a line share one baseline.
        let y = BASELINE - (b as i32 - 'R' as i32) as f32;
        path.push((x, y));
    }
    if path.len() > 1 {
        glyph.paths.push(path);
    }
    glyph
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_printable_ascii() {
        let f = embedded();
        for ch in 32u8..=126 {
            assert!(f.glyph(ch).is_some(), "missing glyph for {}", ch as char);
        }
    }

    #[test]
    fn capitals_share_a_baseline() {
        let f = embedded();
        let mut feet = Vec::new();
        let mut tops = Vec::new();
        for ch in b"AEHIMRTX" {
            let g = f.glyph(*ch).unwrap();
            feet.push(
                g.paths
                    .iter()
                    .flatten()
                    .map(|(_, y)| *y)
                    .fold(f32::INFINITY, f32::min),
            );
            tops.push(
                g.paths
                    .iter()
                    .flatten()
                    .map(|(_, y)| *y)
                    .fold(f32::NEG_INFINITY, f32::max),
            );
        }
        let (fmin, fmax) = (
            feet.iter().cloned().fold(f32::INFINITY, f32::min),
            feet.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        );
        assert!(fmax - fmin < 0.01, "capitals not aligned: {:?}", feet);
        assert!(
            (fmin - 0.0).abs() < 0.01,
            "baseline should be 0, got {}",
            fmin
        );
        // Hershey capitals are 21 units tall
        for t in tops {
            assert!(
                (t - 21.0).abs() < 0.01,
                "cap height should be 21, got {}",
                t
            );
        }
    }

    #[test]
    fn descenders_go_below_the_baseline() {
        let f = embedded();
        for ch in b"gjpqy" {
            let g = f.glyph(*ch).unwrap();
            let bottom = g
                .paths
                .iter()
                .flatten()
                .map(|(_, y)| *y)
                .fold(f32::INFINITY, f32::min);
            assert!(
                bottom < -1.0,
                "{} should descend below the baseline",
                *ch as char
            );
        }
    }
}
