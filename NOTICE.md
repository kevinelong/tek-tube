# Third party material

## Hershey vector font (`assets/futural.jhf`)

The alpha (text) mode glyph outlines are Hershey vector fonts.

```
USE RESTRICTION:
	This distribution of the Hershey Fonts may be used by anyone for
	any purpose, commercial or otherwise, providing that:
		1. The following acknowledgements must be distributed with
			the font data:
			- The Hershey Fonts were originally created by Dr.
				A. V. Hershey while working at the U. S.
				National Bureau of Standards.
			- The format of the Font data in this distribution
				was originally created by
					James Hurt
					Cognition, Inc.
					900 Technology Park Drive
					Billerica, MA 01821
					(mit-eddie!ci-dandelion!hurt)
		2. The font data in this distribution may be converted into
			any other format *EXCEPT* the format distributed by
			the U.S. NTIS (which organization holds the rights
			to the distribution and use of the font data in that
			particular format).
```

`futural.jhf` was obtained from
<https://github.com/kamalmostafa/hershey-fonts> (glyph data permissively
licensed; the loader in that repository is GPL and was **not** used - the
`.jhf` parser in `src/font.rs` was written from the format description in the
original distribution notes).

## Reference photographs (`ref/`, `docs/reference-real-4010.jpg`)

Colour and texture calibration uses photographs of **real hardware only**
(no emulator or simulator screenshots):

* `tek4010_screen.jpg` - a Tektronix 4010-1 in the vintageTEK museum
  (<https://vintagetek.org/dvst-graphic-terminals/>) showing the same DISSPLA
  demo that is `samples/isco14.plt`.  Used for: stored stroke colour, flood gun
  level and its blotchy field, corner vignette, halation vs distance.
* `tek4052.jpg` - a Tektronix 4052 (<https://w140.com/tekwiki/wiki/4052>),
  used to confirm the field/hue on the machine the source video uses.
* `tek611.jpg` - a Tektronix 611 storage display (vintageTEK), used as a second
  data point for the background-to-stroke brightness ratio.
* The source video itself (frames extracted from
  <https://www.youtube.com/watch?v=2SbCIP1m6hs>) for the flash colour, the
  trail length and the drawing order.

These photographs remain the property of their sources and are kept here only
as measurement references.

## Sample plot files (`samples/*.plt`)

Cultural/historical Tektronix 4010/4014 plot files collected by Chuck Forsberg
and redistributed by <https://github.com/rricharz/Tek4010> (`pltfiles/`).  That
directory is explicitly excluded from the GPL of that project and is provided
for personal use.  They are test inputs for this emulator only.

`display`-relevant references used while writing the decoder:

* Tektronix 4010/4014 User Manuals and the 4010/4010-1 Maintenance Manual
  (Tektronix, 1971-1975) for the coordinate encoding, escape set, line styles,
  point plot tables and tube behaviour
* `Tektronix 4010` (Wikipedia) for the protocol summary and the tube physics
  (flood gun, writing gun, stored charge, writing speed)
* `rricharz/Tek4010` (GPL) was read as a behavioural reference for how these
  particular files were produced, in particular the byte-range driven field
  skipping and the Enhanced Graphics Module extra byte position.  No code from
  it is used; `src/` here is an independent implementation.
