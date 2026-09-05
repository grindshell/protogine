//! Explicit, fallible adapters; no CLI invocation or implicit null omission.

use tot::Value;

pub(super) fn export(value: &Value, format: &str) -> mlua::Result<String> {
    match format {
        "json" => Ok(tot::json::to_string(value)),
        "yaml" => yaml_serde::to_string(&yaml(value, "$")?).map_err(mlua::Error::external),
        "toml" => {
            if !matches!(value, Value::Object(_)) {
                return Err(mlua::Error::runtime("TOML requires an object root"));
            }
            toml::to_string_pretty(&toml_value(value, "$")?).map_err(mlua::Error::external)
        }
        _ => Err(mlua::Error::runtime(
            "export format must be json, yaml, or toml",
        )),
    }
}

fn fail(path: &str, reason: &str) -> mlua::Error {
    mlua::Error::runtime(format!("{path}: {reason}"))
}

fn child(path: &str, key: &str) -> String {
    format!("{path}[{key:?}]")
}

fn yaml(value: &Value, path: &str) -> mlua::Result<yaml_serde::Value> {
    use yaml_serde::Value as Y;
    Ok(match value {
        Value::Null => Y::Null,
        Value::Bool(v) => Y::Bool(*v),
        Value::String(v) => Y::String(v.clone()),
        Value::Float(v) => Y::Number(v.as_f64().into()),
        Value::Integer(v) => {
            if let Some(v) = v.as_i64() {
                Y::Number(v.into())
            } else if let Some(v) = v.as_u64() {
                Y::Number(v.into())
            } else {
                return Err(fail(
                    path,
                    "YAML export requires a signed/unsigned 64-bit integer",
                ));
            }
        }
        Value::Array(items) => Y::Sequence(
            items
                .iter()
                .enumerate()
                .map(|(i, v)| yaml(v, &format!("{path}[{}]", i + 1)))
                .collect::<mlua::Result<_>>()?,
        ),
        Value::Object(map) => {
            let mut out = yaml_serde::Mapping::new();
            for (key, value) in map.iter() {
                out.insert(Y::String(key.into()), yaml(value, &child(path, key))?);
            }
            Y::Mapping(out)
        }
    })
}

fn toml_value(value: &Value, path: &str) -> mlua::Result<toml::Value> {
    use toml::Value as T;
    Ok(match value {
        Value::Null => return Err(fail(path, "TOML has no null value")),
        Value::Bool(v) => T::Boolean(*v),
        Value::String(v) => T::String(v.clone()),
        Value::Float(v) => T::Float(v.as_f64()),
        Value::Integer(v) => T::Integer(
            v.as_i64()
                .ok_or_else(|| fail(path, "TOML integers must fit signed 64-bit range"))?,
        ),
        Value::Array(items) => T::Array(
            items
                .iter()
                .enumerate()
                .map(|(i, v)| toml_value(v, &format!("{path}[{}]", i + 1)))
                .collect::<mlua::Result<_>>()?,
        ),
        Value::Object(map) => {
            let mut out = toml::Table::new();
            for (key, value) in map.iter() {
                out.insert(key.into(), toml_value(value, &child(path, key))?);
            }
            T::Table(out)
        }
    })
}
