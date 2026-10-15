//! The registry of well-known [`super::Metadata::attributes`]: names every
//! script and client should use for the same piece of data, each with the
//! [`AttributeType`] its values must have. An attribute under one of these
//! names but of another type is rejected on upload and metadata update
//! (`service::check_attributes`); any other name is free for whatever a
//! single script or document type needs.
//!
//! Grouped below by the document types they're mostly found on, though none
//! is restricted to them: a `reference` makes as much sense on a contract as
//! on an invoice.

use super::AttributeType;

/// One entry of the registry: an attribute name, the type its values must
/// have, and what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WellKnownAttribute {
    pub name: &'static str,
    pub r#type: AttributeType,
    pub description: &'static str,
}

// Any document.

pub const ISSUED_ON: WellKnownAttribute = WellKnownAttribute {
    name: "issued_on",
    r#type: AttributeType::Date,
    description: "The date the document was issued on, as printed on it.",
};

pub const ISSUER: WellKnownAttribute = WellKnownAttribute {
    name: "issuer",
    r#type: AttributeType::String,
    description: "The name of the organization or person who issued the document.",
};

pub const REFERENCE: WellKnownAttribute = WellKnownAttribute {
    name: "reference",
    r#type: AttributeType::String,
    description: "The issuer's own identifier for the document (invoice, contract or notice number...).",
};

pub const PERIOD_START: WellKnownAttribute = WellKnownAttribute {
    name: "period_start",
    r#type: AttributeType::Date,
    description: "The first day of the period the document covers (a payslip's month, a statement's...).",
};

pub const PERIOD_END: WellKnownAttribute = WellKnownAttribute {
    name: "period_end",
    r#type: AttributeType::Date,
    description: "The last day of the period the document covers.",
};

// Invoices, receipts, fee summaries, bookings.

pub const TOTAL_AMOUNT: WellKnownAttribute = WellKnownAttribute {
    name: "total_amount",
    r#type: AttributeType::Amount,
    description: "The total amount charged, taxes included.",
};

pub const DUE_ON: WellKnownAttribute = WellKnownAttribute {
    name: "due_on",
    r#type: AttributeType::Date,
    description: "The date payment is due by.",
};

// Payslips.

pub const GROSS_PAY: WellKnownAttribute = WellKnownAttribute {
    name: "gross_pay",
    r#type: AttributeType::Amount,
    description: "The pay before deductions.",
};

pub const NET_PAY: WellKnownAttribute = WellKnownAttribute {
    name: "net_pay",
    r#type: AttributeType::Amount,
    description: "The pay actually paid out, after deductions.",
};

// Tax notices, returns and certificates.

pub const TAX_YEAR: WellKnownAttribute = WellKnownAttribute {
    name: "tax_year",
    r#type: AttributeType::Integer,
    description: "The year of income the document is about (not the year it was issued in).",
};

pub const TAX_AMOUNT: WellKnownAttribute = WellKnownAttribute {
    name: "tax_amount",
    r#type: AttributeType::Amount,
    description: "The amount of tax due, or refunded if negative.",
};

/// Every well-known attribute, in the order they're declared above.
pub const ALL: &[WellKnownAttribute] = &[
    ISSUED_ON,
    ISSUER,
    REFERENCE,
    PERIOD_START,
    PERIOD_END,
    TOTAL_AMOUNT,
    DUE_ON,
    GROSS_PAY,
    NET_PAY,
    TAX_YEAR,
    TAX_AMOUNT,
];

/// The well-known attribute named `name`, if any.
pub fn get(name: &str) -> Option<&'static WellKnownAttribute> {
    ALL.iter().find(|attribute| attribute.name == name)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn names_are_unique() {
        let names: HashSet<_> = ALL.iter().map(|attribute| attribute.name).collect();

        assert_eq!(names.len(), ALL.len());
    }

    #[test]
    fn get_finds_an_attribute_by_name() {
        assert_eq!(get("net_pay"), Some(&NET_PAY));
        assert_eq!(get("price"), None);
    }
}
