pub const LOGICAL_WIDTH: f64 = 770.0;
pub const LOGICAL_HEIGHT: f64 = 450.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub width: f64,
    pub height: f64,
    pub dpr: f64,
}

impl Viewport {
    pub fn new(css_width: f64, css_height: f64, dpr: f64) -> Self {
        Self {
            width: css_width.max(1.0),
            height: css_height.max(1.0),
            dpr: dpr.max(1.0),
        }
    }

    pub fn logical_to_css(self, x: f64, y: f64) -> (f64, f64) {
        (
            x * self.width / LOGICAL_WIDTH,
            y * self.height / LOGICAL_HEIGHT,
        )
    }

    pub fn css_to_logical(self, x: f64, y: f64) -> (f64, f64) {
        (
            x * LOGICAL_WIDTH / self.width,
            y * LOGICAL_HEIGHT / self.height,
        )
    }

    pub fn bitmap_size(self) -> (u32, u32) {
        (
            (self.width * self.dpr).round().max(1.0) as u32,
            (self.height * self.dpr).round().max(1.0) as u32,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_logical_coordinates() {
        let viewport = Viewport::new(1540.0, 900.0, 2.0);
        let css = viewport.logical_to_css(231.0, 135.0);
        assert_eq!(viewport.css_to_logical(css.0, css.1), (231.0, 135.0));
        assert_eq!(viewport.bitmap_size(), (3080, 1800));
    }

    #[test]
    fn viewport_clamps_invalid_values() {
        let viewport = Viewport::new(0.0, -1.0, 0.0);
        assert_eq!(viewport, Viewport::new(1.0, 1.0, 1.0));
    }
}
