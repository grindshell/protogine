//! Library exporters with explicit refusal of unsupported target values.

use tot::Value;

pub(super) fn export(value: &Value, format: &str) -> mlua::Result<String> {
    match format {
        "json" => Ok(tot::json::to_string(value)),
        "yaml" => tot::yaml::to_string(value).map_err(mlua::Error::external),
        // The upstream default omits nulls; the script API must reject them.
        "toml" => tot::toml::to_string(value, tot::toml::NullPolicy::Error)
            .map(|output| output.text)
            .map_err(mlua::Error::external),
        _ => Err(mlua::Error::runtime(
            "export format must be json, yaml, or toml",
        )),
    }
}
