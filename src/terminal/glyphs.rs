//! The characters the terminal draws itself instead of asking the font.
//!
//! Everything here would in principle come out of the font, but these are the
//! characters whose whole job is to touch their neighbours: a rule drawn a
//! pixel short of the cell edge leaves a gap in a box a program drew, and a
//! font that draws `─` from x = 1 to x = width - 1 cannot tile. Drawing them
//! from the cell's own rectangle means the pieces always meet, and they stay
//! crisp at any font size.
//!
//! The blocks covered, by their Unicode names:
//!
//! * **Box Drawing** `U+2500..=U+257F` — lines, corners, tees, crosses, the
//!   double-line set, the dashed lines, the rounded corners (`╭╮╯╰`) and the
//!   diagonals (`╱╲╳`).
//! * **Block Elements** `U+2580..=U+259F` — `█▀▄▌▐` and friends, the quadrants
//!   (`▚▞`) and the three shades `░▒▓`.
//! * **Braille Patterns** `U+2800..=U+28FF` — the eight dots of a cell.
//! * **Symbols for Legacy Computing** `U+1FB00..=U+1FB3B` — the block sextants,
//!   which split the cell into two columns and three rows.
//! * **Miscellaneous Technical** `U+23BA..=U+23BD` — the four horizontal scan
//!   lines the DEC special graphics set uses for the `⎺⎻─⎼⎽` character.
//! * **Private Use Area** `U+E0B0..=U+E0B3` — the Powerline separators, the
//!   wedges a shell prompt uses between coloured segments.

/// A rectangle of the screen texture, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) x: i32,
    pub(crate) y: i32,
    pub(crate) width: i32,
    pub(crate) height: i32,
}

impl Rect {
    fn right(self) -> i32 {
        self.x + self.width
    }

    fn bottom(self) -> i32 {
        self.y + self.height
    }

    fn centre(self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }

    /// The rectangle `(fx0, fy0)` to `(fx1, fy1)` of the cell, as fractions of
    /// its width and height, rounded to whole pixels. The far edge is the
    /// cell's own edge when the fraction is `1.0`, so a piece never falls a
    /// pixel short and leaves a seam.
    fn part(self, fx0: f32, fy0: f32, fx1: f32, fy1: f32) -> Self {
        let edge = |origin: i32, extent: i32, fraction: f32| -> i32 {
            if fraction == 1.0 {
                origin + extent
            } else {
                origin + (fraction * extent as f32).round() as i32
            }
        };

        let (x0, x1) = (edge(self.x, self.width, fx0), edge(self.x, self.width, fx1));
        let (y0, y1) = (
            edge(self.y, self.height, fy0),
            edge(self.y, self.height, fy1),
        );

        Self {
            x: x0,
            y: y0,
            width: (x1 - x0).max(1),
            height: (y1 - y0).max(1),
        }
    }
}

/// Draws the character `ch` inside `cell` when the terminal is the one that
/// knows how to draw it, handing every piece to `out` as a rectangle and an
/// ink coverage (`255` for solid). Returns false when the font should draw the
/// character instead.
pub(crate) fn draw(ch: char, cell: Rect, out: &mut impl FnMut(Rect, u8)) -> bool {
    if cell.width <= 0 || cell.height <= 0 {
        return false;
    }

    match ch as u32 {
        0x23BA..=0x23BD => scan_line(ch as u32, cell, out),
        0x2500..=0x257F => box_drawing(ch as u32, cell, out),
        0x2580..=0x259F => block_element(ch as u32, cell, out),
        0x2800..=0x28FF => braille(ch as u32, cell, out),
        0xE0B0..=0xE0B3 => powerline(ch as u32, cell, out),
        0x1FB00..=0x1FB3B => sextant(ch as u32, cell, out),
        _ => return false,
    }

    true
}

/// The ink of the four horizontal scan lines, `U+23BA..=U+23BD`: a rule across
/// the cell at the top, at a quarter, at three quarters, and at the bottom.
/// The DEC special graphics set turns them into `⎺⎻─⎼⎽`, which is how the VT100
/// drew a horizontal line at four different heights.
fn scan_line(ch: u32, cell: Rect, out: &mut impl FnMut(Rect, u8)) {
    let fraction = match ch {
        0x23BA => 0.0,
        0x23BB => 0.25,
        0x23BC => 0.75,
        _ => 1.0,
    };

    let thickness = light(cell);
    let centre = (fraction * cell.height as f32).round() as i32;
    // The last one is flush with the bottom edge rather than its centre.
    let y = cell.y + (centre - thickness / 2).min(cell.height - thickness);

    out(
        Rect {
            x: cell.x,
            y,
            width: cell.width,
            height: thickness,
        },
        255,
    );
}

/// How thick a light line is, in pixels: a twelfth of the cell's narrow side,
/// so it grows with the font. The heavy lines are twice that, which is what
/// the faces of the fonts here draw.
fn light(cell: Rect) -> i32 {
    let minor = cell.width.min(cell.height).max(1);

    ((minor as f32) / 12.0).round().max(1.0) as i32
}

/// How a line in the box-drawing block is drawn.
const NONE: u8 = 0;
const LIGHT: u8 = 1;
const HEAVY: u8 = 2;
const DOUBLE: u8 = 3;

