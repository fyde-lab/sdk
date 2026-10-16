use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, ErrorKind};

/// One typed value of a document's [`crate::Metadata::attributes`]. Serialized
/// adjacently tagged, so `Float(32.3)` round-trips as
/// `{"type": "float", "value": 32.3}`: the type always travels with the
/// value, and can never disagree with it.
///
/// This set of variants, and the fields of each, is a wire format (see
/// `AttributeWire`): it's
/// encrypted into every changelog event, and a client that doesn't know a
/// variant can't decode the metadata carrying it at all. Never remove or
/// change one; add a new variant only knowing every older client fails on
/// documents that use it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(from = "AttributeWire", into = "AttributeWire")]
pub enum Attribute {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    /// A calendar date, as an ISO 8601 `YYYY-MM-DD` string — which sorts
    /// chronologically as plain text.
    Date(String),
    /// A sum of money: `minor_units` of `currency`'s ISO 4217 minor unit
    /// (cents for `"EUR"`, so 2150.50 € is `minor_units: 215050`), never a
    /// float, so it adds up without rounding errors.
    Amount {
        minor_units: i64,
        currency: String,
    },
    /// One value picked among a closed list: `selected` is always one of
    /// `options`, which every value it could be changed to is offered from.
    Select {
        selected: String,
        options: Vec<String>,
    },
}

/// [`Attribute`]'s serialized form. A struct variant's fields can't sit
/// directly in an adjacently tagged enum's `value`: MessagePack encodes them
/// positionally (as an array), which serde can't decode back into a struct
/// variant there. Wrapping them in a struct of their own, as each wire
/// variant below does, decodes the same way any other struct field does.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
enum AttributeWire {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Date(String),
    Amount(AmountWire),
    Select(SelectWire),
}

#[derive(Serialize, Deserialize)]
struct AmountWire {
    minor_units: i64,
    currency: String,
}

#[derive(Serialize, Deserialize)]
struct SelectWire {
    selected: String,
    options: Vec<String>,
}

impl From<AttributeWire> for Attribute {
    fn from(wire: AttributeWire) -> Self {
        match wire {
            AttributeWire::String(value) => Self::String(value),
            AttributeWire::Integer(value) => Self::Integer(value),
            AttributeWire::Float(value) => Self::Float(value),
            AttributeWire::Boolean(value) => Self::Boolean(value),
            AttributeWire::Date(value) => Self::Date(value),
            AttributeWire::Amount(AmountWire {
                minor_units,
                currency,
            }) => Self::Amount {
                minor_units,
                currency,
            },
            AttributeWire::Select(SelectWire { selected, options }) => {
                Self::Select { selected, options }
            }
        }
    }
}

impl From<Attribute> for AttributeWire {
    fn from(attribute: Attribute) -> Self {
        match attribute {
            Attribute::String(value) => Self::String(value),
            Attribute::Integer(value) => Self::Integer(value),
            Attribute::Float(value) => Self::Float(value),
            Attribute::Boolean(value) => Self::Boolean(value),
            Attribute::Date(value) => Self::Date(value),
            Attribute::Amount {
                minor_units,
                currency,
            } => Self::Amount(AmountWire {
                minor_units,
                currency,
            }),
            Attribute::Select { selected, options } => {
                Self::Select(SelectWire { selected, options })
            }
        }
    }
}

impl Attribute {
    pub fn r#type(&self) -> AttributeType {
        match self {
            Self::String(_) => AttributeType::String,
            Self::Integer(_) => AttributeType::Integer,
            Self::Float(_) => AttributeType::Float,
            Self::Boolean(_) => AttributeType::Boolean,
            Self::Date(_) => AttributeType::Date,
            Self::Amount { .. } => AttributeType::Amount,
            Self::Select { .. } => AttributeType::Select,
        }
    }

    /// The name this attribute's type is serialized under (`"float"`...).
    pub fn type_name(&self) -> &'static str {
        self.r#type().as_str()
    }
}

/// The type of an [`Attribute`], without its value — what an
/// [`AttributeDefinition`] expects its values to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttributeType {
    String,
    Integer,
    Float,
    Boolean,
    Date,
    Amount,
    Select,
}

impl AttributeType {
    /// The name [`Attribute`]s of this type are serialized under.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Boolean => "boolean",
            Self::Date => "date",
            Self::Amount => "amount",
            Self::Select => "select",
        }
    }
}

impl fmt::Display for AttributeType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AttributeType {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        [
            Self::String,
            Self::Integer,
            Self::Float,
            Self::Boolean,
            Self::Date,
            Self::Amount,
            Self::Select,
        ]
        .into_iter()
        .find(|r#type| r#type.as_str() == s)
        .ok_or_else(|| ErrorKind::InvalidAttribute(format!("unknown attribute type {s:?}")).into())
    }
}

