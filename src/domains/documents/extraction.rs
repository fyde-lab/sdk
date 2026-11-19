//! The rules attribute extraction follows (see
//! `DocumentsClient::extract_attributes`): which of the user's attribute
//! definitions GLiNER looks for in an upload's transcript, under which
//! label, and the typed value each one gets from the best span found for it.
//! Pure functions: the model itself is `gliner`'s.

use std::collections::BTreeMap;

use super::attributes::{
    Attribute, AttributeDefinition, AttributeType, MAX_ATTRIBUTES, check_values,
};
use super::gliner::Span;

/// The lowest score a span needs to give an attribute its value: GLiNER's
/// own default.
pub(super) const THRESHOLD: f32 = 0.5;

/// How many words of a transcript are read, from its start: the first pages
/// carry what identifies a document (dates, amounts, references), and the
/// model costs about a call per 320 words.
pub(super) const MAX_WORDS: usize = 2000;

/// The currency of an amount whose span names none: the vault's documents
/// are mostly French.
const DEFAULT_CURRENCY: &str = "EUR";

/// An attribute definition GLiNER looks for.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Target {
    /// The attribute's name, what its value is stored under.
    pub(super) name: String,
    /// What GLiNER is asked to find, in plain words.
    pub(super) label: String,
    r#type: AttributeType,
}

/// The definitions worth looking for, in order: those whose type a span of
/// text can give a value (not booleans, nor selects, whose options a
/// definition doesn't carry), each labelled by its name in plain words
/// (`net_pay` → "net pay"), which GLiNER reads in any language.
pub(super) fn targets(definitions: &[AttributeDefinition]) -> Vec<Target> {
    definitions
        .iter()
        .filter(|definition| {
            matches!(
                definition.r#type(),
                AttributeType::String
                    | AttributeType::Integer
                    | AttributeType::Float
                    | AttributeType::Date
                    | AttributeType::Amount
            )
        })
        .map(|definition| Target {
            name: definition.name().to_string(),
            label: definition.name().replace('_', " "),
            r#type: definition.r#type(),
        })
        .collect()
}

/// The attributes `spans` (GLiNER's, best first, over `text`, labelled by
/// index into `targets`) give: each target takes the value of its best span
/// that reads as its type and is a valid attribute (see [`check_values`]),
/// and has none without one. At most [`MAX_ATTRIBUTES`].
pub(super) fn attributes(
    text: &str,
    targets: &[Target],
    spans: &[Span],
) -> BTreeMap<String, Attribute> {
    let mut found = BTreeMap::new();
    for span in spans {
        if found.len() == MAX_ATTRIBUTES {
            break;
        }
        let Some(target) = targets.get(span.label) else {
            continue;
        };
        if found.contains_key(&target.name) {
            continue;
        }
        let Some(value) = text
            .get(span.bytes.clone())
            .and_then(|span_text| parse(span_text, target.r#type))
        else {
            continue;
        };
        let attribute = BTreeMap::from([(target.name.clone(), value)]);
        if check_values(&attribute).is_ok() {
            found.extend(attribute);
        }
    }
    found
}

/// The value of type `type` `text` reads as, if it does.
fn parse(text: &str, r#type: AttributeType) -> Option<Attribute> {
    match r#type {
        AttributeType::String => {
            let value = text.split_whitespace().collect::<Vec<_>>().join(" ");
            (!value.is_empty()).then_some(Attribute::String(value))
        }
        AttributeType::Integer => {
            let number = only_number(text)?;
            if number.fraction.bytes().any(|digit| digit != b'0') {
                return None;
            }
            let value: i64 = number.integer.parse().ok()?;
            Some(Attribute::Integer(if number.negative {
                -value
            } else {
                value
            }))
        }
        AttributeType::Float => {
            let number = only_number(text)?;
            let value: f64 = format!("{}.{}0", number.integer, number.fraction)
                .parse()
                .ok()?;
            Some(Attribute::Float(if number.negative {
                -value
            } else {
                value
            }))
        }
        AttributeType::Amount => amount(text),
        AttributeType::Date => date(text).map(Attribute::Date),
        AttributeType::Boolean | AttributeType::Select => None,
    }
}

/// A number read from text, its digits as written.
#[derive(Debug, PartialEq)]
struct Number {
    negative: bool,
    /// Its integer part's digits, without thousands separators.
    integer: String,
    /// Its fractional part's digits, empty without one.
    fraction: String,
}

/// The one number in `text`, if it holds exactly one (see [`numbers`]).
fn only_number(text: &str) -> Option<Number> {
    let mut numbers = numbers(text).into_iter();
    let number = numbers.next()?;
    numbers.next().is_none().then_some(number)
}

/// Every number in `text`, in order: a run of digits, separators between
/// them (`.`, `,`, `'` and spaces, non-breaking ones included) and a `-` or
/// `−` right before it, that reads as a number (see [`number`]).
fn numbers(text: &str) -> Vec<Number> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let negative = i > 0 && matches!(chars[i - 1], '-' | '−');
        let mut run = String::new();
        while i < chars.len() {
            let c = chars[i];
            if c.is_ascii_digit() {
                run.push(c);
            } else if is_separator(c) && chars.get(i + 1).is_some_and(char::is_ascii_digit) {
                run.push(if c.is_whitespace() { ' ' } else { c });
            } else {
                break;
            }
            i += 1;
        }
        if let Some(mut parsed) = number(&run) {
            parsed.negative = negative;
            found.push(parsed);
        }
    }
    found
}