/// The arms of every box-drawing character that is a set of straight lines,
/// indexed by `ch - 0x2500`, as `(left, right, up, down)`.
///
/// The names are the Unicode ones with "BOX DRAWINGS" dropped, generated from
/// the Unicode character database, so `250D` really is "down light and right
/// heavy" and the table is not a guess. The nineteen entries that are not a
/// set of arms — the dashes, the rounded corners and the diagonals — carry
/// `(NONE, NONE, NONE, NONE)` and are drawn by their own functions, which run
/// before this table is read.
#[rustfmt::skip]
const BOX: [(u8, u8, u8, u8); 128] = [
    (LIGHT, LIGHT, NONE, NONE), // 2500 light horizontal
    (HEAVY, HEAVY, NONE, NONE), // 2501 heavy horizontal
    (NONE, NONE, LIGHT, LIGHT), // 2502 light vertical
    (NONE, NONE, HEAVY, HEAVY), // 2503 heavy vertical
    (NONE, NONE, NONE, NONE), // 2504 light triple dash horizontal
    (NONE, NONE, NONE, NONE), // 2505 heavy triple dash horizontal
    (NONE, NONE, NONE, NONE), // 2506 light triple dash vertical
    (NONE, NONE, NONE, NONE), // 2507 heavy triple dash vertical
    (NONE, NONE, NONE, NONE), // 2508 light quadruple dash horizontal
    (NONE, NONE, NONE, NONE), // 2509 heavy quadruple dash horizontal
    (NONE, NONE, NONE, NONE), // 250A light quadruple dash vertical
    (NONE, NONE, NONE, NONE), // 250B heavy quadruple dash vertical
    (NONE, LIGHT, NONE, LIGHT), // 250C light down and right
    (NONE, HEAVY, NONE, LIGHT), // 250D down light and right heavy
    (NONE, LIGHT, NONE, HEAVY), // 250E down heavy and right light
    (NONE, HEAVY, NONE, HEAVY), // 250F heavy down and right
    (LIGHT, NONE, NONE, LIGHT), // 2510 light down and left
    (HEAVY, NONE, NONE, LIGHT), // 2511 down light and left heavy
    (LIGHT, NONE, NONE, HEAVY), // 2512 down heavy and left light
    (HEAVY, NONE, NONE, HEAVY), // 2513 heavy down and left
    (NONE, LIGHT, LIGHT, NONE), // 2514 light up and right
    (NONE, HEAVY, LIGHT, NONE), // 2515 up light and right heavy
    (NONE, LIGHT, HEAVY, NONE), // 2516 up heavy and right light
    (NONE, HEAVY, HEAVY, NONE), // 2517 heavy up and right
    (LIGHT, NONE, LIGHT, NONE), // 2518 light up and left
    (HEAVY, NONE, LIGHT, NONE), // 2519 up light and left heavy
    (LIGHT, NONE, HEAVY, NONE), // 251A up heavy and left light
    (HEAVY, NONE, HEAVY, NONE), // 251B heavy up and left
    (NONE, LIGHT, LIGHT, LIGHT), // 251C light vertical and right
    (NONE, HEAVY, LIGHT, LIGHT), // 251D vertical light and right heavy
    (NONE, LIGHT, HEAVY, LIGHT), // 251E up heavy and right down light
    (NONE, LIGHT, LIGHT, HEAVY), // 251F down heavy and right up light
    (NONE, LIGHT, HEAVY, HEAVY), // 2520 vertical heavy and right light
    (NONE, HEAVY, HEAVY, LIGHT), // 2521 down light and right up heavy
    (NONE, HEAVY, LIGHT, HEAVY), // 2522 up light and right down heavy
    (NONE, HEAVY, HEAVY, HEAVY), // 2523 heavy vertical and right
    (LIGHT, NONE, LIGHT, LIGHT), // 2524 light vertical and left
    (HEAVY, NONE, LIGHT, LIGHT), // 2525 vertical light and left heavy
    (LIGHT, NONE, HEAVY, LIGHT), // 2526 up heavy and left down light
    (LIGHT, NONE, LIGHT, HEAVY), // 2527 down heavy and left up light
    (LIGHT, NONE, HEAVY, HEAVY), // 2528 vertical heavy and left light
    (HEAVY, NONE, HEAVY, LIGHT), // 2529 down light and left up heavy
    (HEAVY, NONE, LIGHT, HEAVY), // 252A up light and left down heavy
    (HEAVY, NONE, HEAVY, HEAVY), // 252B heavy vertical and left
    (LIGHT, LIGHT, NONE, LIGHT), // 252C light down and horizontal
    (HEAVY, LIGHT, NONE, LIGHT), // 252D left heavy and right down light
    (LIGHT, HEAVY, NONE, LIGHT), // 252E right heavy and left down light
    (HEAVY, HEAVY, NONE, LIGHT), // 252F down light and horizontal heavy
    (LIGHT, LIGHT, NONE, HEAVY), // 2530 down heavy and horizontal light
    (HEAVY, LIGHT, NONE, HEAVY), // 2531 right light and left down heavy
    (LIGHT, HEAVY, NONE, HEAVY), // 2532 left light and right down heavy
    (HEAVY, HEAVY, NONE, HEAVY), // 2533 heavy down and horizontal
    (LIGHT, LIGHT, LIGHT, NONE), // 2534 light up and horizontal
    (HEAVY, LIGHT, LIGHT, NONE), // 2535 left heavy and right up light
    (LIGHT, HEAVY, LIGHT, NONE), // 2536 right heavy and left up light
    (HEAVY, HEAVY, LIGHT, NONE), // 2537 up light and horizontal heavy
    (LIGHT, LIGHT, HEAVY, NONE), // 2538 up heavy and horizontal light
    (HEAVY, LIGHT, HEAVY, NONE), // 2539 right light and left up heavy
    (LIGHT, HEAVY, HEAVY, NONE), // 253A left light and right up heavy
    (HEAVY, HEAVY, HEAVY, NONE), // 253B heavy up and horizontal
    (LIGHT, LIGHT, LIGHT, LIGHT), // 253C light vertical and horizontal
    (HEAVY, LIGHT, LIGHT, LIGHT), // 253D left heavy and right vertical light
    (LIGHT, HEAVY, LIGHT, LIGHT), // 253E right heavy and left vertical light
    (HEAVY, HEAVY, LIGHT, LIGHT), // 253F vertical light and horizontal heavy
    (LIGHT, LIGHT, HEAVY, LIGHT), // 2540 up heavy and down horizontal light
    (LIGHT, LIGHT, LIGHT, HEAVY), // 2541 down heavy and up horizontal light
    (LIGHT, LIGHT, HEAVY, HEAVY), // 2542 vertical heavy and horizontal light
    (HEAVY, LIGHT, HEAVY, LIGHT), // 2543 left up heavy and right down light
    (LIGHT, HEAVY, HEAVY, LIGHT), // 2544 right up heavy and left down light
    (HEAVY, LIGHT, LIGHT, HEAVY), // 2545 left down heavy and right up light
    (LIGHT, HEAVY, LIGHT, HEAVY), // 2546 right down heavy and left up light
    (HEAVY, HEAVY, HEAVY, LIGHT), // 2547 down light and up horizontal heavy
    (HEAVY, HEAVY, LIGHT, HEAVY), // 2548 up light and down horizontal heavy
    (HEAVY, LIGHT, HEAVY, HEAVY), // 2549 right light and left vertical heavy
    (LIGHT, HEAVY, HEAVY, HEAVY), // 254A left light and right vertical heavy
    (HEAVY, HEAVY, HEAVY, HEAVY), // 254B heavy vertical and horizontal
    (NONE, NONE, NONE, NONE), // 254C light double dash horizontal
    (NONE, NONE, NONE, NONE), // 254D heavy double dash horizontal
    (NONE, NONE, NONE, NONE), // 254E light double dash vertical
    (NONE, NONE, NONE, NONE), // 254F heavy double dash vertical
    (DOUBLE, DOUBLE, NONE, NONE), // 2550 double horizontal
    (NONE, NONE, DOUBLE, DOUBLE), // 2551 double vertical
    (NONE, DOUBLE, NONE, LIGHT), // 2552 down single and right double
    (NONE, LIGHT, NONE, DOUBLE), // 2553 down double and right single
    (NONE, DOUBLE, NONE, DOUBLE), // 2554 double down and right
    (DOUBLE, NONE, NONE, LIGHT), // 2555 down single and left double
    (LIGHT, NONE, NONE, DOUBLE), // 2556 down double and left single
    (DOUBLE, NONE, NONE, DOUBLE), // 2557 double down and left
    (NONE, DOUBLE, LIGHT, NONE), // 2558 up single and right double
    (NONE, LIGHT, DOUBLE, NONE), // 2559 up double and right single
    (NONE, DOUBLE, DOUBLE, NONE), // 255A double up and right
    (DOUBLE, NONE, LIGHT, NONE), // 255B up single and left double
    (LIGHT, NONE, DOUBLE, NONE), // 255C up double and left single
    (DOUBLE, NONE, DOUBLE, NONE), // 255D double up and left
    (NONE, DOUBLE, LIGHT, LIGHT), // 255E vertical single and right double
    (NONE, LIGHT, DOUBLE, DOUBLE), // 255F vertical double and right single
    (NONE, DOUBLE, DOUBLE, DOUBLE), // 2560 double vertical and right
    (DOUBLE, NONE, LIGHT, LIGHT), // 2561 vertical single and left double
    (LIGHT, NONE, DOUBLE, DOUBLE), // 2562 vertical double and left single
    (DOUBLE, NONE, DOUBLE, DOUBLE), // 2563 double vertical and left
    (DOUBLE, DOUBLE, NONE, LIGHT), // 2564 down single and horizontal double
    (LIGHT, LIGHT, NONE, DOUBLE), // 2565 down double and horizontal single
    (DOUBLE, DOUBLE, NONE, DOUBLE), // 2566 double down and horizontal
    (DOUBLE, DOUBLE, LIGHT, NONE), // 2567 up single and horizontal double
    (LIGHT, LIGHT, DOUBLE, NONE), // 2568 up double and horizontal single
    (DOUBLE, DOUBLE, DOUBLE, NONE), // 2569 double up and horizontal
    (DOUBLE, DOUBLE, LIGHT, LIGHT), // 256A vertical single and horizontal double
    (LIGHT, LIGHT, DOUBLE, DOUBLE), // 256B vertical double and horizontal single
    (DOUBLE, DOUBLE, DOUBLE, DOUBLE), // 256C double vertical and horizontal
    (NONE, NONE, NONE, NONE), // 256D light arc down and right
    (NONE, NONE, NONE, NONE), // 256E light arc down and left
    (NONE, NONE, NONE, NONE), // 256F light arc up and left
    (NONE, NONE, NONE, NONE), // 2570 light arc up and right
    (NONE, NONE, NONE, NONE), // 2571 light diagonal upper right to lower left
    (NONE, NONE, NONE, NONE), // 2572 light diagonal upper left to lower right
    (NONE, NONE, NONE, NONE), // 2573 light diagonal cross
    (LIGHT, NONE, NONE, NONE), // 2574 light left
    (NONE, NONE, LIGHT, NONE), // 2575 light up
    (NONE, LIGHT, NONE, NONE), // 2576 light right
    (NONE, NONE, NONE, LIGHT), // 2577 light down
    (HEAVY, NONE, NONE, NONE), // 2578 heavy left
    (NONE, NONE, HEAVY, NONE), // 2579 heavy up
    (NONE, HEAVY, NONE, NONE), // 257A heavy right
    (NONE, NONE, NONE, HEAVY), // 257B heavy down
    (LIGHT, HEAVY, NONE, NONE), // 257C light left and heavy right
    (NONE, NONE, LIGHT, HEAVY), // 257D light up and heavy down
    (HEAVY, LIGHT, NONE, NONE), // 257E heavy left and light right
    (NONE, NONE, HEAVY, LIGHT), // 257F heavy up and light down
];

