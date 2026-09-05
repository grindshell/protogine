//! Owned presentation data, independent of the window and graphics backend.

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
}
