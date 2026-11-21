use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use async_trait::async_trait;
use regex::Regex;

use super::super::DocumentType;
use super::super::attributes::Attribute;
use super::super::gliner::{Span, SpanFinder};
use super::{EXTRACTION_RETRY_AFTER, Service};
use crate::{ErrorContext as _, Result};

/// Spans scoring lower are ignored. GLiNER's default is 0.5; this one keeps
/// weaker candidates since only one value per attribute is kept anyway.
const THRESHOLD: f32 = 0.3;

/// Attribute values sit at the top of documents; reading further only costs
/// time.
const MAX_WORDS: usize = 1200;

/// The currency of every amount found: the documents read are French, and
/// [`parse_amount`] only reads euros.
const CURRENCY: &str = "EUR";

/// What an attribute's span is parsed as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// An [`Attribute::Amount`] in euros.
    Amount,
    /// An [`Attribute::Date`].
    Date,
    /// A month or a date range, stored as two [`Attribute::Date`]s,
    /// `{name}_start` and `{name}_end`.
    Period,
    /// An [`Attribute::Integer`] year.
    Year,
    /// An [`Attribute::String`], the span's text.
    Text,
}

/// An attribute looked for.
struct Target {
    /// The name its value is stored under, or the prefix of its two names
    /// for a [`Kind::Period`]: each a default attribute definition on the
    /// server where one fits, with the type `kind` gives it.
    name: &'static str,
    kind: Kind,
    /// What GLiNER is asked to find: a short entity type works better than
    /// a sentence.
    label: &'static str,
}

const fn target(name: &'static str, kind: Kind, label: &'static str) -> Target {
    Target { name, kind, label }
}

/// Asked for alongside the attributes but never reported: look-alikes (an
/// id, a company's share capital) that GLiNER would otherwise take for an
/// attribute. Taking their span first keeps it from being one.
const DECOYS: &[&str] = &[
    "capital social",
    "numéro de TVA intracommunautaire",
    "numéro SIRET",
    "numéro de sécurité sociale",
    "numéro de référence",
    "taux",
];

/// What to look for in the documents of a type.
struct Class {
    targets: &'static [Target],
    /// Look-alikes specific to the type, asked for on top of [`DECOYS`].
    decoys: &'static [&'static str],
}

impl Class {
    /// Every label GLiNER is asked for: the targets', in order (so a span's
    /// label index is its target's), then the decoys'.
    fn labels(&self) -> Vec<String> {
        self.targets
            .iter()
            .map(|target| target.label)
            .chain(DECOYS.iter().chain(self.decoys).copied())
            .map(str::to_string)
            .collect()
    }
}

use Kind::{Amount, Date, Period, Text, Year};

const INVOICE: Class = Class {
    targets: &[
        target("issuer", Text, "fournisseur"),
        target("reference", Text, "numéro de facture"),
        target("issued_on", Date, "date de facture"),
        target("total_amount", Amount, "montant total TTC"),
    ],
    decoys: &[],
};
const PAYSLIP: Class = Class {
    targets: &[
        target("employer", Text, "employeur"),
        target("period", Period, "période de paie"),
        target("gross_pay", Amount, "salaire brut total"),
        target("net_pay", Amount, "net à payer"),
    ],
    decoys: &["salaire de base"],
};
const TAX_NOTICE: Class = Class {
    targets: &[
        target("tax_year", Year, "année des revenus"),
        target("reference_tax_income", Amount, "revenu fiscal de référence"),
        target("tax_amount", Amount, "montant de l'impôt"),
    ],
    decoys: &[],
};
const STATEMENT: Class = Class {
    targets: &[
        target("bank", Text, "banque"),
        target("statement_date", Date, "date du relevé"),
        target("balance", Amount, "solde"),
    ],
    decoys: &[],
};
const HEALTH_REIMBURSEMENT: Class = Class {
    targets: &[
        target("insurer", Text, "organisme d'assurance maladie"),
        target("care_date", Date, "date des soins"),
        target("reimbursed_amount", Amount, "total remboursé"),
    ],
    decoys: &[],
};
const DONATION: Class = Class {
    targets: &[
        target("organization", Text, "association"),
        target("donation_date", Date, "date du don"),
        target("donation_amount", Amount, "montant du don"),
    ],
    decoys: &[],
};
const GENERIC: Class = Class {
    targets: &[
        target("issuer", Text, "organisme"),
        target("document_date", Date, "date"),
        target("amount", Amount, "montant"),
    ],
    decoys: &[],
};

