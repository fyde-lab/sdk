use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use super::super::embedder::Embedder;
use super::super::{Category, DocumentType, Subcategory};
use super::{Classification, Service};
use crate::{ErrorContext as _, ErrorKind, Result};

/// A `(Category, Subcategory)` pair, described the way such documents read.
pub(super) struct Label {
    pub(super) category: Category,
    /// `None` where no subcategory fits (a category alone is allowed).
    pub(super) subcategory: Option<Subcategory>,
    /// What such documents contain, phrased like them: e5 matches on
    /// content, not on label names. A label scores its best description.
    pub(super) descriptions: &'static [&'static str],
}

const fn label(
    category: Category,
    subcategory: Option<Subcategory>,
    descriptions: &'static [&'static str],
) -> Label {
    Label {
        category,
        subcategory,
        descriptions,
    }
}

/// Every label a document can be classified under: one per subcategory,
/// plus one for each category that has none.
pub(super) const LABELS: &[Label] = &[
    label(
        Category::Housing,
        Some(Subcategory::Energy),
        &[
            "facture d'électricité ou de gaz : consommation en kWh, abonnement, relevé du compteur, fournisseur d'énergie",
            "échéancier de paiement électricité et gaz, mensualisation, contrat de fourniture d'énergie",
        ],
    ),
    label(
        Category::Housing,
        Some(Subcategory::Water),
        &[
            "facture d'eau : consommation en m³, abonnement eau potable et assainissement, relevé du compteur d'eau",
        ],
    ),
    label(
        Category::Housing,
        Some(Subcategory::HomeInsurance),
        &[
            "assurance habitation multirisque : attestation d'assurance du logement, avis d'échéance, cotisation annuelle",
        ],
    ),
    label(
        Category::Housing,
        Some(Subcategory::HomeServices),
        &[
            "services à domicile : ménage, repassage, jardinage, entretien de la maison, petit bricolage",
        ],
    ),
    label(
        Category::Housing,
        Some(Subcategory::Rent),
        &[
            "quittance de loyer, bail de location, charges locatives, charges de copropriété, appel de fonds du syndic",
        ],
    ),
    label(
        Category::Housing,
        Some(Subcategory::Property),
        &[
            "achat ou vente d'un bien immobilier : compromis de vente, acte de vente, diagnostics, agence immobilière, notaire",
        ],
    ),
    label(
        Category::Housing,
        Some(Subcategory::Works),
        &[
            "travaux dans le logement : devis ou facture d'artisan, rénovation, plomberie, électricité, installation de chauffage",
        ],
    ),
    label(
        Category::Telecom,
        Some(Subcategory::Mobile),
        &[
            "facture de forfait mobile : téléphone portable, appels, SMS, données internet mobile en Go",
        ],
    ),
    label(
        Category::Telecom,
        Some(Subcategory::Internet),
        &["facture box internet : abonnement fibre ou ADSL, téléphone fixe et télévision"],
    ),
    label(
        Category::Money,
        Some(Subcategory::Banking),
        &[
            "relevé de compte bancaire : solde, opérations, virements, prélèvements, paiements par carte",
            "RIB, convention de compte, frais bancaires, mandat de prélèvement SEPA",
        ],
    ),
    label(
        Category::Money,
        Some(Subcategory::Savings),
        &["épargne sur livret : livret A, LDDS, LEP, PEL, relevé du livret, intérêts annuels"],
    ),
    label(
        Category::Money,
        Some(Subcategory::Investments),
        &[
            "placements financiers : assurance vie, PEA, compte-titres, SCPI, relevé de portefeuille, valorisation",
        ],
    ),
    label(
        Category::Money,
        Some(Subcategory::Loans),
        &[
            "prêt immobilier ou crédit à la consommation : offre de prêt, tableau d'amortissement, échéances, taux, capital restant dû",
        ],
    ),
    label(
        Category::Money,
        Some(Subcategory::Gaming),
        &["jeux d'argent : paris sportifs, poker, loterie, casino en ligne, relevé des gains"],
    ),
    label(
        Category::Taxes,
        Some(Subcategory::IncomeTax),
        &[
            "avis d'impôt sur le revenu : revenu fiscal de référence, impôt net, prélèvement à la source",
            "déclaration des revenus en ligne, accusé de réception de télédéclaration, avis de situation déclarative",
            "formulaire 2042 de déclaration des revenus : traitements et salaires, pensions, revenus de capitaux mobiliers, revenus fonciers, charges déductibles, situation du foyer fiscal",
        ],
    ),
    label(
        Category::Taxes,
        Some(Subcategory::TaxCertificates),
        &[
            "imprimé fiscal unique (IFU), attestation fiscale annuelle à reporter sur la déclaration de revenus",
            "attestation fiscale de services à la personne ouvrant droit à crédit d'impôt",
        ],
    ),
    label(
        Category::Taxes,
        Some(Subcategory::PropertyTax),
        &[
            "avis de taxe foncière : propriétés bâties ou non bâties, base d'imposition, taux communal, taxe d'enlèvement des ordures ménagères",
        ],
    ),
    label(
        Category::Taxes,
        Some(Subcategory::HousingTax),
        &[
            "avis de taxe d'habitation : résidence secondaire, valeur locative, taux voté par la commune, avis de dégrèvement",
        ],
    ),
    label(
        Category::Health,
        Some(Subcategory::HealthInsurance),
        &[
            "Assurance maladie (CPAM, ameli) : relevé des remboursements de soins, attestation de droits, carte Vitale",
            "détail des versements de la caisse d'assurance maladie : dates des soins, nature des prestations, base de remboursement, taux, montant versé, franchise",
        ],
    ),
    label(
        Category::Health,
        Some(Subcategory::ComplementaryHealth),
        &[
            "mutuelle santé complémentaire : décompte de la mutuelle, part complémentaire remboursée, cotisation, garanties du contrat",
        ],
    ),
    label(
        Category::Health,
        Some(Subcategory::Care),
        &[
            "soins médicaux : ordonnance, compte rendu médical, facture de médecin, dentiste, pharmacie, hôpital, analyses",
        ],
    ),
    label(
        Category::Health,
        Some(Subcategory::DeathDisabilityCover),
        &[
            "contrat de prévoyance : garantie décès, invalidité, incapacité de travail, capital versé aux bénéficiaires",
        ],
    ),
    label(
        Category::Work,
        Some(Subcategory::Payslips),
        &[
            "bulletin de salaire : salaire brut, cotisations sociales, net à payer, net imposable, employeur",
        ],
    ),
    label(
        Category::Work,
        Some(Subcategory::Employment),
        &[
            "contrat de travail, attestation employeur, certificat de travail, solde de tout compte",
            "contrat de travail à durée indéterminée ou déterminée entre l'employeur et le salarié : engagement, poste occupé, période d'essai, rémunération, durée du travail, convention collective",
        ],
    ),
    label(
        Category::Work,
        Some(Subcategory::HouseholdEmployer),
        &[
            "particulier employeur : CESU, Pajemploi, salaire d'un employé à domicile ou d'une assistante maternelle",
        ],
    ),
    label(
        Category::Work,
        Some(Subcategory::Freelance),
        &[
            "activité indépendante ou portage salarial : auto-entrepreneur, URSSAF, facturation de missions, chiffre d'affaires",
            "compte rendu d'activité (CRA) mensuel d'un consultant : jours travaillés sur la mission, client, signature",
        ],
    ),
    label(
        Category::Work,
        Some(Subcategory::Unemployment),
        &[
            "France Travail (Pôle emploi) : allocation chômage, avis de paiement, attestation de droits, inscription comme demandeur d'emploi",
        ],
    ),
    label(
        Category::Work,
        Some(Subcategory::Retirement),
        &[
            "retraite : pension de retraite de base ou complémentaire, relevé de carrière, trimestres validés, caisse de retraite",
        ],
    ),
    label(
        Category::Family,
        Some(Subcategory::Benefits),
        &[
            "prestations familiales de la CAF : allocations familiales, aide au logement, prime d'activité, congé parental",
        ],
    ),
    label(
        Category::Family,
        Some(Subcategory::Childcare),
        &[
            "garde d'enfant : crèche, halte-garderie, assistante maternelle, centre de loisirs, frais de garde",
        ],
    ),
    label(
        Category::Identity,
        Some(Subcategory::IdDocuments),
        &[
            "pièce d'identité : carte nationale d'identité, passeport, titre de séjour, permis de conduire, date de validité",
        ],
    ),
    label(
        Category::Identity,
        Some(Subcategory::CivilStatus),
        &[
            "état civil : acte de naissance, de mariage ou de décès, livret de famille, PACS, mairie",
        ],
    ),
    label(
        Category::Legal,
        Some(Subcategory::InheritanceGifts),
        &[
            "succession ou donation : notaire, héritiers, défunt, acte de donation, déclaration de succession, partage",
        ],
    ),
    label(
        Category::Legal,
        Some(Subcategory::Company),
        &[
            "société : statuts, extrait Kbis, assemblée générale des associés, procès-verbal, approbation des comptes",
        ],
    ),
    label(
        Category::Legal,
        Some(Subcategory::Justice),
        &[
            "procédure judiciaire : tribunal, jugement, avocat, huissier de justice, assignation, convocation à une audience",
        ],
    ),
    label(
        Category::Transport,
        Some(Subcategory::Train),
        &["billet de train : trajet, gare de départ et d'arrivée, voiture, place, TGV, TER"],
    ),
    label(
        Category::Transport,
        Some(Subcategory::Flight),
        &["billet d'avion, carte d'embarquement, réservation de vol, compagnie aérienne, aéroport"],
    ),
    label(
        Category::Transport,
        Some(Subcategory::Road),
        &["route : péage d'autoroute, carburant, stationnement, recharge électrique, amende"],
    ),
    label(
        Category::Transport,
        Some(Subcategory::Ride),
        &[
            "course de VTC ou de taxi, location de trottinette ou de vélo en libre-service, autopartage",
        ],
    ),
    label(
        Category::Transport,
        Some(Subcategory::Travel),
        &["voyage : réservation d'hôtel, location de vacances, séjour, agence de voyage"],
    ),
    label(
        Category::Transport,
        Some(Subcategory::Vehicle),
        &[
            "véhicule : certificat d'immatriculation (carte grise), achat d'une voiture, entretien, révision, contrôle technique",
        ],
    ),
    label(
        Category::Transport,
        Some(Subcategory::VehicleInsurance),
        &[
            "assurance automobile ou deux-roues : attestation d'assurance, carte verte, avis d'échéance, bonus-malus",
        ],
    ),
    label(
        Category::Pets,
        Some(Subcategory::Veterinary),
        &[
            "vétérinaire : consultation, vaccination, soins pour chien ou chat, facture de la clinique vétérinaire",
        ],
    ),
    label(
        Category::Pets,
        Some(Subcategory::PetCare),
        &["garde d'animaux : pension pour chien ou chat, toilettage, promenade, pet-sitter"],
    ),
    label(
        Category::Pets,
        Some(Subcategory::PetInsurance),
        &["assurance santé pour chien ou chat : cotisation, remboursement des frais vétérinaires"],
    ),
    label(
        Category::Pets,
        Some(Subcategory::PetSupplies),
        &[
            "animalerie : nourriture pour chien ou chat, croquettes, litière, accessoires pour animaux",
        ],
    ),
    label(
        Category::Shopping,
        Some(Subcategory::Online),
        &[
            "commande en ligne : facture d'achat sur un site marchand, articles commandés, livraison",
        ],
    ),
    label(
        Category::Shopping,
        Some(Subcategory::Groceries),
        &["courses alimentaires : ticket de caisse de supermarché, drive, épicerie"],
    ),
    label(
        Category::Shopping,
        Some(Subcategory::FoodDelivery),
        &["livraison de repas : commande à un restaurant livrée à domicile"],
    ),
    label(
        Category::Shopping,
        Some(Subcategory::Electronics),
        &[
            "achat d'appareil électronique : ordinateur, smartphone, tablette, télévision, numéro de série",
        ],
    ),
    label(
        Category::Shopping,
        Some(Subcategory::HomeEquipment),
        &[
            "équipement de la maison : meuble, électroménager, lave-linge, réfrigérateur, literie, décoration",
        ],
    ),
    label(
        Category::Shopping,
        Some(Subcategory::Clothing),
        &["vêtements et chaussures : achat en magasin ou en ligne, taille, coloris, retour"],
    ),
    label(
        Category::Shopping,
        Some(Subcategory::Secondhand),
        &[
            "achat ou vente d'occasion entre particuliers : Vinted, Leboncoin, bordereau d'envoi, protection acheteur",
        ],
    ),
    label(
        Category::Leisure,
        Some(Subcategory::Sports),
        &[
            "sport : licence sportive, adhésion à un club, abonnement à une salle de sport, certificat médical",
        ],
    ),
    label(
        Category::Leisure,
        Some(Subcategory::Events),
        &["billet de spectacle, concert, cinéma, musée ou événement : date, salle, placement"],
    ),
    label(
        Category::Leisure,
        Some(Subcategory::Associations),
        &["adhésion à une association : cotisation annuelle, carte de membre, bulletin d'adhésion"],
    ),
    label(
        Category::Education,
        Some(Subcategory::School),
        &[
            "école, collège ou lycée : inscription scolaire, cantine, frais de scolarité, bulletin scolaire, périscolaire",
        ],
    ),
    label(
        Category::Education,
        Some(Subcategory::HigherEducation),
        &[
            "études supérieures : inscription à l'université ou en école, frais de scolarité, certificat de scolarité, CVEC",
        ],
    ),
    label(
        Category::Education,
        Some(Subcategory::Training),
        &[
            "formation professionnelle : CPF, organisme de formation, convention de formation, attestation de fin de formation",
        ],
    ),
    label(
        Category::Subscriptions,
        Some(Subcategory::Media),
        &["abonnement média : streaming vidéo ou musique, plateforme de séries et de films"],
    ),
    label(
        Category::Subscriptions,
        Some(Subcategory::CloudSoftware),
        &[
            "abonnement logiciel ou cloud : hébergement web, serveur, nom de domaine, stockage en ligne, licence",
            "facture d'hébergeur ou de registrar : nom de domaine, zone DNS, serveur VPS ou dédié, certificat SSL, renouvellement pour 12 mois",
        ],
    ),
    label(
        Category::Subscriptions,
        Some(Subcategory::Press),
        &[
            "abonnement presse : journal, magazine, quotidien en ligne, numéros papier ou numériques",
        ],
    ),
    label(
        Category::Donations,
        None,
        &[
            "reçu fiscal de don à une association ou une fondation, ouvrant droit à réduction d'impôt",
        ],
    ),
];