fn is_separator(c: char) -> bool {
    matches!(c, '.' | ',' | '\'') || c.is_whitespace()
}

/// Reads `run` (digits, and single separators between them: see
/// [`numbers`]) as a number, French or English style: `1 234,56`,
/// `1.234,56`, `1,234.56`, `1'234.56`. With both `.` and `,`, the last is
/// the decimal separator; alone, one that comes once is too, unless it's
/// followed by exactly three digits after a nonzero integer part with no
/// other separator (`1.234`, `12,500`: thousands). Every other separator
/// groups thousands, so the groups after the first must have three digits.
fn number(run: &str) -> Option<Number> {
    let dots = run.matches('.').count();
    let commas = run.matches(',').count();
    let decimal = match (dots, commas) {
        (0, 0) => None,
        (_, 0) | (0, _) => {
            let (separator, count) = if dots > 0 { ('.', dots) } else { (',', commas) };
            let (before, after) = run.rsplit_once(separator)?;
            let thousands = count > 1
                || (after.len() == 3
                    && before != "0"
                    && before.chars().all(|c| c.is_ascii_digit()));
            (!thousands).then_some(separator)
        }
        _ => {
            let last = run.rfind(['.', ','])?;
            let separator = run[last..].chars().next()?;
            if run.matches(separator).count() != 1 {
                return None;
            }
            Some(separator)
        }
    };

    let (integer, fraction) = match decimal {
        Some(separator) => run.rsplit_once(separator)?,
        None => (run, ""),
    };
    if !fraction.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let groups: Vec<&str> = integer.split(['.', ',', '\'', ' ']).collect();
    let well_grouped = groups.iter().all(|group| !group.is_empty())
        && (groups.len() == 1
            || (groups[0].len() <= 3 && groups[1..].iter().all(|group| group.len() == 3)));
    well_grouped.then(|| Number {
        negative: false,
        integer: groups.concat(),
        fraction: fraction.to_string(),
    })
}

/// The currencies an amount's span can name, by symbol or code (matched as
/// a whole word, ignoring case), with how many decimals their minor unit
/// has.
const CURRENCIES: &[(&[&str], &str, usize)] = &[
    (&["€", "eur", "euro", "euros"], "EUR", 2),
    (&["$", "usd"], "USD", 2),
    (&["£", "gbp"], "GBP", 2),
    (&["chf"], "CHF", 2),
    (&["cad"], "CAD", 2),
];

