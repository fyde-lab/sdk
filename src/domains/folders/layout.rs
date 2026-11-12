use super::{FolderAppearance, FolderColor, FolderIcon};

/// A folder of the proposed layout (see `Service::create_proposed_layout`),
/// named and described in French and English (`(name, description)`).
pub(super) struct ProposedFolder {
    pub(super) fr: (&'static str, &'static str),
    pub(super) en: (&'static str, &'static str),
    /// Set on top-level folders only.
    pub(super) appearance: Option<FolderAppearance>,
    pub(super) children: &'static [ProposedFolder],
}

/// The folders a new vault can start with: one per part of life, with a
/// subfolder per kind of paper. Each subfolder (and a top-level folder
/// without any) is described the way its documents read, which is what new
/// documents are filed by; a top-level folder holding subfolders has no
/// description, so documents go to its subfolders rather than to it.
pub(super) const PROPOSED_LAYOUT: &[ProposedFolder] = &[
    ProposedFolder {
        fr: ("Logement", ""),
        en: ("Housing", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Sand,
            icon: FolderIcon::Home,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Énergie",
                    "Factures d'électricité ou de gaz : consommation en kWh, abonnement, relevé du compteur ; échéanciers et contrats de fourniture d'énergie",
                ),
                en: (
                    "Energy",
                    "Electricity or gas bills: consumption in kWh, subscription, meter readings; energy payment schedules and supply contracts",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Eau",
                    "Facture d'eau : distribution d'eau potable, assainissement collectif, relevé du compteur d'eau, consommation en m³, prix du mètre cube, redevances de l'agence de l'eau",
                ),
                en: (
                    "Water",
                    "Water bills: drinking water supply, sewerage, water meter readings, consumption in m³, price per cubic metre, water agency charges",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Assurance habitation",
                    "Assurance habitation multirisque : attestation d'assurance du logement, conditions particulières, avis d'échéance, garanties dégâts des eaux, incendie, vol, responsabilité civile, locataire ou propriétaire",
                ),
                en: (
                    "Home insurance",
                    "Home insurance: certificate of home insurance, policy schedule, renewal notices, water damage, fire, theft and liability cover, for tenants or owners",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Services à domicile",
                    "Ménage, repassage et jardinage à domicile : facture de services à la personne, heures d'intervention",
                ),
                en: (
                    "Home services",
                    "Cleaning, ironing and gardening at home: home services invoices, hours worked",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Loyer et charges",
                    "Location du logement : quittance de loyer, bail, avis d'échéance du loyer, provision pour charges locatives, dépôt de garantie, état des lieux ; charges de copropriété et appels de fonds du syndic",
                ),
                en: (
                    "Rent and charges",
                    "Renting a home: rent receipts, lease, rent due notices, service charge provisions, deposit, check-in inventory; co-ownership charges and calls for funds from the building manager",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Achat et vente",
                    "Transaction immobilière : compromis de vente, acte de vente notarié, diagnostics DPE et amiante, agence immobilière",
                ),
                en: (
                    "Buying and selling",
                    "Property transaction: preliminary sale agreement, notarial deed of sale, energy performance and asbestos surveys, estate agent",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Travaux",
                    "Travaux dans le logement : devis ou facture d'un artisan, pose et installation, rénovation, plomberie, électricité, chauffage, poêle ou cheminée, menuiserie, main-d'œuvre et fournitures",
                ),
                en: (
                    "Works",
                    "Works in the home: tradesperson's quote or invoice, fitting and installation, renovation, plumbing, electrics, heating, stove or fireplace, joinery, labour and supplies",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Télécom", ""),
        en: ("Telecom", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Blue,
            icon: FolderIcon::Phone,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Mobile",
                    "Forfait mobile : facture de la ligne de téléphone portable, appels, SMS, Go de data",
                ),
                en: (
                    "Mobile",
                    "Mobile plan: mobile phone line bill, calls, texts, GB of data",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Internet",
                    "Facture d'abonnement internet à la maison : box fibre optique ou ADSL, ligne fixe en 09, téléphone fixe, décodeur TV, location de la box, débit internet",
                ),
                en: (
                    "Internet",
                    "Home internet subscription bills: fibre or ADSL box, landline, home phone, TV set-top box, box rental, internet speed",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Argent", ""),
        en: ("Money", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Green,
            icon: FolderIcon::Wallet,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Banque",
                    "Relevé de compte courant : opérations, carte, virements, prélèvements, solde",
                ),
                en: (
                    "Banking",
                    "Current account statement: transactions, card, transfers, direct debits, balance",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Épargne",
                    "Épargne sur livret : livret A, LDDS, LEP, PEL, relevé du livret, intérêts annuels",
                ),
                en: (
                    "Savings",
                    "Savings accounts: passbook savings, statements, annual interest",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Placements",
                    "Assurance vie, PEA, actions et SCPI : relevé de portefeuille, valorisation, unités de compte",
                ),
                en: (
                    "Investments",
                    "Life insurance, share savings plan, shares and real estate funds: portfolio statement, valuation, units",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Crédits",
                    "Crédit immobilier ou à la consommation : offre de prêt, tableau d'amortissement, mensualités, taux d'intérêt, TAEG, capital restant dû, assurance emprunteur, remboursement anticipé",
                ),
                en: (
                    "Loans",
                    "Mortgage or consumer credit: loan offer, repayment schedule, monthly instalments, interest rate, APR, outstanding principal, borrower insurance, early repayment",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Jeux d'argent",
                    "Jeux d'argent : paris sportifs, poker, loterie, casino en ligne, compte joueur, mises, gains et retraits",
                ),
                en: (
                    "Gambling",
                    "Gambling: sports betting, poker, lottery, online casino, player account, stakes, winnings and withdrawals",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Impôts", ""),
        en: ("Taxes", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Brick,
            icon: FolderIcon::Landmark,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Impôt sur le revenu",
                    "Avis d'impôt sur le revenu, déclaration des revenus, revenu fiscal de référence",
                ),
                en: (
                    "Income tax",
                    "Income tax notices, tax returns, reference taxable income",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Attestations fiscales",
                    "Attestation fiscale annuelle à reporter sur la déclaration de revenus : imprimé fiscal unique (IFU), attestation pour le crédit d'impôt services à la personne ou frais de garde, montants déductibles",
                ),
                en: (
                    "Tax certificates",
                    "Annual tax certificate to report on the tax return: single tax form, certificate for the home services or childcare tax credit, deductible amounts",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Taxe foncière",
                    "Avis de taxes foncières : propriétés bâties, taxe foncière communale et intercommunale, taxe d'enlèvement des ordures ménagères, montant à payer par le propriétaire",
                ),
                en: (
                    "Property tax",
                    "Property tax notices: built property, local and intercommunal property tax, household waste collection tax, amount payable by the owner",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Taxe d'habitation",
                    "Avis de taxe d'habitation : résidence secondaire, valeur locative brute, taux voté par la commune, majoration, contribution à l'audiovisuel public, dégrèvement",
                ),
                en: (
                    "Housing tax",
                    "Housing tax notices: second home, gross rental value, rate voted by the town, surcharge, public broadcasting fee, relief",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Santé", ""),
        en: ("Health", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Rose,
            icon: FolderIcon::Heart,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Assurance maladie",
                    "Assurance maladie, CPAM, ameli : relevé de remboursements de soins par la Sécurité sociale, base de remboursement, participation forfaitaire, attestation de droits, carte Vitale, indemnités journalières",
                ),
                en: (
                    "Health insurance",
                    "National health insurance: statements of care reimbursed by social security, reimbursement base, flat-rate contribution, certificate of entitlement, health card, daily sickness benefits",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Mutuelle",
                    "Mutuelle santé complémentaire : décompte de remboursement de la part complémentaire, tiers payant, carte de mutuelle, adhérent et bénéficiaires, garanties optique, dentaire et hospitalisation",
                ),
                en: (
                    "Complementary health",
                    "Complementary health insurance: statements of the top-up reimbursement, third-party payment, insurance card, member and beneficiaries, optical, dental and hospital cover",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Soins",
                    "Soins médicaux : ordonnance, consultation chez le médecin, dentiste, kinésithérapeute, pharmacie, hôpital, clinique, analyses de laboratoire, compte rendu médical, patient",
                ),
                en: (
                    "Care",
                    "Medical care: prescriptions, doctor's appointments, dentist, physiotherapist, pharmacy, hospital, clinic, lab tests, medical reports, patient",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Prévoyance",
                    "Prévoyance : garantie décès, invalidité et incapacité de travail, rente, capital versé aux bénéficiaires, contrat de prévoyance individuelle ou collective",
                ),
                en: (
                    "Death and disability cover",
                    "Death and disability cover: death, disability and incapacity benefits, annuity, lump sum paid to beneficiaries, individual or group cover",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Travail", ""),
        en: ("Work", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Teal,
            icon: FolderIcon::Briefcase,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Bulletins de paie",
                    "Fiche de paie du salarié : salaire brut, cotisations, net à payer avant impôt, net imposable",
                ),
                en: (
                    "Payslips",
                    "Employee payslip: gross salary, contributions, net pay before tax, taxable pay",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Contrats et attestations",
                    "Emploi salarié : contrat de travail à durée indéterminée ou déterminée, période d'essai, avenant, attestation employeur, certificat de travail, reçu pour solde de tout compte, rupture du contrat",
                ),
                en: (
                    "Employment",
                    "Employment: permanent or fixed-term employment contract, probation period, amendment, employer certificate, certificate of employment, final settlement receipt, termination",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Particulier employeur",
                    "Particulier employeur : bulletin de salaire établi par Pajemploi ou le Cesu pour l'assistante maternelle ou l'employé à domicile, heures et jours d'accueil, indemnités d'entretien",
                ),
                en: (
                    "Household employer",
                    "Household employer: payslips issued through Pajemploi or Cesu for a childminder or home employee, hours and days of care, maintenance allowances",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Indépendant",
                    "Activité de freelance ou portage salarial : facture d'une mission de prestation au client, compte rendu d'activité (CRA) avec les jours travaillés, contrat de prestation, auto-entrepreneur, chiffre d'affaires, Urssaf",
                ),
                en: (
                    "Freelance",
                    "Freelance or umbrella company work: invoices for a service mission to the client, activity reports with days worked, service contracts, sole trader, turnover, social contributions",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Chômage",
                    "France Travail (Pôle emploi) : allocation chômage, avis de paiement, attestation de droits, inscription comme demandeur d'emploi",
                ),
                en: (
                    "Unemployment",
                    "Unemployment office: unemployment benefit, payment notices, certificate of entitlement, jobseeker registration",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Retraite",
                    "Retraite : relevé de carrière, trimestres validés, estimation indicative globale, pension de retraite de base ou complémentaire (Agirc-Arrco), caisse de retraite, retraité",
                ),
                en: (
                    "Retirement",
                    "Retirement: career statement, validated quarters, overall pension estimate, basic or supplementary pension, pension fund, retiree",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Famille", ""),
        en: ("Family", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Ochre,
            icon: FolderIcon::Users,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Prestations",
                    "CAF, Caisse d'allocations familiales : attestation de paiement, allocations familiales, aide personnalisée au logement (APL), prime d'activité, PAJE, quotient familial, allocataire",
                ),
                en: (
                    "Benefits",
                    "Family allowance fund: payment certificates, child benefit, housing benefit, in-work bonus, early childhood benefits, family quotient, claimant",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Garde d'enfants",
                    "Garde d'enfant : crèche, micro-crèche, halte-garderie, contrat d'accueil, tarif horaire, frais de garde, centre de loisirs, projet pédagogique pour les tout-petits",
                ),
                en: (
                    "Childcare",
                    "Childcare: nursery, micro-nursery, day care, care agreement, hourly rate, childcare fees, holiday club, educational project for toddlers",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Identité", ""),
        en: ("Identity", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Cyan,
            icon: FolderIcon::IdCard,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Pièces d'identité",
                    "Pièces d'identité : carte nationale d'identité, passeport, titre de séjour, permis de conduire",
                ),
                en: (
                    "ID documents",
                    "ID documents: identity card, passport, residence permit, driving licence",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "État civil",
                    "État civil : actes de naissance, de mariage ou de décès, livret de famille, PACS",
                ),
                en: (
                    "Civil status",
                    "Civil status: birth, marriage or death certificates, family record book, civil partnership",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Juridique", ""),
        en: ("Legal", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Slate,
            icon: FolderIcon::Scale,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Successions et donations",
                    "Succession ou donation : notaire, défunt, héritiers, acte de notoriété, déclaration de succession, donation, partage, legs, assurance vie au décès",
                ),
                en: (
                    "Inheritance and gifts",
                    "Inheritance or gift: notary, deceased, heirs, affidavit of inheritance, estate declaration, gift, division, bequest, life insurance on death",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Société",
                    "Société : statuts, extrait Kbis, assemblée générale des associés, procès-verbal, approbation des comptes",
                ),
                en: (
                    "Company",
                    "Company: articles of association, registration extract, shareholders' meeting minutes, approval of accounts",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Justice",
                    "Procédure judiciaire : tribunal, jugement, avocat, commissaire de justice, huissier, assignation, convocation à une audience, plainte, litige",
                ),
                en: (
                    "Justice",
                    "Legal proceedings: court, judgment, lawyer, bailiff, writ of summons, hearing notice, complaint, dispute",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Transport", ""),
        en: ("Transport", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Indigo,
            icon: FolderIcon::Train,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Train",
                    "Billets de train : SNCF, TGV, TER, trajet aller ou retour, gare de départ et d'arrivée, voiture et place, carte de réduction, e-billet",
                ),
                en: (
                    "Train",
                    "Train tickets: rail operator, high-speed or regional train, single or return journey, departure and arrival stations, coach and seat, railcard, e-ticket",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Avion",
                    "Billets d'avion : réservation de vol, compagnie aérienne, aéroport de départ et d'arrivée, numéro de vol, carte d'embarquement, bagages",
                ),
                en: (
                    "Flights",
                    "Plane tickets: flight booking, airline, departure and arrival airports, flight number, boarding pass, luggage",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Route",
                    "Route : péages d'autoroute, carburant, stationnement, recharge électrique, amendes",
                ),
                en: (
                    "Road",
                    "Road: motorway tolls, fuel, parking, EV charging, fines",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Courses et partage",
                    "Courses de VTC ou de taxi, trottinette ou vélo en libre-service, autopartage, covoiturage : reçu de course, trajet, chauffeur",
                ),
                en: (
                    "Rides",
                    "Ride-hailing or taxi rides, shared scooters or bikes, car sharing, carpooling: ride receipts, journey, driver",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Voyages",
                    "Voyages et vacances : réservation d'hôtel, location de vacances, séjour, gîte, camping, office de tourisme, agence de voyage, dates d'arrivée et de départ",
                ),
                en: (
                    "Travel",
                    "Travel and holidays: hotel bookings, holiday rentals, stays, cottages, campsites, tourist office, travel agency, arrival and departure dates",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Véhicule",
                    "Véhicule : certificat d'immatriculation (carte grise), ANTS, achat ou vente d'une voiture, entretien, révision, garage, contrôle technique, cession",
                ),
                en: (
                    "Vehicle",
                    "Vehicle: registration certificate, buying or selling a car, servicing, garage, roadworthiness test, transfer of ownership",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Assurance auto",
                    "Assurance de la voiture : contrat auto, carte verte, bonus-malus, véhicule assuré",
                ),
                en: (
                    "Vehicle insurance",
                    "Car insurance: motor policy, green card, no-claims bonus, insured vehicle",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Animaux", ""),
        en: ("Pets", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Rust,
            icon: FolderIcon::Paw,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Vétérinaire",
                    "Vétérinaire : facture de la clinique vétérinaire, consultation, vaccin, vermifuge, stérilisation, soins pour un chien ou un chat",
                ),
                en: (
                    "Vet",
                    "Vet: veterinary clinic bills, consultations, vaccinations, deworming, neutering, care for a dog or cat",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Garde",
                    "Garde d'animaux : pension, toilettage, promenade, pet-sitter",
                ),
                en: (
                    "Pet care",
                    "Pet care: boarding, grooming, walking, pet sitter",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Assurance animaux",
                    "Assurance pour chien ou chat : contrat d'assurance santé animale, remboursement des frais vétérinaires, identification de l'animal, cotisation",
                ),
                en: (
                    "Pet insurance",
                    "Insurance for dogs or cats: pet health insurance policy, reimbursed vet fees, pet identification, premium",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Fournitures",
                    "Achats pour chien ou chat : croquettes, nourriture pour animaux, friandises, litière, jouets, laisse, animalerie",
                ),
                en: (
                    "Pet supplies",
                    "Purchases for dogs or cats: kibble, pet food, treats, litter, toys, leads, pet shop",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Achats", ""),
        en: ("Shopping", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Amber,
            icon: FolderIcon::Bag,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "En ligne",
                    "Commandes en ligne : factures d'achat sur un site marchand, articles commandés, livraison",
                ),
                en: (
                    "Online",
                    "Online orders: purchase invoices from online shops, items ordered, delivery",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Courses",
                    "Courses alimentaires : ticket de caisse de supermarché, drive, épicerie, produits alimentaires",
                ),
                en: (
                    "Groceries",
                    "Groceries: supermarket receipts, click and collect, corner shop, food products",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Repas livrés",
                    "Livraison de repas : commande à un restaurant livrée à domicile, plats, frais de livraison, livreur",
                ),
                en: (
                    "Food delivery",
                    "Food delivery: restaurant orders delivered home, dishes, delivery fees, courier",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Électronique",
                    "Achat d'appareils électroniques et accessoires : ordinateur, smartphone, tablette, télévision, coque, câble, pièce détachée, numéro de série, garantie",
                ),
                en: (
                    "Electronics",
                    "Electronic devices and accessories: computer, smartphone, tablet, TV, case, cable, spare part, serial number, warranty",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Maison",
                    "Équipement de la maison : meubles, électroménager, literie, décoration, bricolage et jardinage",
                ),
                en: (
                    "Home equipment",
                    "Home equipment: furniture, appliances, bedding, decoration, DIY and gardening",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Vêtements",
                    "Vêtements et chaussures : achat en magasin ou en ligne, taille, retour ou échange",
                ),
                en: (
                    "Clothing",
                    "Clothes and shoes: in-store or online purchases, size, returns or exchanges",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Occasion",
                    "Achat ou vente d'occasion entre particuliers : Vinted, Leboncoin, bordereau d'envoi, point relais, acheteur et vendeur",
                ),
                en: (
                    "Second-hand",
                    "Second-hand buying or selling between individuals: Vinted, Leboncoin, shipping labels, pickup points, buyer and seller",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Loisirs", ""),
        en: ("Leisure", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Plum,
            icon: FolderIcon::Ticket,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Sport",
                    "Sport : licence sportive, adhésion à un club, abonnement à une salle de sport, cours, stage, forfait de ski, certificat médical de non contre-indication",
                ),
                en: (
                    "Sports",
                    "Sports: sports licence, club membership, gym subscription, lessons, courses, ski pass, medical certificate of fitness",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Sorties",
                    "Billets de spectacle, concert, cinéma, musée ou événement",
                ),
                en: (
                    "Events",
                    "Tickets for shows, concerts, cinema, museums or events",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Associations",
                    "Association loi 1901 : adhésion, cotisation de membre, assemblée générale, bureau, carte de membre",
                ),
                en: (
                    "Associations",
                    "Non-profit associations: membership, member's fee, general meeting, board, membership card",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Éducation", ""),
        en: ("Education", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Leaf,
            icon: FolderIcon::GraduationCap,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "École",
                    "École, collège ou lycée : inscription scolaire, cantine, garderie périscolaire, assurance scolaire, frais de scolarité, bulletin scolaire, élève",
                ),
                en: (
                    "School",
                    "School: school enrolment, canteen, after-school care, school insurance, school fees, school reports, pupil",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Études supérieures",
                    "Université et grandes écoles : inscription, étudiant, diplôme",
                ),
                en: (
                    "Higher education",
                    "Universities and colleges: enrolment, student, degree",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Formation",
                    "Formation professionnelle : CPF, organisme de formation, programme, convention de formation, attestation de fin de formation, stagiaire",
                ),
                en: (
                    "Training",
                    "Professional training: training account, training provider, programme, training agreement, certificate of completion, trainee",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: ("Abonnements", ""),
        en: ("Subscriptions", ""),
        appearance: Some(FolderAppearance {
            color: FolderColor::Violet,
            icon: FolderIcon::Repeat,
        }),
        children: &[
            ProposedFolder {
                fr: (
                    "Médias",
                    "Abonnements de streaming vidéo ou musique : séries, films, musique, plateforme, abonnement mensuel",
                ),
                en: (
                    "Media",
                    "Video or music streaming subscriptions: series, films, music, platform, monthly subscription",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Logiciels et cloud",
                    "Facture ou reçu d'abonnement en ligne (invoice, receipt) : logiciel, application, moteur de recherche, assistant IA, hébergement, cloud, nom de domaine, plan mensuel",
                ),
                en: (
                    "Software and cloud",
                    "Online subscription invoice or receipt: software, app, search engine, AI assistant, hosting, cloud, domain name, monthly plan",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Presse",
                    "Abonnements presse : journal, magazine, quotidien en ligne, numéros",
                ),
                en: (
                    "Press",
                    "Press subscriptions: newspapers, magazines, online news, issues",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: (
            "Dons",
            "Reçu fiscal de don à une association ou une fondation : reçu au titre des dons, donateur, réduction d'impôt, articles 200 et 238 bis du code général des impôts",
        ),
        en: (
            "Donations",
            "Tax receipt for a donation to a charity or foundation: donation receipt, donor, tax reduction, articles 200 and 238 bis of the French tax code",
        ),
        appearance: Some(FolderAppearance {
            color: FolderColor::Moss,
            icon: FolderIcon::Gift,
        }),
        children: &[],
    },
];
