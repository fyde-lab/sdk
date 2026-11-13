use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use super::super::DocumentType;
use super::super::embedder::Embedder;
use super::super::service::SIMILARITY_FLOOR;
use super::{Classification, FiledDocument, Service};
use crate::{ErrorContext as _, ErrorKind, Result};

/// A [`DocumentType`], described the way such documents read: e5 matches
/// on content, not on label names. A type scores its best description.
pub(super) struct TypeLabel {
    pub(super) r#type: DocumentType,
    pub(super) descriptions: &'static [&'static str],
}

const fn type_label(r#type: DocumentType, descriptions: &'static [&'static str]) -> TypeLabel {
    TypeLabel {
        r#type,
        descriptions,
    }
}

/// Every type a document can be classified as. [`DocumentType::Other`] has
/// no description: a catch-all would attract every hesitant document.
pub(super) const TYPES: &[TypeLabel] = &[
    type_label(
        DocumentType::Invoice,
        &[
            "facture : numéro de facture, date d'émission, désignation des produits ou prestations, montant HT, TVA, total TTC à payer",
        ],
    ),
    type_label(
        DocumentType::Receipt,
        &[
            "reçu de paiement : paiement reçu, payé par carte, montant réglé, merci pour votre achat",
        ],
    ),
    type_label(
        DocumentType::Statement,
        &[
            "relevé de compte : solde en début et fin de période, liste des opérations au débit et au crédit",
        ],
    ),
    type_label(
        DocumentType::ReimbursementStatement,
        &[
            "décompte de remboursement de soins : date des soins, montant remboursé par l'assurance maladie ou la mutuelle, versé sur votre compte",
        ],
    ),
    type_label(
        DocumentType::Payslip,
        &[
            "bulletin de paie : salaire de base, heures, cotisations salariales et patronales, net à payer avant impôt",
        ],
    ),
    type_label(
        DocumentType::Contract,
        &[
            "contrat entre les parties : objet, durée, obligations, conditions de résiliation, fait en deux exemplaires, signatures",
            "contrat de travail entre les soussignés, l'employeur et le salarié : article 1 engagement, période d'essai, rémunération, fait en deux exemplaires originaux",
        ],
    ),
    type_label(
        DocumentType::InsuranceCertificate,
        &[
            "attestation d'assurance : l'assureur certifie que l'assuré est garanti par le contrat pour la période de validité",
        ],
    ),
    type_label(
        DocumentType::InsuranceCard,
        &[
            "carte de tiers payant de la mutuelle : numéro d'adhérent, bénéficiaires, à présenter au pharmacien et aux professionnels de santé",
        ],
    ),
    type_label(
        DocumentType::RightsCertificate,
        &[
            "attestation de droits à l'assurance maladie : bénéficiaires, numéro de sécurité sociale, période d'ouverture des droits",
        ],
    ),
    type_label(
        DocumentType::ProofOfResidence,
        &[
            "justificatif de domicile : attestation d'hébergement ou de résidence à l'adresse indiquée",
        ],
    ),
    type_label(
        DocumentType::Rib,
        &[
            "relevé d'identité bancaire : titulaire du compte, IBAN, BIC, code banque, code guichet, domiciliation",
        ],
    ),
    type_label(
        DocumentType::TaxNotice,
        &[
            "avis d'imposition : impôt sur le revenu, taxe foncière ou taxe d'habitation, montant à payer, date limite de paiement",
        ],
    ),
    type_label(
        DocumentType::TaxReturn,
        &[
            "déclaration des revenus : revenus déclarés, accusé de réception de la télédéclaration, avis de situation déclarative",
        ],
    ),
    type_label(
        DocumentType::TaxCertificate,
        &[
            "attestation fiscale : imprimé fiscal unique, reçu fiscal, sommes à reporter sur la déclaration de revenus, réduction ou crédit d'impôt",
            "reçu au titre des dons : le bénéficiaire certifie avoir reçu du donateur la somme indiquée, ouvrant droit à la réduction d'impôt",
        ],
    ),
    type_label(
        DocumentType::EmploymentCertificate,
        &[
            "attestation employeur ou certificat de travail : l'employeur certifie que le salarié est employé depuis le, en qualité de",
        ],
    ),
    type_label(
        DocumentType::FeeSummary,
        &[
            "récapitulatif annuel des frais bancaires : total des frais et commissions prélevés sur votre compte au cours de l'année",
        ],
    ),
    type_label(
        DocumentType::Booking,
        &[
            "confirmation de réservation : billet, voyageur, date et heure de départ et d'arrivée, hôtel ou séjour réservé",
        ],
    ),
    type_label(
        DocumentType::MedicalDocument,
        &[
            "document médical : ordonnance, prescription, compte rendu de consultation, résultats d'analyses, patient",
        ],
    ),
    type_label(
        DocumentType::Quote,
        &[
            "devis : estimation du prix des travaux ou prestations proposés, validité de l'offre, bon pour accord",
        ],
    ),
    type_label(
        DocumentType::IdDocument,
        &[
            "pièce d'identité : nom, prénoms, date et lieu de naissance, nationalité, numéro du document, date d'expiration",
        ],
    ),
    type_label(
        DocumentType::CivilStatusRecord,
        &[
            "extrait ou copie intégrale d'acte d'état civil : acte de naissance, de mariage ou de décès, officier de l'état civil, mentions marginales",
        ],
    ),
    type_label(
        DocumentType::Letter,
        &[
            "courrier : Madame, Monsieur, objet de la lettre, nous vous prions d'agréer l'expression de nos salutations distinguées",
        ],
    ),
    type_label(
        DocumentType::Acknowledgement,
        &[
            "accusé de réception : nous avons bien reçu votre demande ou votre dossier, numéro de suivi, date de réception",
        ],
    ),
    type_label(
        DocumentType::Form,
        &[
            "formulaire à remplir : cases à cocher, champs à compléter, cadre réservé à l'administration, date et signature",
        ],
    ),
    type_label(
        DocumentType::PaymentSchedule,
        &[
            "échéancier de paiement : dates des prélèvements mensuels, montant de chaque échéance, mensualisation",
        ],
    ),
    type_label(
        DocumentType::Mandate,
        &[
            "mandat de prélèvement SEPA ou procuration : référence unique du mandat, identifiant créancier, le mandant autorise le mandataire",
        ],
    ),
    type_label(
        DocumentType::TermsAndConditions,
        &[
            "conditions générales de vente ou d'utilisation : articles, objet, durée, résiliation, responsabilité, données personnelles",
        ],
    ),
    type_label(
        DocumentType::Brochure,
        &[
            "brochure commerciale : présentation de l'offre, avantages, tarifs, découvrez nos services",
        ],
    ),
    type_label(
        DocumentType::Timesheet,
        &[
            "compte rendu d'activité ou feuille de temps : jours ou heures travaillés par date, mission, validation du client",
        ],
    ),
    type_label(
        DocumentType::InspectionReport,
        &[
            "rapport de diagnostic ou d'inspection : contrôle technique, diagnostic de performance énergétique, amiante, constatations, conclusions",
        ],
    ),
    type_label(
        DocumentType::NotarialDeed,
        &[
            "acte notarié : par-devant maître, notaire, les parties comparaissent, acte authentique, publicité foncière",
        ],
    ),
    type_label(
        DocumentType::Attestation,
        &[
            "attestation sur l'honneur : je soussigné atteste sur l'honneur, fait pour servir et valoir ce que de droit",
        ],
    ),
    type_label(
        DocumentType::PaymentReminder,
        &[
            "relance de paiement : facture impayée, sauf erreur de notre part, montant restant dû, mise en demeure de régler",
        ],
    ),
    type_label(
        DocumentType::Fine,
        &[
            "avis de contravention : amende forfaitaire, infraction constatée, excès de vitesse, stationnement, ANTAI, points retirés",
        ],
    ),
    type_label(
        DocumentType::Warranty,
        &[
            "certificat de garantie : produit garanti, durée de la garantie, numéro de série, conditions d'extension de garantie",
        ],
    ),
    type_label(
        DocumentType::Resume,
        &[
            "curriculum vitae : expérience professionnelle, formation, compétences, langues, centres d'intérêt",
        ],
    ),
    type_label(
        DocumentType::Minutes,
        &[
            "procès-verbal de réunion ou d'assemblée générale : présents, ordre du jour, résolutions adoptées, votes",
        ],
    ),
    type_label(
        DocumentType::Diploma,
        &[
            "diplôme : le titre est délivré à, a obtenu le grade, mention, session, université ou ministère",
        ],
    ),
];

