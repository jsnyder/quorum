//! Inline semigraphical primitives for the stats dashboard.
//!
//! Deliberately tiny and dependency-free — we do NOT pull in `textplots`
//! (it stretches line heights in some terminals and violates DESIGN.md §1's
//! minimalist aesthetic). Each primitive has a Unicode form and an ASCII
//! fallback controlled by a single `unicode` flag.

const HBAR_WIDTH: usize = 10;

/// Horizontal bar, fixed width 10 cells, proportional fill.
/// Unicode: █ filled, · padded.  ASCII: # filled, . padded.
pub fn hbar(value: f64, max: f64, unicode: bool) -> String {
    let ratio = if max <= 0.0 || !value.is_finite() {
        0.0
    } else {
        (value / max).clamp(0.0, 1.0)
    };
    let filled = (ratio * HBAR_WIDTH as f64).round() as usize;
    let filled = filled.min(HBAR_WIDTH);
    let (fill_ch, pad_ch) = if unicode { ('█', '·') } else { ('#', '.') };
    let mut out = String::with_capacity(HBAR_WIDTH * 3);
    for _ in 0..filled {
        out.push(fill_ch);
    }
    for _ in filled..HBAR_WIDTH {
        out.push(pad_ch);
    }
    out
}

/// Sparkline. Uses U+2581–U+2588 (8 levels) in Unicode mode; `_.-=^` (5 levels) in ASCII.
pub fn sparkline(points: &[f64], unicode: bool) -> String {
    if points.is_empty() {
        return String::new();
    }
    // Scale over finite values only. NaN/±∞ render as the lowest level so they
    // don't swamp the scale and make real trends unreadable.
    let (min, max) = points.iter().copied().filter(|p| p.is_finite())
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| (lo.min(p), hi.max(p)));
    let range = max - min;

    let unicode_levels: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let ascii_levels: [char; 5] = ['_', '.', '-', '=', '^'];
    let levels: &[char] = if unicode { &unicode_levels } else { &ascii_levels };

    let mut out = String::with_capacity(points.len() * 3);
    for &p in points {
        let idx = if !p.is_finite() || range <= 0.0 {
            0 // flat or non-finite → lowest level so visually quiet
        } else {
            let ratio = ((p - min) / range).clamp(0.0, 1.0);
            (ratio * (levels.len() as f64 - 1.0)).round() as usize
        };
        out.push(levels[idx.min(levels.len() - 1)]);
    }
    out
}