/// A [`DocumentType`], described the way such documents read (see
/// [`Label::descriptions`]).
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

/// How much a category's best label must score above every other
/// category's for the document to be filed under it.
const CATEGORY_MARGIN: f32 = 0.005;

/// How much the best label within the chosen category must score above
/// that category's other labels for its subcategory to be kept.
const SUBCATEGORY_MARGIN: f32 = 0.005;

/// How much the best type must score above every other type to be kept.
const TYPE_MARGIN: f32 = 0.005;

/// How much of a transcript is embedded, in bytes of whitespace-collapsed
/// text: the model only reads its first 512 tokens anyway, well within it,
/// so a huge transcript isn't tokenized whole for nothing.
const MAX_TEXT_BYTES: usize = 16 * 1024;

/// The embedding of every description in [`LABELS`] and [`TYPES`], in
/// their order.
pub(super) struct Embedded {
    pub(super) labels: Vec<Vec<Vec<f32>>>,
    pub(super) types: Vec<Vec<Vec<f32>>>,
}

/// [`Embedded`] descriptions per model (see [`Embedder::model`]), each
/// computed on the first classification needing it.
#[derive(Default)]
pub(super) struct DescriptionVectors {
    /// Held while embedding, so concurrent first classifications embed the
    /// descriptions once.
    by_model: tokio::sync::Mutex<HashMap<String, Arc<Embedded>>>,
}

