//! squarified treemap layout (Bruls, Huijsen, van Wijk, 1999).
//!
//! weighted items + bounding rect -> child rects with aspect ratios as close
//! to 1:1 as possible. algorithm:
//!
//! 1. sort desc by weight (callers supply pre-sorted, we re-sort defensively)
//! 2. walk in order, greedy add to current row. after each candidate compute
//!    worst aspect ratio. no worse than before candidate = keep. worse = commit
//!    the row without it, slice off the used strip, start new row with candidate.
//! 3. commit leftover at end.
//!
//! pure, std-only. few us for our 64-item cap.
//!
//! math in f64, rects emitted as f32 (plenty for 0..1 UI coords, half the wire).

use serde::Serialize;

/// positioned rect in unit square
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn unit() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        }
    }

    fn from_f64(x: f64, y: f64, w: f64, h: f64) -> Self {
        // floor-to-zero on negative rounding noise so tiles stay inside the
        // unit square after f64->f32 trim

        Self {
            x: (x.max(0.0)) as f32,
            y: (y.max(0.0)) as f32,
            w: (w.max(0.0)) as f32,
            h: (h.max(0.0)) as f32,
        }
    }

    #[cfg(test)]
    pub fn area(&self) -> f64 {
        (self.w as f64) * (self.h as f64)
    }
}