/// The definition of an attribute documents can carry: the name its values
/// are stored under, the type they must have, and what it means. Default
/// definitions are shared by every user; the others belong to the user who
/// defined them (see [`super::Service::set`]). Kept on the fyde server, in
/// clear — unlike attribute values, which only ever travel inside a
/// document's encrypted metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeDefinition {
    pub(super) id: Uuid,
    pub(super) name: String,
    pub(super) r#type: AttributeType,
    pub(super) description: String,
    pub(super) is_default: bool,
}

impl AttributeDefinition {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn r#type(&self) -> AttributeType {
        self.r#type
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    /// Whether this is a default definition, shared by every user, rather
    /// than one the user defined.
    pub fn is_default(&self) -> bool {
        self.is_default
    }
}

/// Parameters for [`super::Service::set`].
#[derive(Debug, Clone, PartialEq)]
pub struct SetAttributeRequest {
    pub name: String,
    pub r#type: AttributeType,
    pub description: String,
}

/// Builds an [`AttributeDefinition`] filled with random-but-plausible data,
/// overridable field by field, for use in tests. Defaults to one of the
/// user's own.
#[cfg(test)]
pub(crate) struct FakeAttributeDefinition {
    definition: AttributeDefinition,
}

#[cfg(test)]
impl FakeAttributeDefinition {
    pub(crate) fn new() -> Self {
        Self {
            definition: AttributeDefinition {
                id: Uuid::new_v4(),
                name: format!("attribute_{}", crate::tools::testing::random_hex(8)),
                r#type: AttributeType::String,
                description: crate::tools::testing::random_word().to_string(),
                is_default: false,
            },
        }
    }

    pub(crate) fn with_id(mut self, id: Uuid) -> Self {
        self.definition.id = id;
        self
    }

    pub(crate) fn with_name(mut self, name: &str) -> Self {
        self.definition.name = name.to_string();
        self
    }

    pub(crate) fn with_type(mut self, r#type: AttributeType) -> Self {
        self.definition.r#type = r#type;
        self
    }

    pub(crate) fn with_description(mut self, description: &str) -> Self {
        self.definition.description = description.to_string();
        self
    }

    pub(crate) fn with_is_default(mut self, is_default: bool) -> Self {
        self.definition.is_default = is_default;
        self
    }

    pub(crate) fn build(self) -> AttributeDefinition {
        self.definition
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn attribute_types_round_trip_through_their_name() {
        for name in [
            "string", "integer", "float", "boolean", "date", "amount", "select",
        ] {
            assert_eq!(AttributeType::from_str(name).unwrap().as_str(), name);
        }
        assert_eq!(
            AttributeType::from_str("money").unwrap_err().code(),
            crate::ErrorCode::InvalidAttribute
        );
    }

    #[test]
    fn with_methods_override_defaults() {
        let id = Uuid::new_v4();

        let definition = FakeAttributeDefinition::new()
            .with_id(id)
            .with_name("net_pay")
            .with_type(AttributeType::Amount)
            .with_description("The pay")
            .with_is_default(true)
            .build();

        assert_eq!(
            definition,
            AttributeDefinition {
                id,
                name: "net_pay".to_string(),
                r#type: AttributeType::Amount,
                description: "The pay".to_string(),
                is_default: true,
            }
        );
    }

    #[test]
    fn attribute_serializes_its_type_alongside_its_value() {
        let attributes = BTreeMap::from([
            ("price".to_string(), Attribute::Float(32.3)),
            ("paid".to_string(), Attribute::Boolean(true)),
        ]);

        let json = serde_json::to_value(&attributes).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "price": {"type": "float", "value": 32.3},
                "paid": {"type": "boolean", "value": true},
            })
        );
        assert_eq!(
            serde_json::from_value::<BTreeMap<String, Attribute>>(json).unwrap(),
            attributes
        );
    }

    #[test]
    fn date_amount_and_select_attributes_serialize_their_value_as_json() {
        let attributes = BTreeMap::from([
            (
                "issued_on".to_string(),
                Attribute::Date("2026-10-15".to_string()),
            ),
            (
                "total_amount".to_string(),
                Attribute::Amount {
                    minor_units: 3230,
                    currency: "EUR".to_string(),
                },
            ),
            (
                "status".to_string(),
                Attribute::Select {
                    selected: "paid".to_string(),
                    options: vec!["paid".to_string(), "unpaid".to_string()],
                },
            ),
        ]);

        let json = serde_json::to_value(&attributes).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "issued_on": {"type": "date", "value": "2026-10-15"},
                "total_amount": {"type": "amount", "value": {"minor_units": 3230, "currency": "EUR"}},
                "status": {"type": "select", "value": {"selected": "paid", "options": ["paid", "unpaid"]}},
            })
        );
        assert_eq!(
            serde_json::from_value::<BTreeMap<String, Attribute>>(json).unwrap(),
            attributes
        );
    }
}
