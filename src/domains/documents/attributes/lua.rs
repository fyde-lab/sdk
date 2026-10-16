//! Converts attributes to and from the Lua tables classification and
//! scraper scripts read and write them as — an adapter between the Lua VMs
//! (`parser/vm.rs`, `scrapers/host/documents.rs`) and [`Attribute`], with no
//! business rules: a value that converts is checked by [`super::Service`]
//! like any other.

use std::collections::BTreeMap;

use mlua::Lua;

use super::Attribute;
use crate::{ErrorContext as _, ErrorKind, Result};

/// Mirrors `attributes` as a Lua table of `{ type = ..., value = ... }`
/// entries keyed by name — the same shape [`Attribute`] serializes to — so
/// a script adds one with e.g.
/// `document.metadata.attributes.price = { type = "float", value = 32.3 }`.
/// A date's value is its `"YYYY-MM-DD"` string, an amount's a
/// `{ minor_units = 215050, currency = "EUR" }` table, and a select's a
/// `{ selected = "paid", options = { "paid", "unpaid" } }` table.
pub(crate) fn attributes_to_table(
    lua: &Lua,
    attributes: &BTreeMap<String, Attribute>,
) -> Result<mlua::Table> {
    let attributes_table = lua
        .create_table()
        .context("failed to create the document.metadata.attributes lua table")?;
    for (name, attribute) in attributes {
        let attribute_table = lua
            .create_table()
            .with_context(|| format!("failed to create the lua table for attribute {name:?}"))?;
        attribute_table
            .set("type", attribute.type_name())
            .with_context(|| format!("failed to set the type of attribute {name:?}"))?;
        let value = match attribute {
            Attribute::String(value) | Attribute::Date(value) => {
                attribute_table.set("value", value.as_str())
            }
            Attribute::Integer(value) => attribute_table.set("value", *value),
            Attribute::Float(value) => attribute_table.set("value", *value),
            Attribute::Boolean(value) => attribute_table.set("value", *value),
            Attribute::Amount {
                minor_units,
                currency,
            } => lua
                .create_table_from([
                    ("minor_units", mlua::Value::Integer(*minor_units)),
                    (
                        "currency",
                        mlua::Value::String(lua.create_string(currency)?),
                    ),
                ])
                .and_then(|value| attribute_table.set("value", value)),
            Attribute::Select { selected, options } => lua
                .create_sequence_from(options.iter().map(String::as_str))
                .and_then(|options| {
                    lua.create_table_from([
                        (
                            "selected",
                            mlua::Value::String(lua.create_string(selected)?),
                        ),
                        ("options", mlua::Value::Table(options)),
                    ])
                })
                .and_then(|value| attribute_table.set("value", value)),
        };
        value.with_context(|| format!("failed to set the value of attribute {name:?}"))?;
        attributes_table
            .set(name.as_str(), attribute_table)
            .with_context(|| format!("failed to set attribute {name:?}"))?;
    }

    Ok(attributes_table)
}

/// Reads `document.metadata.attributes` back out of the table
/// [`attributes_to_table`] wrote, validating every entry a script added or
/// changed: an unknown `type`, or a `value` that doesn't fit it, fails with
/// [`ErrorKind::InvalidAttribute`]. A script that set the whole table to
/// `nil` cleared every attribute. Also parses `fyde.save_document`'s
/// `attributes` field, which takes the same shape.
pub(crate) fn attributes_from_table(
    attributes_table: Option<mlua::Table>,
) -> Result<BTreeMap<String, Attribute>> {
    let Some(attributes_table) = attributes_table else {
        return Ok(BTreeMap::new());
    };

    let mut attributes = BTreeMap::new();
    for pair in attributes_table.pairs::<String, mlua::Table>() {
        let (name, attribute_table) =
            pair.context("script left an invalid entry in document.metadata.attributes")?;
        let r#type: String = attribute_table
            .get("type")
            .with_context(|| format!("failed to read the type of attribute {name:?} back"))?;
        let value: mlua::Value = attribute_table
            .get("value")
            .with_context(|| format!("failed to read the value of attribute {name:?} back"))?;
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
        .with_context(|| format!("failed to read the minor_units of attribute {name:?} back"))?
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
        .with_context(|| format!("failed to read the currency of attribute {name:?} back"))?
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
        .with_context(|| format!("failed to read the selected value of attribute {name:?} back"))?
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
        .with_context(|| format!("failed to read the options of attribute {name:?} back"))?
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
        match option
            .with_context(|| format!("failed to read an option of attribute {name:?} back"))?
        {
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