/// How much the best type must score above every other type to be kept.
/// Tuned on a corpus of ~1,200 real French documents: 0.01 types 91% of
/// those it types correctly (85% at 0.005), leaving the rest untyped.
const TYPE_MARGIN: f32 = 0.01;

/// How many of a folder's documents closest to a new one its score
/// averages (see [`folder_scores`]): enough that one document filed there
/// by mistake doesn't draw every document like it, few enough that a
/// folder holding many kinds of papers still matches each of them.
const FOLDER_NEIGHBOURS: usize = 3;

/// How much the best folder must score above every other folder holding
/// documents for a new one to be filed in it, else it stays at the root (À
/// classer). Kept from when folders were matched on their descriptions, not
/// retuned on documents compared with documents yet.
const FOLDER_MARGIN: f32 = 0.01;

/// The score below which a folder isn't a match, even as the only one
/// holding documents: the search's [`SIMILARITY_FLOOR`], which is also what
/// [`crate::DEFAULT_SIMILAR_MIN_CONFIDENCE`] keeps, so a document is only
/// filed with documents [`super::super::Service::similar`] would list as
/// similar to it.
const FOLDER_FLOOR: f32 = SIMILARITY_FLOOR;

/// How much of a transcript is embedded, in bytes of whitespace-collapsed
/// text: the model only reads its first 512 tokens anyway, well within it,
/// so a huge transcript isn't tokenized whole for nothing.
const MAX_TEXT_BYTES: usize = 16 * 1024;

/// The embedding of every description in [`TYPES`], in their order.
pub(super) struct Embedded {
    pub(super) types: Vec<Vec<Vec<f32>>>,
}

/// [`Embedded`] type descriptions per model (see [`Embedder::model`]), each
/// computed on the first classification needing it.
#[derive(Default)]
pub(super) struct DescriptionVectors {
    /// Held while embedding, so concurrent first classifications embed the
    /// descriptions once.
    by_model: tokio::sync::Mutex<HashMap<String, Arc<Embedded>>>,
}

/// The default [`Service`]: embeds a document's text with an injected
/// [`Embedder`], and the type descriptions into an injected
/// [`DescriptionVectors`], then [`classify`]s.
pub(super) struct Classifier {
    embedder: Arc<dyn Embedder>,
    descriptions: Arc<DescriptionVectors>,
}

impl Classifier {
    pub(super) fn new(embedder: Arc<dyn Embedder>, descriptions: Arc<DescriptionVectors>) -> Self {
        Self {
            embedder,
            descriptions,
        }
    }

    /// The type descriptions embedded with the embedder's model, embedding
    /// them unless that's already done: a failure is retried on the next
    /// call.
    async fn type_descriptions(&self) -> Result<Arc<Embedded>> {
        let model = self.embedder.model();
        let mut by_model = self.descriptions.by_model.lock().await;
        if let Some(embedded) = by_model.get(&model) {
            return Ok(embedded.clone());
        }

        let mut types = Vec::new();
        for label in TYPES {
            let mut vectors = Vec::with_capacity(label.descriptions.len());
            for description in label.descriptions {
                vectors.push(self.embed_query(description).await.with_context(|| {
                    "failed to embed the document type descriptions".to_string()
                })?);
            }
            types.push(vectors);
        }
        let embedded = Arc::new(Embedded { types });
        by_model.insert(model, embedded.clone());
        Ok(embedded)
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embedder
            .embed_query(text.to_string())
            .await
            .with_context(|| format!("failed to embed description {text:?}"))
    }
}

#[async_trait]
impl Service for Classifier {
    async fn classify(&self, text: &str, filed: &[FiledDocument]) -> Result<Classification> {
        let text = classified_text(text);
        if text.is_empty() {
            return Ok(Classification {
                r#type: None,
                folder_id: file(filed),
            });
        }

        // First: it loads the model, which embedding the descriptions as
        // queries won't do.
        let document = self
            .embedder
            .embed_passages(vec![text])
            .await
            .context("failed to embed the document's text")?
            .pop()
            .ok_or_else(|| {
                ErrorKind::InvalidModel("the embedding model returned no vector".to_string())
            })?;
        let types = self.type_descriptions().await?;

        Ok(classify(&document, &types, filed))
    }
}

