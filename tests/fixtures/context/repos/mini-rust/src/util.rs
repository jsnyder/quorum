/// Clamps a value between lo and hi (inclusive).
pub fn clamp<T: Ord>(v: T, lo: T, hi: T) -> T {
    if v < lo { lo } else if v > hi { hi } else { v }
}