/// What to look for in a document of type `r#type`. `doc-ai-poc` chose by
/// category instead, which documents no longer have: its local tax, rent
/// and benefits classes have no type of their own, so such documents get
/// their type's (a local tax notice is a [`DocumentType::TaxNotice`], a
/// rent receipt a [`DocumentType::Receipt`]) or the generic one.
fn class(r#type: Option<DocumentType>) -> &'static Class {
    match r#type {
        Some(DocumentType::Invoice | DocumentType::Receipt) => &INVOICE,
        Some(DocumentType::Payslip) => &PAYSLIP,
        Some(DocumentType::TaxNotice) => &TAX_NOTICE,
        Some(DocumentType::Statement) => &STATEMENT,
        Some(DocumentType::ReimbursementStatement) => &HEALTH_REIMBURSEMENT,
        // Donation receipts are tax certificates: the reduction they open.
        Some(DocumentType::TaxCertificate) => &DONATION,
        _ => &GENERIC,
    }
}

/// The default [`Service`], finding spans with GLiNER.
pub(super) struct Extractor {
    spans: Arc<dyn SpanFinder>,
    /// When finding spans last failed (see [`EXTRACTION_RETRY_AFTER`]).
    failed_at: Mutex<Option<tokio::time::Instant>>,
}

impl Extractor {
    pub(super) fn new(spans: Arc<dyn SpanFinder>) -> Self {
        Self {
            spans,
            failed_at: Mutex::new(None),
        }
    }

    fn failed_at(&self) -> std::sync::MutexGuard<'_, Option<tokio::time::Instant>> {
        self.failed_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

#[async_trait]
impl Service for Extractor {
    async fn extract(
        &self,
        transcript: &str,
        r#type: Option<DocumentType>,
    ) -> Result<BTreeMap<String, Attribute>> {
        if self
            .failed_at()
            .is_some_and(|at| at.elapsed() < EXTRACTION_RETRY_AFTER)
        {
            tracing::debug!("not extracting attributes, the GLiNER model failed recently");
            return Ok(BTreeMap::new());
        }

        let class = class(r#type);
        let spans = match self
            .spans
            .spans(transcript.to_string(), class.labels(), THRESHOLD, MAX_WORDS)
            .await
        {
            Ok(spans) => spans,
            Err(err) => {
                *self.failed_at() = Some(tokio::time::Instant::now());
                return Err(err).context("failed to look for attribute values with GLiNER");
            }
        };
        Ok(attributes(transcript, class, spans))
    }
}

/// A span's value, parsed as its target's [`Kind`].
#[derive(Debug, Clone, PartialEq)]
enum Value {
    /// Minor units of [`CURRENCY`].
    Amount(i64),
    Date(String),
    Period {
        start: String,
        end: String,
    },
    Year(u32),
    Text(String),
}

impl Value {
    /// `raw` (a span's whitespace-collapsed text) parsed as `kind`, `None`
    /// if it doesn't read as one.
    fn parse(kind: Kind, raw: &str) -> Option<Self> {
        Some(match kind {
            Amount => Self::Amount(parse_amount(raw)?),
            Date => Self::Date(parse_date(raw)?),
            Period => {
                let (start, end) = parse_period(raw)?;
                Self::Period { start, end }
            }
            Year => Self::Year(parse_year(raw)?),
            Text => Self::Text(raw.to_string()),
        })
    }

