//! Box-drawing (U+2500–U+257F) and block elements (U+2580–U+259F) drawn as shapes that fill
//! the whole cell, so frames and block art join without gaps at any line height (a font glyph
//! would leave a seam between rows).

use egui::{Color32, Painter, Pos2, Rect, Stroke, Vec2};

/// Draws `c` into `cell` if it is a supported box or block character.
pub fn draw(p: &Painter, c: char, cell: Rect, color: Color32, ppp: f32) -> bool {
    let code = c as u32;
    match code {
        0x2580..=0x259F => {
            block(p, code, cell, color);
            true
        }
        0x2500..=0x257F => lines(p, code, cell, color, ppp),
        _ => false,
    }
}

/// Weight of one arm: 0 none, 1 light, 2 heavy, 3 double.
type Arms = [u8; 4]; // up, right, down, left

fn arms(code: u32) -> Option<Arms> {
    const fn a(u: u8, r: u8, d: u8, l: u8) -> Arms {
        [u, r, d, l]
    }
    Some(match code {
        0x2500 => a(0, 1, 0, 1),
        0x2501 => a(0, 2, 0, 2),
        0x2502 => a(1, 0, 1, 0),
        0x2503 => a(2, 0, 2, 0),
        0x250C => a(0, 1, 1, 0),
        0x250D => a(0, 2, 1, 0),
        0x250E => a(0, 1, 2, 0),
        0x250F => a(0, 2, 2, 0),
        0x2510 => a(0, 0, 1, 1),
        0x2511 => a(0, 0, 1, 2),
        0x2512 => a(0, 0, 2, 1),
        0x2513 => a(0, 0, 2, 2),
        0x2514 => a(1, 1, 0, 0),
        0x2515 => a(1, 2, 0, 0),
        0x2516 => a(2, 1, 0, 0),
        0x2517 => a(2, 2, 0, 0),
        0x2518 => a(1, 0, 0, 1),
        0x2519 => a(1, 0, 0, 2),
        0x251A => a(2, 0, 0, 1),
        0x251B => a(2, 0, 0, 2),
        0x251C => a(1, 1, 1, 0),
        0x251D => a(1, 2, 1, 0),
        0x251E => a(2, 1, 1, 0),
        0x251F => a(1, 1, 2, 0),
        0x2520 => a(2, 1, 2, 0),
        0x2521 => a(2, 2, 1, 0),
        0x2522 => a(1, 2, 2, 0),
        0x2523 => a(2, 2, 2, 0),
        0x2524 => a(1, 0, 1, 1),
        0x2525 => a(1, 0, 1, 2),
        0x2526 => a(2, 0, 1, 1),
        0x2527 => a(1, 0, 2, 1),
        0x2528 => a(2, 0, 2, 1),
        0x2529 => a(2, 0, 1, 2),
        0x252A => a(1, 0, 2, 2),
        0x252B => a(2, 0, 2, 2),
        0x252C => a(0, 1, 1, 1),
        0x252D => a(0, 1, 1, 2),
        0x252E => a(0, 2, 1, 1),
        0x252F => a(0, 2, 1, 2),
        0x2530 => a(0, 1, 2, 1),
        0x2531 => a(0, 1, 2, 2),
        0x2532 => a(0, 2, 2, 1),
        0x2533 => a(0, 2, 2, 2),
        0x2534 => a(1, 1, 0, 1),
        0x2535 => a(1, 1, 0, 2),
        0x2536 => a(1, 2, 0, 1),
        0x2537 => a(1, 2, 0, 2),
        0x2538 => a(2, 1, 0, 1),
        0x2539 => a(2, 1, 0, 2),
        0x253A => a(2, 2, 0, 1),
        0x253B => a(2, 2, 0, 2),
        0x253C => a(1, 1, 1, 1),
        0x253D => a(1, 1, 1, 2),
        0x253E => a(1, 2, 1, 1),
        0x253F => a(1, 2, 1, 2),
        0x2540 => a(2, 1, 1, 1),
        0x2541 => a(1, 1, 2, 1),
        0x2542 => a(2, 1, 2, 1),
        0x2543 => a(2, 1, 1, 2),
        0x2544 => a(2, 2, 1, 1),
        0x2545 => a(1, 1, 2, 2),
        0x2546 => a(1, 2, 2, 1),
        0x2547 => a(2, 2, 1, 2),
        0x2548 => a(1, 2, 2, 2),
        0x2549 => a(2, 1, 2, 2),
        0x254A => a(2, 2, 2, 1),
        0x254B => a(2, 2, 2, 2),
        0x2550 => a(0, 3, 0, 3),
        0x2551 => a(3, 0, 3, 0),
        0x2552 => a(0, 3, 1, 0),
        0x2553 => a(0, 1, 3, 0),
        0x2554 => a(0, 3, 3, 0),
        0x2555 => a(0, 0, 1, 3),
        0x2556 => a(0, 0, 3, 1),
        0x2557 => a(0, 0, 3, 3),
        0x2558 => a(1, 3, 0, 0),
        0x2559 => a(3, 1, 0, 0),
        0x255A => a(3, 3, 0, 0),
        0x255B => a(1, 0, 0, 3),
        0x255C => a(3, 0, 0, 1),
        0x255D => a(3, 0, 0, 3),
        0x255E => a(1, 3, 1, 0),
        0x255F => a(3, 1, 3, 0),
        0x2560 => a(3, 3, 3, 0),
        0x2561 => a(1, 0, 1, 3),
        0x2562 => a(3, 0, 3, 1),
        0x2563 => a(3, 0, 3, 3),
        0x2564 => a(0, 3, 1, 3),
        0x2565 => a(0, 1, 3, 1),
        0x2566 => a(0, 3, 3, 3),
        0x2567 => a(1, 3, 0, 3),
        0x2568 => a(3, 1, 0, 1),
        0x2569 => a(3, 3, 0, 3),
        0x256A => a(1, 3, 1, 3),
        0x256B => a(3, 1, 3, 1),
        0x256C => a(3, 3, 3, 3),
        0x2574 => a(0, 0, 0, 1),
        0x2575 => a(1, 0, 0, 0),
        0x2576 => a(0, 1, 0, 0),
        0x2577 => a(0, 0, 1, 0),
        0x2578 => a(0, 0, 0, 2),
        0x2579 => a(2, 0, 0, 0),
        0x257A => a(0, 2, 0, 0),
        0x257B => a(0, 0, 2, 0),
        0x257C => a(0, 2, 0, 1),
        0x257D => a(1, 0, 2, 0),
        0x257E => a(0, 1, 0, 2),
        0x257F => a(2, 0, 1, 0),
        _ => return None,
    })
}