fn box_drawing(ch: u32, cell: Rect, out: &mut impl FnMut(Rect, u8)) {
    let arms = match ch {
        // The dashed lines: a straight line the length of the cell, broken
        // into 2, 3 or 4 dashes.
        0x2504 => return dashes(cell, 3, LIGHT, false, out),
        0x2505 => return dashes(cell, 3, HEAVY, false, out),
        0x2506 => return dashes(cell, 3, LIGHT, true, out),
        0x2507 => return dashes(cell, 3, HEAVY, true, out),
        0x2508 => return dashes(cell, 4, LIGHT, false, out),
        0x2509 => return dashes(cell, 4, HEAVY, false, out),
        0x250A => return dashes(cell, 4, LIGHT, true, out),
        0x250B => return dashes(cell, 4, HEAVY, true, out),
        0x254C => return dashes(cell, 2, LIGHT, false, out),
        0x254D => return dashes(cell, 2, HEAVY, false, out),
        0x254E => return dashes(cell, 2, LIGHT, true, out),
        0x254F => return dashes(cell, 2, HEAVY, true, out),
        // The rounded corners are the square ones with the corner replaced by
        // a quarter of an ellipse, centred on the corner the arms point away
        // from, so it still meets the arms at the middle of each edge.
        0x256D => return arc(cell, (1.0, 1.0), out),
        0x256E => return arc(cell, (0.0, 1.0), out),
        0x256F => return arc(cell, (0.0, 0.0), out),
        0x2570 => return arc(cell, (1.0, 0.0), out),
        0x2571 => return diagonal(cell, true, false, out),
        0x2572 => return diagonal(cell, false, true, out),
        0x2573 => return diagonal(cell, true, true, out),
        _ => BOX[(ch - 0x2500) as usize],
    };

    let thin = light(cell);
    let heavy = (thin * 2).max(2);
    let (cx, cy) = cell.centre();

    // A horizontal arm runs from the cell's centre to one of its vertical
    // edges and a vertical one from the centre to a horizontal edge, so the
    // two arms of a corner overlap there and the arms of a straight line meet
    // in the middle.
    for (weight, start, end, horizontal) in [
        (arms.0, cell.x, cx, true),
        (arms.1, cx, cell.right(), true),
        (arms.2, cell.y, cy, false),
        (arms.3, cy, cell.bottom(), false),
    ] {
        let thickness = match weight {
            LIGHT => thin,
            HEAVY => heavy,
            DOUBLE => thin,
            _ => continue,
        };

        // A double line is the same line drawn twice, one on each side of
        // where a single line would sit.
        let offsets: &[i32] = if weight == DOUBLE {
            &[-thickness, thickness]
        } else {
            &[0]
        };

        for offset in offsets {
            let rect = if horizontal {
                Rect {
                    x: start,
                    y: cy + offset - thickness / 2,
                    width: end - start,
                    height: thickness,
                }
            } else {
                Rect {
                    x: cx + offset - thickness / 2,
                    y: start,
                    width: thickness,
                    height: end - start,
                }
            };

            out(rect, 255);
        }
    }
}

