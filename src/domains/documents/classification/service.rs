use std::sync::Arc;

use async_trait::async_trait;

use super::super::gliner::{Span, SpanFinder};
use super::super::{Category, DocumentType, Subcategory};
use super::{Classification, Service};
use crate::{ErrorContext as _, Result};

/// A `(Category, Subcategory)` pair, with the cues that give it away.
pub(super) struct Label {
    pub(super) category: Category,
    /// `None` where no subcategory fits (a category alone is allowed).
    pub(super) subcategory: Option<Subcategory>,
    /// What such documents contain, as GLiNER entity labels: short noun
    /// phrases naming a span of the text, not descriptions of the whole
    /// document. A label scores its best cue's best span.
    pub(super) cues: &'static [&'static str],
}

const fn label(
    category: Category,
    subcategory: Option<Subcategory>,
    cues: &'static [&'static str],
) -> Label {
    Label {
        category,
        subcategory,
        cues,
    }
}

/// Every label a document can be classified under. Two mappings are
/// deliberate: local taxes are [`Category::Taxes`] alone, and rent and
/// co-ownership charges [`Category::Housing`] alone.
pub(super) const LABELS: &[Label] = &[
    label(
        Category::Housing,
        Some(Subcategory::Energy),
        &["consommation d'électricité ou de gaz"],
    ),
    label(
        Category::Housing,
        Some(Subcategory::Water),
        &["consommation d'eau"],
    ),
    label(
        Category::Housing,
        Some(Subcategory::Internet),
        &["abonnement box internet"],
    ),
    label(
        Category::Housing,
        Some(Subcategory::HomeInsurance),
        &["assurance habitation"],
    ),
    label(
        Category::Housing,
        Some(Subcategory::HomeServices),
        &["travaux ou service à domicile"],
    ),
    label(
        Category::Housing,
        None,
        &["loyer ou charges de copropriété"],
    ),
    label(
        Category::Telecom,
        Some(Subcategory::Mobile),
        &["forfait mobile"],
    ),
    label(
        Category::Money,
        Some(Subcategory::Banking),
        &["solde du compte bancaire"],
    ),
    label(
        Category::Money,
        Some(Subcategory::Savings),
        &["livret d'épargne ou assurance vie"],
    ),
    label(
        Category::Money,
        Some(Subcategory::Gaming),
        &["pari ou gain de jeu d'argent"],
    ),
    label(
        Category::Taxes,
        Some(Subcategory::IncomeTax),
        &["impôt sur le revenu"],
    ),
    label(
        Category::Taxes,
        Some(Subcategory::TaxCertificates),
        &["attestation fiscale"],
    ),
    label(
        Category::Taxes,
        None,
        &["taxe foncière ou taxe d'habitation"],
    ),
    label(
        Category::Health,
        Some(Subcategory::HealthInsurance),
        &["remboursement de l'assurance maladie"],
    ),
    label(
        Category::Health,
        Some(Subcategory::ComplementaryHealth),
        &["remboursement de la mutuelle"],
    ),
    label(Category::Health, Some(Subcategory::Care), &["acte médical"]),
    label(
        Category::Work,
        Some(Subcategory::Payslips),
        &["salaire net à payer"],
    ),
    label(
        Category::Work,
        Some(Subcategory::Employment),
        &["contrat de travail ou attestation employeur"],
    ),
    label(
        Category::Work,
        Some(Subcategory::HouseholdEmployer),
        &["salaire d'un employé à domicile"],
    ),
    label(
        Category::Family,
        Some(Subcategory::Benefits),
        &["prestation de la CAF"],
    ),
    label(
        Category::Family,
        Some(Subcategory::ChildcareSchool),
        &["frais de crèche, de cantine ou de scolarité"],
    ),
    label(
        Category::Transport,
        Some(Subcategory::Train),
        &["trajet en train"],
    ),
    label(
        Category::Transport,
        Some(Subcategory::Flight),
        &["vol en avion"],
    ),
    label(
        Category::Transport,
        Some(Subcategory::Road),
        &["péage, carburant ou stationnement"],
    ),
    label(
        Category::Transport,
        Some(Subcategory::Ride),
        &["course de VTC ou de taxi"],
    ),
    label(
        Category::Transport,
        Some(Subcategory::Travel),
        &["réservation d'hôtel ou de séjour"],
    ),
    label(
        Category::Shopping,
        Some(Subcategory::Online),
        &["commande en ligne"],
    ),
    label(
        Category::Shopping,
        Some(Subcategory::Pets),
        &["soin vétérinaire ou produit pour animal"],
    ),
    label(
        Category::Shopping,
        Some(Subcategory::Groceries),
        &["courses alimentaires"],
    ),
    label(
        Category::Shopping,
        Some(Subcategory::FoodDelivery),
        &["livraison de repas"],
    ),
    label(
        Category::Subscriptions,
        Some(Subcategory::Media),
        &["abonnement streaming ou presse"],
    ),
    label(
        Category::Subscriptions,
        Some(Subcategory::CloudSoftware),
        &["hébergement, nom de domaine ou logiciel"],
    ),
    label(Category::Donations, None, &["don à une association"]),
];

