//! tot values in VM-owned storage, with explicit integer/null/array identities.

mod export;

use super::utilities::UtilityBudget;
use mlua::{
    AnyUserData, FromLuaMulti, Lua, LuaString, MetaMethod, MultiValue, Scope, Table, UserData,
    UserDataFields, UserDataMethods, Value,
};
use std::collections::HashSet;

struct Null;

impl UserData for Null {}

struct Integer(LuaString);
impl UserData for Integer {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("text", |_, this| Ok(this.0.clone()));
    }
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| Ok(this.0.clone()));
    }
}

pub(super) struct Data {
    array_meta: Table,
    null: AnyUserData,
}

impl Data {
    pub(super) fn new(lua: &Lua) -> mlua::Result<Self> {
        let array_meta = lua.create_table()?;
        array_meta.raw_set("__metatable", "data array")?;
        array_meta.set_readonly(true);
        Ok(Self {
            array_meta,
            null: lua.create_userdata(Null)?,
        })
    }

    pub(super) fn bind<'s>(
        &'s self,
        lua: &Lua,
        scope: &'s Scope<'s, '_>,
        budget: &'s UtilityBudget<'_>,
    ) -> mlua::Result<Table> {
        let api = lua.create_table()?;
        api.raw_set("null", self.null.clone())?;
        api.raw_set(
            "parse",
            scope.create_function(move |lua, args: MultiValue| {
                budget.begin()?;
                let source = LuaString::from_lua_multi(args, lua)?;
                budget.bytes(source.as_bytes().len())?;
                let value = match tot::parse(&source.to_str()?) {
                    Ok(value) => value,
                    Err(error) => {
                        // The pinned tot 0.1.0 has no typed error kind. Match its
                        // depth diagnostic exactly, since syntax errors can quote
                        // user text. Recheck this diagnostic when updating tot.
                        budget.limit(
                            error.message == "maximum nesting depth of 128 exceeded",
                            "data nesting limit exceeded",
                        )?;
                        return Err(mlua::Error::external(error));
                    }
                };
                let mut walk = Walk::new(budget);
                self.to_lua(lua, &value, 0, &mut walk)
            })?,
        )?;
        api.raw_set(
            "integer",
            scope.create_function(move |lua, text: Value| {
                budget.begin()?;
                let Value::String(text) = text else {
                    return Err(mlua::Error::runtime(
                        "integer requires decimal text, not a Luau number",
                    ));
                };
                budget.bytes(text.as_bytes().len())?;
                parse_integer(&text)?;
                lua.create_userdata(Integer(text))
            })?,
        )?;
        api.raw_set(
            "number",
            scope.create_function(move |_, value: Value| {
                budget.begin()?;
                match value {
                    Value::Integer(n) => Ok(n as f64),
                    Value::Number(n) if n.is_finite() => Ok(n),
                    Value::UserData(v) if v.is::<Integer>() => {
                        let value = parse_integer(&v.borrow::<Integer>()?.0)?;
                        match value.as_i64() {
                            Some(n)
                                if (-9_007_199_254_740_991..=9_007_199_254_740_991)
                                    .contains(&n) =>
                            {
                                Ok(n as f64)
                            }
                            _ => Err(mlua::Error::runtime(
                                "integer exceeds Luau's safe integer range",
                            )),
                        }
                    }
                    _ => Err(mlua::Error::runtime(
                        "expected a finite number or data integer",
                    )),
                }
            })?,
        )?;
        api.raw_set(
            "kind",
            scope.create_function(move |_, value: Value| {
                budget.begin()?;
                self.kind(&value)
            })?,
        )?;
        api.raw_set(
            "array",
            scope.create_function(move |lua, args: MultiValue| {
                budget.begin()?;
                let table = Table::from_lua_multi(args, lua)?;
                if table.metatable().is_some() && !self.is_array(&table) {
                    return Err(mlua::Error::runtime("custom metatables are not data"));
                }
                // Validate before mutating the caller's table.
                let mut walk = Walk::new(budget);
                walk.node(0)?;
                self.array_to_tot(&table, 0, &mut walk)?;
                table.set_metatable(Some(self.array_meta.clone()))?;
                Ok(table)
            })?,
        )?;
        api.raw_set(
            "format",
            scope.create_function(move |lua, value: Value| {
                budget.begin()?;
                let value = self.to_tot(value, 0, &mut Walk::new(budget))?;
                let text = tot::format_value(&value);
                budget.bytes(text.len())?;
                lua.create_string(text)
            })?,
        )?;
        api.raw_set(
            "export",
            scope.create_function(move |lua, args: MultiValue| {
                budget.begin()?;
                let (value, format) = <(Value, LuaString)>::from_lua_multi(args, lua)?;
                let value = self.to_tot(value, 0, &mut Walk::new(budget))?;
                let text = export::export(&value, &format.to_str()?)?;
                budget.bytes(text.len())?;
                lua.create_string(text)
            })?,
        )?;
        api.set_readonly(true);
        Ok(api)
    }

    fn is_array(&self, table: &Table) -> bool {
        table
            .metatable()
            .is_some_and(|meta| meta.to_pointer() == self.array_meta.to_pointer())
    }

    fn kind(&self, value: &Value) -> mlua::Result<&'static str> {
        Ok(match value {
            Value::Boolean(_) => "boolean",
            Value::String(_) => "string",
            Value::Integer(_) | Value::Number(_) => "float",
            Value::UserData(v) if v.is::<Null>() => "null",
            Value::UserData(v) if v.is::<Integer>() => "integer",
            Value::Table(table) if self.is_array(table) => "array",
            Value::Table(table) if table.metatable().is_none() => "object",
            _ => return Err(mlua::Error::runtime("unsupported data value")),
        })
    }

    fn to_lua(
        &self,
        lua: &Lua,
        value: &tot::Value,
        depth: usize,
        walk: &mut Walk<'_, '_>,
    ) -> mlua::Result<Value> {
        walk.node(depth)?;
        Ok(match value {
            tot::Value::Null => Value::UserData(self.null.clone()),
            tot::Value::Bool(v) => Value::Boolean(*v),
            tot::Value::Integer(v) => {
                walk.text(v.as_str())?;
                Value::UserData(lua.create_userdata(Integer(lua.create_string(v.as_str())?))?)
            }
            tot::Value::Float(v) => Value::Number(v.as_f64()),
            tot::Value::String(v) => {
                walk.text(v)?;
                Value::String(lua.create_string(v)?)
            }
            tot::Value::Array(items) => {
                let table = lua.create_table()?;
                for (index, value) in items.iter().enumerate() {
                    table.raw_set(index + 1, self.to_lua(lua, value, depth + 1, walk)?)?;
                }
                table.set_metatable(Some(self.array_meta.clone()))?;
                Value::Table(table)
            }
            tot::Value::Object(map) => {
                let table = lua.create_table()?;
                for (key, value) in map.iter() {
                    walk.text(key)?;
                    table.raw_set(key, self.to_lua(lua, value, depth + 1, walk)?)?;
                }
                Value::Table(table)
            }
        })
    }

    fn to_tot(
        &self,
        value: Value,
        depth: usize,
        walk: &mut Walk<'_, '_>,
    ) -> mlua::Result<tot::Value> {
        walk.node(depth)?;
        Ok(match value {
            Value::Boolean(v) => tot::Value::Bool(v),
            Value::Integer(v) => tot::Value::Float(tot::Float::from_f64(v as f64).unwrap()),
            Value::Number(v) => tot::Value::Float(
                tot::Float::from_f64(v)
                    .ok_or_else(|| mlua::Error::runtime("nonfinite numbers are not data"))?,
            ),
            Value::String(v) => {
                let v = v.to_str()?;
                walk.text(&v)?;
                tot::Value::String(v.to_string())
            }
            Value::UserData(v) if v.is::<Null>() => tot::Value::Null,
            Value::UserData(v) if v.is::<Integer>() => {
                let text = v.borrow::<Integer>()?.0.clone();
                walk.text(&text.to_str()?)?;
                tot::Value::Integer(parse_integer(&text)?)
            }
            Value::Table(table) if self.is_array(&table) => {
                self.array_to_tot(&table, depth, walk)?
            }
            Value::Table(table) if table.metatable().is_none() => {
                let pointer = table.to_pointer() as usize;
                if !walk.active.insert(pointer) {
                    return Err(mlua::Error::runtime("cyclic data table"));
                }
                let mut entries = Vec::new();
                for pair in table.pairs::<Value, Value>() {
                    let (key, value) = pair?;
                    let Value::String(key) = key else {
                        return Err(mlua::Error::runtime(
                            "objects require string keys; use data.array for arrays",
                        ));
                    };
                    let key = key.to_str()?;
                    walk.text(&key)?;
                    entries.push((key.to_string(), self.to_tot(value, depth + 1, walk)?));
                }
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                let mut map = tot::Map::new();
                for (key, value) in entries {
                    map.insert(key, value);
                }
                walk.active.remove(&pointer);
                tot::Value::Object(map)
            }
            _ => {
                return Err(mlua::Error::runtime(
                    "unsupported data value; use data.null for null",
                ));
            }
        })
    }

    fn array_to_tot(
        &self,
        table: &Table,
        depth: usize,
        walk: &mut Walk<'_, '_>,
    ) -> mlua::Result<tot::Value> {
        let pointer = table.to_pointer() as usize;
        if !walk.active.insert(pointer) {
            return Err(mlua::Error::runtime("cyclic data table"));
        }
        let len = table.raw_len();
        walk.budget
            .limit(len > 16_384, "data node limit exceeded")?;
        let mut count = 0;
        for pair in table.pairs::<Value, Value>() {
            let (key, _) = pair?;
            let key = match key {
                Value::Integer(n) => n as f64,
                Value::Number(n) => n,
                _ => f64::NAN,
            };
            if !key.is_finite() || key.fract() != 0.0 || key < 1.0 || key > len as f64 {
                return Err(mlua::Error::runtime(
                    "arrays require dense one-based integer keys",
                ));
            }
            count += 1;
            walk.budget
                .limit(count > 16_384, "data node limit exceeded")?;
        }
        if count != len {
            return Err(mlua::Error::runtime("sparse arrays are not data"));
        }
        let mut out = Vec::with_capacity(len);
        for i in 1..=len {
            out.push(self.to_tot(table.raw_get(i)?, depth + 1, walk)?);
        }
        walk.active.remove(&pointer);
        Ok(tot::Value::Array(out))
    }
}