/// A straight line across the cell, made of `count` dashes.
fn dashes(cell: Rect, count: i32, weight: u8, vertical: bool, out: &mut impl FnMut(Rect, u8)) {
    let thickness = match weight {
        HEAVY => (light(cell) * 2).max(2),
        _ => light(cell),
    };
    let extent = if vertical { cell.height } else { cell.width };
    let unit = extent as f32 / (2 * count - 1) as f32;
    let (cx, cy) = cell.centre();

    for index in 0..count {
        let start = (index as f32 * 2.0 * unit).round() as i32;
        let length = (unit.round() as i32).max(1);

        let rect = if vertical {
            Rect {
                x: cx - thickness / 2,
                y: cell.y + start,
                width: thickness,
                height: length,
            }
        } else {
            Rect {
                x: cell.x + start,
                y: cy - thickness / 2,
                width: length,
                height: thickness,
            }
        };

        out(rect, 255);
    }
}

/// A quarter of an ellipse centred on the corner `(fx, fy)` of the cell, with
/// the cell's own half-width and half-height for its radii. Drawn a pixel at a
/// time, because no rectangle can follow it.
fn arc(cell: Rect, corner: (f32, f32), out: &mut impl FnMut(Rect, u8)) {
    let (x, y) = (
        cell.x as f32 + corner.0 * cell.width as f32,
        cell.y as f32 + corner.1 * cell.height as f32,
    );
    let (a, b) = (cell.width as f32 / 2.0, cell.height as f32 / 2.0);
    let tolerance = light(cell) as f32 / 2.0 / a.min(b);

    pixels(
        cell,
        |px, py| {
            let (dx, dy) = ((px - x) / a, (py - y) / b);

            ((dx * dx + dy * dy).sqrt() - 1.0).abs() <= tolerance
        },
        out,
    );
}

