//! Shared `?limit=` clamping used by the paged `/api/*` endpoints.
//!
//! Every list endpoint exposes its own `DEFAULT_LIMIT` / `MAX_LIMIT` tuned
//! to what its payload renders comfortably, but the clamping rule is
//! identical: accept a caller value inside `[1, max]`, otherwise fall back
//! to `default`. That rule used to be copy-pasted into eight modules; it now
//! lives here and each module passes its own constants.

/// Clamp a caller-supplied `?limit=` to `[1, max]`, falling back to
/// `default` when the value is absent or out of range.
#[must_use]
pub fn clamp_limit(requested: Option<i64>, default: i64, max: i64) -> i64 {
    match requested {
        Some(value) if (1..=max).contains(&value) => value,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_range_value_is_kept() {
        assert_eq!(clamp_limit(Some(42), 100, 500), 42);
        assert_eq!(clamp_limit(Some(1), 100, 500), 1);
        assert_eq!(clamp_limit(Some(500), 100, 500), 500);
    }

    #[test]
    fn out_of_range_or_missing_falls_back_to_default() {
        assert_eq!(clamp_limit(None, 100, 500), 100);
        assert_eq!(clamp_limit(Some(0), 100, 500), 100);
        assert_eq!(clamp_limit(Some(-5), 100, 500), 100);
        assert_eq!(clamp_limit(Some(501), 100, 500), 100);
    }
}
