#!/usr/bin/env python3
"""Write a Tektronix 4010 stream to stdout: text, then a vector plot.

A 4010 is a terminal on a serial line, so anything that can write bytes can
drive it - a shell command, a plotting package, a script:

    python tools/tekplot.py | tek-tube - --device 4010

The protocol used here is the one `src/protocol.rs` decodes:

* printable ASCII draws characters until a control code changes mode
* `GS` (0x1D) enters graph mode
* a coordinate is four bytes, high-Y, low-Y, high-X, low-X, each five bits, the
  byte's ASCII range (0x20..0x3F, 0x40..0x5F, 0x60..0x7F) telling the terminal
  which field arrived.  The first coordinate after `GS` moves, the rest draw.
* `CR` (0x0D) returns to alpha mode, which is how a plotting library gets back
  to writing its labels
* `FF` (0x0C) erases the whole target, the only way to clear a storage tube
"""

import math
import sys

GS, CR, LF, FF = 0x1D, 0x0D, 0x0A, 0x0C


def coord(out, x, y):
    """Append one 10-bit target coordinate (the 4010 raster is 1024x780)."""
    x = max(0, min(1023, int(x)))
    y = max(0, min(779, int(y)))
    out += bytes((0x20 | (y >> 5), 0x60 | (y & 31), 0x20 | (x >> 5), 0x40 | (x & 31)))


def move(out, x, y):
    """Move the beam with the pen up.  `GS` re-enters graph mode and makes the
    coordinate after it a move instead of a draw, so a new polyline starts with
    a `move` - without it, every polyline is joined to the last one."""
    out.append(GS)
    coord(out, x, y)


def main():
    out = bytearray()
    out += b"TEK-TUBE AS A TERMINAL\r\n"
    out += b"a python program writing 4010 protocol down the pipe\r\n"

    left, right, bottom, top = 120, 950, 120, 600

    out.append(GS)
    # the first coordinate after GS is a move, not a draw
    move(out, left, top)
    coord(out, right, top)
    coord(out, right, bottom)
    coord(out, left, bottom)
    coord(out, left, top)

    # grid
    for i in range(1, 10):
        x = left + (right - left) * i / 10
        move(out, x, bottom)
        coord(out, x, top)
    for i in range(1, 4):
        y = bottom + (top - bottom) * i / 4
        move(out, left, y)
        coord(out, right, y)

    # the curve: one move, then a long polyline, so the beam can be seen running
    move(out, left, bottom + (top - bottom) / 2)
    for i in range(1, 421):
        t = i / 420
        x = left + (right - left) * t
        y = bottom + (top - bottom) * (0.5 + 0.45 * math.sin(t * 6.0 * math.pi))
        coord(out, x, y)

    out.append(CR)
    out.append(LF)
    out += b"y = sin(6*pi*t) sampled at 420 points\r\n"
    sys.stdout.buffer.write(out)


if __name__ == "__main__":
    main()
