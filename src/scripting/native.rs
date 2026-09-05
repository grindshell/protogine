//! Scoped Luau buffer adapter. No VM storage or kernel borrow crosses FFI.

use super::{Budget, CallbackLogs};
use crate::{
    manifest::valid_id,
    plugins::{CallError, MAX_BATCH_BYTES, PluginSet},
};
use mlua::{Buffer, FromLuaMulti, Lua, LuaString, MultiValue, Scope, Table};
use std::io::Read;

pub(super) fn bind<'s>(
    lua: &Lua,
    scope: &'s Scope<'s, '_>,
    mut plugins: Option<&'s mut PluginSet>,
    callable: bool,
    budget: &'s Budget,
    logs: &'s CallbackLogs,
) -> mlua::Result<Table> {
    let native = lua.create_table()?;
    let metadata = lua.create_table()?;
    if let Some(plugins) = &plugins {
        for info in plugins.infos() {
            let functions = lua.create_table()?;
            for function in &info.functions {
                let descriptor = lua.create_table()?;
                descriptor.raw_set("schema", function.schema.as_str())?;
                descriptor.raw_set("schema_version", function.schema_version)?;
                descriptor.set_readonly(true);
                functions.raw_set(function.id.as_str(), descriptor)?;
            }
            functions.set_readonly(true);
            metadata.raw_set(info.id.as_str(), functions)?;
        }
    }
    metadata.set_readonly(true);
    native.raw_set("plugins", metadata)?;
    let mut calls = 0usize;
    let mut transferred = 0usize;
    native.raw_set(
        "call",
        scope.create_function_mut(move |lua, args: MultiValue| {
            if let Some(fault) = budget.check() {
                return Err(mlua::Error::runtime(fault));
            }
            if !callable {
                return Err(mlua::Error::runtime("native calls require init or update"));
            }
            // Count malformed and missing arguments too: typed closure arguments
            // would fail mlua's conversion before reaching the attempt budget.
            calls += 1;
            if calls > 128 {
                let fault = "native call/buffer limit exceeded";
                budget.fail(fault);
                return Err(mlua::Error::runtime(fault));
            }
            let (plugin, function, input, output) =
                <(LuaString, LuaString, Buffer, Buffer)>::from_lua_multi(args, lua)?;
            let size = input.len().checked_add(output.len());
            if size.is_none_or(|n| n > MAX_BATCH_BYTES) {
                budget.fail("native call/buffer limit exceeded");
            } else {
                transferred += size.unwrap();
                if transferred > 64 * 1024 * 1024 {
                    budget.fail("native transfer limit exceeded");
                }
            }
            if let Some(fault) = budget.check() {
                return Err(mlua::Error::runtime(fault));
            }
            let plugin = plugin.to_str()?;
            let function = function.to_str()?;
            if !valid_id(&plugin) || !valid_id(&function) {
                return Err(mlua::Error::runtime("invalid native identifier"));
            }
            let plugins = plugins
                .as_deref_mut()
                .ok_or_else(|| mlua::Error::runtime("unknown native plugin"))?;
            let mut source = Vec::new();
            source
                .try_reserve_exact(input.len())
                .map_err(|_| mlua::Error::runtime("native scratch allocation failed"))?;
            source.resize(input.len(), 0);
            input
                .cursor()
                .read_exact(&mut source)
                .map_err(mlua::Error::external)?;
            if let Some(fault) = budget.check() {
                return Err(mlua::Error::runtime(fault));
            }
            let result = plugins.call(&plugin, &function, &source, output.len());
            // Record the primary native fault before log/deadline checks.
            if let Err(CallError::Fault(error)) = &result {
                budget.fail(error.to_string());
            }
            for message in plugins.take_logs() {
                logs.push(budget, message)?;
            }
            finish_call(budget, &output, result)
        })?,
    )?;
    native.set_readonly(true);
    Ok(native)
}

fn finish_call(
    budget: &Budget,
    output: &Buffer,
    result: Result<Vec<u8>, CallError>,
) -> mlua::Result<usize> {
    // Check after FFI and before publishing either the result or output bytes.
    if let Some(fault) = budget.check() {
        return Err(mlua::Error::runtime(fault));
    }
    let bytes = result.map_err(mlua::Error::external)?;
    // Validated prefix only; no VM work occurs during the foreign call.
    output.write_bytes(0, &bytes);
    Ok(bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn expired_native_result_is_not_published() {
        let lua = Lua::new();
        let budget = Budget::default();
        let output = lua.create_buffer([85; 4]).unwrap();
        let successful_result = || Ok(vec![238]);
        assert_eq!(
            finish_call(&budget, &output, successful_result()).unwrap(),
            1
        );
        assert_eq!(output.to_vec(), [238, 85, 85, 85]);

        output.write_bytes(0, &[85; 4]);
        budget.deadline.set(Some(Instant::now()));
        let error = finish_call(&budget, &output, successful_result()).unwrap_err();
        assert!(error.to_string().contains("script deadline exceeded"));
        assert_eq!(output.to_vec(), [85; 4]);
        budget.deadline.set(None);
        assert_eq!(budget.check().as_deref(), Some("script deadline exceeded"));
    }
}