/// The amount `text` reads as: its one number (see [`only_number`]) in the
/// currency it names, else in [`DEFAULT_CURRENCY`]. None with more decimals
/// than the currency's minor unit has.
fn amount(text: &str) -> Option<Attribute> {
    let number = only_number(text)?;
    let lowercase = text.to_lowercase();
    let words: Vec<&str> = lowercase
        .split(|c: char| !c.is_alphabetic())
        .filter(|word| !word.is_empty())
        .collect();
    let (currency, decimals) = CURRENCIES
        .iter()
        .find(|(names, _, _)| {
            names.iter().any(|name| {
                if name.chars().all(char::is_alphabetic) {
                    words.contains(name)
                } else {
                    lowercase.contains(name)
                }
            })
        })
        .map_or((DEFAULT_CURRENCY, 2), |(_, code, decimals)| {
            (*code, *decimals)
        });

    let fraction = number.fraction.trim_end_matches('0');
    if fraction.len() > decimals {
        return None;
    }
    let minor_digits = format!("{}{fraction:0<decimals$}", number.integer);
    let minor_units: i64 = minor_digits.parse().ok()?;
    Some(Attribute::Amount {
        minor_units: if number.negative {
            -minor_units
        } else {
            minor_units
        },
        currency: currency.to_string(),
    })
}

/// A piece of a date's text (see [`tokens`]).
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Digits(String),
    Word(String),
    Mark(char),
}

/// `text` as digits, words (lowercase) and punctuation marks, whitespace
/// dropped: `1er octobre` is `1`, `er`, `octobre`.
fn tokens(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    for c in text.chars() {
        let extended = match tokens.last_mut() {
            Some(Token::Digits(digits)) if c.is_ascii_digit() => {
                digits.push(c);
                true
            }
            Some(Token::Word(word)) if c.is_alphabetic() => {
                word.extend(c.to_lowercase());
                true
            }
            _ => false,
        };
        if extended {
            continue;
        }
        tokens.push(if c.is_ascii_digit() {
            Token::Digits(c.to_string())
        } else if c.is_alphabetic() {
            Token::Word(c.to_lowercase().collect())
        } else if c.is_whitespace() {
            // Only ends the token before it.
            Token::Mark(' ')
        } else {
            Token::Mark(c)
        });
    }
    tokens.retain(|token| *token != Token::Mark(' '));
    tokens
}

/// The months a date's text can name, French and English, in full or
/// abbreviated, accents optional.
const MONTHS: [&[&str]; 12] = [
    &["janvier", "janv", "january", "jan"],
    &[
        "février", "fevrier", "févr", "fevr", "fév", "fev", "february", "feb",
    ],
    &["mars", "march", "mar"],
    &["avril", "avr", "april", "apr"],
    &["mai", "may"],
    &["juin", "june", "jun"],
    &["juillet", "juil", "july", "jul"],
    &["août", "aout", "august", "aug"],
    &["septembre", "sept", "september", "sep"],
    &["octobre", "october", "oct"],
    &["novembre", "november", "nov"],
    &["décembre", "decembre", "déc", "dec", "december"],
];

fn month(word: &str) -> Option<u32> {
    (1..)
        .zip(MONTHS)
        .find_map(|(number, names)| names.contains(&word).then_some(number))
}

/// The first date in `text`, as `YYYY-MM-DD` (not checked to be a real
/// calendar date: [`check_values`] does): `15/10/2026` (day first, also with
/// `.` or `-`, or a two-digit year in the 2000s), `2026-10-15`,
/// `15 octobre 2026`, `1er oct. 2026`, `October 15, 2026`.
fn date(text: &str) -> Option<String> {
    let tokens = tokens(text);
    (0..tokens.len()).find_map(|start| date_at(&tokens[start..]))
}

