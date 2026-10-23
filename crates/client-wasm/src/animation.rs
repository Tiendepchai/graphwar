const MIN_DURATION_MS: f64 = 280.0;
const MAX_DURATION_MS: f64 = 2_200.0;
const MILLIS_PER_POINT: f64 = 5.0;

pub fn frame_delta_ms(previous_timestamp: Option<f64>, timestamp: f64) -> f64 {
    previous_timestamp.map_or(0.0, |previous| (timestamp - previous).clamp(0.0, 100.0))
}

pub fn visible_points(path_len: usize, elapsed_ms: f64) -> usize {
    if path_len <= 1 {
        return path_len;
    }
    let duration = (path_len as f64 * MILLIS_PER_POINT).clamp(MIN_DURATION_MS, MAX_DURATION_MS);
    let progress = (elapsed_ms / duration).clamp(0.0, 1.0);
    1 + ((path_len - 1) as f64 * progress).floor() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_delta_uses_raf_clock_and_bounds_resumed_frames() {
        assert_eq!(frame_delta_ms(None, 20_000.0), 0.0);
        assert_eq!(frame_delta_ms(Some(1_000.0), 1_008.0), 8.0);
        assert_eq!(frame_delta_ms(Some(1_000.0), 900.0), 0.0);
        assert_eq!(frame_delta_ms(Some(1_000.0), 20_000.0), 100.0);
    }

    #[test]
    fn timeline_is_bounded_and_finishes() {
        assert_eq!(visible_points(0, 10.0), 0);
        assert_eq!(visible_points(100, 0.0), 1);
        assert!(visible_points(100, 250.0) < 100);
        assert_eq!(visible_points(100, 10_000.0), 100);
    }
}