/// Line thickness in points for a weight, at least one physical pixel.
fn thickness(cell: Rect, weight: u8, ppp: f32) -> f32 {
    let px = 1.0 / ppp;
    let light = ((cell.width() / 8.0) * ppp).round().max(1.0) / ppp;
    match weight {
        2 => (light * 2.0).max(2.0 * px),
        _ => light,
    }
}

fn snap(v: f32, ppp: f32) -> f32 {
    (v * ppp).round() / ppp
}

fn lines(p: &Painter, code: u32, cell: Rect, color: Color32, ppp: f32) -> bool {
    let c = Pos2::new(snap(cell.center().x, ppp), snap(cell.center().y, ppp));
    if let Some(arms) = arms(code) {
        let light = thickness(cell, 1, ppp);
        // Arms overlap the centre by half the widest crossing line so joints are solid.
        let over = arms.iter().map(|w| thickness(cell, *w, ppp)).fold(0.0f32, f32::max) / 2.0;
        for (i, w) in arms.iter().enumerate() {
            if *w == 0 {
                continue;
            }
            let t = thickness(cell, *w, ppp);
            let (from, to) = match i {
                0 => (Pos2::new(c.x, cell.top()), Pos2::new(c.x, c.y + over)),
                1 => (Pos2::new(c.x - over, c.y), Pos2::new(cell.right(), c.y)),
                2 => (Pos2::new(c.x, c.y - over), Pos2::new(c.x, cell.bottom())),
                _ => (Pos2::new(cell.left(), c.y), Pos2::new(c.x + over, c.y)),
            };
            if *w == 3 {
                // Double line: two light lines either side of the centre line.
                let gap = (light * 1.5).max(1.5 / ppp);
                let off = if i % 2 == 0 { Vec2::new(gap, 0.0) } else { Vec2::new(0.0, gap) };
                bar(p, from - off, to - off, light, color);
                bar(p, from + off, to + off, light, color);
            } else {
                bar(p, from, to, t, color);
            }
        }
        return true;
    }
    match code {
        // Dashed lines: (dashes, weight, vertical).
        0x2504..=0x250B | 0x254C..=0x254F => {
            let (n, w, vertical) = match code {
                0x2504 => (3, 1, false),
                0x2505 => (3, 2, false),
                0x2506 => (3, 1, true),
                0x2507 => (3, 2, true),
                0x2508 => (4, 1, false),
                0x2509 => (4, 2, false),
                0x250A => (4, 1, true),
                0x250B => (4, 2, true),
                0x254C => (2, 1, false),
                0x254D => (2, 2, false),
                0x254E => (2, 1, true),
                _ => (2, 2, true),
            };
            let t = thickness(cell, w, ppp);
            let len = if vertical { cell.height() } else { cell.width() };
            let seg = len / n as f32;
            for k in 0..n {
                let a = k as f32 * seg + seg * 0.15;
                let b = (k + 1) as f32 * seg - seg * 0.15;
                if vertical {
                    bar(p, Pos2::new(c.x, cell.top() + a), Pos2::new(c.x, cell.top() + b), t, color);
                } else {
                    bar(p, Pos2::new(cell.left() + a, c.y), Pos2::new(cell.left() + b, c.y), t, color);
                }
            }
            true
        }
        // Rounded corners: a quarter arc joining the two arms.
        0x256D..=0x2570 => {
            let t = thickness(cell, 1, ppp);
            let r = (cell.width() / 2.0).min(cell.height() / 2.0);
            let (h_end, v_end, center) = match code {
                0x256D => (cell.right(), cell.bottom(), Pos2::new(c.x + r, c.y + r)), // ╭ down and right
                0x256E => (cell.left(), cell.bottom(), Pos2::new(c.x - r, c.y + r)),  // ╮ down and left
                0x256F => (cell.left(), cell.top(), Pos2::new(c.x - r, c.y - r)),     // ╯ up and left
                _ => (cell.right(), cell.top(), Pos2::new(c.x + r, c.y - r)),         // ╰ up and right
            };
            let sx = (center.x - c.x).signum();
            let sy = (center.y - c.y).signum();
            // Straight parts from where the arc ends to the cell edges.
            bar(p, Pos2::new(center.x, c.y), Pos2::new(h_end, c.y), t, color);
            bar(p, Pos2::new(c.x, center.y), Pos2::new(c.x, v_end), t, color);
            let steps = 10;
            let pts: Vec<Pos2> = (0..=steps)
                .map(|k| {
                    let a = std::f32::consts::FRAC_PI_2 * k as f32 / steps as f32;
                    Pos2::new(center.x - sx * r * a.cos(), center.y - sy * r * a.sin())
                })
                .collect();
            p.add(egui::Shape::line(pts, Stroke::new(t, color)));
            true
        }
        0x2571..=0x2573 => {
            let t = thickness(cell, 1, ppp);
            let stroke = Stroke::new(t, color);
            if code != 0x2572 {
                p.line_segment([cell.left_bottom(), cell.right_top()], stroke);
            }
            if code != 0x2571 {
                p.line_segment([cell.left_top(), cell.right_bottom()], stroke);
            }
            true
        }
        _ => false,
    }
}

