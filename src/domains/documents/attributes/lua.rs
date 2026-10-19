//! Converts attributes from the Lua tables scraper scripts write them as —
//! an adapter between the Lua VM (`scrapers/host/documents.rs`) and
//! [`Attribute`], with no business rules: a value that converts is checked
//! by [`super::Service`] like any other.

use std::collections::BTreeMap;

use super::Attribute;
use crate::{ErrorContext as _, ErrorKind, Result};

/// Parses `fyde.save_document`'s `attributes` field: a Lua table of
/// `{ type = ..., value = ... }` entries keyed by name — the same shape
/// [`Attribute`] serializes to, e.g. `{ type = "float", value = 32.3 }`. A
/// date's value is its `"YYYY-MM-DD"` string, an amount's a
/// `{ minor_units = 215050, currency = "EUR" }` table, and a select's a
/// `{ selected = "paid", options = { "paid", "unpaid" } }` table. An
/// unknown `type`, or a `value` that doesn't fit it, fails with
/// [`ErrorKind::InvalidAttribute`]; no table at all means no attributes.
pub(crate) fn attributes_from_table(
    attributes_table: Option<mlua::Table>,
) -> Result<BTreeMap<String, Attribute>> {
    let Some(attributes_table) = attributes_table else {
        return Ok(BTreeMap::new());
    };

    let mut attributes = BTreeMap::new();
    for pair in attributes_table.pairs::<String, mlua::Table>() {
        let (name, attribute_table) =
            pair.context("script passed an invalid entry in a document's attributes")?;
        let r#type: String = attribute_table
            .get("type")
            .with_context(|| format!("failed to read the type of attribute {name:?}"))?;
        let value: mlua::Value = attribute_table
            .get("value")
            .with_context(|| format!("failed to read the value of attribute {name:?}"))?;
        let attribute = match (r#type.as_str(), value) {
            ("string", mlua::Value::String(value)) => {
                Attribute::String(lua_to_string(&value, &name, "value")?)
            }
            ("date", mlua::Value::String(value)) => {
                Attribute::Date(lua_to_string(&value, &name, "value")?)
            }
            ("integer", mlua::Value::Integer(value)) => Attribute::Integer(value),
            ("float", mlua::Value::Number(value)) => Attribute::Float(value),
            ("float", mlua::Value::Integer(value)) => Attribute::Float(value as f64),
            ("boolean", mlua::Value::Boolean(value)) => Attribute::Boolean(value),
            ("amount", mlua::Value::Table(value)) => amount_from_table(&value, &name)?,
            ("select", mlua::Value::Table(value)) => select_from_table(&value, &name)?,
            ("string" | "date" | "integer" | "float" | "boolean" | "amount" | "select", value) => {
                return Err(ErrorKind::InvalidAttribute(format!(
                    "attribute {name:?} has a {} value, not a valid {type}",
                    value.type_name()
                ))
                .into());
            }
            (other, _) => {
                return Err(ErrorKind::InvalidAttribute(format!(
                    "attribute {name:?} has unknown type {other:?}"
                ))
                .into());
            }
        };
        attributes.insert(name, attribute);
    }

    Ok(attributes)
}

/// Reads an amount attribute's `{ minor_units = ..., currency = ... }`
/// value: `minor_units` must be a Lua integer (`math.tointeger` turns a
/// computed float like `21.5 * 100` into one), never a fractional number.
fn amount_from_table(value: &mlua::Table, name: &str) -> Result<Attribute> {
    let minor_units = match value
        .get::<mlua::Value>("minor_units")
        .with_context(|| format!("failed to read the minor_units of attribute {name:?}"))?
    {
        mlua::Value::Integer(minor_units) => minor_units,
        other => {
            return Err(ErrorKind::InvalidAttribute(format!(
                "attribute {name:?} has a {} minor_units, not an integer",
                other.type_name()
            ))
            .into());
        }
    };
    let currency = match value
        .get::<mlua::Value>("currency")
        .with_context(|| format!("failed to read the currency of attribute {name:?}"))?
    {
        mlua::Value::String(currency) => lua_to_string(&currency, name, "currency")?,
        other => {
            return Err(ErrorKind::InvalidAttribute(format!(
                "attribute {name:?} has a {} currency, not a string",
                other.type_name()
            ))
            .into());
        }
    };

    Ok(Attribute::Amount {
        minor_units,
        currency,
    })
}

