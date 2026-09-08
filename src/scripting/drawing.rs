use super::assets::{ImageHandle, Images};
use super::utilities::{UtilityBudget, plain};
use crate::drawing::{
    DRAW_COMMAND_LIMIT, DrawCommand, SourceRect, Sprite, valid_color, valid_coordinate,
    valid_extent,
};
use mlua::{AnyUserData, FromLuaMulti, Lua, MultiValue, Scope, Table, Value};
use std::cell::RefCell;

/// Every option a sprite accepts. Key inspection stops at the first unexpected
/// name and is bounded by these counts, so a hostile table costs no more than
/// the fields it is allowed to carry.
const SPRITE_FIELDS: &[&str] = &["source", "width", "height", "flip_x", "flip_y", "tint"];
const SOURCE_FIELDS: &[&str] = &["x", "y", "width", "height"];
const TINT_FIELDS: &[&str] = &["r", "g", "b", "a"];

fn color(values: [f64; 4]) -> mlua::Result<[f32; 4]> {
    if !valid_color(values) {
        return Err(mlua::Error::runtime(
            "draw colors must be finite and in [0, 1]",
        ));
    }
    Ok(values.map(|n| n as f32))
}

pub(super) fn bind<'s>(
    lua: &Lua,
    scope: &'s Scope<'s, '_>,
    budget: &'s UtilityBudget<'_>,
    images: &'s Images,
    commands: &'s RefCell<Vec<DrawCommand>>,
) -> mlua::Result<Table> {
    let append = move |command| {
        let mut commands = commands.borrow_mut();
        budget.limit(
            commands.len() >= DRAW_COMMAND_LIMIT,
            "draw command limit exceeded",
        )?;
        commands.push(command);
        Ok(())
    };
    let draw = lua.create_table()?;
    draw.raw_set(
        "clear",
        scope.create_function(move |_, (r, g, b, a): (f64, f64, f64, f64)| {
            append(DrawCommand::Clear(color([r, g, b, a])?))
        })?,
    )?;
    draw.raw_set(
        "rect",
        scope.create_function(
            move |_, (x, y, w, h, r, g, b, a): (f64, f64, f64, f64, f64, f64, f64, f64)| {
                if ![x, y].iter().copied().all(valid_coordinate)
                    || ![w, h].iter().copied().all(valid_extent)
                {
                    return Err(mlua::Error::runtime(
                        "draw coordinates or sizes are outside the finite pixel range",
                    ));
                }
                append(DrawCommand::Rect {
                    x: x as f32,
                    y: y as f32,
                    width: w as f32,
                    height: h as f32,
                    color: color([r, g, b, a])?,
                })
            },
        )?,
    )?;
    draw.raw_set(
        "sprite",
        scope.create_function(move |lua, args: MultiValue| {
            // Counted before argument and option conversion, so refusals are
            // bounded as well as accepted calls.
            budget.sprite_call()?;
            let (value, x, y, options) =
                <(AnyUserData, f64, f64, Option<Table>)>::from_lua_multi(args, lua)?;
            // Take the identity and dimensions, then release both borrows: the
            // option tables below run VM code.
            let (image, image_width, image_height) = {
                let handle = value.borrow::<ImageHandle>()?;
                let (width, height) = images.drawable(&handle)?;
                (handle.id(), width, height)
            };
            let options = sprite_options(options.as_ref())?;
            let source = options.source.unwrap_or(SourceRect {
                x: 0,
                y: 0,
                width: image_width,
                height: image_height,
            });
            if !source.fits(image_width, image_height) {
                return Err(mlua::Error::runtime(
                    "sprite source rectangle does not fit the image",
                ));
            }
            // An omitted destination extent follows the selected source, so a
            // cropped sprite defaults to its crop rather than the whole image.
            let width = options.width.unwrap_or(f64::from(source.width));
            let height = options.height.unwrap_or(f64::from(source.height));
            // The f64 checks must precede narrowing, since an out-of-range
            // authored value can only be seen before it becomes an f32.
            if !valid_coordinate(x) || !valid_coordinate(y) {
                return Err(mlua::Error::runtime(
                    "sprite coordinates are outside the finite pixel range",
                ));
            }
            if !valid_extent(width) || !valid_extent(height) {
                return Err(mlua::Error::runtime(
                    "sprite destination size is outside the finite pixel range",
                ));
            }
            let sprite = Sprite {
                image,
                source,
                x: x as f32,
                y: y as f32,
                width: width as f32,
                height: height as f32,
                flip_x: options.flip_x,
                flip_y: options.flip_y,
                tint: color(options.tint)?,
            };
            // Every published sprite also passes the assembled check the
            // renderer applies to copied commands, so the two cannot drift.
            // Each range boundary is exact in f32, so this rejects nothing the
            // checks above accepted.
            sprite.check().map_err(mlua::Error::runtime)?;
            append(DrawCommand::Sprite(sprite))
        })?,
    )?;
    draw.set_readonly(true);
    Ok(draw)
}