/// The part of `transcript` a document is typed from: its first
/// [`MAX_TEXT_BYTES`] of text, whitespace collapsed (the model's
/// tokenizer splits on whitespace anyway), cut on a character boundary.
/// Empty for a blank transcript.
fn classified_text(transcript: &str) -> String {
    let mut text = String::new();
    for word in transcript.split_whitespace() {
        if text.len() >= MAX_TEXT_BYTES {
            break;
        }
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(word);
    }

    let mut end = text.len().min(MAX_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text
}

/// Classifies the `document` vector: the clearly best type against the
/// embedded type descriptions, and the folder the `filed` documents point
/// to (see [`file()`]).
pub(super) fn classify(
    document: &[f32],
    types: &Embedded,
    filed: &[FiledDocument],
) -> Classification {
    let r#type = decide(
        TYPES
            .iter()
            .zip(&types.types)
            .map(|(label, vectors)| (label.r#type, best_similarity(document, vectors))),
        TYPE_MARGIN,
    );

    Classification {
        r#type,
        folder_id: file(filed),
    }
}

/// The folder a new document belongs in, given how similar the documents
/// already `filed` are to it: the best-scoring folder (see
/// [`folder_scores`]) if it beats every other by [`FOLDER_MARGIN`] and
/// scores at least [`FOLDER_FLOOR`]. `None` without any filed document.
pub(super) fn file(filed: &[FiledDocument]) -> Option<Uuid> {
    let scores = folder_scores(filed);
    decide(scores.iter().copied(), FOLDER_MARGIN).filter(|chosen| {
        scores
            .iter()
            .any(|(id, score)| id == chosen && *score >= FOLDER_FLOOR)
    })
}

/// Each folder holding one of `filed` with its score: the mean similarity
/// to the new document of its [`FOLDER_NEIGHBOURS`] documents closest to
/// it (of all of them, when it holds fewer).
fn folder_scores(filed: &[FiledDocument]) -> Vec<(Uuid, f32)> {
    let mut by_folder: HashMap<Uuid, Vec<f32>> = HashMap::new();
    for document in filed {
        by_folder
            .entry(document.folder_id)
            .or_default()
            .push(document.similarity);
    }

    by_folder
        .into_iter()
        .map(|(folder_id, mut similarities)| {
            similarities.sort_by(|a, b| b.total_cmp(a));
            similarities.truncate(FOLDER_NEIGHBOURS);
            let mean = similarities.iter().sum::<f32>() / similarities.len() as f32;
            (folder_id, mean)
        })
        .collect()
}

/// The cosine similarity of `document` and `description` (both
/// L2-normalized, so their dot product).
fn similarity(document: &[f32], description: &[f32]) -> f32 {
    description.iter().zip(document).map(|(a, b)| a * b).sum()
}

/// The highest cosine similarity of `document` to any of `descriptions`,
/// or `f32::MIN` for none.
pub(super) fn best_similarity(document: &[f32], descriptions: &[Vec<f32>]) -> f32 {
    descriptions
        .iter()
        .map(|description| similarity(document, description))
        .fold(f32::MIN, f32::max)
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::super::embedder::MockEmbedder;
    use super::*;
    use crate::ErrorCode;

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

    #[test]
    fn best_similarity_is_the_highest_dot_product() {
        let descriptions = vec![vec![1.0, 0.0], vec![0.5, 0.5], vec![0.0, 1.0]];

        assert_eq!(best_similarity(&[0.25, 0.75], &descriptions), 0.75);
    }

    #[test]
    fn best_similarity_of_no_description_never_wins() {
        assert_eq!(best_similarity(&[1.0], &[]), f32::MIN);
    }

    #[test]
    fn every_type_but_other_has_exactly_one_entry() {
        for r#type in DocumentType::ALL {
            let entries = TYPES.iter().filter(|label| label.r#type == *r#type).count();
            let expected = usize::from(*r#type != DocumentType::Other);
            assert_eq!(entries, expected, "{type}");
        }
    }

    #[test]
    fn classified_text_collapses_whitespace() {
        assert_eq!(
            classified_text("  Facture\n\n n°12\t TTC  "),
            "Facture n°12 TTC"
        );
    }

    #[test]
    fn classified_text_of_a_blank_transcript_is_empty() {
        assert_eq!(classified_text(" \n\t "), "");
    }

    #[test]
    fn classified_text_is_capped_on_a_character_boundary() {
        // 3-byte characters: the cap falls inside one.
        assert_ne!(MAX_TEXT_BYTES % 3, 0);
        let transcript = format!("{} {}", "€".repeat(MAX_TEXT_BYTES), "suite");

        let text = classified_text(&transcript);

        assert_eq!(text, "€".repeat(MAX_TEXT_BYTES / 3));
    }

    #[test]
    fn classified_text_stops_adding_words_past_the_cap() {
        let transcript = "mot ".repeat(MAX_TEXT_BYTES);

        let text = classified_text(&transcript);

        assert_eq!(text.len(), MAX_TEXT_BYTES);
        assert!(text.starts_with("mot mot"));
    }

    /// Every type description, in [`TYPES`] order, then `extra` texts:
    /// each is embedded along its own axis.
    fn texts(extra: &[String]) -> Vec<String> {
        TYPES
            .iter()
            .flat_map(|label| label.descriptions.iter())
            .map(|description| description.to_string())
            .chain(extra.iter().cloned())
            .collect()
    }

    /// A vector of `dimensions` with `score` along each of `axes`.
    fn along(dimensions: usize, axes: &[(usize, f32)]) -> Vec<f32> {
        let mut vector = vec![0.0; dimensions];
        for (axis, score) in axes {
            vector[*axis] = *score;
        }
        vector
    }

    fn type_axis(r#type: DocumentType) -> usize {
        let label = TYPES.iter().find(|label| label.r#type == r#type).unwrap();
        texts(&[])
            .iter()
            .position(|text| text == label.descriptions[0])
            .unwrap()
    }

    /// The type descriptions embedded along their own axes, out of
    /// `dimensions`.
    fn embedded_types(dimensions: usize) -> Embedded {
        let all = texts(&[]);
        Embedded {
            types: TYPES
                .iter()
                .map(|label| {
                    label
                        .descriptions
                        .iter()
                        .map(|description| {
                            let axis = all.iter().position(|text| text == description).unwrap();
                            along(dimensions, &[(axis, 1.0)])
                        })
                        .collect()
                })
                .collect(),
        }
    }

    #[test]
    fn classify_picks_the_clearly_best_type() {
        let dimensions = texts(&[]).len();
        let document = along(dimensions, &[(type_axis(DocumentType::Invoice), 1.0)]);

        let classification = classify(&document, &embedded_types(dimensions), &[]);

        assert_eq!(classification.r#type, Some(DocumentType::Invoice));
        assert_eq!(classification.folder_id, None);
    }

    #[test]
    fn classify_gives_no_type_when_types_tie() {
        let dimensions = texts(&[]).len();
        let document = along(
            dimensions,
            &[
                (type_axis(DocumentType::Invoice), 1.0),
                (type_axis(DocumentType::Receipt), 1.0),
            ],
        );

        let classification = classify(&document, &embedded_types(dimensions), &[]);

        assert_eq!(classification.r#type, None);
    }

    #[test]
    fn classify_files_by_the_filed_documents() {
        let dimensions = texts(&[]).len();
        let document = along(dimensions, &[(type_axis(DocumentType::Payslip), 1.0)]);
        let folder_id = Uuid::now_v7();

        let classification = classify(
            &document,
            &embedded_types(dimensions),
            &filed(folder_id, &[0.95]),
        );

        assert_eq!(
            classification,
            Classification {
                r#type: Some(DocumentType::Payslip),
                folder_id: Some(folder_id),
            }
        );
    }

    /// A document filed in `folder_id` for each of `similarities`.
    fn filed(folder_id: Uuid, similarities: &[f32]) -> Vec<FiledDocument> {
        similarities
            .iter()
            .map(|similarity| FiledDocument {
                folder_id,
                similarity: *similarity,
            })
            .collect()
    }

    #[test]
    fn file_picks_the_folder_whose_documents_are_the_closest() {
        let (energy, payslips) = (Uuid::now_v7(), Uuid::now_v7());
        let documents = [filed(energy, &[0.88, 0.87]), filed(payslips, &[0.96, 0.95])].concat();

        assert_eq!(file(&documents), Some(payslips));
    }

    #[test]
    fn file_gives_none_without_filed_documents() {
        assert_eq!(file(&[]), None);
    }

    #[test]
    fn file_gives_none_when_two_folders_are_within_the_margin() {
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let documents = [filed(a, &[0.92]), filed(b, &[0.92 - FOLDER_MARGIN * 0.5])].concat();

        assert_eq!(file(&documents), None);
    }

    #[test]
    fn file_keeps_a_folder_beating_the_runner_up_by_the_margin() {
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let documents = [filed(a, &[0.92]), filed(b, &[0.92 - FOLDER_MARGIN * 2.0])].concat();

        assert_eq!(file(&documents), Some(a));
    }

    #[test]
    fn file_files_in_a_lone_folder_only_from_the_floor() {
        let folder_id = Uuid::now_v7();

        assert_eq!(
            file(&filed(folder_id, &[FOLDER_FLOOR + 0.01])),
            Some(folder_id)
        );
        assert_eq!(file(&filed(folder_id, &[FOLDER_FLOOR - 0.01])), None);
    }

    #[test]
    fn file_scores_a_folder_by_its_closest_documents_on_average() {
        // One document like the new one, filed among unrelated ones, draws
        // it less than a folder of documents all like it.
        let (stray, regular) = (Uuid::now_v7(), Uuid::now_v7());
        let documents = [
            filed(stray, &[0.97, 0.81, 0.8, 0.8]),
            filed(regular, &[0.94, 0.93, 0.93, 0.5]),
        ]
        .concat();

        assert_eq!(file(&documents), Some(regular));
    }

    #[test]
    fn file_scores_a_folder_holding_few_documents_by_those_it_holds() {
        let (new, full) = (Uuid::now_v7(), Uuid::now_v7());
        let documents = [filed(new, &[0.95]), filed(full, &[0.9, 0.9, 0.9, 0.9])].concat();

        assert_eq!(file(&documents), Some(new));
    }

    /// An embedder embedding each of `all` texts (see [`texts`]) along its
    /// own axis, and documents as `document`, expecting `passages` document
    /// embeddings and `queries` text ones.
    fn embedder_along_axes(
        all: Vec<String>,
        document: Vec<f32>,
        passages: usize,
        queries: usize,
    ) -> MockEmbedder {
        let mut embedder = MockEmbedder::new();
        embedder.expect_model().return_const("e5.onnx".to_string());
        if passages > 0 {
            embedder
                .expect_embed_passages()
                .times(passages)
                .returning(move |_| Ok(vec![document.clone()]));
        }
        let dimensions = all.len();
        embedder
            .expect_embed_query()
            .times(queries)
            .returning(move |query| {
                let axis = all.iter().position(|text| *text == query).unwrap();
                Ok(along(dimensions, &[(axis, 1.0)]))
            });
        embedder
    }

    fn classifier(embedder: MockEmbedder, descriptions: &Arc<DescriptionVectors>) -> Classifier {
        Classifier::new(Arc::new(embedder), descriptions.clone())
    }

    #[tokio::test]
    async fn classifier_types_the_document_s_text_and_files_it_by_the_filed_documents() {
        let all = texts(&[]);
        let document = along(all.len(), &[(type_axis(DocumentType::Payslip), 1.0)]);
        let mut embedder = embedder_along_axes(all.clone(), Vec::new(), 0, all.len());
        embedder
            .expect_embed_passages()
            .withf(|passages| passages == &["Bulletin de paie net à payer".to_string()])
            .times(1)
            .returning(move |_| Ok(vec![document.clone()]));
        let classifier = classifier(embedder, &Arc::default());
        let (energy, payslips) = (Uuid::now_v7(), Uuid::now_v7());
        let documents = [filed(energy, &[0.86]), filed(payslips, &[0.95, 0.94])].concat();

        let classification = classifier
            .classify("Bulletin de paie\n  net à payer", &documents)
            .await
            .unwrap();

        assert_eq!(
            classification,
            Classification {
                r#type: Some(DocumentType::Payslip),
                folder_id: Some(payslips),
            }
        );
    }

    #[tokio::test]
    async fn classifier_files_a_blank_text_without_embedding_it() {
        let classifier = classifier(MockEmbedder::new(), &Arc::default());
        let folder_id = Uuid::now_v7();

        let classification = classifier
            .classify(" \n ", &filed(folder_id, &[0.95]))
            .await
            .unwrap();

        assert_eq!(
            classification,
            Classification {
                r#type: None,
                folder_id: Some(folder_id),
            }
        );
    }

    #[tokio::test]
    async fn classifier_embeds_each_description_once_across_classifications() {
        let all = texts(&[]);
        let document = along(all.len(), &[(type_axis(DocumentType::Invoice), 1.0)]);
        let embedder = embedder_along_axes(all.clone(), document, 2, all.len());
        let classifier = classifier(embedder, &Arc::default());

        for _ in 0..2 {
            let classification = classifier.classify("Facture EDF", &[]).await.unwrap();
            assert_eq!(classification.r#type, Some(DocumentType::Invoice));
        }
    }

    #[tokio::test]
    async fn classifiers_sharing_descriptions_embed_them_once_per_model() {
        let shared = Arc::<DescriptionVectors>::default();
        let all = texts(&[]);
        let document = along(all.len(), &[(type_axis(DocumentType::Invoice), 1.0)]);
        let first = classifier(
            embedder_along_axes(all.clone(), document.clone(), 1, all.len()),
            &shared,
        );
        let same_model = classifier(
            embedder_along_axes(all.clone(), document.clone(), 1, 0),
            &shared,
        );
        let other_model = {
            let mut embedder = MockEmbedder::new();
            embedder
                .expect_model()
                .return_const("other.onnx".to_string());
            let document = document.clone();
            embedder
                .expect_embed_passages()
                .times(1)
                .returning(move |_| Ok(vec![document.clone()]));
            let dimensions = all.len();
            let texts = all.clone();
            embedder
                .expect_embed_query()
                .times(all.len())
                .returning(move |query| {
                    let axis = texts.iter().position(|text| *text == query).unwrap();
                    Ok(along(dimensions, &[(axis, 1.0)]))
                });
            classifier(embedder, &shared)
        };

        for classifier in [first, same_model, other_model] {
            let classification = classifier.classify("Facture EDF", &[]).await.unwrap();
            assert_eq!(classification.r#type, Some(DocumentType::Invoice));
        }
    }

    #[tokio::test]
    async fn classifier_fails_when_the_document_can_t_be_embedded() {
        let mut embedder = MockEmbedder::new();
        embedder
            .expect_embed_passages()
            .times(1)
            .returning(|_| Err(ErrorKind::ModelDownloadStatus(503).into()));
        let classifier = classifier(embedder, &Arc::default());

        let err = classifier.classify("Facture", &[]).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelDownloadFailed);
        assert!(
            err.to_string()
                .contains("failed to embed the document's text"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn classifier_fails_when_the_model_returns_no_vector() {
        let mut embedder = MockEmbedder::new();
        embedder
            .expect_embed_passages()
            .times(1)
            .returning(|_| Ok(Vec::new()));
        let classifier = classifier(embedder, &Arc::default());

        let err = classifier.classify("Facture", &[]).await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
    }

    #[tokio::test]
    async fn classifier_retries_type_descriptions_that_failed_to_embed() {
        let all = texts(&[]);
        let dimensions = all.len();
        let document = along(dimensions, &[(type_axis(DocumentType::Invoice), 1.0)]);
        let mut embedder = MockEmbedder::new();
        embedder.expect_model().return_const("e5.onnx".to_string());
        embedder
            .expect_embed_passages()
            .times(2)
            .returning(move |_| Ok(vec![document.clone()]));
        let queries = AtomicUsize::new(0);
        embedder
            .expect_embed_query()
            .times(1 + all.len())
            .returning(move |query| {
                if queries.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(ErrorKind::ModelNotDownloaded("e5".to_string()).into());
                }
                let axis = all.iter().position(|text| *text == query).unwrap();
                Ok(along(dimensions, &[(axis, 1.0)]))
            });
        let classifier = classifier(embedder, &Arc::default());

        let err = classifier.classify("Facture", &[]).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("failed to embed the document type descriptions"),
            "{err}"
        );
        let classification = classifier.classify("Facture", &[]).await.unwrap();

        assert_eq!(classification.r#type, Some(DocumentType::Invoice));
    }

    /// A synthetic document, the proposed layout's French folder it's filed
    /// in (or should be), and its type (`None`: not checked, no type fitting
    /// it).
    struct Case {
        text: &'static str,
        folder: &'static str,
        r#type: Option<DocumentType>,
    }

    const fn case(text: &'static str, folder: &'static str, r#type: Option<DocumentType>) -> Case {
        Case {
            text,
            folder,
            r#type,
        }
    }

    /// Made-up documents, one per top-level folder at least, plus the
    /// `doc-ai-poc` corpus' weak spots: local taxes (filed under income tax
    /// half of the time) and employment contracts.
    const CASES: &[Case] = &[
        case(
            "Électricité de France — Facture n° 30012345678 du 14/03/2025. Période du 12/01/2025 au 11/03/2025. \
             Relevé du compteur Linky : index 45210 kWh. Consommation : 612 kWh. Abonnement option base 9 kVA : 31,46 €. \
             Consommation : 154,32 €. Taxes et contributions : 22,10 €. Montant total TTC : 207,88 €, prélevé le 28/03/2025.",
            "Énergie",
            Some(DocumentType::Invoice),
        ),
        case(
            "Quittance de loyer — mois de mars 2025. Je soussigné Jean Martin, propriétaire du logement situé \
             12 rue des Lilas, 69003 Lyon, déclare avoir reçu de Madame Claire Durand, locataire, la somme de \
             850,00 € au titre du paiement du loyer (780,00 €) et des charges (70,00 €) pour la période du 01/03/2025 \
             au 31/03/2025, et lui en donne quittance, sous réserve de tous mes droits.",
            "Loyer et charges",
            None,
        ),
        case(
            "Free Mobile — Votre facture du mois de février 2025. Ligne 06 12 34 56 78. Forfait Free 5G 150 Go : \
             19,99 €. Appels, SMS et MMS illimités en France métropolitaine. Données consommées : 42,3 Go. \
             Communications hors forfait : 0,00 €. Total TTC : 19,99 €, prélevé sur votre compte le 05/03/2025.",
            "Mobile",
            Some(DocumentType::Invoice),
        ),
        case(
            "Banque Populaire — Relevé de compte courant n° 12345678901 du 01/02/2025 au 28/02/2025. \
             Solde créditeur au 01/02/2025 : 1 254,30 €. 03/02 CB CARREFOUR MARKET 54,20. 05/02 VIR SEPA SALAIRE \
             FEVRIER +2 310,00. 10/02 PRLV SEPA EDF 207,88. 15/02 CB SNCF 89,00. 20/02 RETRAIT DAB 60,00. \
             Total des opérations : débit 411,08, crédit 2 310,00. Nouveau solde créditeur au 28/02/2025 : 3 153,22 €.",
            "Banque",
            Some(DocumentType::Statement),
        ),
        case(
            "Direction générale des Finances publiques — Avis d'impôt 2025 sur les revenus de l'année 2024. \
             Impôt sur le revenu. Nombre de parts : 2,5. Revenu fiscal de référence : 52 340 €. Revenu brut global : \
             58 120 €. Impôt sur le revenu net : 3 412 €. Retenue à la source prélevée en 2024 : 3 250 €. \
             Reste à payer : 162 €, prélevé en 4 fois à partir de septembre.",
            "Impôt sur le revenu",
            Some(DocumentType::TaxNotice),
        ),
        case(
            "Direction générale des Finances publiques — Avis d'impôt 2025 Taxes foncières. Propriétés bâties. \
             Commune de Villeurbanne. Adresse du bien : 12 rue des Lilas. Base d'imposition : 2 140 €. \
             Taux communal 45,12 %, taxe d'enlèvement des ordures ménagères 8,10 %. Montant de votre impôt : \
             1 138 €. Date limite de paiement : 15/10/2025.",
            "Taxe foncière",
            Some(DocumentType::TaxNotice),
        ),
        case(
            "Avis de taxe d'habitation 2024 sur les résidences secondaires et autres locaux meublés non affectés \
             à l'habitation principale. Commune d'Annecy. Valeur locative brute : 3 980 €. Taux voté par la \
             commune : 18,62 %. Majoration résidences secondaires : 60 %. Montant à payer : 1 186 €.",
            "Taxe d'habitation",
            Some(DocumentType::TaxNotice),
        ),
        case(
            "L'Assurance Maladie — Relevé de vos remboursements de soins. Assuré : Claire Durand. \
             Date des soins 04/02/2025, consultation médecin généraliste, base de remboursement 30,00 €, taux 70 %, \
             montant remboursé 20,00 €, participation forfaitaire 2,00 €. Date des soins 11/02/2025, pharmacie, \
             montant remboursé 12,45 €. Total versé sur votre compte le 14/02/2025 : 30,45 €.",
            "Assurance maladie",
            Some(DocumentType::ReimbursementStatement),
        ),
        case(
            "Dr Sophie Lambert, médecin généraliste, 3 place Bellecour, Lyon. Ordonnance du 04/02/2025 pour \
             Mme Claire Durand. Paracétamol 1 g : un comprimé trois fois par jour pendant 5 jours. \
             Ibuprofène 400 mg : si douleur, sans dépasser 3 par jour. À renouveler une fois.",
            "Soins",
            Some(DocumentType::MedicalDocument),
        ),
        case(
            "Bulletin de paie — Période du 01/02/2025 au 28/02/2025. Employeur : Société Exemple SAS, SIRET \
             123 456 789 00012. Salarié : Claire Durand, ingénieure, coefficient 130. Salaire de base 3 200,00 €. \
             Sécurité sociale maladie, vieillesse, retraite complémentaire, CSG déductible et non déductible. \
             Total des cotisations salariales : 712,40 €. Net à payer avant impôt sur le revenu : 2 487,60 €. \
             Impôt sur le revenu prélevé à la source : 124,38 €. Net payé : 2 363,22 €. Net imposable : 2 581,20 €.",
            "Bulletins de paie",
            Some(DocumentType::Payslip),
        ),
        case(
            "CONTRAT DE TRAVAIL À DURÉE INDÉTERMINÉE. Entre les soussignés : la société Exemple SAS, au capital \
             de 10 000 €, représentée par M. Paul Bernard, gérant, ci-après l'employeur, et Mme Claire Durand, \
             demeurant à Lyon, ci-après la salariée. Article 1 — Engagement : la salariée est engagée à compter du \
             1er avril 2025 en qualité d'ingénieure logiciel, statut cadre. Article 2 — Période d'essai de quatre mois. \
             Article 3 — Rémunération annuelle brute de 48 000 €. Article 4 — Durée du travail : forfait de 218 jours. \
             Article 5 — Convention collective Syntec. Fait à Lyon, le 15 mars 2025, en deux exemplaires originaux.",
            "Contrats et attestations",
            Some(DocumentType::Contract),
        ),
        case(
            "Caisse d'allocations familiales du Rhône — Notification de vos droits. Allocataire : Claire Durand. \
             À compter de mars 2025, vous bénéficiez des allocations familiales pour vos deux enfants à charge : \
             141,99 € par mois, et de l'aide personnalisée au logement : 112,00 € par mois, versées le 5 de chaque mois.",
            "Prestations",
            None,
        ),
        case(
            "SNCF Connect — Votre e-billet. Aller simple le 18/04/2025. Départ Paris Gare de Lyon 08h04, arrivée \
             Lyon Part-Dieu 10h00. TGV INOUI n° 6603, voiture 12, place 45, 2nde classe, fenêtre. Passager : \
             Claire Durand, carte Avantage Adulte. Prix payé : 49,00 €. Référence de dossier : QWERTY.",
            "Train",
            Some(DocumentType::Booking),
        ),
        case(
            "Amazon.fr — Facture. Numéro de commande 402-1234567-8901234, passée le 02/03/2025. Articles commandés : \
             1 × Bouilloire électrique 1,7 L, 29,99 €. 2 × Câble USB-C, 9,99 €. Livraison gratuite à Claire Durand, \
             12 rue des Lilas, Lyon. Total HT 41,64 €, TVA 20 % 8,33 €, total TTC 49,97 €. Payé par carte Visa.",
            "En ligne",
            Some(DocumentType::Invoice),
        ),
        case(
            "OVHcloud — Facture FR12345678 du 01/03/2025. Désignation : renouvellement du nom de domaine \
             exemple.fr pour 12 mois, 7,79 € HT. Serveur privé virtuel VPS Starter, 1 vCore, 2 Go, mars 2025, \
             4,20 € HT. Total HT 11,99 €, TVA 20 % 2,40 €, total TTC 14,39 €.",
            "Logiciels et cloud",
            Some(DocumentType::Invoice),
        ),
        case(
            "Reçu au titre des dons à certains organismes d'intérêt général, articles 200 et 238 bis du code \
             général des impôts. Association Les Restaurants du Cœur, reconnue d'utilité publique. Donateur : \
             Claire Durand, Lyon. Somme de 150,00 € versée le 12/12/2024 par virement, au titre d'un don manuel. \
             Le bénéficiaire certifie sur l'honneur que les dons ouvrent droit à la réduction d'impôt.",
            "Dons",
            Some(DocumentType::TaxCertificate),
        ),
    ];

    /// More made-up documents, each of a kind one of [`CASES`] is, filed in
    /// the same folder: half from the same issuer, half from another one.
    const NEW_CASES: &[Case] = &[
        case(
            "Électricité de France — Facture n° 30012398765 du 14/05/2025. Période du 12/03/2025 au 11/05/2025. \
             Relevé du compteur Linky : index 45698 kWh. Consommation : 488 kWh. Abonnement option base 9 kVA : 31,46 €. \
             Consommation : 123,05 €. Taxes et contributions : 18,72 €. Montant total TTC : 173,23 €, prélevé le 28/05/2025.",
            "Énergie",
            None,
        ),
        case(
            "ENGIE — Votre facture de gaz naturel du 03/02/2025. Point de comptage et d'estimation n° GI012345. \
             Index relevé : 18 452 m³, soit 1 874 kWh consommés du 01/12/2024 au 31/01/2025. Abonnement : 24,10 €. \
             Consommation de gaz : 168,40 €. Taxe intérieure sur la consommation de gaz naturel : 30,92 €. Total TTC à payer : 223,42 €.",
            "Énergie",
            None,
        ),
        case(
            "Quittance de loyer — mois d'avril 2025. Je soussigné Jean Martin, propriétaire du logement situé \
             12 rue des Lilas, 69003 Lyon, déclare avoir reçu de Madame Claire Durand, locataire, la somme de \
             850,00 € au titre du paiement du loyer (780,00 €) et des charges (70,00 €) pour la période du 01/04/2025 \
             au 30/04/2025, et lui en donne quittance.",
            "Loyer et charges",
            None,
        ),
        case(
            "Bouygues Telecom — Facture du 08/03/2025. Ligne mobile 07 98 76 54 32, forfait Sensation 100 Go : 24,99 €. \
             Appels et SMS illimités, 100 Go d'internet mobile en France, 15 Go depuis l'Europe. Hors forfait : 1,20 € \
             (appel vers un numéro surtaxé). Montant total TTC : 26,19 €, prélevé le 18/03/2025.",
            "Mobile",
            None,
        ),
        case(
            "Banque Populaire — Relevé de compte courant n° 12345678901 du 01/03/2025 au 31/03/2025. \
             Solde créditeur au 01/03/2025 : 3 153,22 €. 04/03 CB MONOPRIX 38,70. 05/03 VIR SEPA SALAIRE \
             MARS +2 310,00. 10/03 PRLV SEPA FREE MOBILE 19,99. 22/03 CB FNAC 129,00. 27/03 RETRAIT DAB 40,00. \
             Total des opérations : débit 227,69, crédit 2 310,00. Nouveau solde créditeur au 31/03/2025 : 5 235,53 €.",
            "Banque",
            None,
        ),
        case(
            "Direction générale des Finances publiques — Avis d'impôt 2024 sur les revenus de l'année 2023. \
             Impôt sur le revenu. Nombre de parts : 2,5. Revenu fiscal de référence : 49 870 €. Revenu brut global : \
             55 410 €. Impôt sur le revenu net : 3 104 €. Retenue à la source prélevée en 2023 : 3 180 €. \
             Montant restitué : 76 €, versé sur votre compte bancaire.",
            "Impôt sur le revenu",
            None,
        ),
        case(
            "Direction générale des Finances publiques — Avis d'impôt 2024 Taxes foncières. Propriétés bâties. \
             Commune de Villeurbanne. Adresse du bien : 12 rue des Lilas. Base d'imposition : 2 080 €. \
             Taux communal 44,05 %, taxe d'enlèvement des ordures ménagères 8,10 %. Montant de votre impôt : \
             1 085 €. Date limite de paiement : 15/10/2024.",
            "Taxe foncière",
            None,
        ),
        case(
            "L'Assurance Maladie — Relevé de vos remboursements de soins. Assurée : Claire Durand. \
             Date des soins 12/03/2025, consultation chez un médecin spécialiste, base de remboursement 50,00 €, \
             taux 70 %, montant remboursé 33,00 €, participation forfaitaire 2,00 €. Total versé sur votre compte le 17/03/2025 : 33,00 €.",
            "Assurance maladie",
            None,
        ),
        case(
            "Bulletin de paie — Période du 01/03/2025 au 31/03/2025. Employeur : Société Exemple SAS, SIRET \
             123 456 789 00012. Salarié : Claire Durand, ingénieure, coefficient 130. Salaire de base 3 200,00 €. \
             Prime exceptionnelle 300,00 €. Total des cotisations salariales : 790,15 €. Net à payer avant impôt \
             sur le revenu : 2 709,85 €. Impôt sur le revenu prélevé à la source : 135,49 €. Net payé : 2 574,36 €.",
            "Bulletins de paie",
            None,
        ),
        case(
            "Caisse d'allocations familiales du Rhône — Attestation de paiement. Allocataire : Claire Durand. \
             Nous vous avons versé en avril 2025 : allocations familiales 141,99 €, aide personnalisée au logement \
             112,00 €. Cette attestation peut servir de justificatif auprès des organismes qui vous la demandent.",
            "Prestations",
            None,
        ),
        case(
            "SNCF Connect — Votre e-billet. Aller simple le 02/06/2025. Départ Lyon Part-Dieu 17h04, arrivée \
             Marseille Saint-Charles 18h45. TGV INOUI n° 6177, voiture 5, place 82, 2nde classe. Passager : \
             Claire Durand. Prix payé : 35,00 €. Référence de dossier : AZERTY.",
            "Train",
            None,
        ),
        case(
            "Cdiscount — Facture de votre commande n° 2503151234ABC du 15/03/2025. Article : aspirateur sans fil \
             25,2 V, quantité 1, 149,99 € TTC. Frais de livraison : 4,99 €. Adresse de livraison : Claire Durand, \
             12 rue des Lilas, Lyon. Total HT 129,15 €, TVA 20 % 25,83 €, total TTC 154,98 €. Réglé par carte bancaire.",
            "En ligne",
            None,
        ),
        case(
            "OVHcloud — Facture FR12398765 du 01/04/2025. Désignation : serveur privé virtuel VPS Starter, \
             1 vCore, 2 Go, avril 2025, 4,20 € HT. Sauvegarde automatique, 1,00 € HT. Total HT 5,20 €, \
             TVA 20 % 1,04 €, total TTC 6,24 €.",
            "Logiciels et cloud",
            None,
        ),
        case(
            "Reçu fiscal — Fondation Abbé Pierre pour le logement des défavorisés, reconnue d'utilité publique. \
             Reçu au titre des dons, article 200 du code général des impôts. Donatrice : Claire Durand, Lyon. \
             Montant du don : 80,00 €, versé le 05/11/2024 par prélèvement. Vous bénéficiez d'une réduction d'impôt de 75 %.",
            "Dons",
            None,
        ),
    ];

    /// Made-up documents of kinds no folder holds.
    const UNRELATED: &[&str] = &[
        "Recette du gâteau au chocolat : faire fondre 200 g de chocolat noir avec 150 g de beurre. Ajouter \
         150 g de sucre, 4 œufs puis 50 g de farine. Verser dans un moule beurré et cuire 25 minutes à 180 °C.",
        "Carnet de vaccination du chien Rex, berger australien né le 03/05/2021, puce électronique \
         250269812345678. Vaccin CHPPiL et rage le 12/06/2024, rappel prévu le 12/06/2025. Vermifuge le 01/09/2024. \
         Clinique vétérinaire des Lilas, Dr Morel.",
    ];

    /// The first characters of `text`, to tell which document a line is about.
    fn beginning(text: &str) -> String {
        text.chars().take(60).collect()
    }

    /// multilingual-e5-small (see `FYDE_TEST_MODELS_DIR`), downloaded to
    /// `temp` unless that's set.
    fn multilingual_e5(temp: &tempfile::TempDir) -> Arc<dyn Embedder> {
        let dir = std::env::var_os("FYDE_TEST_MODELS_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| temp.path().to_path_buf());
        super::super::super::init_embedder(Arc::new(crate::tools::models::ModelStore::new(dir)))
    }

    /// Downloads multilingual-e5-small (see `FYDE_TEST_MODELS_DIR`) and
    /// checks it types every one of [`CASES`] as expected.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads a 135 MB model from Hugging Face"]
    async fn multilingual_e5_types_synthetic_documents() {
        let temp = tempfile::tempdir().unwrap();
        let classifier = Classifier::new(multilingual_e5(&temp), Arc::default());

        let mut wrong = Vec::new();
        for case in CASES {
            let classification = classifier.classify(case.text, &[]).await.unwrap();
            let r#type = case.r#type.or(classification.r#type);
            println!("{:?} <- {}", classification.r#type, beginning(case.text));
            if classification.r#type != r#type {
                wrong.push(format!(
                    "{:?} instead of {type:?} <- {}",
                    classification.r#type,
                    beginning(case.text)
                ));
            }
        }

        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// Downloads multilingual-e5-small (see `FYDE_TEST_MODELS_DIR`), files
    /// each of [`CASES`] in its folder of the French proposed layout, then
    /// checks every one of [`NEW_CASES`] is filed with the documents like it,
    /// and every one of [`UNRELATED`] nowhere.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads a 135 MB model from Hugging Face"]
    async fn multilingual_e5_files_new_documents_with_the_documents_like_them() {
        let temp = tempfile::tempdir().unwrap();
        let embedder = multilingual_e5(&temp);
        let folders = crate::domains::folders::proposed_layout_folders("fr");
        let id_of = |name: &str| {
            folders
                .iter()
                .find(|folder| folder.name == name)
                .unwrap_or_else(|| panic!("no folder {name:?}"))
                .id
        };
        let name_of = |id: Option<Uuid>| {
            id.and_then(|id| folders.iter().find(|folder| folder.id == id))
                .map(|folder| folder.name.as_str())
        };
        let embed = async |text: &str| {
            embedder
                .embed_passages(vec![text.to_string()])
                .await
                .unwrap()
                .pop()
                .unwrap()
        };
        let mut vault = Vec::new();
        for case in CASES {
            vault.push((id_of(case.folder), embed(case.text).await));
        }
        let filed = |document: &[f32]| -> Vec<FiledDocument> {
            vault
                .iter()
                .map(|(folder_id, vector)| FiledDocument {
                    folder_id: *folder_id,
                    similarity: similarity(document, vector),
                })
                .collect()
        };

        let mut wrong = Vec::new();
        let expected = NEW_CASES
            .iter()
            .map(|case| (case.text, Some(case.folder)))
            .chain(UNRELATED.iter().map(|text| (*text, None)));
        for (text, folder) in expected {
            let filed = filed(&embed(text).await);
            let mut scores: Vec<(Option<&str>, f32)> = folder_scores(&filed)
                .into_iter()
                .map(|(id, score)| (name_of(Some(id)), score))
                .collect();
            scores.sort_by(|(_, a), (_, b)| b.total_cmp(a));
            let chosen = name_of(file(&filed));
            println!("{chosen:?} {:?} <- {}", &scores[..2], beginning(text));
            if chosen != folder {
                wrong.push(format!(
                    "{chosen:?} instead of {folder:?} <- {}",
                    beginning(text)
                ));
            }
        }

        assert!(wrong.is_empty(), "{wrong:#?}");
    }
}