/// A [`DocumentType`], with the cues that give it away (see
/// [`Label::cues`]).
pub(super) struct TypeLabel {
    pub(super) r#type: DocumentType,
    pub(super) cues: &'static [&'static str],
}

const fn type_label(r#type: DocumentType, cues: &'static [&'static str]) -> TypeLabel {
    TypeLabel { r#type, cues }
}

/// Every type a document can be classified as. [`DocumentType::Other`] has
/// no cue: a catch-all would attract every hesitant document.
pub(super) const TYPES: &[TypeLabel] = &[
    type_label(DocumentType::Invoice, &["numéro de facture"]),
    type_label(DocumentType::Receipt, &["reçu de paiement"]),
    type_label(DocumentType::Statement, &["relevé de compte"]),
    type_label(
        DocumentType::ReimbursementStatement,
        &["décompte de remboursement de soins"],
    ),
    type_label(DocumentType::Payslip, &["bulletin de paie"]),
    type_label(DocumentType::Contract, &["contrat signé entre les parties"]),
    type_label(
        DocumentType::InsuranceCertificate,
        &["attestation d'assurance"],
    ),
    type_label(DocumentType::InsuranceCard, &["carte de tiers payant"]),
    type_label(
        DocumentType::RightsCertificate,
        &["attestation de droits à l'assurance maladie"],
    ),
    type_label(
        DocumentType::ProofOfResidence,
        &["justificatif de domicile"],
    ),
    type_label(DocumentType::Rib, &["IBAN"]),
    type_label(DocumentType::TaxNotice, &["avis d'imposition"]),
    type_label(DocumentType::TaxReturn, &["déclaration des revenus"]),
    type_label(
        DocumentType::TaxCertificate,
        &["attestation fiscale ou reçu fiscal"],
    ),
    type_label(
        DocumentType::EmploymentCertificate,
        &["certificat de travail ou attestation employeur"],
    ),
    type_label(
        DocumentType::FeeSummary,
        &["récapitulatif annuel des frais bancaires"],
    ),
    type_label(DocumentType::Booking, &["numéro de réservation"]),
    type_label(DocumentType::MedicalDocument, &["ordonnance médicale"]),
];

/// The lowest span score a cue counts at: below it, the cue isn't in the
/// text at all.
const THRESHOLD: f32 = 0.4;

/// How many of a transcript's first words are read: what gives a document
/// away (its title, its issuer, its first lines) is at its top, and reading
/// within one GLiNER window keeps it to one model run per call.
const MAX_WORDS: usize = 300;

/// How much a category's best label must score above every other
/// category's for the document to be filed under it.
const CATEGORY_MARGIN: f32 = 0.05;

/// How much the best label within the chosen category must score above
/// that category's other labels for its subcategory to be kept.
const SUBCATEGORY_MARGIN: f32 = 0.05;

/// How much the best type must score above every other type to be kept.
const TYPE_MARGIN: f32 = 0.05;

/// The default [`Service`]: looks for every cue of [`LABELS`], then of
/// [`TYPES`], with an injected [`SpanFinder`], then [`classify`]s.
pub(super) struct Classifier {
    spans: Arc<dyn SpanFinder>,
}

impl Classifier {
    pub(super) fn new(spans: Arc<dyn SpanFinder>) -> Self {
        Self { spans }
    }

    /// The best score of each of `labels` (each a list of cues) among the
    /// spans of `text`: `None` for a label none of whose cues was found.
    async fn scores(
        &self,
        text: &str,
        labels: impl Iterator<Item = &'static [&'static str]>,
    ) -> Result<Vec<Option<f32>>> {
        let mut cues = Vec::new();
        let mut owners = Vec::new();
        let mut count = 0;
        for (index, label_cues) in labels.enumerate() {
            for cue in label_cues {
                cues.push(cue.to_string());
                owners.push(index);
            }
            count = index + 1;
        }

        let spans = self
            .spans
            .spans(text.to_string(), cues, THRESHOLD, MAX_WORDS)
            .await?;
        Ok(best_scores(&spans, &owners, count))
    }
}