/// Reads a select attribute's `{ selected = ..., options = { ... } }` value:
/// `selected` must be a string, and `options` a sequence of strings.
fn select_from_table(value: &mlua::Table, name: &str) -> Result<Attribute> {
    let selected = match value
        .get::<mlua::Value>("selected")
        .with_context(|| format!("failed to read the selected value of attribute {name:?}"))?
    {
        mlua::Value::String(selected) => lua_to_string(&selected, name, "selected value")?,
        other => {
            return Err(ErrorKind::InvalidAttribute(format!(
                "attribute {name:?} has a {} selected value, not a string",
                other.type_name()
            ))
            .into());
        }
    };
    let options_table = match value
        .get::<mlua::Value>("options")
        .with_context(|| format!("failed to read the options of attribute {name:?}"))?
    {
        mlua::Value::Table(options) => options,
        other => {
            return Err(ErrorKind::InvalidAttribute(format!(
                "attribute {name:?} has {} options, not a list of strings",
                other.type_name()
            ))
            .into());
        }
    };
    let mut options = Vec::new();
    for option in options_table.sequence_values::<mlua::Value>() {
        match option.with_context(|| format!("failed to read an option of attribute {name:?}"))? {
            mlua::Value::String(option) => options.push(lua_to_string(&option, name, "option")?),
            other => {
                return Err(ErrorKind::InvalidAttribute(format!(
                    "attribute {name:?} has a {} option, not a string",
                    other.type_name()
                ))
                .into());
            }
        }
    }

    Ok(Attribute::Select { selected, options })
}

/// Converts `value`, the `what` of attribute `name`, to a Rust string,
/// failing if it isn't valid UTF-8.
fn lua_to_string(value: &mlua::LuaString, name: &str, what: &str) -> Result<String> {
    Ok(value
        .to_str()
        .with_context(|| format!("attribute {name:?} has a non-UTF-8 {what}"))?
        .to_string())
}

#[cfg(test)]
mod tests {
    use mlua::Lua;

    use super::*;

    /// Evaluates the Lua table expression `table` and parses it with
    /// [`attributes_from_table`].
    fn parse(table: &str) -> Result<BTreeMap<String, Attribute>> {
        let lua = Lua::new();
        let table: mlua::Table = lua.load(table).eval().unwrap();
        attributes_from_table(Some(table))
    }

    #[test]
    fn attributes_from_table_parses_every_attribute_type() {
        let attributes = parse(
            r#"{
                paid = { type = "boolean", value = true },
                price = { type = "float", value = 32.3 },
                quantity = { type = "integer", value = 2 },
                reference = { type = "string", value = "F-42" },
                due_on = { type = "date", value = "2026-11-01" },
                total_amount = {
                    type = "amount",
                    value = { minor_units = math.floor(32.3 * 100 + 0.5), currency = "EUR" },
                },
                status = {
                    type = "select",
                    value = { selected = "unpaid", options = { "paid", "unpaid" } },
                },
            }"#,
        )
        .unwrap();

        assert_eq!(
            attributes,
            BTreeMap::from([
                (
                    "due_on".to_string(),
                    Attribute::Date("2026-11-01".to_string())
                ),
                ("paid".to_string(), Attribute::Boolean(true)),
                ("price".to_string(), Attribute::Float(32.3)),
                ("quantity".to_string(), Attribute::Integer(2)),
                (
                    "reference".to_string(),
                    Attribute::String("F-42".to_string())
                ),
                (
                    "status".to_string(),
                    Attribute::Select {
                        selected: "unpaid".to_string(),
                        options: vec!["paid".to_string(), "unpaid".to_string()],
                    }
                ),
                (
                    "total_amount".to_string(),
                    Attribute::Amount {
                        minor_units: 3230,
                        currency: "EUR".to_string(),
                    }
                ),
            ])
        );
    }

    #[test]
    fn attributes_from_table_returns_no_attributes_without_a_table() {
        assert!(attributes_from_table(None).unwrap().is_empty());
    }

    #[test]
    fn attributes_from_table_rejects_malformed_attributes() {
        for table in [
            r#"{ price = { type = "money", value = 32.3 } }"#,
            r#"{ paid = { type = "boolean", value = "yes" } }"#,
            r#"{ price = { type = "amount", value = 32.3 } }"#,
            r#"{ price = { type = "amount", value = { minor_units = 32.3, currency = "EUR" } } }"#,
            r#"{ price = { type = "amount", value = { minor_units = 3230 } } }"#,
            r#"{ status = { type = "select", value = { selected = 1, options = { "1" } } } }"#,
            r#"{ status = { type = "select", value = { selected = "1", options = { 1 } } } }"#,
            r#"{ status = { type = "select", value = { selected = "1", options = "1" } } }"#,
            r#"{ on = { type = "date", value = 20261015 } }"#,
        ] {
            let err = parse(table).unwrap_err();

            assert_eq!(err.code(), crate::ErrorCode::InvalidAttribute, "{table}");
        }
    }
}