/// An axis-aligned bar of thickness `t` centred on the segment.
fn bar(p: &Painter, from: Pos2, to: Pos2, t: f32, color: Color32) {
    let r = if (from.x - to.x).abs() < f32::EPSILON {
        Rect::from_min_max(Pos2::new(from.x - t / 2.0, from.y.min(to.y)), Pos2::new(from.x + t / 2.0, from.y.max(to.y)))
    } else {
        Rect::from_min_max(Pos2::new(from.x.min(to.x), from.y - t / 2.0), Pos2::new(from.x.max(to.x), from.y + t / 2.0))
    };
    p.rect_filled(r, 0.0, color);
}

fn block(p: &Painter, code: u32, cell: Rect, color: Color32) {
    let (x0, y0, w, h) = (cell.left(), cell.top(), cell.width(), cell.height());
    let part = |fx0: f32, fy0: f32, fx1: f32, fy1: f32, col: Color32| {
        p.rect_filled(
            Rect::from_min_max(Pos2::new(x0 + w * fx0, y0 + h * fy0), Pos2::new(x0 + w * fx1, y0 + h * fy1)),
            0.0,
            col,
        );
    };
    let eighth = |n: f32| n / 8.0;
    match code {
        0x2580 => part(0.0, 0.0, 1.0, 0.5, color),
        0x2581..=0x2588 => part(0.0, 1.0 - eighth((code - 0x2580) as f32), 1.0, 1.0, color),
        0x2589..=0x258F => part(0.0, 0.0, eighth((0x2590 - code) as f32), 1.0, color),
        0x2590 => part(0.5, 0.0, 1.0, 1.0, color),
        0x2591 => part(0.0, 0.0, 1.0, 1.0, color.gamma_multiply(0.25)),
        0x2592 => part(0.0, 0.0, 1.0, 1.0, color.gamma_multiply(0.5)),
        0x2593 => part(0.0, 0.0, 1.0, 1.0, color.gamma_multiply(0.75)),
        0x2594 => part(0.0, 0.0, 1.0, eighth(1.0), color),
        0x2595 => part(1.0 - eighth(1.0), 0.0, 1.0, 1.0, color),
        _ => {
            // Quadrants: upper-left, upper-right, lower-left, lower-right.
            let q: [bool; 4] = match code {
                0x2596 => [false, false, true, false],
                0x2597 => [false, false, false, true],
                0x2598 => [true, false, false, false],
                0x2599 => [true, false, true, true],
                0x259A => [true, false, false, true],
                0x259B => [true, true, true, false],
                0x259C => [true, true, false, true],
                0x259D => [false, true, false, false],
                0x259E => [false, true, true, false],
                _ => [false, true, true, true],
            };
            let quads = [(0.0, 0.0), (0.5, 0.0), (0.0, 0.5), (0.5, 0.5)];
            for (on, (fx, fy)) in q.iter().zip(quads) {
                if *on {
                    part(fx, fy, fx + 0.5, fy + 0.5, color);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_character_has_a_drawing() {
        for code in 0x2500..=0x257Fu32 {
            let dashed = matches!(code, 0x2504..=0x250B | 0x254C..=0x254F);
            let special = matches!(code, 0x256D..=0x2573);
            assert!(arms(code).is_some() || dashed || special, "U+{code:04X} not drawn");
        }
    }
}