fn parse_integer(text: &LuaString) -> mlua::Result<tot::Integer> {
    let text = text.to_str()?;
    let digits = text.strip_prefix('-').unwrap_or(&text);
    if digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return Err(mlua::Error::runtime("expected a canonical decimal integer"));
    }
    match tot::parse_value(&text).map_err(mlua::Error::external)? {
        tot::Value::Integer(value) => Ok(value),
        _ => Err(mlua::Error::runtime("expected an integer")),
    }
}

struct Walk<'a, 'b> {
    budget: &'a UtilityBudget<'b>,
    nodes: usize,
    bytes: usize,
    active: HashSet<usize>,
}

impl<'a, 'b> Walk<'a, 'b> {
    fn new(budget: &'a UtilityBudget<'b>) -> Self {
        Self {
            budget,
            nodes: 0,
            bytes: 0,
            active: HashSet::new(),
        }
    }
    fn node(&mut self, depth: usize) -> mlua::Result<()> {
        self.nodes += 1;
        self.budget
            .limit(depth > 64, "data nesting limit exceeded")?;
        self.budget
            .limit(self.nodes > 16_384, "data node limit exceeded")
    }
    fn text(&mut self, text: &str) -> mlua::Result<()> {
        self.bytes += text.len();
        self.budget.bytes(self.bytes)
    }
}