/// The date `tokens` start with, if they do (see [`date`]).
fn date_at(tokens: &[Token]) -> Option<String> {
    let digits = |index: usize| match tokens.get(index) {
        Some(Token::Digits(digits)) => Some(digits.as_str()),
        _ => None,
    };
    let format = |year: &str, month: u32, day: &str| -> Option<String> {
        let year = match year.len() {
            4 => year.to_string(),
            2 => format!("20{year}"),
            _ => return None,
        };
        let day: u32 = (day.len() <= 2).then(|| day.parse().ok())??;
        Some(format!("{year}-{month:02}-{day:02}"))
    };

    // Numeric: 15/10/2026, 2026-10-15.
    if let (
        Some(first),
        Some(Token::Mark(mark)),
        Some(second),
        Some(Token::Mark(mark2)),
        Some(third),
    ) = (
        digits(0),
        tokens.get(1),
        digits(2),
        tokens.get(3),
        digits(4),
    ) && mark == mark2
        && matches!(*mark, '/' | '.' | '-')
    {
        let (year, month, day) = if first.len() == 4 {
            (first, second, third)
        } else {
            (third, second, first)
        };
        let month: u32 = (month.len() <= 2).then(|| month.parse().ok())??;
        return format(year, month, day);
    }

    // Day first: 15 octobre 2026, 1er oct. 2026.
    if let Some(day) = digits(0) {
        let mut next = 1;
        if tokens.get(next) == Some(&Token::Word("er".to_string())) {
            next += 1;
        }
        if let Some(Token::Word(word)) = tokens.get(next)
            && let Some(month) = month(word)
        {
            next += 1;
            if tokens.get(next) == Some(&Token::Mark('.')) {
                next += 1;
            }
            if let Some(year) = digits(next).filter(|year| year.len() == 4) {
                return format(year, month, day);
            }
        }
        return None;
    }

    // Month first: October 15, 2026.
    if let Some(Token::Word(word)) = tokens.first()
        && let Some(month) = month(word)
    {
        let mut next = 1;
        if tokens.get(next) == Some(&Token::Mark('.')) {
            next += 1;
        }
        let day = digits(next)?;
        next += 1;
        if tokens.get(next) == Some(&Token::Mark(',')) {
            next += 1;
        }
        let year = digits(next).filter(|year| year.len() == 4)?;
        return format(year, month, day);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::super::attributes::FakeAttributeDefinition;
    use super::*;

    fn definition(name: &str, r#type: AttributeType) -> AttributeDefinition {
        FakeAttributeDefinition::new()
            .with_name(name)
            .with_type(r#type)
            .build()
    }

    fn target(name: &str, r#type: AttributeType) -> Target {
        Target {
            name: name.to_string(),
            label: name.replace('_', " "),
            r#type,
        }
    }

    /// The span of `text`'s first `needle`, for `label`, scored `score`.
    fn span(text: &str, needle: &str, label: usize, score: f32) -> Span {
        let start = text.find(needle).unwrap();
        Span {
            label,
            bytes: start..start + needle.len(),
            score,
        }
    }

    fn amount_of(minor_units: i64, currency: &str) -> Attribute {
        Attribute::Amount {
            minor_units,
            currency: currency.to_string(),
        }
    }

    #[test]
    fn targets_label_each_extractable_definition_by_its_name_in_plain_words() {
        let definitions = [
            definition("net_pay", AttributeType::Amount),
            definition("paid", AttributeType::Boolean),
            definition("issued_on", AttributeType::Date),
            definition("status", AttributeType::Select),
            definition("invoice_number", AttributeType::String),
            definition("pages", AttributeType::Integer),
            definition("rate", AttributeType::Float),
        ];

        let targets = targets(&definitions);

        assert_eq!(
            targets,
            vec![
                target("net_pay", AttributeType::Amount),
                target("issued_on", AttributeType::Date),
                target("invoice_number", AttributeType::String),
                target("pages", AttributeType::Integer),
                target("rate", AttributeType::Float),
            ]
        );
        assert_eq!(targets[0].label, "net pay");
    }

    #[test]
    fn attributes_take_each_targets_best_span_that_reads_as_its_type() {
        let text = "Facture FAC-2026-001 du 15/10/2026, total 1 234,56 € (dont TVA 205,76 €).";
        let targets = [
            target("invoice_number", AttributeType::String),
            target("issued_on", AttributeType::Date),
            target("total_amount", AttributeType::Amount),
        ];
        let spans = [
            span(text, "1 234,56 €", 2, 0.95),
            // Not a date: the date's next span is taken instead.
            span(text, "Facture", 1, 0.9),
            span(text, "FAC-2026-001", 0, 0.85),
            span(text, "15/10/2026", 1, 0.8),
            // Below the best: ignored.
            span(text, "205,76 €", 2, 0.7),
        ];

        let found = attributes(text, &targets, &spans);

        assert_eq!(
            found,
            BTreeMap::from([
                (
                    "invoice_number".to_string(),
                    Attribute::String("FAC-2026-001".to_string())
                ),
                (
                    "issued_on".to_string(),
                    Attribute::Date("2026-10-15".to_string())
                ),
                ("total_amount".to_string(), amount_of(123_456, "EUR")),
            ])
        );
    }

    #[test]
    fn attributes_skip_values_that_arent_valid_attributes_and_unknown_labels() {
        let text = "Échéance le 31/02/2026.";
        let targets = [target("due_on", AttributeType::Date)];
        let spans = [
            // No such calendar date.
            span(text, "31/02/2026", 0, 0.9),
            span(text, "Échéance", 3, 0.8),
        ];

        assert!(attributes(text, &targets, &spans).is_empty());
    }

    #[test]
    fn attributes_skip_spans_outside_the_text() {
        let targets = [target("name", AttributeType::String)];
        let spans = [Span {
            label: 0,
            bytes: 3..40,
            score: 0.9,
        }];

        assert!(attributes("short", &targets, &spans).is_empty());
    }

    #[test]
    fn strings_collapse_their_whitespace() {
        assert_eq!(
            parse("  Jean \n Dupont ", AttributeType::String),
            Some(Attribute::String("Jean Dupont".to_string()))
        );
        assert_eq!(parse(" \n ", AttributeType::String), None);
    }

    #[test]
    fn integers_read_the_spans_one_whole_number() {
        assert_eq!(
            parse("12 500 km", AttributeType::Integer),
            Some(Attribute::Integer(12_500))
        );
        assert_eq!(
            parse("-42", AttributeType::Integer),
            Some(Attribute::Integer(-42))
        );
        assert_eq!(
            parse("3,00", AttributeType::Integer),
            Some(Attribute::Integer(3))
        );
        assert_eq!(parse("3,5", AttributeType::Integer), None);
        assert_eq!(parse("3 sur 12,5", AttributeType::Integer), None);
        assert_eq!(parse("aucun", AttributeType::Integer), None);
    }

    #[test]
    fn floats_read_french_and_english_notations() {
        assert_eq!(
            parse("1 234,5", AttributeType::Float),
            Some(Attribute::Float(1234.5))
        );
        assert_eq!(
            parse("1,234.5", AttributeType::Float),
            Some(Attribute::Float(1234.5))
        );
        assert_eq!(
            parse("taux : 0,125", AttributeType::Float),
            Some(Attribute::Float(0.125))
        );
        assert_eq!(
            parse("−2.5", AttributeType::Float),
            Some(Attribute::Float(-2.5))
        );
    }

    #[test]
    fn numbers_tell_thousands_from_decimals() {
        let read = |run: &str| number(run).map(|number| (number.integer, number.fraction));
        let parts =
            |integer: &str, fraction: &str| Some((integer.to_string(), fraction.to_string()));

        assert_eq!(read("1234"), parts("1234", ""));
        assert_eq!(read("1 234,56"), parts("1234", "56"));
        assert_eq!(read("1.234,56"), parts("1234", "56"));
        assert_eq!(read("1,234.56"), parts("1234", "56"));
        assert_eq!(read("1'234.56"), parts("1234", "56"));
        assert_eq!(read("1.234"), parts("1234", ""));
        assert_eq!(read("12,500"), parts("12500", ""));
        assert_eq!(read("1.234.567"), parts("1234567", ""));
        assert_eq!(read("0,125"), parts("0", "125"));
        assert_eq!(read("1 234,567"), parts("1234", "567"));
        assert_eq!(read("2150,5"), parts("2150", "5"));
        // Badly grouped thousands, or two decimal separators.
        assert_eq!(read("12 34"), None);
        assert_eq!(read("1.2.3"), None);
        assert_eq!(read("1,234.5,6"), None);
    }

    #[test]
    fn amounts_are_minor_units_of_the_currency_the_span_names() {
        assert_eq!(
            parse("2 150,50 €", AttributeType::Amount),
            Some(amount_of(215_050, "EUR"))
        );
        assert_eq!(
            parse("Net à payer : 1 980 euros", AttributeType::Amount),
            Some(amount_of(198_000, "EUR"))
        );
        assert_eq!(
            parse("$1,234.5", AttributeType::Amount),
            Some(amount_of(123_450, "USD"))
        );
        assert_eq!(
            parse("CHF 12.30", AttributeType::Amount),
            Some(amount_of(1_230, "CHF"))
        );
        assert_eq!(
            parse("-15,00 EUR", AttributeType::Amount),
            Some(amount_of(-1_500, "EUR"))
        );
    }

    #[test]
    fn amounts_without_a_currency_are_in_euros() {
        assert_eq!(
            parse("1 234,56", AttributeType::Amount),
            Some(amount_of(123_456, "EUR"))
        );
    }

    #[test]
    fn amounts_refuse_more_decimals_than_cents_and_several_numbers() {
        assert_eq!(parse("1,2345 €", AttributeType::Amount), None);
        assert_eq!(parse("12 € puis 15 €", AttributeType::Amount), None);
        assert_eq!(
            parse("12,500 €", AttributeType::Amount),
            Some(amount_of(1_250_000, "EUR"))
        );
    }

    #[test]
    fn amounts_only_match_currency_codes_as_whole_words() {
        // "eur" inside "valeur" isn't a currency, nor is "cad" in "cadeau".
        assert_eq!(
            parse("valeur 12 $", AttributeType::Amount),
            Some(amount_of(1_200, "USD"))
        );
        assert_eq!(
            parse("cadeau 12", AttributeType::Amount),
            Some(amount_of(1_200, "EUR"))
        );
    }

    #[test]
    fn dates_read_numeric_notations_day_first() {
        let read = |text| parse(text, AttributeType::Date);
        let date = |value: &str| Some(Attribute::Date(value.to_string()));

        assert_eq!(read("le 15/10/2026"), date("2026-10-15"));
        assert_eq!(read("5.3.2026"), date("2026-03-05"));
        assert_eq!(read("15-10-26"), date("2026-10-15"));
        assert_eq!(read("2026-10-15"), date("2026-10-15"));
        // Mixed separators aren't a date.
        assert_eq!(read("15/10-2026"), None);
    }

    #[test]
    fn dates_read_french_and_english_month_names() {
        let read = |text| parse(text, AttributeType::Date);
        let date = |value: &str| Some(Attribute::Date(value.to_string()));

        assert_eq!(read("15 octobre 2026"), date("2026-10-15"));
        assert_eq!(read("Paris, le 1er février 2026"), date("2026-02-01"));
        assert_eq!(read("3 déc. 2025"), date("2025-12-03"));
        assert_eq!(read("October 15, 2026"), date("2026-10-15"));
        assert_eq!(read("Sept. 9 2026"), date("2026-09-09"));
        assert_eq!(read("15 octobre"), None);
        assert_eq!(read("octobre 2026"), None);
    }

    #[test]
    fn booleans_and_selects_are_never_extracted() {
        assert_eq!(parse("oui", AttributeType::Boolean), None);
        assert_eq!(parse("payée", AttributeType::Select), None);
    }
}
