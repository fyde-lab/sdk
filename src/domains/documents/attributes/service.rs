use std::collections::BTreeMap;

use async_trait::async_trait;

use crate::{ErrorContext as _, ErrorKind, Result};

use super::grpc_client::FydeClient;
use super::{Attribute, AttributeDefinition, Service, SetAttributeRequest};

/// The longest an attribute definition's description can be, in bytes —
/// the same bound the server enforces.
const MAX_DESCRIPTION_LEN: usize = 1024;

/// The default [`Service`] implementation: definitions come from the fyde
/// server through an injected [`FydeClient`]; checking values against them
/// happens here.
pub(super) struct AttributesClient<C: FydeClient> {
    client: C,
}

impl<C: FydeClient> AttributesClient<C> {
    pub(super) fn new(client: C) -> Self {
        Self { client }
    }
}

#[async_trait]
impl<C: FydeClient> Service for AttributesClient<C> {
    async fn list(&self) -> Result<Vec<AttributeDefinition>> {
        self.client
            .list_attributes()
            .await
            .context("failed to list attribute definitions")
    }

    async fn set(&self, request: SetAttributeRequest) -> Result<AttributeDefinition> {
        // Checked here too, so an invalid request fails with
        // `InvalidAttribute` before reaching the server.
        check_attribute_name(&request.name)?;
        if request.description.len() > MAX_DESCRIPTION_LEN {
            return Err(ErrorKind::InvalidAttribute(format!(
                "the description of attribute {:?} is longer than {MAX_DESCRIPTION_LEN} bytes",
                request.name
            ))
            .into());
        }

        let name = request.name.clone();
        self.client
            .set_attribute(request)
            .await
            .with_context(|| format!("failed to set attribute definition {name:?}"))
    }

    async fn check(&self, attributes: &BTreeMap<String, Attribute>) -> Result<()> {
        check_values(attributes)?;
        // Nothing to look up: spares a round trip for the many documents
        // carrying no attribute at all.
        if attributes.is_empty() {
            return Ok(());
        }

        let definitions = self
            .list()
            .await
            .context("failed to fetch the attribute definitions to check attributes against")?;
        check_against_definitions(attributes, &definitions)
    }
}

/// Rejects an attribute whose type isn't the one its definition, if any,
/// expects. An attribute nothing defines is accepted as is.
fn check_against_definitions(
    attributes: &BTreeMap<String, Attribute>,
    definitions: &[AttributeDefinition],
) -> Result<()> {
    for (name, attribute) in attributes {
        if let Some(definition) = definitions
            .iter()
            .find(|definition| definition.name() == name)
            && definition.r#type() != attribute.r#type()
        {
            return Err(ErrorKind::InvalidAttribute(format!(
                "attribute {name:?} is a {}, but is defined as a {}",
                attribute.r#type(),
                definition.r#type()
            ))
            .into());
        }
    }

    Ok(())
}

/// The most attributes a single document can carry: every one of them is
/// re-encrypted into each of the document's changelog events, and cached on
/// every device of the account.
const MAX_ATTRIBUTES: usize = 64;
/// The longest an attribute name can be, in bytes.
const MAX_ATTRIBUTE_NAME_LEN: usize = 64;
/// The longest a string, or select option, attribute value can be, in bytes.
const MAX_ATTRIBUTE_TEXT_LEN: usize = 1024;
/// The most options a select attribute can offer.
const MAX_SELECT_OPTIONS: usize = 64;

/// Rejects attributes that are too many, or one that isn't valid on its
/// own, whatever it's defined as: see [`check_attribute_name`] and
/// [`check_attribute_value`]. Needs no server, unlike
/// [`Service::check`](super::Service::check), which also checks them
/// against the user's definitions — so the dev documents service, which has
/// none, still rejects what can't ever be valid.
pub(crate) fn check_values(attributes: &BTreeMap<String, Attribute>) -> Result<()> {
    if attributes.len() > MAX_ATTRIBUTES {
        return Err(ErrorKind::InvalidAttribute(format!(
            "a document can't have more than {MAX_ATTRIBUTES} attributes, got {}",
            attributes.len()
        ))
        .into());
    }
    for (name, attribute) in attributes {
        check_attribute_name(name)?;
        check_attribute_value(name, attribute)?;
    }

    Ok(())
}

