//! Owned presentation data, independent of the window and graphics backend.

use crate::assets::ImageId;

pub const DRAW_COMMAND_LIMIT: usize = 10_000;
pub const DRAW_COORDINATE_LIMIT: f64 = 1_000_000.0;

/// Colors use normalized RGBA. Rectangles use top-left pixel coordinates and
/// nonnegative sizes. The scripting boundary validates before narrowing to f32.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DrawCommand {
    Clear([f32; 4]),
    Rect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: [f32; 4],
    },
    Sprite(Sprite),
}

/// A half-open crop in image pixels: columns `x..x + width`, rows
/// `y..y + height`. Coordinates stay integer-valued until the renderer converts
/// them, and are never tile indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl SourceRect {
    /// Positive extents whose checked half-open bounds lie inside the image.
    pub fn fits(self, width: u32, height: u32) -> bool {
        self.width > 0
            && self.height > 0
            && self
                .x
                .checked_add(self.width)
                .is_some_and(|right| right <= width)
            && self
                .y
                .checked_add(self.height)
                .is_some_and(|bottom| bottom <= height)
    }
}

/// One cropped image draw: an identity and scalars, never pixels or a GPU
/// reference. A copy stays inspectable after its session ends, but can only be
/// rendered against the live store that issued its `ImageId`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sprite {
    pub image: ImageId,
    pub source: SourceRect,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    pub tint: [f32; 4],
}

impl Sprite {
    /// The scalar ranges every published sprite satisfies, independent of any
    /// image. The bindings check the authored f64 values with the predicates
    /// below before narrowing; the renderer rechecks copied and Rust-supplied
    /// commands here, so neither has its own copy of the rules. Source extents
    /// are checked against the image's dimensions by `SourceRect::fits`.
    pub fn check(&self) -> Result<(), &'static str> {
        if !valid_coordinate(f64::from(self.x)) || !valid_coordinate(f64::from(self.y)) {
            return Err("sprite coordinates are outside the finite pixel range");
        }
        if !valid_extent(f64::from(self.width)) || !valid_extent(f64::from(self.height)) {
            return Err("sprite destination size is outside the finite pixel range");
        }
        if self.source.width == 0 || self.source.height == 0 {
            return Err("sprite source rectangle must have positive extents");
        }
        if !valid_color(self.tint.map(f64::from)) {
            return Err("sprite tint must be finite and in [0, 1]");
        }
        Ok(())
    }
}

/// A finite top-left pixel position within the supported range.
pub fn valid_coordinate(value: f64) -> bool {
    value.is_finite() && value.abs() <= DRAW_COORDINATE_LIMIT
}

/// A finite nonnegative size. Zero area is a valid, counted no-op; a negative
/// size is an error rather than flip shorthand.
pub fn valid_extent(value: f64) -> bool {
    value.is_finite() && (0.0..=DRAW_COORDINATE_LIMIT).contains(&value)
}

/// Finite normalized RGBA.
pub fn valid_color(values: [f64; 4]) -> bool {
    values
        .iter()
        .all(|n| n.is_finite() && (0.0..=1.0).contains(n))
}