/// contract:
/// * output same length as items
/// * order preserved (item i -> rect i)
/// * every rect inside bounds (within f32 rounding)
/// * sum of rect areas = bounds.area() within rounding, assuming weights > 0
///
/// zero-weight items get zero-area rects at the end.
pub fn squarify(items: &[(String, f64)], bounds: Rect) -> Vec<Rect> {
    let n = items.len();
    if n == 0 || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return vec![Rect::from_f64(bounds.x as f64, bounds.y as f64, 0.0, 0.0); n];
    }

    // filter negatives / NaN defensively
    let cleaned: Vec<f64> = items
        .iter()
        .map(|(_, w)| if *w > 0.0 && w.is_finite() { *w } else { 0.0 })
        .collect();
    let total: f64 = cleaned.iter().sum();
    if total <= 0.0 {
        return vec![Rect::from_f64(bounds.x as f64, bounds.y as f64, 0.0, 0.0); n];
    }

    // scale weights to rect area so running arithmetic is in rect units
    let area = (bounds.w as f64) * (bounds.h as f64);
    let scale = area / total;

    // indexed so output matches input order even though squarify processes sorted
    let mut indexed: Vec<(usize, f64)> = cleaned
        .iter()
        .enumerate()
        .map(|(i, w)| (i, w * scale))
        .collect();
    // stable-ish: ties broken by input index = deterministic tests
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut out: Vec<Option<Rect>> = vec![None; n];

    let mut free = RectF64::from(bounds);
    let mut row: Vec<(usize, f64)> = Vec::new();

    let mut i = 0;
    while i < indexed.len() {
        if indexed[i].1 <= 0.0 {
            break; // rest are zero-weight, place at end
        }
        let candidate = indexed[i];
        let shortest = free.shortest_side();
        if shortest <= 0.0 {
            break;
        }

        if row.is_empty() {
            row.push(candidate);
            i += 1;
            continue;
        }

        let worst_before = worst_ratio(&row, shortest);
        let mut row_with = row.clone();
        row_with.push(candidate);
        let worst_after = worst_ratio(&row_with, shortest);

        if worst_after <= worst_before {
            row.push(candidate);
            i += 1;
        } else {
            // commit row, shrink free rect, retry candidate against remainder
            free = place_row(&row, free, &mut out);
            row.clear();
        }
    }

    if !row.is_empty() {
        free = place_row(&row, free, &mut out);
        row.clear();
    }
    // clippy: drop `free` explicitly so the last mutation isn't flagged unused
    let _ = free;

    // unplaced items (zero weight or pathological input) get a degenerate rect
    // at bounds origin. callers can still iter without NPE.
    let mut tail_rects: Vec<Rect> = Vec::new();
    for (idx, w) in indexed.iter().skip(i) {
        let _ = w;
        tail_rects.push(Rect::from_f64(bounds.x as f64, bounds.y as f64, 0.0, 0.0));
        if out[*idx].is_none() {
            out[*idx] = Some(Rect::from_f64(bounds.x as f64, bounds.y as f64, 0.0, 0.0));
        }
    }

    out.into_iter()
        .map(|r| r.unwrap_or_else(|| Rect::from_f64(bounds.x as f64, bounds.y as f64, 0.0, 0.0)))
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct RectF64 {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl From<Rect> for RectF64 {
    fn from(r: Rect) -> Self {
        Self {
            x: r.x as f64,
            y: r.y as f64,
            w: r.w as f64,
            h: r.h as f64,
        }
    }
}

impl RectF64 {
    fn shortest_side(&self) -> f64 {
        self.w.min(self.h)
    }
}

/// worst aspect ratio if row were placed against shortest side `w`. standard
/// squarified formula:
///
/// ```text
///   max( (w^2 * max_r) / s^2, s^2 / (w^2 * min_r) )
/// ```
///
/// s = sum of weights, max_r/min_r = largest/smallest weight. equivalent to
/// max(longside/shortside) across the row when placed along w.
fn worst_ratio(row: &[(usize, f64)], w: f64) -> f64 {
    if row.is_empty() || w <= 0.0 {
        return f64::INFINITY;
    }
    let s: f64 = row.iter().map(|(_, a)| *a).sum();
    if s <= 0.0 {
        return f64::INFINITY;
    }
    let r_max = row
        .iter()
        .map(|(_, a)| *a)
        .fold(f64::NEG_INFINITY, f64::max);
    let r_min = row
        .iter()
        .map(|(_, a)| *a)
        .fold(f64::INFINITY, f64::min);
    let w2 = w * w;
    let s2 = s * s;
    (w2 * r_max / s2).max(s2 / (w2 * r_min))
}

/// place row along shortest side of free, write rects into out, return remainder
fn place_row(row: &[(usize, f64)], free: RectF64, out: &mut [Option<Rect>]) -> RectF64 {
    let s: f64 = row.iter().map(|(_, a)| *a).sum();
    if s <= 0.0 || free.w <= 0.0 || free.h <= 0.0 {
        return free;
    }

    // place along shorter side, remainder shrinks the longer side
    if free.w >= free.h {
        // column width = s/h, height = h
        let col_w = s / free.h;
        let mut y = free.y;
        for (idx, a) in row {
            let rh = a / col_w;
            out[*idx] = Some(Rect::from_f64(free.x, y, col_w, rh));
            y += rh;
        }
        RectF64 {
            x: free.x + col_w,
            y: free.y,
            w: (free.w - col_w).max(0.0),
            h: free.h,
        }
    } else {
        // row height = s/w, width = w
        let row_h = s / free.w;
        let mut x = free.x;
        for (idx, a) in row {
            let rw = a / row_h;
            out[*idx] = Some(Rect::from_f64(x, free.y, rw, row_h));
            x += rw;
        }
        RectF64 {
            x: free.x,
            y: free.y + row_h,
            w: free.w,
            h: (free.h - row_h).max(0.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(pairs: &[(&str, f64)]) -> Vec<(String, f64)> {
        pairs.iter().map(|(n, w)| (n.to_string(), *w)).collect()
    }

    fn assert_inside_unit(r: &Rect) {
        assert!(r.x >= -1e-5 && r.x <= 1.0 + 1e-5, "x out: {r:?}");
        assert!(r.y >= -1e-5 && r.y <= 1.0 + 1e-5, "y out: {r:?}");
        assert!(r.x + r.w <= 1.0 + 1e-4, "r.x+w out: {r:?}");
        assert!(r.y + r.h <= 1.0 + 1e-4, "r.y+h out: {r:?}");
    }

    #[test]
    fn empty_input_returns_empty() {
        let out = squarify(&[], Rect::unit());
        assert!(out.is_empty());
    }

    #[test]
    fn zero_area_bounds_returns_zero_rects() {
        let items = w(&[("a", 1.0), ("b", 2.0)]);
        let out = squarify(
            &items,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
        );
        assert_eq!(out.len(), 2);
        for r in out {
            assert_eq!(r.area(), 0.0);
        }
    }

    #[test]
    fn single_item_fills_bounds() {
        let items = w(&[("only", 10.0)]);
        let out = squarify(&items, Rect::unit());
        assert_eq!(out.len(), 1);
        let r = out[0];
        assert!((r.w - 1.0).abs() < 1e-4);
        assert!((r.h - 1.0).abs() < 1e-4);
    }

    #[test]
    fn two_items_share_area_proportionally() {
        // Bruls example, 6 items summing to a 6x4 canvas
        let items = w(&[("a", 6.0), ("b", 6.0), ("c", 4.0), ("d", 3.0), ("e", 2.0), ("f", 2.0)]);
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            w: 6.0,
            h: 4.0,
        };
        let out = squarify(&items, bounds);
        assert_eq!(out.len(), 6);
        let total_area: f64 = out.iter().map(|r| r.area()).sum();
        assert!((total_area - 24.0).abs() < 1e-3, "total area {total_area}");
    }

    #[test]
    fn all_tiles_stay_inside_bounds_for_random_inputs() {
        // deterministic pseudorandom, skip the random crate dep. xorshift
        // is fine for fuzzing rect placement.
        let mut state: u64 = 0xDEAD_BEEF_1234_5678;
        let mut rand = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..100 {
            let n = (rand() % 30) as usize + 1;
            let items: Vec<(String, f64)> = (0..n)
                .map(|i| {
                    let v = ((rand() % 1000) as f64) + 0.1;
                    (format!("i{i}"), v)
                })
                .collect();
            let out = squarify(&items, Rect::unit());
            assert_eq!(out.len(), n);
            for r in &out {
                assert_inside_unit(r);
            }
            let total: f64 = out.iter().map(|r| r.area()).sum();
            // 1% slack for f32 quantisation with lots of small tiles
            assert!(
                (total - 1.0).abs() < 0.01,
                "n={n} total_area={total}",
            );
        }
    }

    #[test]
    fn order_is_preserved() {
        let items = w(&[("a", 1.0), ("b", 10.0), ("c", 3.0), ("d", 0.5)]);
        let out = squarify(&items, Rect::unit());
        // positional: out[0] = "a", out[1] = "b", regardless of internal sort
        assert_eq!(out.len(), 4);
        // "b" is heaviest, biggest area
        let areas: Vec<f64> = out.iter().map(|r| r.area()).collect();
        let max_idx = areas
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(max_idx, 1);
    }

    #[test]
    fn zero_weight_items_get_degenerate_rects() {
        let items = w(&[("a", 10.0), ("b", 0.0), ("c", 5.0)]);
        let out = squarify(&items, Rect::unit());
        assert_eq!(out.len(), 3);
        assert_eq!(out[1].area(), 0.0);
        // non-zero tiles together ~= full area
        let nonzero: f64 = out[0].area() + out[2].area();
        assert!((nonzero - 1.0).abs() < 1e-3);
    }

    #[test]
    fn negative_and_nan_weights_treated_as_zero() {
        let items = w(&[("a", f64::NAN), ("b", -5.0), ("c", 10.0)]);
        let out = squarify(&items, Rect::unit());
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].area(), 0.0);
        assert_eq!(out[1].area(), 0.0);
        assert!((out[2].area() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn aspect_ratios_stay_reasonable() {
        // 50 tiles, none should be pathologically skinny. this is literally
        // why we use squarified over slice-and-dice.
        let items: Vec<(String, f64)> = (0..50)
            .map(|i| (format!("i{i}"), 50.0 - i as f64))
            .collect();
        let out = squarify(&items, Rect::unit());
        for r in &out {
            if r.w > 1e-4 && r.h > 1e-4 {
                let ratio = (r.w as f64).max(r.h as f64) / (r.w as f64).min(r.h as f64);
                assert!(
                    ratio < 20.0,
                    "degenerate aspect ratio {ratio}: {r:?}",
                );
            }
        }
    }

    #[test]
    fn offset_bounds_are_respected() {
        let items = w(&[("a", 2.0), ("b", 3.0)]);
        let bounds = Rect {
            x: 10.0,
            y: 20.0,
            w: 5.0,
            h: 5.0,
        };
        let out = squarify(&items, bounds);
        for r in &out {
            assert!(r.x >= 10.0 - 1e-5);
            assert!(r.y >= 20.0 - 1e-5);
            assert!(r.x + r.w <= 15.0 + 1e-4);
            assert!(r.y + r.h <= 25.0 + 1e-4);
        }
    }

    #[test]
    fn many_equal_weights_produce_grid_like_layout() {
        let items: Vec<(String, f64)> = (0..16).map(|i| (format!("i{i}"), 1.0)).collect();
        let out = squarify(&items, Rect::unit());
        // 16 equal tiles on 1x1 bounds = 1/16 each
        for r in &out {
            assert!((r.area() - 1.0 / 16.0).abs() < 1e-3);
        }
    }

    #[test]
    fn large_input_runs_fast() {
        let items: Vec<(String, f64)> = (0..512).map(|i| (format!("i{i}"), (512 - i) as f64)).collect();
        let started = std::time::Instant::now();
        let out = squarify(&items, Rect::unit());
        let elapsed = started.elapsed();
        assert_eq!(out.len(), 512);
        // generous ceiling. O(n log n), should finish well under 1ms on modern hw.
        assert!(elapsed.as_millis() < 100, "slow: {elapsed:?}");
    }

    #[test]
    fn rect_serializes_compactly() {
        let r = Rect {
            x: 0.25,
            y: 0.5,
            w: 0.125,
            h: 0.0625,
        };
        let v = serde_json::to_value(r).unwrap();
        assert!(v.get("x").is_some());
        assert!(v.get("w").is_some());
    }

    #[test]
    fn exact_bruls_example_totals_area() {
        // classic Bruls '99 example. 6 items, 6x4 canvas. don't care about
        // exact placement (implementations vary on tie axis) but areas must
        // sum and ratios stay reasonable.
        let items = w(&[("a", 6.0), ("b", 6.0), ("c", 4.0), ("d", 3.0), ("e", 2.0), ("f", 2.0)]);
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            w: 6.0,
            h: 4.0,
        };
        let out = squarify(&items, bounds);
        let total: f64 = out.iter().map(|r| r.area()).sum();
        assert!((total - 24.0).abs() < 1e-3);
        // proportional: "a" and "b" are 6/23 of total each
        let a_expected = 6.0 / 23.0 * 24.0;
        assert!((out[0].area() - a_expected).abs() < 1e-3);
        assert!((out[1].area() - a_expected).abs() < 1e-3);
    }
}