/// Rejects a name that isn't `snake_case` ASCII (a lowercase letter, then
/// lowercase letters, digits or underscores), or is longer than
/// [`MAX_ATTRIBUTE_NAME_LEN`].
fn check_attribute_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let is_snake_case = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !is_snake_case {
        return Err(ErrorKind::InvalidAttribute(format!(
            "attribute name {name:?} isn't snake_case (a lowercase letter, then lowercase letters, digits or underscores)"
        ))
        .into());
    }
    if name.len() > MAX_ATTRIBUTE_NAME_LEN {
        return Err(ErrorKind::InvalidAttribute(format!(
            "attribute name {name:?} is longer than {MAX_ATTRIBUTE_NAME_LEN} bytes"
        ))
        .into());
    }

    Ok(())
}

/// Rejects an attribute whose value doesn't fit its type:
/// - a float that isn't finite (`NaN`/infinity have no JSON representation,
///   so couldn't be cached),
/// - a string, or select option, longer than [`MAX_ATTRIBUTE_TEXT_LEN`],
/// - a date that isn't a real `YYYY-MM-DD` calendar date,
/// - an amount whose currency isn't an ISO 4217 code (three uppercase
///   letters),
/// - a select with no options, more than [`MAX_SELECT_OPTIONS`], a blank or
///   duplicate one, or a `selected` value that isn't one of them.
fn check_attribute_value(name: &str, attribute: &Attribute) -> Result<()> {
    let invalid = |reason: String| -> Result<()> {
        Err(ErrorKind::InvalidAttribute(format!("attribute {name:?} {reason}")).into())
    };

    match attribute {
        Attribute::Integer(_) | Attribute::Boolean(_) => {}
        Attribute::Float(value) => {
            if !value.is_finite() {
                return invalid(format!("has a non-finite value {value}"));
            }
        }
        Attribute::String(value) => {
            if value.len() > MAX_ATTRIBUTE_TEXT_LEN {
                return invalid(format!("is longer than {MAX_ATTRIBUTE_TEXT_LEN} bytes"));
            }
        }
        Attribute::Date(value) => {
            if !is_calendar_date(value) {
                return invalid(format!("has {value:?}, not a YYYY-MM-DD calendar date"));
            }
        }
        Attribute::Amount { currency, .. } => {
            if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_uppercase()) {
                return invalid(format!(
                    "has currency {currency:?}, not an ISO 4217 code like \"EUR\""
                ));
            }
        }
        Attribute::Select { selected, options } => {
            if options.is_empty() {
                return invalid("is a select without any option".to_string());
            }
            if options.len() > MAX_SELECT_OPTIONS {
                return invalid(format!(
                    "has more than {MAX_SELECT_OPTIONS} options, got {}",
                    options.len()
                ));
            }
            for (i, option) in options.iter().enumerate() {
                if option.trim().is_empty() {
                    return invalid("has a blank option".to_string());
                }
                if option.len() > MAX_ATTRIBUTE_TEXT_LEN {
                    return invalid(format!(
                        "has an option longer than {MAX_ATTRIBUTE_TEXT_LEN} bytes"
                    ));
                }
                if options[..i].contains(option) {
                    return invalid(format!("has option {option:?} more than once"));
                }
            }
            if !options.contains(selected) {
                return invalid(format!(
                    "has {selected:?} selected, which isn't one of its options"
                ));
            }
        }
    }

    Ok(())
}