/// The default [`Service`]: embeds a document's text with an injected
/// [`Embedder`], and the descriptions into an injected
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

    /// The descriptions embedded with the embedder's model, embedding them
    /// unless that's already done: a failure is retried on the next call.
    async fn descriptions(&self) -> Result<Arc<Embedded>> {
        let model = self.embedder.model();
        let mut by_model = self.descriptions.by_model.lock().await;
        if let Some(embedded) = by_model.get(&model) {
            return Ok(embedded.clone());
        }

        let embedded = Arc::new(Embedded {
            labels: self
                .embed_descriptions(LABELS.iter().map(|label| label.descriptions))
                .await
                .context("failed to embed the category descriptions")?,
            types: self
                .embed_descriptions(TYPES.iter().map(|label| label.descriptions))
                .await
                .context("failed to embed the document type descriptions")?,
        });
        by_model.insert(model, embedded.clone());
        Ok(embedded)
    }

    /// Embeds each label's descriptions as queries, matched against
    /// documents' passages.
    async fn embed_descriptions(
        &self,
        labels: impl Iterator<Item = &'static [&'static str]>,
    ) -> Result<Vec<Vec<Vec<f32>>>> {
        let mut embedded = Vec::new();
        for descriptions in labels {
            let mut vectors = Vec::with_capacity(descriptions.len());
            for description in descriptions {
                vectors.push(
                    self.embedder
                        .embed_query(description.to_string())
                        .await
                        .with_context(|| format!("failed to embed description {description:?}"))?,
                );
            }
            embedded.push(vectors);
        }
        Ok(embedded)
    }
}