#[async_trait]
impl Service for Classifier {
    async fn classify(&self, text: &str) -> Result<Classification> {
        if text.trim().is_empty() {
            return Ok(Classification::default());
        }

        let labels = self
            .scores(text, LABELS.iter().map(|label| label.cues))
            .await
            .context("failed to look for the category cues")?;
        let types = self
            .scores(text, TYPES.iter().map(|label| label.cues))
            .await
            .context("failed to look for the document type cues")?;

        Ok(classify(&labels, &types))
    }
}

/// The best score of each of `count` labels among `spans`, whose
/// [`Span::label`] is a cue's index, `owners` mapping each cue to its
/// label's: `None` for a label with no span.
fn best_scores(spans: &[Span], owners: &[usize], count: usize) -> Vec<Option<f32>> {
    let mut best = vec![None; count];
    for span in spans {
        let Some(&owner) = owners.get(span.label) else {
            continue;
        };
        let score: &mut Option<f32> = &mut best[owner];
        *score = Some(score.map_or(span.score, |best| best.max(span.score)));
    }
    best
}

/// Classifies a document from the best score of each of [`LABELS`]
/// (`labels`) and [`TYPES`] (`types`), in their order: the category whose
/// best label is clearly best, then the subcategory (or lack of one) of its
/// clearly best label, and the clearly best type (see [`decide`]). Labels
/// with no cue found don't compete.
pub(super) fn classify(labels: &[Option<f32>], types: &[Option<f32>]) -> Classification {
    let found: Vec<(&Label, f32)> = LABELS
        .iter()
        .zip(labels)
        .filter_map(|(label, score)| score.map(|score| (label, score)))
        .collect();

    let category = decide(
        found.iter().map(|(label, score)| (label.category, *score)),
        CATEGORY_MARGIN,
    );
    let subcategory = category.and_then(|category| {
        decide(
            found
                .iter()
                .filter(|(label, _)| label.category == category)
                .map(|(label, score)| (label.subcategory, *score)),
            SUBCATEGORY_MARGIN,
        )
        .flatten()
    });
    let r#type = decide(
        TYPES
            .iter()
            .zip(types)
            .filter_map(|(label, score)| score.map(|score| (label.r#type, score))),
        TYPE_MARGIN,
    );

    Classification {
        category,
        subcategory,
        r#type,
    }
}

/// The choice with the highest score among `scores` (a choice scoring its
/// best when listed more than once), if it beats every other choice by at
/// least `margin`: `None` for a tie or a closer runner-up, or no choice at
/// all. A lone choice has nothing to beat.
pub(super) fn decide<T: Copy + PartialEq>(
    scores: impl IntoIterator<Item = (T, f32)>,
    margin: f32,
) -> Option<T> {
    let mut best: Vec<(T, f32)> = Vec::new();
    for (choice, score) in scores {
        match best.iter_mut().find(|(seen, _)| *seen == choice) {
            Some((_, best)) => *best = best.max(score),
            None => best.push((choice, score)),
        }
    }
    best.sort_by(|(_, a), (_, b)| b.total_cmp(a));

    match best[..] {
        [] => None,
        [(only, _)] => Some(only),
        [(first, a), (_, b), ..] => (a > b && a - b >= margin).then_some(first),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::super::gliner::{MockSpanFinder, OnnxGliner};
    use super::super::super::service::check_categories;
    use super::*;
    use crate::ErrorKind;
    use crate::tools::models::ModelStore;

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Choice {
        A,
        B,
        C,
    }

    #[test]
    fn decide_scores_a_choice_by_its_best_score() {
        let scores = [(Choice::A, 0.5), (Choice::B, 0.6), (Choice::A, 0.75)];

        assert_eq!(decide(scores, 0.125), Some(Choice::A));
    }

    #[test]
    fn decide_picks_the_highest_score_wherever_it_is() {
        let scores = [(Choice::A, 0.25), (Choice::B, 0.75), (Choice::C, 0.5)];

        assert_eq!(decide(scores, 0.125), Some(Choice::B));
    }

    #[test]
    fn decide_keeps_a_choice_beating_the_runner_up_by_exactly_the_margin() {
        let scores = [(Choice::A, 0.75), (Choice::B, 0.5)];

        assert_eq!(decide(scores, 0.25), Some(Choice::A));
    }

    #[test]
    fn decide_gives_none_when_the_runner_up_is_within_the_margin() {
        let scores = [(Choice::A, 0.75), (Choice::B, 0.5), (Choice::C, 0.25)];

        assert_eq!(decide(scores, 0.5), None);
    }

    #[test]
    fn decide_gives_none_for_a_tie_even_without_a_margin() {
        let scores = [(Choice::A, 0.5), (Choice::B, 0.5)];

        assert_eq!(decide(scores, 0.0), None);
    }

    #[test]
    fn decide_gives_none_without_choices() {
        assert_eq!(decide(std::iter::empty::<(Choice, f32)>(), 0.0), None);
    }

    #[test]
    fn decide_keeps_a_lone_choice_whatever_the_margin() {
        let scores = [(Choice::A, 0.1), (Choice::A, 0.2)];

        assert_eq!(decide(scores, 1.0), Some(Choice::A));
    }

    fn span(label: usize, score: f32) -> Span {
        Span {
            label,
            bytes: 0..1,
            score,
        }
    }

    #[test]
    fn best_scores_keeps_each_labels_best_span_across_its_cues() {
        // Cues 0 and 1 are label 0's, cue 2 label 2's; label 1 has none found.
        let spans = [span(0, 0.5), span(1, 0.75), span(2, 0.625), span(0, 0.6)];

        assert_eq!(
            best_scores(&spans, &[0, 0, 2], 3),
            vec![Some(0.75), None, Some(0.625)]
        );
    }

    #[test]
    fn best_scores_ignores_a_span_of_an_unknown_cue() {
        assert_eq!(best_scores(&[span(5, 0.9)], &[0], 1), vec![None]);
    }

    /// Scores for [`classify`]: `found`'s `(index, score)` pairs, every
    /// other of `count` labels not found.
    fn scores(count: usize, found: &[(usize, f32)]) -> Vec<Option<f32>> {
        let mut scores = vec![None; count];
        for (index, score) in found {
            scores[*index] = Some(*score);
        }
        scores
    }

    fn label_index(category: Category, subcategory: Option<Subcategory>) -> usize {
        LABELS
            .iter()
            .position(|label| label.category == category && label.subcategory == subcategory)
            .unwrap()
    }

    fn type_index(r#type: DocumentType) -> usize {
        TYPES
            .iter()
            .position(|label| label.r#type == r#type)
            .unwrap()
    }

    #[test]
    fn classify_picks_the_clearly_best_category_subcategory_and_type() {
        let labels = scores(
            LABELS.len(),
            &[
                (
                    label_index(Category::Housing, Some(Subcategory::Energy)),
                    0.9,
                ),
                (
                    label_index(Category::Housing, Some(Subcategory::Water)),
                    0.5,
                ),
                (
                    label_index(Category::Telecom, Some(Subcategory::Mobile)),
                    0.6,
                ),
            ],
        );
        let types = scores(
            TYPES.len(),
            &[
                (type_index(DocumentType::Invoice), 0.8),
                (type_index(DocumentType::Receipt), 0.45),
            ],
        );

        assert_eq!(
            classify(&labels, &types),
            Classification {
                category: Some(Category::Housing),
                subcategory: Some(Subcategory::Energy),
                r#type: Some(DocumentType::Invoice),
            }
        );
    }

    #[test]
    fn classify_finds_nothing_without_any_cue() {
        assert_eq!(
            classify(&vec![None; LABELS.len()], &vec![None; TYPES.len()]),
            Classification::default()
        );
    }

    #[test]
    fn classify_keeps_the_category_but_not_a_subcategory_too_close_to_call() {
        let labels = scores(
            LABELS.len(),
            &[
                (
                    label_index(Category::Taxes, Some(Subcategory::IncomeTax)),
                    0.8,
                ),
                (
                    label_index(Category::Taxes, Some(Subcategory::TaxCertificates)),
                    0.79,
                ),
                (label_index(Category::Health, Some(Subcategory::Care)), 0.5),
            ],
        );

        let classification = classify(&labels, &vec![None; TYPES.len()]);

        assert_eq!(classification.category, Some(Category::Taxes));
        assert_eq!(classification.subcategory, None);
        assert_eq!(classification.r#type, None);
    }

    #[test]
    fn classify_files_local_taxes_under_taxes_alone() {
        let labels = scores(
            LABELS.len(),
            &[
                (label_index(Category::Taxes, None), 0.9),
                (
                    label_index(Category::Taxes, Some(Subcategory::IncomeTax)),
                    0.6,
                ),
            ],
        );

        let classification = classify(&labels, &vec![None; TYPES.len()]);

        assert_eq!(classification.category, Some(Category::Taxes));
        assert_eq!(classification.subcategory, None);
    }

    #[test]
    fn classify_leaves_the_category_out_when_two_are_too_close_to_call() {
        let labels = scores(
            LABELS.len(),
            &[
                (
                    label_index(Category::Work, Some(Subcategory::Payslips)),
                    0.7,
                ),
                (
                    label_index(Category::Money, Some(Subcategory::Banking)),
                    0.68,
                ),
            ],
        );

        assert_eq!(
            classify(&labels, &vec![None; TYPES.len()]),
            Classification::default()
        );
    }

    #[tokio::test]
    async fn classify_looks_for_every_cue_of_the_labels_then_of_the_types() {
        let mut spans = MockSpanFinder::new();
        let mut sequence = mockall::Sequence::new();
        let label_cues: Vec<String> = LABELS
            .iter()
            .flat_map(|label| label.cues.iter().map(|cue| cue.to_string()))
            .collect();
        let type_cues: Vec<String> = TYPES
            .iter()
            .flat_map(|label| label.cues.iter().map(|cue| cue.to_string()))
            .collect();
        let payslips = label_cues
            .iter()
            .position(|cue| {
                cue == LABELS[label_index(Category::Work, Some(Subcategory::Payslips))].cues[0]
            })
            .unwrap();
        let payslip = type_cues
            .iter()
            .position(|cue| cue == TYPES[type_index(DocumentType::Payslip)].cues[0])
            .unwrap();
        spans
            .expect_spans()
            .withf(move |text, cues, threshold, max_words| {
                text == "Bulletin de paie"
                    && *cues == label_cues
                    && *threshold == THRESHOLD
                    && *max_words == MAX_WORDS
            })
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_, _, _, _| Ok(vec![span(payslips, 0.9)]));
        spans
            .expect_spans()
            .withf(move |_, cues, _, _| *cues == type_cues)
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_, _, _, _| Ok(vec![span(payslip, 0.8)]));

        let classification = Classifier::new(Arc::new(spans))
            .classify("Bulletin de paie")
            .await
            .unwrap();

        assert_eq!(
            classification,
            Classification {
                category: Some(Category::Work),
                subcategory: Some(Subcategory::Payslips),
                r#type: Some(DocumentType::Payslip),
            }
        );
    }

    #[tokio::test]
    async fn classify_never_runs_the_model_for_a_blank_text() {
        // No expectations: the mock panics if it's asked for spans.
        let classifier = Classifier::new(Arc::new(MockSpanFinder::new()));

        assert_eq!(
            classifier.classify(" \n\t ").await.unwrap(),
            Classification::default()
        );
    }

    #[tokio::test]
    async fn classify_fails_with_context_when_the_model_cant_be_fetched() {
        let mut spans = MockSpanFinder::new();
        spans
            .expect_spans()
            .returning(|_, _, _, _| Err(ErrorKind::ModelDownloadStatus(503).into()));

        let err = Classifier::new(Arc::new(spans))
            .classify("Facture")
            .await
            .unwrap_err();

        assert_eq!(err.code(), crate::ErrorCode::ModelDownloadFailed);
        assert!(err.has_context(), "missing context: {err}");
    }

    #[test]
    fn every_label_is_a_valid_category_and_subcategory_pair() {
        for label in LABELS {
            check_categories(Some(label.category), label.subcategory).unwrap();
        }
    }

    #[test]
    fn every_label_and_type_has_cues_none_blank_or_shared() {
        let cues: Vec<&str> = LABELS
            .iter()
            .flat_map(|label| label.cues)
            .chain(TYPES.iter().flat_map(|label| label.cues))
            .copied()
            .collect();

        for label in LABELS {
            assert!(!label.cues.is_empty());
        }
        for label in TYPES {
            assert!(!label.cues.is_empty());
        }
        for cue in &cues {
            assert!(!cue.trim().is_empty());
        }
        let label_cues: Vec<&str> = LABELS
            .iter()
            .flat_map(|label| label.cues)
            .copied()
            .collect();
        let mut unique = label_cues.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), label_cues.len(), "a cue names two labels");
    }

    #[test]
    fn every_category_and_subcategory_has_exactly_one_label() {
        for category in Category::ALL {
            let labels = LABELS.iter().filter(|label| label.category == *category);
            assert!(labels.count() >= 1, "{category} has no label");
        }
        for subcategory in Subcategory::ALL {
            let labels = LABELS
                .iter()
                .filter(|label| label.subcategory == Some(*subcategory));
            assert_eq!(labels.count(), 1, "{subcategory}");
        }
        for category in Category::ALL {
            let labels = LABELS
                .iter()
                .filter(|label| label.category == *category && label.subcategory.is_none());
            assert!(
                labels.count() <= 1,
                "{category} has two labels without a subcategory"
            );
        }
    }

    #[test]
    fn every_type_but_other_has_exactly_one_entry() {
        for r#type in DocumentType::ALL {
            let entries = TYPES.iter().filter(|label| label.r#type == *r#type).count();
            let expected = usize::from(*r#type != DocumentType::Other);
            assert_eq!(entries, expected, "{type}");
        }
    }

    /// Synthetic documents, every name and number made up, each with the
    /// classification it should get.
    const SAMPLES: &[(&str, Classification)] = &[
        (
            "FACTURE ÉLECTRICITÉ\nÉnergie Exemple SA\nPériode du 01/01/2024 au 29/02/2024\n\
             Consommation : 412 kWh relevés sur votre compteur\nAbonnement 6 kVA : 25,40 €\n\
             Montant total TTC à payer : 98,12 €",
            Classification {
                category: Some(Category::Housing),
                subcategory: Some(Subcategory::Energy),
                r#type: Some(DocumentType::Invoice),
            },
        ),
        (
            "BULLETIN DE PAIE\nEmployeur : Exemple SAS\nSalarié : Camille Martin\n\
             Période : mars 2024\nSalaire de base 2 400,00 €\nCotisations salariales 540,00 €\n\
             Net à payer avant impôt sur le revenu : 1 860,00 €",
            Classification {
                category: Some(Category::Work),
                subcategory: Some(Subcategory::Payslips),
                r#type: Some(DocumentType::Payslip),
            },
        ),
        (
            "AVIS D'IMPÔT 2024 SUR LES REVENUS 2023\nDirection générale des Finances publiques\n\
             Revenu fiscal de référence : 32 000 €\nImpôt sur le revenu net : 1 850 €\n\
             Montant prélevé à la source : 1 850 €",
            Classification {
                category: Some(Category::Taxes),
                subcategory: Some(Subcategory::IncomeTax),
                r#type: Some(DocumentType::TaxNotice),
            },
        ),
    ];

    /// Downloads GLiNER-X small (into `FYDE_TEST_MODELS_DIR` when set, to
    /// reuse it across runs) and classifies [`SAMPLES`], printing what each
    /// label and type scored: the cues and margins are tuned with it.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads a 173 MB model from Hugging Face"]
    async fn gliner_x_small_classifies_sample_documents() {
        let temp = tempfile::tempdir().unwrap();
        let dir = std::env::var_os("FYDE_TEST_MODELS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| temp.path().to_path_buf());
        let classifier = Classifier::new(Arc::new(OnnxGliner::new(Arc::new(ModelStore::new(dir)))));

        for (text, expected) in SAMPLES {
            let labels = classifier
                .scores(text, LABELS.iter().map(|label| label.cues))
                .await
                .unwrap();
            let types = classifier
                .scores(text, TYPES.iter().map(|label| label.cues))
                .await
                .unwrap();
            eprintln!("{:?}", text.lines().next());
            for (label, score) in LABELS.iter().zip(&labels) {
                if let Some(score) = score {
                    eprintln!("  {:?}/{:?}: {score}", label.category, label.subcategory);
                }
            }
            for (label, score) in TYPES.iter().zip(&types) {
                if let Some(score) = score {
                    eprintln!("  {:?}: {score}", label.r#type);
                }
            }

            assert_eq!(classify(&labels, &types), *expected, "{text}");
        }
    }
}
