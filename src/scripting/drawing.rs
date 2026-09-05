use super::utilities::UtilityBudget;
use crate::drawing::{DRAW_COMMAND_LIMIT, DRAW_COORDINATE_LIMIT, DrawCommand};
use mlua::{Lua, Scope, Table};
use std::cell::RefCell;

fn color(values: [f64; 4]) -> mlua::Result<[f32; 4]> {
    if values
        .iter()
        .any(|n| !n.is_finite() || !(0.0..=1.0).contains(n))
    {
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
                if [x, y]
                    .iter()
                    .any(|n| !n.is_finite() || n.abs() > DRAW_COORDINATE_LIMIT)
                    || [w, h]
                        .iter()
                        .any(|n| !n.is_finite() || !(0.0..=DRAW_COORDINATE_LIMIT).contains(n))
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
    draw.set_readonly(true);
    Ok(draw)
}