/// Trend direction from first vs last point, with ~5% tolerance for "flat".
pub fn trend_arrow(points: &[f64], unicode: bool) -> &'static str {
    if points.len() < 2 {
        return if unicode { "→" } else { "=" };
    }
    let first = *points.first().unwrap();
    let last = *points.last().unwrap();
    // Treat non-finite endpoints as "unknown" → flat, so we never misreport direction.
    if !first.is_finite() || !last.is_finite() {
        return if unicode { "→" } else { "=" };
    }
    let denom = first.abs().max(1e-9);
    let change = (last - first) / denom;
    let flat_tolerance = 0.05;
    if change > flat_tolerance {
        if unicode { "↑" } else { "+" }
    } else if change < -flat_tolerance {
        if unicode { "↓" } else { "-" }
    } else {
        if unicode { "→" } else { "=" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- hbar ---

    #[test]
    fn hbar_zero_is_all_padding() {
        assert_eq!(hbar(0.0, 100.0, true), "··········");
    }

    #[test]
    fn hbar_full_is_all_fill() {
        assert_eq!(hbar(100.0, 100.0, true), "██████████");
    }

    #[test]
    fn hbar_half_is_half_fill() {
        assert_eq!(hbar(50.0, 100.0, true), "█████·····");
    }

    #[test]
    fn hbar_over_max_clamps_to_full() {
        assert_eq!(hbar(200.0, 100.0, true), "██████████");
    }

    #[test]
    fn hbar_negative_clamps_to_empty() {
        assert_eq!(hbar(-10.0, 100.0, true), "··········");
    }

    #[test]
    fn hbar_max_zero_is_empty_safely() {
        // Avoid division by zero
        assert_eq!(hbar(5.0, 0.0, true), "··········");
    }

    #[test]
    fn hbar_ascii_fallback() {
        assert_eq!(hbar(50.0, 100.0, false), "#####.....");
    }

    #[test]
    fn hbar_always_10_cells() {
        for v in [0.0, 1.0, 23.4, 50.0, 99.9, 100.0] {
            let bar = hbar(v, 100.0, true);
            // Bars use chars, not bytes. Unicode block chars are 3 bytes each.
            assert_eq!(bar.chars().count(), 10, "value {} produced {:?}", v, bar);
        }
    }

    // --- sparkline ---

    #[test]
    fn sparkline_empty_input_yields_empty_output() {
        assert_eq!(sparkline(&[], true), "");
    }

    #[test]
    fn sparkline_all_same_uses_mid_level() {
        let s = sparkline(&[5.0, 5.0, 5.0], true);
        assert_eq!(s.chars().count(), 3);
        // Flat line should use the lowest level (U+2581 = ▁) by convention.
        // We'll assert all chars are identical.
        let first = s.chars().next().unwrap();
        assert!(s.chars().all(|c| c == first));
    }

    #[test]
    fn sparkline_monotonic_increase_uses_progression() {
        let s = sparkline(&[1.0, 2.0, 3.0, 4.0, 5.0], true);
        let chars: Vec<char> = s.chars().collect();
        assert_eq!(chars.len(), 5);
        // Each char's unicode codepoint should be >= the previous (8 levels, monotone).
        for pair in chars.windows(2) {
            assert!(pair[1] as u32 >= pair[0] as u32,
                "non-monotone: {:?} -> {:?}", pair[0], pair[1]);
        }
    }

    #[test]
    fn sparkline_nan_does_not_flatten_other_points() {
        // With NaN present, the remaining finite points must still span multiple levels
        // if their values differ — if the impl collapses everything to one level, the
        // graph is useless.
        let s = sparkline(&[1.0, f64::NAN, 3.0, 5.0], true);
        assert_eq!(s.chars().count(), 4);
        let unique: std::collections::BTreeSet<char> = s.chars().collect();
        assert!(unique.len() > 1,
            "finite points 1,3,5 should produce >1 distinct levels even with NaN, got {:?}", s);
    }

    #[test]
    fn sparkline_infinity_does_not_flatten_other_points() {
        let s = sparkline(&[1.0, f64::INFINITY, 3.0, 5.0], true);
        assert_eq!(s.chars().count(), 4);
        let unique: std::collections::BTreeSet<char> = s.chars().collect();
        assert!(unique.len() > 1,
            "finite points 1,3,5 should produce >1 distinct levels even with ∞, got {:?}", s);
    }

    #[test]
    fn sparkline_unicode_uses_block_range() {
        let s = sparkline(&[1.0, 5.0, 3.0], true);
        for c in s.chars() {
            // U+2581..=U+2588 are the 8 lower-block variants
            let cp = c as u32;
            assert!((0x2581..=0x2588).contains(&cp),
                "char {:?} codepoint {:04X} is outside U+2581-2588", c, cp);
        }
    }

    #[test]
    fn sparkline_ascii_fallback_length_matches_input() {
        let s = sparkline(&[1.0, 2.0, 3.0, 4.0, 5.0], false);
        assert_eq!(s.chars().count(), 5);
        // Fallback chars must be printable ASCII
        for c in s.chars() {
            assert!(c.is_ascii() && !c.is_ascii_control(), "{:?} not printable ascii", c);
        }
    }

    // --- trend_arrow ---

    #[test]
    fn trend_arrow_empty_points_is_flat() {
        assert_eq!(trend_arrow(&[], true), "→");
        assert_eq!(trend_arrow(&[], false), "=");
    }

    #[test]
    fn trend_arrow_rising() {
        assert_eq!(trend_arrow(&[1.0, 2.0, 3.0], true), "↑");
        assert_eq!(trend_arrow(&[1.0, 2.0, 3.0], false), "+");
    }

    #[test]
    fn trend_arrow_falling() {
        assert_eq!(trend_arrow(&[3.0, 2.0, 1.0], true), "↓");
        assert_eq!(trend_arrow(&[3.0, 2.0, 1.0], false), "-");
    }

    #[test]
    fn trend_arrow_nan_endpoints_are_flat() {
        // NaN endpoints must be treated as "unknown"; do not misreport direction.
        assert_eq!(trend_arrow(&[f64::NAN, 5.0], true), "→");
        assert_eq!(trend_arrow(&[1.0, f64::NAN], true), "→");
    }

    #[test]
    fn trend_arrow_infinite_endpoints_are_flat() {
        assert_eq!(trend_arrow(&[1.0, f64::INFINITY], true), "→");
        assert_eq!(trend_arrow(&[f64::INFINITY, 1.0], true), "→");
    }

    #[test]
    fn trend_arrow_flat_when_close() {
        // Small changes below ~5% tolerance counted as flat.
        assert_eq!(trend_arrow(&[1.00, 1.01], true), "→");
    }
}