/// The diagonals `╱` and `╲`, and `╳` when both are asked for.
fn diagonal(cell: Rect, rising: bool, falling: bool, out: &mut impl FnMut(Rect, u8)) {
    let (w, h) = (cell.width as f32, cell.height as f32);
    let scale = 1.0 / (1.0 / (w * w) + 1.0 / (h * h)).sqrt();
    let half = light(cell) as f32 / 2.0;

    pixels(
        cell,
        |px, py| {
            let (x, y) = (px - cell.x as f32, py - cell.y as f32);
            // `╱` runs from the top right corner to the bottom left one and
            // `╲` from the top left to the bottom right; the distance to each
            // line is the value of its equation divided by its gradient.
            let rising = rising && ((x / w + y / h - 1.0) * scale).abs() <= half;
            let falling = falling && ((x / w - y / h) * scale).abs() <= half;

            rising || falling
        },
        out,
    );
}

/// Emits one pixel at a time for as long as `ink` accepts the pixel's centre.
fn pixels(cell: Rect, ink: impl Fn(f32, f32) -> bool, out: &mut impl FnMut(Rect, u8)) {
    for y in cell.y..cell.bottom() {
        for x in cell.x..cell.right() {
            if ink(x as f32 + 0.5, y as f32 + 0.5) {
                out(
                    Rect {
                        x,
                        y,
                        width: 1,
                        height: 1,
                    },
                    255,
                );
            }
        }
    }
}

/// The block elements: the rectangles `█▀▄▌▐▖▗▘▙▚▛▜▝▞▟`, the eighth and
/// quarter blocks, and the three shades.
fn block_element(ch: u32, cell: Rect, out: &mut impl FnMut(Rect, u8)) {
    // The shades are a rectangle of the whole cell, inked at a quarter, a half
    // and three quarters, rather than a dither pattern: at these sizes the two
    // look the same and this one scales.
    let shade = match ch {
        0x2591 => Some(64),
        0x2592 => Some(128),
        0x2593 => Some(191),
        _ => None,
    };

    if let Some(coverage) = shade {
        out(cell, coverage);

        return;
    }

    let (h, q) = (0.5, 0.25);
    let (e, t, s) = (0.125, 0.375, 0.625);

    // `(fx0, fy0, fx1, fy1)` of one piece, or two for the quadrants that are
    // not a single rectangle.
    let pieces: &[(f32, f32, f32, f32)] = match ch {
        0x2580 => &[(0.0, 0.0, 1.0, h)],                   // upper half
        0x2581 => &[(0.0, 1.0 - e, 1.0, 1.0)],             // lower one eighth
        0x2582 => &[(0.0, 1.0 - q, 1.0, 1.0)],             // lower one quarter
        0x2583 => &[(0.0, 1.0 - t, 1.0, 1.0)],             // lower three eighths
        0x2584 => &[(0.0, h, 1.0, 1.0)],                   // lower half
        0x2585 => &[(0.0, s, 1.0, 1.0)],                   // lower five eighths
        0x2586 => &[(0.0, q, 1.0, 1.0)],                   // lower three quarters
        0x2587 => &[(0.0, e, 1.0, 1.0)],                   // lower seven eighths
        0x2588 => &[(0.0, 0.0, 1.0, 1.0)],                 // full block
        0x2589 => &[(0.0, 0.0, 1.0 - e, 1.0)],             // left seven eighths
        0x258A => &[(0.0, 0.0, 1.0 - q, 1.0)],             // left three quarters
        0x258B => &[(0.0, 0.0, 1.0 - t, 1.0)],             // left five eighths
        0x258C => &[(0.0, 0.0, h, 1.0)],                   // left half
        0x258D => &[(0.0, 0.0, t, 1.0)],                   // left three eighths
        0x258E => &[(0.0, 0.0, q, 1.0)],                   // left one quarter
        0x258F => &[(0.0, 0.0, e, 1.0)],                   // left one eighth
        0x2590 => &[(h, 0.0, 1.0, 1.0)],                   // right half
        0x2594 => &[(0.0, 0.0, 1.0, e)],                   // upper one eighth
        0x2595 => &[(1.0 - e, 0.0, 1.0, 1.0)],             // right one eighth
        0x2596 => &[(0.0, h, h, 1.0)],                     // quadrant lower left
        0x2597 => &[(h, h, 1.0, 1.0)],                     // quadrant lower right
        0x2598 => &[(0.0, 0.0, h, h)],                     // quadrant upper left
        0x259A => &[(0.0, 0.0, h, h), (h, h, 1.0, 1.0)],   // upper left, lower right
        0x259D => &[(h, 0.0, 1.0, h)],                     // quadrant upper right
        0x259E => &[(h, 0.0, 1.0, h), (0.0, h, h, 1.0)],   // upper right, lower left
        0x2599 => &[(0.0, 0.0, h, h), (0.0, h, 1.0, 1.0)], // all but upper right
        0x259B => &[(0.0, 0.0, 1.0, h), (0.0, h, h, 1.0)], // all but lower right
        0x259C => &[(0.0, 0.0, 1.0, h), (h, h, 1.0, 1.0)], // all but lower left
        0x259F => &[(h, 0.0, 1.0, 1.0), (0.0, h, h, 1.0)], // all but upper left
        _ => return,
    };

    for &(x0, y0, x1, y1) in pieces {
        out(cell.part(x0, y0, x1, y1), 255);
    }
}