    /// The attributes this value is stored as under `name`: a period as its
    /// `{name}_start` and `{name}_end` dates.
    fn into_attributes(self, name: &str) -> Vec<(String, Attribute)> {
        match self {
            Self::Amount(minor_units) => vec![(
                name.to_string(),
                Attribute::Amount {
                    minor_units,
                    currency: CURRENCY.to_string(),
                },
            )],
            Self::Date(date) => vec![(name.to_string(), Attribute::Date(date))],
            Self::Period { start, end } => vec![
                (format!("{name}_start"), Attribute::Date(start)),
                (format!("{name}_end"), Attribute::Date(end)),
            ],
            Self::Year(year) => vec![(name.to_string(), Attribute::Integer(i64::from(year)))],
            Self::Text(text) => vec![(name.to_string(), Attribute::String(text))],
        }
    }
}

/// The best span for each of `class`'s targets GLiNER found in `text`, as
/// attributes. Spans that don't parse as their target's kind are dropped
/// first; then, best first, a span overlapping one already taken (a
/// decoy's included) is dropped (GLiNER's flat decoding), so one piece of
/// text is never two attributes. A span outside `text`, or not on its
/// character boundaries, is ignored.
fn attributes(text: &str, class: &Class, mut spans: Vec<Span>) -> BTreeMap<String, Attribute> {
    // Stable: equal scores keep GLiNER's order.
    spans.sort_by(|a, b| b.score.total_cmp(&a.score));
    let decoys = DECOYS.len() + class.decoys.len();

    let mut taken: Vec<Range<usize>> = Vec::new();
    let mut found: HashSet<&str> = HashSet::new();
    let mut attributes = BTreeMap::new();
    for span in spans {
        let bytes = span.bytes;
        if taken
            .iter()
            .any(|t| bytes.start < t.end && t.start < bytes.end)
        {
            continue;
        }
        let Some(raw) = text.get(bytes.clone()) else {
            continue;
        };
        let Some(target) = class.targets.get(span.label) else {
            if span.label < class.targets.len() + decoys {
                taken.push(bytes);
            }
            continue;
        };
        let raw = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        let Some(value) = Value::parse(target.kind, &raw) else {
            continue;
        };
        taken.push(bytes);
        if found.insert(target.name) {
            attributes.extend(value.into_attributes(target.name));
        }
    }
    attributes
}

/// "1 234,56 €", "1.234,56", "1234.56", "12 EUR", "9,77 euro(s)",
/// "- 1 138,93 €-" → minor units. Anything else (an id, "10 jours",
/// "Page 1/1", "203,49 hrs", a bare "83") is no amount.
fn parse_amount(s: &str) -> Option<i64> {
    // `[0-9]`, not `\d`, which takes any script's digits: they'd then be
    // dropped below ("1２3,45 €" read as 13,45 €).
    static AMOUNT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?ix)^
              (?P<prefix>€\s*)?
              (?P<sign>-)?\s*
              (?P<units>[0-9]{1,3}(?:[\ .'\u{a0}\u{202f}][0-9]{3})+|[0-9]{1,7})
              (?:[.,](?P<decimals>[0-9]{1,2}))?
              \s*(?P<currency>€|eur|euros?|euro\(s\))?
              (?P<padding>-)?
              (?:\s*(?:ttc|ht))?
            $",
        )
        .expect("the amount regex is valid")
    });
    let c = AMOUNT.captures(s.trim())?;
    // A bare integer is more often a count, a day or an id than money.
    let grouped = c["units"].contains(|ch: char| !ch.is_ascii_digit());
    let currency = c.name("prefix").is_some() || c.name("currency").is_some();
    if c.name("decimals").is_none() && !grouped && !currency {
        return None;
    }
    // Spreadsheets' accounting format pads amounts with dashes
    // ("- 1 138,93 €-"): no sign then.
    let negative = match (c.name("padding"), c.name("currency")) {
        (Some(_), Some(_)) => false,
        (Some(_), None) => return None,
        (None, _) => c.name("sign").is_some(),
    };
    let units: i64 = c["units"]
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()?;
    let decimals = c.name("decimals").map_or("", |m| m.as_str());
    let cents: i64 = format!("{decimals:0<2}").parse().ok()?;
    let value = units.checked_mul(100)?.checked_add(cents)?;
    Some(if negative { -value } else { value })
}

/// "05/03/2024", "2024-03-05", "5 mars 2024", "1er janvier 2016" →
/// "YYYY-MM-DD". A day the month doesn't have ("31/04/2024") is no date.
fn parse_date(s: &str) -> Option<String> {
    let (y, m, d) = day_month_year(s)?;
    (d <= days_in_month(y, m)).then(|| iso(y, m, d))
}

/// (year, month, day), the day only checked to be 1 to 31.
fn day_month_year(s: &str) -> Option<(u32, u32, u32)> {
    let tokens = tokens(s);
    tokens.windows(3).find_map(|w| {
        let (y, m, d) = if w[0].len() == 4 {
            (number(&w[0])?, month(&w[1])?, number(&w[2])?)
        } else {
            (number(&w[2])?, month(&w[1])?, number(&w[0])?)
        };
        let y = if y < 100 { 2000 + y } else { y };
        ((1900..2100).contains(&y) && (1..=31).contains(&d)).then_some((y, m, d))
    })
}

/// "du 1/01/2026 au 31/01/2026" → ("2026-01-01", "2026-01-31"),
/// "mars 2016" → its first and last days. An end past its month's last day
/// ("au 31/02/2026") is that last day: payslips print such ends.
fn parse_period(s: &str) -> Option<(String, String)> {
    let dates: Vec<(u32, u32, u32)> = s
        .split([' ', '-'])
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .split(|w| matches!(w.to_lowercase().as_str(), "au" | "à" | "to"))
        .filter_map(|part| day_month_year(&part.join(" ")))
        .collect();
    match dates[..] {
        [(y1, m1, d1), (y2, m2, d2), ..] => {
            if d1 > days_in_month(y1, m1) || (y1, m1, d1) > (y2, m2, d2) {
                return None;
            }
            return Some((iso(y1, m1, d1), iso(y2, m2, d2.min(days_in_month(y2, m2)))));
        }
        [_] => return None,
        [] => {}
    }
    tokens(s).windows(2).find_map(|w| {
        let m = month(&w[0])?;
        let y = number(&w[1]).filter(|y| (1900..2100).contains(y))?;
        Some((iso(y, m, 1), iso(y, m, days_in_month(y, m))))
    })
}

/// The first four-digit year from 1900 to 2099 in `s`.
fn parse_year(s: &str) -> Option<u32> {
    s.split(|c: char| !c.is_ascii_digit())
        .filter(|t| t.len() == 4)
        .filter_map(|t| t.parse().ok())
        .find(|y| (1900..2100).contains(y))
}

/// `s`'s alphanumeric runs, lowercased.
fn tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn iso(y: u32, m: u32, d: u32) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        2 if y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400)) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Leading digits ("1er" → 1).
fn number(t: &str) -> Option<u32> {
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// A month, as a number from 1 to 12 or a French or English name
/// (abbreviated or not).
fn month(t: &str) -> Option<u32> {
    if let Some(m) = number(t) {
        return (1..=12).contains(&m).then_some(m);
    }
    const NAMES: &[(&str, u32)] = &[
        ("janv", 1),
        ("jan", 1),
        ("févr", 2),
        ("fevr", 2),
        ("fév", 2),
        ("fev", 2),
        ("feb", 2),
        ("mar", 3),
        ("avr", 4),
        ("apr", 4),
        ("mai", 5),
        ("may", 5),
        ("juin", 6),
        ("jun", 6),
        ("juil", 7),
        ("jul", 7),
        ("août", 8),
        ("aout", 8),
        ("aug", 8),
        ("sep", 9),
        ("oct", 10),
        ("nov", 11),
        ("déc", 12),
        ("dec", 12),
    ];
    (t.chars().count() >= 3)
        .then(|| {
            NAMES
                .iter()
                .find(|(p, _)| t.starts_with(p))
                .map(|&(_, m)| m)
        })
        .flatten()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::super::attributes::check_values;
    use super::super::super::gliner::{MockSpanFinder, OnnxGliner};
    use super::*;
    use crate::ErrorKind;
    use crate::tools::models::ModelStore;

    /// A span of `text` covering the first `needle` in it.
    fn span(text: &str, needle: &str, label: usize, score: f32) -> Span {
        let start = text.find(needle).unwrap();
        Span {
            label,
            bytes: start..start + needle.len(),
            score,
        }
    }

    fn euros(minor_units: i64) -> Attribute {
        Attribute::Amount {
            minor_units,
            currency: "EUR".to_string(),
        }
    }

    fn date(value: &str) -> Attribute {
        Attribute::Date(value.to_string())
    }

    fn found(entries: &[(&str, Attribute)]) -> BTreeMap<String, Attribute> {
        entries
            .iter()
            .map(|(name, attribute)| (name.to_string(), attribute.clone()))
            .collect()
    }

    #[tokio::test]
    async fn extract_looks_for_the_types_labels_then_the_decoys() {
        let mut spans = MockSpanFinder::new();
        spans
            .expect_spans()
            .withf(|text, labels, threshold, max_words| {
                text == "Bulletin de paie"
                    && labels[..5]
                        == [
                            "employeur",
                            "période de paie",
                            "salaire brut total",
                            "net à payer",
                            "capital social",
                        ]
                    && labels.last().map(String::as_str) == Some("salaire de base")
                    && labels.len() == 4 + DECOYS.len() + 1
                    && *threshold == THRESHOLD
                    && *max_words == MAX_WORDS
            })
            .times(1)
            .returning(|_, _, _, _| Ok(Vec::new()));
        let extractor = Extractor::new(Arc::new(spans));

        let found = extractor
            .extract("Bulletin de paie", Some(DocumentType::Payslip))
            .await
            .unwrap();

        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn extract_looks_for_what_any_document_carries_without_a_type() {
        for r#type in [None, Some(DocumentType::Letter), Some(DocumentType::Other)] {
            let mut spans = MockSpanFinder::new();
            spans
                .expect_spans()
                .withf(|_, labels, _, _| labels[..3] == ["organisme", "date", "montant"])
                .times(1)
                .returning(|_, _, _, _| Ok(Vec::new()));
            let extractor = Extractor::new(Arc::new(spans));

            extractor.extract("Courrier", r#type).await.unwrap();
        }
    }

    #[tokio::test]
    async fn extract_reads_each_targets_best_span_as_its_type() {
        let text = "Employeur : Atelier  Exemple\nPériode du 1/01/2026 au 31/01/2026\n\
                    Salaire brut : 2 500,00 €\nNet à payer : 1 950,40 €";
        let returned = vec![
            span(text, "1 950,40 €", 3, 0.9),
            span(text, "Atelier  Exemple", 0, 0.8),
            span(text, "1/01/2026 au 31/01/2026", 1, 0.7),
            span(text, "2 500,00 €", 2, 0.6),
            // Worse than the net pay found first.
            span(text, "2 500,00", 3, 0.5),
        ];
        let mut spans = MockSpanFinder::new();
        spans
            .expect_spans()
            .times(1)
            .returning(move |_, _, _, _| Ok(returned.clone()));
        let extractor = Extractor::new(Arc::new(spans));

        let found = extractor
            .extract(text, Some(DocumentType::Payslip))
            .await
            .unwrap();

        assert_eq!(
            found,
            self::found(&[
                ("employer", Attribute::String("Atelier Exemple".to_string())),
                ("period_start", date("2026-01-01")),
                ("period_end", date("2026-01-31")),
                ("gross_pay", euros(250_000)),
                ("net_pay", euros(195_040)),
            ])
        );
    }

    #[tokio::test]
    async fn extract_reads_years_as_integers() {
        let text = "Avis d'impôt 2025 sur les revenus 2024";
        let returned = vec![span(text, "revenus 2024", 0, 0.7)];
        let mut spans = MockSpanFinder::new();
        spans
            .expect_spans()
            .returning(move |_, _, _, _| Ok(returned.clone()));
        let extractor = Extractor::new(Arc::new(spans));

        let found = extractor
            .extract(text, Some(DocumentType::TaxNotice))
            .await
            .unwrap();

        assert_eq!(
            found,
            self::found(&[("tax_year", Attribute::Integer(2024))])
        );
    }

    #[tokio::test(start_paused = true)]
    async fn extract_fails_then_finds_nothing_for_a_while_after_the_model_fails() {
        let mut spans = MockSpanFinder::new();
        spans
            .expect_spans()
            .times(2)
            .returning(|_, _, _, _| Err(ErrorKind::ModelDownloadStatus(503).into()));
        let extractor = Extractor::new(Arc::new(spans));
        let extract = || extractor.extract("Facture", Some(DocumentType::Invoice));

        let err = extract().await.unwrap_err();
        assert!(err.has_context());

        tokio::time::advance(EXTRACTION_RETRY_AFTER - std::time::Duration::from_secs(1)).await;
        assert!(extract().await.unwrap().is_empty());

        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        extract().await.unwrap_err();
    }

    #[test]
    fn attributes_drop_a_span_overlapping_a_better_one_a_decoys_included() {
        let text = "Capital social 10 000 € - Montant total TTC 180,00 €";
        let decoy = INVOICE.targets.len();
        let spans = vec![
            span(text, "Capital social 10 000 €", decoy, 0.9),
            span(text, "10 000 €", 3, 0.8),
            span(text, "180,00 €", 3, 0.7),
        ];

        assert_eq!(
            attributes(text, &INVOICE, spans),
            found(&[("total_amount", euros(18_000))])
        );
    }

    #[test]
    fn attributes_skip_a_span_that_doesnt_read_as_its_type_for_the_next_best() {
        let text = "Facture n° 83 du 14 mars 2024, total 180,00 €";
        let spans = vec![
            span(text, "83", 3, 0.9),
            span(text, "14 mars 2024", 3, 0.8),
            span(text, "180,00 €", 3, 0.7),
        ];

        assert_eq!(
            attributes(text, &INVOICE, spans),
            found(&[("total_amount", euros(18_000))])
        );
    }

    #[test]
    fn attributes_skip_a_span_unparsed_without_blocking_the_text_it_covers() {
        let text = "Date de facture : 14 mars 2024";
        let spans = vec![
            // Not a total: it doesn't keep the date from being found.
            span(text, "14 mars 2024", 3, 0.9),
            span(text, "14 mars 2024", 2, 0.8),
        ];

        assert_eq!(
            attributes(text, &INVOICE, spans),
            found(&[("issued_on", date("2024-03-14"))])
        );
    }

    #[test]
    fn attributes_read_spans_best_first_whatever_their_order() {
        let text = "Total : 150,00 € puis 180,00 €";
        let spans = vec![
            span(text, "150,00 €", 3, 0.4),
            span(text, "180,00 €", 3, 0.9),
        ];

        assert_eq!(
            attributes(text, &INVOICE, spans),
            found(&[("total_amount", euros(18_000))])
        );
    }

    #[test]
    fn attributes_ignore_spans_outside_the_text_or_its_characters_and_unknown_labels() {
        let text = "Montant : 12,00 €";
        let euro = text.find('€').unwrap();
        let spans = vec![
            Span {
                label: 3,
                bytes: 0..text.len() + 1,
                score: 0.9,
            },
            Span {
                label: 3,
                bytes: euro + 1..euro + 2,
                score: 0.8,
            },
            Span {
                label: 99,
                bytes: 0..text.len(),
                score: 0.7,
            },
            span(text, "12,00 €", 3, 0.6),
        ];

        assert_eq!(
            attributes(text, &INVOICE, spans),
            found(&[("total_amount", euros(1_200))])
        );
    }

    #[test]
    fn every_class_stores_valid_attributes_each_under_its_own_name() {
        for class in DocumentType::ALL
            .iter()
            .map(|r#type| class(Some(*r#type)))
            .chain([class(None)])
        {
            let stored: Vec<(String, Attribute)> = class
                .targets
                .iter()
                .flat_map(|target| {
                    let value = match target.kind {
                        Amount => Value::Amount(1),
                        Date => Value::Date("2024-01-01".to_string()),
                        Period => Value::Period {
                            start: "2024-01-01".to_string(),
                            end: "2024-01-31".to_string(),
                        },
                        Year => Value::Year(2024),
                        Text => Value::Text("x".to_string()),
                    };
                    value.into_attributes(target.name)
                })
                .collect();
            let attributes: BTreeMap<String, Attribute> = stored.iter().cloned().collect();

            check_values(&attributes).unwrap();
            assert_eq!(attributes.len(), stored.len(), "{:?}", attributes.keys());
        }
    }

    #[test]
    fn amounts() {
        assert_eq!(parse_amount("1 234,56 €"), Some(123_456));
        assert_eq!(parse_amount("1.234,5"), Some(123_450));
        assert_eq!(parse_amount("1234.56"), Some(123_456));
        assert_eq!(parse_amount("12 €"), Some(1_200));
        assert_eq!(parse_amount("12 000"), Some(1_200_000));
        assert_eq!(parse_amount("-9,58"), Some(-958));
        assert_eq!(parse_amount("9,77 euro(s)"), Some(977));
        assert_eq!(parse_amount("21.00 EUR"), Some(2_100));
        assert_eq!(parse_amount("néant"), None);
        assert_eq!(parse_amount("1 94 03 53 062 239 10"), None);
        assert_eq!(parse_amount("Page 1/1"), None);
        assert_eq!(parse_amount("10 jours"), None);
        assert_eq!(parse_amount("203,49 hrs"), None);
        assert_eq!(parse_amount("25,00 25,00 70 %"), None);
        assert_eq!(parse_amount("2114531502934"), None);
        assert_eq!(parse_amount("83"), None);
        assert_eq!(parse_amount("8 184"), Some(818_400));
        assert_eq!(parse_amount("0 €"), Some(0));
        assert_eq!(parse_amount("€ 12"), Some(1_200));
        assert_eq!(parse_amount("12 eur"), Some(1_200));
    }

    #[test]
    fn amounts_with_non_ascii_digits_are_no_amount() {
        // Full-width digits, as OCR can read them, or other scripts'.
        assert_eq!(parse_amount("1\u{ff12}3,45 €"), None);
        assert_eq!(parse_amount("\u{ff11}\u{ff12} €"), None);
        assert_eq!(parse_amount("12,\u{ff15}0 €"), None);
        assert_eq!(parse_amount("\u{663}\u{662} €"), None);
    }

    #[test]
    fn accounting_format_amounts() {
        assert_eq!(parse_amount("- 1 138,93 €-"), Some(113_893));
        assert_eq!(parse_amount("-1 015,30 €-"), Some(101_530));
        assert_eq!(parse_amount("3,83 €-"), Some(383));
        assert_eq!(parse_amount("-  €-"), None);
        assert_eq!(parse_amount("1 138,93-"), None);
        assert_eq!(parse_amount("83 -"), None);
    }

    #[test]
    fn dates() {
        assert_eq!(parse_date("05/03/2024").as_deref(), Some("2024-03-05"));
        assert_eq!(parse_date("2024-03-05").as_deref(), Some("2024-03-05"));
        assert_eq!(
            parse_date("le 1er janvier 2016").as_deref(),
            Some("2016-01-01")
        );
        assert_eq!(parse_date("15 déc. 13").as_deref(), Some("2013-12-15"));
        assert_eq!(parse_date("mars 2016"), None);
        assert_eq!(parse_date("29/02/2024").as_deref(), Some("2024-02-29"));
        assert_eq!(parse_date("29/02/2023"), None);
        assert_eq!(parse_date("31/04/2024"), None);
        assert_eq!(parse_date("31/12/2024").as_deref(), Some("2024-12-31"));
    }

    #[test]
    fn periods_and_years() {
        let period = |s| parse_period(s).map(|(a, b)| format!("{a}/{b}"));
        assert_eq!(
            period("DU 1/01/2026 au 31/01/2026").as_deref(),
            Some("2026-01-01/2026-01-31")
        );
        assert_eq!(
            period("1/08/2022 au 31 AOUT 2022").as_deref(),
            Some("2022-08-01/2022-08-31")
        );
        assert_eq!(
            period("Mars 2016").as_deref(),
            Some("2016-03-01/2016-03-31")
        );
        assert_eq!(
            period("février 2024").as_deref(),
            Some("2024-02-01/2024-02-29")
        );
        assert_eq!(
            period("1/02/2026 au 31/02/2026").as_deref(),
            Some("2026-02-01/2026-02-28")
        );
        assert_eq!(period("31/02/2026 au 31/03/2026"), None);
        assert_eq!(period("01/02/2026 au 01/01/2026"), None);
        assert_eq!(period("Du 01/05/2022 au 44712"), None);
        assert_eq!(period("0,00 hrs"), None);
        assert_eq!(parse_year("revenus 2014"), Some(2014));
        assert_eq!(parse_year("50065"), None);
    }

    /// The real model's store: `FYDE_TEST_MODELS_DIR` when set, to reuse a
    /// download across runs, else the returned temporary directory.
    fn real_models() -> (tempfile::TempDir, Arc<ModelStore>) {
        let temp = tempfile::tempdir().unwrap();
        let dir = std::env::var_os("FYDE_TEST_MODELS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| temp.path().to_path_buf());
        (temp, Arc::new(ModelStore::new(dir)))
    }

    /// A synthetic payslip, every name and number made up.
    const PAYSLIP_TEXT: &str = "BULLETIN DE PAIE\n\
        Employeur : Atelier Exemple SARL, 12 rue des Lilas, 69000 Lyon\n\
        SIRET : 123 456 789 00012\n\
        Salarié : Camille Martin\n\
        Période du 01/03/2024 au 31/03/2024\n\
        Salaire de base 2 300,00\n\
        Salaire brut total 2 450,00\n\
        Net à payer avant impôt sur le revenu 1 912,35 €";

    /// Downloads GLiNER-X small and extracts the attributes of a synthetic
    /// payslip, printing what it found for tuning.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads a 173 MB model from Hugging Face"]
    async fn gliner_x_small_extracts_the_attributes_of_a_payslip() {
        let (_temp, models) = real_models();
        let extractor = Extractor::new(Arc::new(OnnxGliner::new(
            models,
            std::time::Duration::from_secs(60),
        )));

        let found = extractor
            .extract(PAYSLIP_TEXT, Some(DocumentType::Payslip))
            .await
            .unwrap();

        eprintln!("{found:?}");
        assert_eq!(found.get("net_pay"), Some(&euros(191_235)));
        assert_eq!(found.get("period_start"), Some(&date("2024-03-01")));
        assert_eq!(found.get("period_end"), Some(&date("2024-03-31")));
    }
}