/// Copied option values. Mutating the caller's tables afterwards cannot change
/// a published command, because nothing here retains a VM reference.
struct SpriteOptions {
    source: Option<SourceRect>,
    width: Option<f64>,
    height: Option<f64>,
    flip_x: bool,
    flip_y: bool,
    tint: [f64; 4],
}

fn sprite_options(options: Option<&Table>) -> mlua::Result<SpriteOptions> {
    let mut parsed = SpriteOptions {
        source: None,
        width: None,
        height: None,
        flip_x: false,
        flip_y: false,
        tint: [1.0; 4],
    };
    let Some(options) = options else {
        return Ok(parsed);
    };
    plain(options, SPRITE_FIELDS, "sprite options")?;
    if let Some(source) = options.raw_get::<Option<Table>>("source")? {
        parsed.source = Some(source_rect(&source)?);
    }
    parsed.width = number(options, "width")?;
    parsed.height = number(options, "height")?;
    parsed.flip_x = boolean(options, "flip_x")?.unwrap_or(false);
    parsed.flip_y = boolean(options, "flip_y")?.unwrap_or(false);
    if let Some(tint) = options.raw_get::<Option<Table>>("tint")? {
        plain(&tint, TINT_FIELDS, "sprite tint")?;
        for (index, key) in TINT_FIELDS.iter().enumerate() {
            parsed.tint[index] = required(&tint, key)?;
        }
    }
    Ok(parsed)
}

/// An explicit source supplies all four fields, in image pixels.
fn source_rect(source: &Table) -> mlua::Result<SourceRect> {
    plain(source, SOURCE_FIELDS, "sprite source")?;
    let mut values = [0u32; 4];
    for (index, key) in SOURCE_FIELDS.iter().enumerate() {
        values[index] = whole(required(source, key)?, key)?;
    }
    let [x, y, width, height] = values;
    if width == 0 || height == 0 {
        return Err(mlua::Error::runtime(
            "sprite source width and height must be positive",
        ));
    }
    Ok(SourceRect {
        x,
        y,
        width,
        height,
    })
}

/// Actual numeric values only: a numeric string is a coercion, not a number.
fn number(table: &Table, key: &str) -> mlua::Result<Option<f64>> {
    match table.raw_get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::Integer(value) => Ok(Some(value as f64)),
        Value::Number(value) => Ok(Some(value)),
        _ => Err(mlua::Error::runtime(format!(
            "sprite {key} must be a number"
        ))),
    }
}

fn required(table: &Table, key: &str) -> mlua::Result<f64> {
    number(table, key)?.ok_or_else(|| mlua::Error::runtime(format!("sprite {key} is required")))
}

fn boolean(table: &Table, key: &str) -> mlua::Result<Option<bool>> {
    match table.raw_get::<Value>(key)? {
        Value::Nil => Ok(None),
        Value::Boolean(value) => Ok(Some(value)),
        _ => Err(mlua::Error::runtime(format!(
            "sprite {key} must be a boolean"
        ))),
    }
}

fn whole(value: f64, key: &str) -> mlua::Result<u32> {
    if !value.is_finite() || value.fract() != 0.0 || value < 0.0 || value > f64::from(u32::MAX) {
        return Err(mlua::Error::runtime(format!(
            "sprite source {key} must be a nonnegative whole number of pixels"
        )));
    }
    Ok(value as u32)
}