/// The braille patterns: eight dots on a two by four grid. Dots 1, 2 and 3
/// run down the left column, 4, 5 and 6 down the right one, and 7 and 8 add a
/// fourth row below them.
fn braille(ch: u32, cell: Rect, out: &mut impl FnMut(Rect, u8)) {
    let pattern = ch - 0x2800;
    let radius = (light(cell) / 2).max(1);

    for bit in 0..8 {
        if pattern & (1 << bit) == 0 {
            continue;
        }

        let (col, row) = match bit {
            0..=2 => (0, bit),
            3..=5 => (1, bit - 3),
            6 => (0, 3),
            _ => (1, 3),
        };

        let centre = (
            cell.x + (2 * col + 1) * cell.width / 4,
            cell.y + (2 * row + 1) * cell.height / 8,
        );

        out(
            Rect {
                x: centre.0 - radius,
                y: centre.1 - radius,
                width: radius * 2,
                height: radius * 2,
            },
            255,
        );
    }
}

/// The block sextants of the legacy computing block: the cell cut into two
/// columns and three rows, with the bit for the sextant set.
///
/// Sextant *n* is numbered the way the Unicode chart numbers it: 1 and 2 are
/// the top row, 3 and 4 the middle one, 5 and 6 the bottom, left to right.
fn sextant(ch: u32, cell: Rect, out: &mut impl FnMut(Rect, u8)) {
    let pattern = SEXTANTS[(ch - 0x1FB00) as usize];

    for bit in 0..6 {
        if pattern & (1 << bit) == 0 {
            continue;
        }

        let (col, row) = (bit % 2, bit / 2);

        out(
            cell.part(
                col as f32 / 2.0,
                row as f32 / 3.0,
                (col + 1) as f32 / 2.0,
                (row + 1) as f32 / 3.0,
            ),
            255,
        );
    }
}

/// Which sextants each character of `U+1FB00..=U+1FB3B` turns on, as a bit per
/// sextant, generated from the names in the Unicode character database
/// ("BLOCK SEXTANT-23456" turns on five of them).
#[rustfmt::skip]
const SEXTANTS: [u8; 60] = [
      1, // 1FB00 1
      2, // 1FB01 2
      3, // 1FB02 12
      4, // 1FB03 3
      5, // 1FB04 13
      6, // 1FB05 23
      7, // 1FB06 123
      8, // 1FB07 4
      9, // 1FB08 14
     10, // 1FB09 24
     11, // 1FB0A 124
     12, // 1FB0B 34
     13, // 1FB0C 134
     14, // 1FB0D 234
     15, // 1FB0E 1234
     16, // 1FB0F 5
     17, // 1FB10 15
     18, // 1FB11 25
     19, // 1FB12 125
     20, // 1FB13 35
     21, // 1FB14 235
     22, // 1FB15 1235
     23, // 1FB16 45
     24, // 1FB17 145
     25, // 1FB18 245
     26, // 1FB19 1245
     27, // 1FB1A 345
     28, // 1FB1B 1345
     29, // 1FB1C 2345
     30, // 1FB1D 12345
     32, // 1FB1E 6
     33, // 1FB1F 16
     34, // 1FB20 26
     35, // 1FB21 126
     36, // 1FB22 36
     37, // 1FB23 136
     38, // 1FB24 236
     39, // 1FB25 1236
     40, // 1FB26 46
     41, // 1FB27 146
     42, // 1FB28 1246
     43, // 1FB29 346
     44, // 1FB2A 1346
     45, // 1FB2B 2346
     46, // 1FB2C 12346
     48, // 1FB2D 56
     49, // 1FB2E 156
     50, // 1FB2F 256
     51, // 1FB30 1256
     52, // 1FB31 356
     53, // 1FB32 1356
     54, // 1FB33 2356
     55, // 1FB34 12356
     56, // 1FB35 456
     57, // 1FB36 1456
     58, // 1FB37 2456
     59, // 1FB38 12456
     60, // 1FB39 3456
     61, // 1FB3A 13456
     62, // 1FB3B 23456
];

/// The Powerline separators, which a shell prompt puts between two coloured
/// segments: a wedge as tall as the cell whose point is at the middle of one
/// side. `E0B1` and `E0B3` are the same wedges with their flat edge inset by
/// a line's width, which is how a prompt avoids a visible step where the
/// wedge meets the block behind it.
fn powerline(ch: u32, cell: Rect, out: &mut impl FnMut(Rect, u8)) {
    let (_, cy) = cell.centre();
    let inset = if matches!(ch, 0xE0B1 | 0xE0B3) {
        light(cell)
    } else {
        0
    };

    let points = match ch {
        0xE0B0 | 0xE0B1 => [
            (cell.x + inset, cell.y),
            (cell.right(), cy),
            (cell.x + inset, cell.bottom()),
        ],
        _ => [
            (cell.right() - inset, cell.y),
            (cell.x, cy),
            (cell.right() - inset, cell.bottom()),
        ],
    };

    pixels(cell, |px, py| inside(points, px, py), out);
}