#[async_trait]
impl Service for Classifier {
    async fn classify(&self, text: &str) -> Result<Classification> {
        let text = classified_text(text);
        if text.is_empty() {
            return Ok(Classification::default());
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
        let descriptions = self.descriptions().await?;

        Ok(classify(&document, &descriptions))
    }
}

/// The part of `transcript` a document is classified from: its first
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

/// Classifies the `document` vector against the embedded `descriptions`:
/// the category whose best label is clearly best, then the subcategory
/// (or lack of one) of its clearly best label, and the clearly best type
/// (see [`decide`]).
pub(super) fn classify(document: &[f32], descriptions: &Embedded) -> Classification {
    let labels: Vec<(&Label, f32)> = LABELS
        .iter()
        .zip(&descriptions.labels)
        .map(|(label, vectors)| (label, best_similarity(document, vectors)))
        .collect();

    let category = decide(
        labels.iter().map(|(label, score)| (label.category, *score)),
        CATEGORY_MARGIN,
    );
    let subcategory = category.and_then(|category| {
        decide(
            labels
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
            .zip(&descriptions.types)
            .map(|(label, vectors)| (label.r#type, best_similarity(document, vectors))),
        TYPE_MARGIN,
    );

    Classification {
        category,
        subcategory,
        r#type,
    }
}

/// The highest cosine similarity of `document` to any of `descriptions`
/// (all L2-normalized, so their dot product), or `f32::MIN` for none.
pub(super) fn best_similarity(document: &[f32], descriptions: &[Vec<f32>]) -> f32 {
    descriptions
        .iter()
        .map(|description| description.iter().zip(document).map(|(a, b)| a * b).sum())
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
    fn every_label_is_a_valid_category_and_subcategory_pair() {
        for label in LABELS {
            if let Some(subcategory) = label.subcategory {
                assert_eq!(subcategory.category(), label.category, "{subcategory:?}");
            }
        }
    }

    #[test]
    fn every_label_and_type_has_descriptions_none_blank() {
        let labels = LABELS.iter().map(|label| label.descriptions);
        let types = TYPES.iter().map(|label| label.descriptions);

        for descriptions in labels.chain(types) {
            assert!(!descriptions.is_empty());
            for description in descriptions {
                assert!(!description.trim().is_empty());
            }
        }
    }

    #[test]
    fn every_category_and_subcategory_has_exactly_one_label() {
        for category in Category::ALL {
            let labels = LABELS.iter().filter(|label| label.category == *category);
            assert!(labels.count() >= 1, "{category:?} has no label");
        }
        for subcategory in Subcategory::ALL {
            let labels = LABELS
                .iter()
                .filter(|label| label.subcategory == Some(*subcategory));
            assert_eq!(labels.count(), 1, "{subcategory:?}");
        }
        for category in Category::ALL {
            let labels = LABELS
                .iter()
                .filter(|label| label.category == *category && label.subcategory.is_none());
            assert!(
                labels.count() <= 1,
                "{category:?} has two labels without a subcategory"
            );
        }
    }

    #[test]
    fn only_a_category_without_subcategories_has_a_label_without_one() {
        for label in LABELS.iter().filter(|label| label.subcategory.is_none()) {
            let category = label.category;
            assert!(
                Subcategory::ALL
                    .iter()
                    .all(|subcategory| subcategory.category() != category),
                "{category:?} has subcategories"
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

    /// Every description, labels' then types', in [`LABELS`]/[`TYPES`]
    /// order: each is embedded along its own axis of this many dimensions.
    fn descriptions() -> Vec<&'static str> {
        let labels = LABELS.iter().flat_map(|label| label.descriptions.iter());
        let types = TYPES.iter().flat_map(|label| label.descriptions.iter());
        labels.chain(types).copied().collect()
    }

    fn axis_of(description: &str) -> usize {
        descriptions()
            .iter()
            .position(|known| *known == description)
            .unwrap()
    }

    /// A vector with `1.0` along each of `axes`.
    fn along(axes: &[usize]) -> Vec<f32> {
        let mut vector = vec![0.0; descriptions().len()];
        for axis in axes {
            vector[*axis] = 1.0;
        }
        vector
    }

    /// Every description embedded along its own axis.
    fn embedded_along_axes() -> Embedded {
        let embed = |labels: &mut dyn Iterator<Item = &'static [&'static str]>| {
            labels
                .map(|descriptions| {
                    descriptions
                        .iter()
                        .map(|description| along(&[axis_of(description)]))
                        .collect()
                })
                .collect()
        };
        Embedded {
            labels: embed(&mut LABELS.iter().map(|label| label.descriptions)),
            types: embed(&mut TYPES.iter().map(|label| label.descriptions)),
        }
    }

    /// The axis of `subcategory`'s label's description number `index`.
    fn label_axis(category: Category, subcategory: Option<Subcategory>, index: usize) -> usize {
        let label = LABELS
            .iter()
            .find(|label| label.category == category && label.subcategory == subcategory)
            .unwrap();
        axis_of(label.descriptions[index])
    }

    fn type_axis(r#type: DocumentType) -> usize {
        let label = TYPES.iter().find(|label| label.r#type == r#type).unwrap();
        axis_of(label.descriptions[0])
    }

    #[test]
    fn classify_picks_the_best_label_and_type() {
        let document = along(&[
            label_axis(Category::Housing, Some(Subcategory::Energy), 0),
            type_axis(DocumentType::Invoice),
        ]);

        let classification = classify(&document, &embedded_along_axes());

        assert_eq!(
            classification,
            Classification {
                category: Some(Category::Housing),
                subcategory: Some(Subcategory::Energy),
                r#type: Some(DocumentType::Invoice),
            }
        );
    }

    #[test]
    fn classify_scores_a_label_by_its_best_description() {
        let document = along(&[label_axis(Category::Housing, Some(Subcategory::Energy), 1)]);

        let classification = classify(&document, &embedded_along_axes());

        assert_eq!(classification.subcategory, Some(Subcategory::Energy));
    }

    #[test]
    fn classify_files_a_label_without_subcategory_under_its_category_alone() {
        let document = along(&[label_axis(Category::Donations, None, 0)]);

        let classification = classify(&document, &embedded_along_axes());

        assert_eq!(classification.category, Some(Category::Donations));
        assert_eq!(classification.subcategory, None);
    }

    #[test]
    fn classify_keeps_the_category_but_no_subcategory_when_its_labels_tie() {
        let document = along(&[
            label_axis(Category::Housing, Some(Subcategory::Energy), 0),
            label_axis(Category::Housing, Some(Subcategory::Water), 0),
        ]);

        let classification = classify(&document, &embedded_along_axes());

        assert_eq!(classification.category, Some(Category::Housing));
        assert_eq!(classification.subcategory, None);
    }

    #[test]
    fn classify_gives_no_category_nor_subcategory_when_categories_tie() {
        let document = along(&[
            label_axis(Category::Housing, Some(Subcategory::Energy), 0),
            label_axis(Category::Telecom, Some(Subcategory::Mobile), 0),
        ]);

        let classification = classify(&document, &embedded_along_axes());

        assert_eq!(classification.category, None);
        assert_eq!(classification.subcategory, None);
    }

    #[test]
    fn classify_gives_no_type_when_types_tie() {
        let document = along(&[
            type_axis(DocumentType::Invoice),
            type_axis(DocumentType::Receipt),
        ]);

        let classification = classify(&document, &embedded_along_axes());

        assert_eq!(classification.r#type, None);
    }

    #[test]
    fn classify_applies_each_margin() {
        let scores = |category: f32, subcategory: f32, r#type: f32| {
            let mut document = vec![0.0; descriptions().len()];
            document[label_axis(Category::Housing, Some(Subcategory::Energy), 0)] = 0.5;
            document[label_axis(Category::Housing, Some(Subcategory::Water), 0)] =
                0.5 - subcategory;
            document[label_axis(Category::Telecom, Some(Subcategory::Mobile), 0)] = 0.5 - category;
            document[type_axis(DocumentType::Invoice)] = 0.5;
            document[type_axis(DocumentType::Receipt)] = 0.5 - r#type;
            classify(&document, &embedded_along_axes())
        };
        // Clear of the margin either way: `f32`s don't subtract exactly.
        let above = |margin: f32| margin * 2.0;
        let below = |margin: f32| margin * 0.5;
        let (category, subcategory, r#type) = (
            above(CATEGORY_MARGIN),
            above(SUBCATEGORY_MARGIN),
            above(TYPE_MARGIN),
        );

        let clear = scores(category, subcategory, r#type);
        assert_eq!(clear.category, Some(Category::Housing));
        assert_eq!(clear.subcategory, Some(Subcategory::Energy));
        assert_eq!(clear.r#type, Some(DocumentType::Invoice));

        let close = scores(category, subcategory, below(TYPE_MARGIN));
        assert_eq!(close.subcategory, Some(Subcategory::Energy));
        assert_eq!(close.r#type, None);

        let close = scores(category, below(SUBCATEGORY_MARGIN), r#type);
        assert_eq!(close.category, Some(Category::Housing));
        assert_eq!(close.subcategory, None);

        let close = scores(below(CATEGORY_MARGIN), subcategory, r#type);
        assert_eq!(close.category, None);
        assert_eq!(close.r#type, Some(DocumentType::Invoice));
    }

    /// An embedder embedding each description along its own axis, and
    /// documents along `document`, expecting `queries` description
    /// embeddings and `passages` document ones.
    fn embedder_along_axes(document: Vec<f32>, passages: usize, queries: usize) -> MockEmbedder {
        let mut embedder = MockEmbedder::new();
        embedder.expect_model().return_const("e5.onnx".to_string());
        embedder
            .expect_embed_passages()
            .times(passages)
            .returning(move |_| Ok(vec![document.clone()]));
        embedder
            .expect_embed_query()
            .times(queries)
            .returning(|query| Ok(along(&[axis_of(&query)])));
        embedder
    }

    fn classifier(embedder: MockEmbedder, descriptions: &Arc<DescriptionVectors>) -> Classifier {
        Classifier::new(Arc::new(embedder), descriptions.clone())
    }

    #[tokio::test]
    async fn classifier_classifies_the_document_s_text() {
        let mut embedder = MockEmbedder::new();
        embedder.expect_model().return_const("e5.onnx".to_string());
        embedder
            .expect_embed_passages()
            .withf(|passages| passages == &["Bulletin de paie net à payer".to_string()])
            .times(1)
            .returning(move |_| {
                Ok(vec![along(&[
                    label_axis(Category::Work, Some(Subcategory::Payslips), 0),
                    type_axis(DocumentType::Payslip),
                ])])
            });
        embedder
            .expect_embed_query()
            .times(descriptions().len())
            .returning(|query| Ok(along(&[axis_of(&query)])));
        let classifier = classifier(embedder, &Arc::default());

        let classification = classifier
            .classify("Bulletin de paie\n  net à payer")
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
    async fn classifier_leaves_a_blank_text_unclassified_without_embedding() {
        let classifier = classifier(MockEmbedder::new(), &Arc::default());

        let classification = classifier.classify(" \n ").await.unwrap();

        assert_eq!(classification, Classification::default());
    }

    #[tokio::test]
    async fn classifier_embeds_the_descriptions_once_across_classifications() {
        let document = along(&[label_axis(Category::Donations, None, 0)]);
        let embedder = embedder_along_axes(document, 2, descriptions().len());
        let classifier = classifier(embedder, &Arc::default());

        for _ in 0..2 {
            let classification = classifier.classify("Taxe foncière").await.unwrap();
            assert_eq!(classification.category, Some(Category::Donations));
        }
    }

    #[tokio::test]
    async fn classifiers_sharing_descriptions_embed_them_once_per_model() {
        let descriptions_vectors = Arc::<DescriptionVectors>::default();
        let document = along(&[label_axis(Category::Donations, None, 0)]);
        let first = classifier(
            embedder_along_axes(document.clone(), 1, descriptions().len()),
            &descriptions_vectors,
        );
        let same_model = classifier(
            embedder_along_axes(document.clone(), 1, 0),
            &descriptions_vectors,
        );
        let mut other_model = MockEmbedder::new();
        other_model
            .expect_model()
            .return_const("other.onnx".to_string());
        other_model
            .expect_embed_passages()
            .times(1)
            .returning(move |_| Ok(vec![document.clone()]));
        other_model
            .expect_embed_query()
            .times(descriptions().len())
            .returning(|query| Ok(along(&[axis_of(&query)])));
        let other_model = classifier(other_model, &descriptions_vectors);

        for classifier in [first, same_model, other_model] {
            let classification = classifier.classify("Taxe foncière").await.unwrap();
            assert_eq!(classification.category, Some(Category::Donations));
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

        let err = classifier.classify("Facture").await.unwrap_err();

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

        let err = classifier.classify("Facture").await.unwrap_err();

        assert_eq!(err.code(), ErrorCode::ModelLoadFailed);
    }

    #[tokio::test]
    async fn classifier_retries_descriptions_that_failed_to_embed() {
        let document = along(&[label_axis(Category::Donations, None, 0)]);
        let mut embedder = MockEmbedder::new();
        embedder.expect_model().return_const("e5.onnx".to_string());
        embedder
            .expect_embed_passages()
            .times(2)
            .returning(move |_| Ok(vec![document.clone()]));
        let queries = AtomicUsize::new(0);
        embedder
            .expect_embed_query()
            .times(1 + descriptions().len())
            .returning(move |query| {
                if queries.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(ErrorKind::ModelNotDownloaded("e5".to_string()).into());
                }
                Ok(along(&[axis_of(&query)]))
            });
        let classifier = classifier(embedder, &Arc::default());

        let err = classifier.classify("Taxe foncière").await.unwrap_err();
        assert!(
            err.to_string()
                .contains("failed to embed the category descriptions"),
            "{err}"
        );
        let classification = classifier.classify("Taxe foncière").await.unwrap();

        assert_eq!(classification.category, Some(Category::Donations));
    }

    /// A synthetic document and how it should be classified (`r#type`
    /// `None`: not checked, no type fitting it).
    struct Case {
        text: &'static str,
        category: Category,
        subcategory: Option<Subcategory>,
        r#type: Option<DocumentType>,
    }

    const fn case(
        text: &'static str,
        category: Category,
        subcategory: Option<Subcategory>,
        r#type: Option<DocumentType>,
    ) -> Case {
        Case {
            text,
            category,
            subcategory,
            r#type,
        }
    }

    /// Made-up documents, one per category at least, plus the
    /// `doc-ai-poc` corpus' weak spots: local taxes (filed under income tax
    /// half of the time) and employment contracts.
    const CASES: &[Case] = &[
        case(
            "Électricité de France — Facture n° 30012345678 du 14/03/2025. Période du 12/01/2025 au 11/03/2025. \
             Relevé du compteur Linky : index 45210 kWh. Consommation : 612 kWh. Abonnement option base 9 kVA : 31,46 €. \
             Consommation : 154,32 €. Taxes et contributions : 22,10 €. Montant total TTC : 207,88 €, prélevé le 28/03/2025.",
            Category::Housing,
            Some(Subcategory::Energy),
            Some(DocumentType::Invoice),
        ),
        case(
            "Quittance de loyer — mois de mars 2025. Je soussigné Jean Martin, propriétaire du logement situé \
             12 rue des Lilas, 69003 Lyon, déclare avoir reçu de Madame Claire Durand, locataire, la somme de \
             850,00 € au titre du paiement du loyer (780,00 €) et des charges (70,00 €) pour la période du 01/03/2025 \
             au 31/03/2025, et lui en donne quittance, sous réserve de tous mes droits.",
            Category::Housing,
            Some(Subcategory::Rent),
            None,
        ),
        case(
            "Free Mobile — Votre facture du mois de février 2025. Ligne 06 12 34 56 78. Forfait Free 5G 150 Go : \
             19,99 €. Appels, SMS et MMS illimités en France métropolitaine. Données consommées : 42,3 Go. \
             Communications hors forfait : 0,00 €. Total TTC : 19,99 €, prélevé sur votre compte le 05/03/2025.",
            Category::Telecom,
            Some(Subcategory::Mobile),
            Some(DocumentType::Invoice),
        ),
        case(
            "Banque Populaire — Relevé de compte courant n° 12345678901 du 01/02/2025 au 28/02/2025. \
             Solde créditeur au 01/02/2025 : 1 254,30 €. 03/02 CB CARREFOUR MARKET 54,20. 05/02 VIR SEPA SALAIRE \
             FEVRIER +2 310,00. 10/02 PRLV SEPA EDF 207,88. 15/02 CB SNCF 89,00. 20/02 RETRAIT DAB 60,00. \
             Total des opérations : débit 411,08, crédit 2 310,00. Nouveau solde créditeur au 28/02/2025 : 3 153,22 €.",
            Category::Money,
            Some(Subcategory::Banking),
            Some(DocumentType::Statement),
        ),
        case(
            "Direction générale des Finances publiques — Avis d'impôt 2025 sur les revenus de l'année 2024. \
             Impôt sur le revenu. Nombre de parts : 2,5. Revenu fiscal de référence : 52 340 €. Revenu brut global : \
             58 120 €. Impôt sur le revenu net : 3 412 €. Retenue à la source prélevée en 2024 : 3 250 €. \
             Reste à payer : 162 €, prélevé en 4 fois à partir de septembre.",
            Category::Taxes,
            Some(Subcategory::IncomeTax),
            Some(DocumentType::TaxNotice),
        ),
        case(
            "Direction générale des Finances publiques — Avis d'impôt 2025 Taxes foncières. Propriétés bâties. \
             Commune de Villeurbanne. Adresse du bien : 12 rue des Lilas. Base d'imposition : 2 140 €. \
             Taux communal 45,12 %, taxe d'enlèvement des ordures ménagères 8,10 %. Montant de votre impôt : \
             1 138 €. Date limite de paiement : 15/10/2025.",
            Category::Taxes,
            Some(Subcategory::PropertyTax),
            Some(DocumentType::TaxNotice),
        ),
        case(
            "Avis de taxe d'habitation 2024 sur les résidences secondaires et autres locaux meublés non affectés \
             à l'habitation principale. Commune d'Annecy. Valeur locative brute : 3 980 €. Taux voté par la \
             commune : 18,62 %. Majoration résidences secondaires : 60 %. Montant à payer : 1 186 €.",
            Category::Taxes,
            Some(Subcategory::HousingTax),
            Some(DocumentType::TaxNotice),
        ),
        case(
            "L'Assurance Maladie — Relevé de vos remboursements de soins. Assuré : Claire Durand. \
             Date des soins 04/02/2025, consultation médecin généraliste, base de remboursement 30,00 €, taux 70 %, \
             montant remboursé 20,00 €, participation forfaitaire 2,00 €. Date des soins 11/02/2025, pharmacie, \
             montant remboursé 12,45 €. Total versé sur votre compte le 14/02/2025 : 30,45 €.",
            Category::Health,
            Some(Subcategory::HealthInsurance),
            Some(DocumentType::ReimbursementStatement),
        ),
        case(
            "Dr Sophie Lambert, médecin généraliste, 3 place Bellecour, Lyon. Ordonnance du 04/02/2025 pour \
             Mme Claire Durand. Paracétamol 1 g : un comprimé trois fois par jour pendant 5 jours. \
             Ibuprofène 400 mg : si douleur, sans dépasser 3 par jour. À renouveler une fois.",
            Category::Health,
            Some(Subcategory::Care),
            Some(DocumentType::MedicalDocument),
        ),
        case(
            "Bulletin de paie — Période du 01/02/2025 au 28/02/2025. Employeur : Société Exemple SAS, SIRET \
             123 456 789 00012. Salarié : Claire Durand, ingénieure, coefficient 130. Salaire de base 3 200,00 €. \
             Sécurité sociale maladie, vieillesse, retraite complémentaire, CSG déductible et non déductible. \
             Total des cotisations salariales : 712,40 €. Net à payer avant impôt sur le revenu : 2 487,60 €. \
             Impôt sur le revenu prélevé à la source : 124,38 €. Net payé : 2 363,22 €. Net imposable : 2 581,20 €.",
            Category::Work,
            Some(Subcategory::Payslips),
            Some(DocumentType::Payslip),
        ),
        case(
            "CONTRAT DE TRAVAIL À DURÉE INDÉTERMINÉE. Entre les soussignés : la société Exemple SAS, au capital \
             de 10 000 €, représentée par M. Paul Bernard, gérant, ci-après l'employeur, et Mme Claire Durand, \
             demeurant à Lyon, ci-après la salariée. Article 1 — Engagement : la salariée est engagée à compter du \
             1er avril 2025 en qualité d'ingénieure logiciel, statut cadre. Article 2 — Période d'essai de quatre mois. \
             Article 3 — Rémunération annuelle brute de 48 000 €. Article 4 — Durée du travail : forfait de 218 jours. \
             Article 5 — Convention collective Syntec. Fait à Lyon, le 15 mars 2025, en deux exemplaires originaux.",
            Category::Work,
            Some(Subcategory::Employment),
            Some(DocumentType::Contract),
        ),
        case(
            "Caisse d'allocations familiales du Rhône — Notification de vos droits. Allocataire : Claire Durand. \
             À compter de mars 2025, vous bénéficiez des allocations familiales pour vos deux enfants à charge : \
             141,99 € par mois, et de l'aide personnalisée au logement : 112,00 € par mois, versées le 5 de chaque mois.",
            Category::Family,
            Some(Subcategory::Benefits),
            None,
        ),
        case(
            "SNCF Connect — Votre e-billet. Aller simple le 18/04/2025. Départ Paris Gare de Lyon 08h04, arrivée \
             Lyon Part-Dieu 10h00. TGV INOUI n° 6603, voiture 12, place 45, 2nde classe, fenêtre. Passager : \
             Claire Durand, carte Avantage Adulte. Prix payé : 49,00 €. Référence de dossier : QWERTY.",
            Category::Transport,
            Some(Subcategory::Train),
            Some(DocumentType::Booking),
        ),
        case(
            "Amazon.fr — Facture. Numéro de commande 402-1234567-8901234, passée le 02/03/2025. Articles commandés : \
             1 × Bouilloire électrique 1,7 L, 29,99 €. 2 × Câble USB-C, 9,99 €. Livraison gratuite à Claire Durand, \
             12 rue des Lilas, Lyon. Total HT 41,64 €, TVA 20 % 8,33 €, total TTC 49,97 €. Payé par carte Visa.",
            Category::Shopping,
            Some(Subcategory::Online),
            Some(DocumentType::Invoice),
        ),
        case(
            "OVHcloud — Facture FR12345678 du 01/03/2025. Désignation : renouvellement du nom de domaine \
             exemple.fr pour 12 mois, 7,79 € HT. Serveur privé virtuel VPS Starter, 1 vCore, 2 Go, mars 2025, \
             4,20 € HT. Total HT 11,99 €, TVA 20 % 2,40 €, total TTC 14,39 €.",
            Category::Subscriptions,
            Some(Subcategory::CloudSoftware),
            Some(DocumentType::Invoice),
        ),
        case(
            "Reçu au titre des dons à certains organismes d'intérêt général, articles 200 et 238 bis du code \
             général des impôts. Association Les Restaurants du Cœur, reconnue d'utilité publique. Donateur : \
             Claire Durand, Lyon. Somme de 150,00 € versée le 12/12/2024 par virement, au titre d'un don manuel. \
             Le bénéficiaire certifie sur l'honneur que les dons ouvrent droit à la réduction d'impôt.",
            Category::Donations,
            None,
            Some(DocumentType::TaxCertificate),
        ),
    ];

    /// Downloads multilingual-e5-small (see `FYDE_TEST_MODELS_DIR`) and
    /// checks it classifies every one of [`CASES`] as expected.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "downloads a 135 MB model from Hugging Face"]
    async fn multilingual_e5_classifies_synthetic_documents() {
        let temp = tempfile::tempdir().unwrap();
        let dir = std::env::var_os("FYDE_TEST_MODELS_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| temp.path().to_path_buf());
        let embedder = super::super::super::init_embedder(Arc::new(
            crate::tools::models::ModelStore::new(dir),
        ));
        let classifier = Classifier::new(embedder, Arc::default());

        let mut wrong = Vec::new();
        for case in CASES {
            let classification = classifier.classify(case.text).await.unwrap();
            let expected = Classification {
                category: Some(case.category),
                subcategory: case.subcategory,
                r#type: case.r#type.or(classification.r#type),
            };
            println!(
                "{:?} {:?} {:?} <- {}",
                classification.category,
                classification.subcategory,
                classification.r#type,
                &case.text[..60]
            );
            if classification != expected {
                wrong.push(format!("{classification:?} instead of {expected:?}"));
            }
        }

        assert!(wrong.is_empty(), "{wrong:#?}");
    }
}
