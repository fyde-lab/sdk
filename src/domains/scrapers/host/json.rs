use mlua::{Lua, LuaSerdeExt, Result as LuaResult, Table, Value, serde::SerializeOptions};

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
            // mlua defaults to representing a JSON `null` as a special null
            // "userdata" sentinel distinct from Lua `nil` (so an encode
            // round-trip can tell an explicit null apart from a missing
            // key). Scripts test decoded fields with the ordinary Lua idiom
            // `if value then`, which that sentinel breaks: it's truthy, since
            // only `nil`/`false` are falsy in Lua. Decode `null` as real
            // `nil` instead so those checks see an absent field, matching
            // Lua convention.
            let options = SerializeOptions::new()
                .serialize_none_to_null(false)
                .serialize_unit_to_null(false);
            lua.to_value_with(&parsed, options)
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
    fn decode_turns_a_json_null_into_lua_nil_not_a_truthy_sentinel() {
        let lua = Lua::new();
        lua.globals().set("json", table(&lua).unwrap()).unwrap();

        let result: bool = lua
            .load(
                r#"
                local decoded = json.decode('{"a": null}')
                return decoded.a == nil and not decoded.a
                "#,
            )
            .eval()
            .unwrap();

        assert!(result);
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