/// Whether a point is inside the triangle `points`, by the sign of the three
/// edge cross products.
fn inside(points: [(i32, i32); 3], x: f32, y: f32) -> bool {
    let side = |a: (i32, i32), b: (i32, i32)| -> f32 {
        (b.0 - a.0) as f32 * (y - a.1 as f32) - (b.1 - a.1) as f32 * (x - a.0 as f32)
    };

    let (a, b, c) = (
        side(points[0], points[1]),
        side(points[1], points[2]),
        side(points[2], points[0]),
    );

    (a >= 0.0 && b >= 0.0 && c >= 0.0) || (a <= 0.0 && b <= 0.0 && c <= 0.0)
}

/// Fills `rect` with `color`, clipped to the buffer.
pub(crate) fn fill_rect(buffer: &mut [u8], w: usize, h: usize, rect: Rect, color: [u8; 3]) {
    fill_alpha_rect(buffer, w, h, rect, color, 255);
}

/// Fills `rect` with `color` at `alpha`, clipped to the buffer: the ink a cell
/// with no background of its own is drawn with, where the alpha is how much of
/// the pixel the ink covers rather than how solid it is. Nothing is blended
/// here — the picture behind the grid is not in this buffer, and the shader
/// puts it back with the alpha.
pub(crate) fn fill_alpha_rect(
    buffer: &mut [u8],
    w: usize,
    h: usize,
    rect: Rect,
    color: [u8; 3],
    alpha: u8,
) {
    for_each_pixel(buffer, w, h, rect, |pixel| {
        pixel[..3].copy_from_slice(&color);
        pixel[3] = alpha;
    });
}

/// Paints `color` over whatever is in `rect` by `coverage`, clipped to the
/// buffer: how a piece with partial ink, like a shade, is drawn over the
/// cell's background.
pub(crate) fn blend_rect(
    buffer: &mut [u8],
    w: usize,
    h: usize,
    rect: Rect,
    bg: [u8; 3],
    color: [u8; 3],
    coverage: u8,
) {
    let blended = blend(bg, color, coverage);

    fill_rect(buffer, w, h, rect, blended);
}

fn for_each_pixel(
    buffer: &mut [u8],
    w: usize,
    h: usize,
    rect: Rect,
    mut paint: impl FnMut(&mut [u8]),
) {
    let left = rect.x.clamp(0, w as i32);
    let right = rect.right().clamp(0, w as i32);
    let top = rect.y.clamp(0, h as i32);
    let bottom = rect.bottom().clamp(0, h as i32);

    for y in top..bottom {
        for x in left..right {
            let pixel = (y as usize * w + x as usize) * 4;

            paint(&mut buffer[pixel..pixel + 4]);
        }
    }
}

/// `fg` over `bg` by `coverage`, in the sRGB bytes the texture holds. Blending
/// there rather than in linear light is what the 8-bit texture does anyway, and
/// what every terminal that draws text this way does.
pub(crate) fn blend(bg: [u8; 3], fg: [u8; 3], coverage: u8) -> [u8; 3] {
    let mix = |bg: u8, fg: u8| -> u8 {
        let blended = bg as i32 + (fg as i32 - bg as i32) * coverage as i32 / 255;

        blended.clamp(0, 255) as u8
    };

    [mix(bg[0], fg[0]), mix(bg[1], fg[1]), mix(bg[2], fg[2])]
}

#[cfg(test)]
mod tests {
    use super::{Rect, draw};

    fn cell(width: i32, height: i32) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// The pieces `ch` is drawn from, as `(rect, coverage)`.
    fn pieces(ch: char, width: i32, height: i32) -> Vec<(Rect, u8)> {
        let mut pieces = Vec::new();
        let mut collect = |rect, coverage| pieces.push((rect, coverage));

        assert!(draw(ch, cell(width, height), &mut collect));

        pieces
    }