/// Whether `value` is an ISO 8601 calendar date, `YYYY-MM-DD`, of a year
/// from 1 to 9999 — a day that actually exists, leap years included.
fn is_calendar_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    let digits_at = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !digits_at(0..4)
        || !digits_at(5..7)
        || !digits_at(8..10)
    {
        return false;
    }
    // Only ASCII digits from here on, so these can't fail.
    let year: u32 = value[0..4].parse().unwrap_or_default();
    let month: u32 = value[5..7].parse().unwrap_or_default();
    let day: u32 = value[8..10].parse().unwrap_or_default();

    let is_leap_year =
        (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year => 29,
        2 => 28,
        _ => return false,
    };

    year >= 1 && (1..=days_in_month).contains(&day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::documents::attributes::grpc_client::MockFydeClient;
    use crate::domains::documents::attributes::{AttributeType, FakeAttributeDefinition};

    fn check_one(name: &str, attribute: Attribute) -> Result<()> {
        check_values(&BTreeMap::from([(name.to_string(), attribute)]))
    }

    fn select(selected: &str, options: &[&str]) -> Attribute {
        Attribute::Select {
            selected: selected.to_string(),
            options: options.iter().map(|option| option.to_string()).collect(),
        }
    }

    fn assert_invalid(result: Result<()>) {
        assert_eq!(
            result.unwrap_err().code(),
            crate::ErrorCode::InvalidAttribute
        );
    }

    #[test]
    fn check_values_accepts_one_valid_attribute_of_every_type() {
        let attributes = BTreeMap::from([
            (
                "reference".to_string(),
                Attribute::String("F-42".to_string()),
            ),
            ("tax_year".to_string(), Attribute::Integer(2025)),
            ("ratio_2".to_string(), Attribute::Float(0.5)),
            ("paid".to_string(), Attribute::Boolean(true)),
            (
                "issued_on".to_string(),
                Attribute::Date("2024-02-29".to_string()),
            ),
            (
                "net_pay".to_string(),
                Attribute::Amount {
                    minor_units: 215050,
                    currency: "EUR".to_string(),
                },
            ),
            ("status".to_string(), select("paid", &["paid", "unpaid"])),
        ]);

        check_values(&attributes).unwrap();
    }

    #[test]
    fn check_values_rejects_a_name_that_isnt_snake_case() {
        for name in ["", " ", "Price", "net-pay", "2nd", "_price", "prix_été"] {
            assert_invalid(check_one(name, Attribute::Integer(1)));
        }
    }

    #[test]
    fn check_values_rejects_a_name_longer_than_the_limit() {
        assert_invalid(check_one(
            &"a".repeat(MAX_ATTRIBUTE_NAME_LEN + 1),
            Attribute::Integer(1),
        ));
    }

    #[test]
    fn check_values_rejects_more_attributes_than_the_limit() {
        let attributes = (0..=MAX_ATTRIBUTES)
            .map(|i| (format!("attribute_{i}"), Attribute::Integer(1)))
            .collect();

        assert_invalid(check_values(&attributes));
    }

    #[test]
    fn check_values_rejects_a_string_longer_than_the_limit() {
        assert_invalid(check_one(
            "note",
            Attribute::String("a".repeat(MAX_ATTRIBUTE_TEXT_LEN + 1)),
        ));
    }

    #[test]
    fn check_values_rejects_a_date_that_isnt_a_calendar_day() {
        for date in [
            "2026-02-29",
            "2026-13-01",
            "2026-04-31",
            "2026-00-10",
            "0000-01-01",
            "2026-1-15",
            "15/10/2026",
            "2026-10-15T00:00:00Z",
        ] {
            assert_invalid(check_one("on", Attribute::Date(date.to_string())));
        }
    }

    #[test]
    fn check_values_rejects_an_amount_whose_currency_isnt_an_iso_4217_code() {
        for currency in ["", "eur", "EURO", "€", "E1R"] {
            assert_invalid(check_one(
                "price",
                Attribute::Amount {
                    minor_units: 100,
                    currency: currency.to_string(),
                },
            ));
        }
    }

    #[test]
    fn check_values_rejects_an_invalid_select() {
        assert_invalid(check_one("status", select("paid", &[])));
        assert_invalid(check_one("status", select("paid", &["unpaid"])));
        assert_invalid(check_one("status", select("paid", &["paid", "paid"])));
        assert_invalid(check_one("status", select("paid", &["paid", " "])));
        let options: Vec<String> = (0..=MAX_SELECT_OPTIONS).map(|i| format!("o{i}")).collect();
        let options: Vec<&str> = options.iter().map(String::as_str).collect();
        assert_invalid(check_one("status", select("o0", &options)));
    }

    fn net_pay_definition() -> AttributeDefinition {
        FakeAttributeDefinition::new()
            .with_name("net_pay")
            .with_type(AttributeType::Amount)
            .with_is_default(true)
            .build()
    }

    fn amount() -> Attribute {
        Attribute::Amount {
            minor_units: 215050,
            currency: "EUR".to_string(),
        }
    }

    #[tokio::test]
    async fn check_accepts_attributes_matching_their_definition_or_undefined() {
        let mut client = MockFydeClient::new();
        client
            .expect_list_attributes()
            .times(1)
            .return_once(|| Ok(vec![net_pay_definition()]));
        let attributes = BTreeMap::from([
            ("net_pay".to_string(), amount()),
            (
                "free_text".to_string(),
                Attribute::String("anything".to_string()),
            ),
        ]);

        AttributesClient::new(client)
            .check(&attributes)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn check_rejects_an_attribute_of_another_type_than_its_definition() {
        let mut client = MockFydeClient::new();
        client
            .expect_list_attributes()
            .times(1)
            .return_once(|| Ok(vec![net_pay_definition()]));
        let attributes = BTreeMap::from([("net_pay".to_string(), Attribute::Float(2150.5))]);

        let err = AttributesClient::new(client)
            .check(&attributes)
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidAttribute);
    }

    #[tokio::test]
    async fn check_rejects_an_invalid_value_without_listing_definitions() {
        // No `expect_list_attributes()`: the server must never be reached.
        let attributes = BTreeMap::from([("Net Pay".to_string(), amount())]);

        let err = AttributesClient::new(MockFydeClient::new())
            .check(&attributes)
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidAttribute);
    }

    #[tokio::test]
    async fn check_never_lists_definitions_for_no_attributes() {
        // No `expect_list_attributes()`: the server must never be reached.
        AttributesClient::new(MockFydeClient::new())
            .check(&BTreeMap::new())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn list_returns_the_servers_definitions() {
        let definitions = vec![net_pay_definition(), FakeAttributeDefinition::new().build()];
        let returned = definitions.clone();
        let mut client = MockFydeClient::new();
        client
            .expect_list_attributes()
            .times(1)
            .return_once(move || Ok(returned));

        let listed = AttributesClient::new(client).list().await.unwrap();

        assert_eq!(listed, definitions);
    }

    #[tokio::test]
    async fn set_sends_a_valid_definition_to_the_server() {
        let request = SetAttributeRequest {
            name: "warranty_end".to_string(),
            r#type: AttributeType::Date,
            description: "When the warranty ends".to_string(),
        };
        let definition = FakeAttributeDefinition::new()
            .with_name("warranty_end")
            .with_type(AttributeType::Date)
            .build();
        let returned = definition.clone();
        let mut client = MockFydeClient::new();
        client
            .expect_set_attribute()
            .withf({
                let request = request.clone();
                move |sent| *sent == request
            })
            .times(1)
            .return_once(move |_| Ok(returned));

        let set = AttributesClient::new(client).set(request).await.unwrap();

        assert_eq!(set, definition);
    }

    #[tokio::test]
    async fn set_rejects_an_invalid_name_without_reaching_the_server() {
        // No `expect_set_attribute()`: the server must never be reached.
        let err = AttributesClient::new(MockFydeClient::new())
            .set(SetAttributeRequest {
                name: "Warranty End".to_string(),
                r#type: AttributeType::Date,
                description: String::new(),
            })
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::InvalidAttribute);
    }
}
