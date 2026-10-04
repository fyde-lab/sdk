use mlua::{Lua, LuaSerdeExt, Result as LuaResult, Table, Value};

/// `fyde.json.decode(text)` turns a JSON string into Lua tables/values, and
/// `fyde.json.encode(value)` turns Lua tables/values back into a JSON
/// string. Ported from `demo-rust-fyde`'s `host/json.rs` (minus its debug
/// report).
pub(super) fn table(lua: &Lua) -> LuaResult<Table> {
    let json = lua.create_table()?;

    json.set(
        "decode",
        lua.create_function(|lua, text: String| {
            let parsed: serde_json::Value =
                serde_json::from_str(&text).map_err(mlua::Error::external)?;
            lua.to_value(&parsed)
        })?,
    )?;

    json.set(
        "encode",
        lua.create_function(|lua, value: Value| {
            let as_json: serde_json::Value = lua.from_value(value)?;
            serde_json::to_string(&as_json).map_err(mlua::Error::external)
        })?,
    )?;

    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_turns_a_json_string_into_a_lua_table() {
        let lua = Lua::new();
        lua.globals().set("json", table(&lua).unwrap()).unwrap();

        let result: Table = lua
            .load(r#"return json.decode('{"a": 1, "b": "two"}')"#)
            .eval()
            .unwrap();

        assert_eq!(result.get::<i64>("a").unwrap(), 1);
        assert_eq!(result.get::<String>("b").unwrap(), "two");
    }

    #[test]
    fn decode_rejects_invalid_json() {
        let lua = Lua::new();
        lua.globals().set("json", table(&lua).unwrap()).unwrap();

        let result: LuaResult<Value> = lua.load(r#"return json.decode("not json")"#).eval();

        assert!(result.is_err());
    }

    #[test]
    fn encode_turns_a_lua_table_into_a_json_string() {
        let lua = Lua::new();
        lua.globals().set("json", table(&lua).unwrap()).unwrap();

        let result: String = lua.load(r#"return json.encode({a = 1})"#).eval().unwrap();

        assert_eq!(result, r#"{"a":1}"#);
    }

    #[test]
    fn encode_then_decode_round_trips() {
        let lua = Lua::new();
        lua.globals().set("json", table(&lua).unwrap()).unwrap();

        let result: Table = lua
            .load(r#"return json.decode(json.encode({a = 1, b = "two"}))"#)
            .eval()
            .unwrap();

        assert_eq!(result.get::<i64>("a").unwrap(), 1);
        assert_eq!(result.get::<String>("b").unwrap(), "two");
    }
}