    /// Which pixels are inked, as rows of a grid.
    fn ink(ch: char, width: i32, height: i32) -> Vec<Vec<bool>> {
        let mut grid = vec![vec![false; width as usize]; height as usize];

        for (rect, coverage) in pieces(ch, width, height) {
            assert!(coverage > 0, "{ch} covered nothing");

            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    grid[y as usize][x as usize] = true;
                }
            }
        }

        grid
    }

    #[test]
    fn a_character_the_font_draws_is_not_claimed() {
        let mut collect = |_: Rect, _: u8| unreachable!();

        assert!(!draw('a', cell(9, 18), &mut collect));
        assert!(!draw('é', cell(9, 18), &mut collect));
    }

    #[test]
    fn a_full_block_fills_the_whole_cell() {
        assert_eq!(pieces('█', 10, 18), vec![(cell(10, 18), 255)]);
    }

    #[test]
    fn a_half_block_fills_half_the_cell() {
        let ink = ink('▀', 10, 18);
        let filled: usize = ink
            .iter()
            .map(|row| row.iter().filter(|pixel| **pixel).count())
            .sum();

        assert_eq!(filled, 10 * 9);
        assert!(ink[0].iter().all(|pixel| *pixel));
        assert!(ink[17].iter().all(|pixel| !*pixel));
    }

    #[test]
    fn the_quadrants_fill_the_quadrants_they_name() {
        // ▚ is "upper left and lower right".
        let ink = ink('▚', 10, 20);

        assert!(ink[0][0] && ink[19][9]);
        assert!(!ink[0][9] && !ink[19][0]);
    }

    #[test]
    fn a_shade_covers_the_cell_at_a_fraction() {
        let pieces = pieces('░', 10, 18);

        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].0, cell(10, 18));
        assert_eq!(pieces[0].1, 64);
    }

    #[test]
    fn a_light_line_spans_the_full_width_on_the_middle_row() {
        let ink = ink('─', 10, 18);

        assert!(ink[9].iter().all(|pixel| *pixel));
        assert!(!ink[8].iter().any(|pixel| *pixel));
        // One pixel thick at this size, and its two neighbours tile with it:
        // the last column of one cell is inked as well as the first of the next.
        assert!(ink[9][0] && ink[9][9]);
    }

    #[test]
    fn a_heavy_line_is_thicker_than_a_light_one() {
        let rows = |ch| {
            ink(ch, 10, 18)
                .iter()
                .filter(|row| row.iter().any(|pixel| *pixel))
                .count()
        };

        assert!(rows('━') > rows('─'));
    }

    #[test]
    fn a_double_line_is_two_lines() {
        let ink = ink('═', 10, 18);
        let rows: Vec<usize> = (0..18)
            .filter(|y| ink[*y].iter().any(|pixel| *pixel))
            .collect();

        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[1] - rows[0], 2);
    }

    #[test]
    fn the_corner_arms_meet_in_the_middle_of_the_cell() {
        // ┌ is "down and right": one arm runs from the middle of the cell to
        // the right edge, the other from the middle to the bottom edge, and
        // the two quadrants above and left of them stay empty.
        let ink = ink('┌', 10, 18);

        assert!(ink[9][5], "the arms meet at the centre");
        assert!(ink[9][9], "one arm runs right from the centre");
        assert!(!ink[9][0], "nothing runs left of it");
        assert!(ink[17][5], "the other runs down from the centre");
        assert!(!ink[0][5], "nothing runs up from it");
    }

    #[test]
    fn the_rounded_corner_bulges_into_the_cell() {
        // ╭ joins the middle of the right edge to the middle of the bottom
        // edge, curving away from the corner it is the rounding of.
        let ink = ink('╭', 10, 18);

        assert!(ink[9][9], "the right edge is joined");
        assert!(ink[17][4] || ink[17][5], "the bottom edge is joined");
        assert!(!ink[0][0], "the far corner stays empty");
    }

    #[test]
    fn the_diagonals_run_the_way_their_names_say() {
        // ╱ is "upper right to lower left".
        let slash = ink('╱', 10, 10);

        assert!(slash[1][8] && slash[8][1]);
        assert!(!slash[1][1] && !slash[8][8]);

        // ╲ is the other diagonal.
        let backslash = ink('╲', 10, 10);

        assert!(backslash[1][1] && backslash[8][8]);
        assert!(!backslash[1][8] && !backslash[8][1]);
    }

    #[test]
    fn a_dashed_line_leaves_gaps() {
        let ink = ink('┄', 16, 18);
        let line = &ink[9];
        let runs =
            line.windows(2).filter(|pair| !pair[0] && pair[1]).count() + usize::from(line[0]);

        // Three dashes, and the line starts and ends with ink so that two
        // dashed lines in neighbouring cells join up.
        assert_eq!(runs, 3, "{line:?}");
        assert!(line[0] && line[15]);
    }

    #[test]
    fn the_braille_pattern_is_the_dots_of_its_number() {
        // U+2801 is dot 1, the top left one.
        let one = ink('\u{2801}', 10, 16);
        let filled: Vec<(usize, usize)> = (0..16)
            .flat_map(|y| (0..10).map(move |x| (y, x)))
            .filter(|(y, x)| one[*y][*x])
            .collect();

        assert!(!filled.is_empty() && filled.iter().all(|(y, x)| *y < 8 && *x < 5));

        // All eight dots light up for U+28FF, in both columns and four rows.
        let full = ink('\u{28FF}', 10, 16);
        // Four dots down the cell, in runs of consecutive rows.
        let rows: Vec<usize> = (0..16)
            .filter(|y| full[*y].iter().any(|pixel| *pixel))
            .collect();
        let runs = rows
            .windows(2)
            .filter(|pair| pair[1] != pair[0] + 1)
            .count()
            + 1;

        assert_eq!(runs, 4, "{rows:?}");
        assert!(
            full[rows[0]][2] && full[rows[0]][7],
            "both columns are lit on the top row"
        );
    }

    #[test]
    fn a_sextant_fills_a_sixth_of_the_cell() {
        // U+1FB02 is "BLOCK SEXTANT-12", the whole top third.
        let ink = ink('\u{1FB02}', 12, 18);
        let filled: usize = ink
            .iter()
            .map(|row| row.iter().filter(|pixel| **pixel).count())
            .sum();

        assert_eq!(filled, 12 * 6);
        assert!(ink[0].iter().all(|pixel| *pixel));
        assert!(ink[17].iter().all(|pixel| !*pixel));
    }

    #[test]
    fn the_powerline_wedge_is_a_triangle() {
        let ink = ink('\u{E0B0}', 12, 18);
        let width = |y: usize| ink[y].iter().filter(|pixel| **pixel).count();

        assert!(
            (0..18).all(|y| ink[y][0]),
            "the flat edge is the full height of the cell"
        );
        assert_eq!(width(0), 1, "the wedge comes to a point at the corners");
        assert!(
            ink[9][9] && !ink[9][11],
            "the middle row reaches the far edge"
        );
        assert!(width(9) > width(0));
    }
}
