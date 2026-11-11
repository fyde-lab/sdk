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
                    "Factures d'eau : consommation en m³, abonnement eau potable et assainissement, relevé du compteur d'eau",
                ),
                en: (
                    "Water",
                    "Water bills: consumption in m³, drinking water and sewerage subscription, water meter readings",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Assurance habitation",
                    "Assurance habitation multirisque : attestation d'assurance du logement, avis d'échéance, cotisation annuelle",
                ),
                en: (
                    "Home insurance",
                    "Home insurance: certificate of insurance for the home, renewal notices, annual premium",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Services à domicile",
                    "Services à domicile : ménage, repassage, jardinage, entretien de la maison, petit bricolage",
                ),
                en: (
                    "Home services",
                    "Home services: cleaning, ironing, gardening, house maintenance, small repairs",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Loyer et charges",
                    "Quittances de loyer, bail de location, charges locatives, charges de copropriété, appels de fonds du syndic",
                ),
                en: (
                    "Rent and charges",
                    "Rent receipts, lease, service charges, co-ownership charges, calls for funds from the building manager",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Achat et vente",
                    "Achat ou vente d'un bien immobilier : compromis de vente, acte de vente, diagnostics, agence immobilière, notaire",
                ),
                en: (
                    "Buying and selling",
                    "Buying or selling a property: preliminary sale agreement, deed of sale, surveys, estate agent, notary",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Travaux",
                    "Travaux dans le logement : devis ou facture d'artisan, rénovation, plomberie, électricité, installation de chauffage",
                ),
                en: (
                    "Works",
                    "Works in the home: tradesperson's quotes or invoices, renovation, plumbing, electrics, heating installation",
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
                    "Factures de forfait mobile : téléphone portable, appels, SMS, données internet mobile en Go",
                ),
                en: (
                    "Mobile",
                    "Mobile plan bills: mobile phone, calls, texts, mobile data in GB",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Internet",
                    "Factures de box internet : abonnement fibre ou ADSL, téléphone fixe et télévision",
                ),
                en: (
                    "Internet",
                    "Internet box bills: fibre or ADSL subscription, landline and TV",
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
                    "Relevés de compte bancaire : solde, opérations, virements, prélèvements, paiements par carte ; RIB, convention de compte, frais bancaires",
                ),
                en: (
                    "Banking",
                    "Bank statements: balance, transactions, transfers, direct debits, card payments; bank details, account agreement, bank fees",
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
                    "Placements financiers : assurance vie, PEA, compte-titres, SCPI, relevé de portefeuille, valorisation",
                ),
                en: (
                    "Investments",
                    "Investments: life insurance, share savings plan, securities account, real estate funds, portfolio statements, valuation",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Crédits",
                    "Prêt immobilier ou crédit à la consommation : offre de prêt, tableau d'amortissement, échéances, taux, capital restant dû",
                ),
                en: (
                    "Loans",
                    "Mortgage or consumer credit: loan offer, repayment schedule, instalments, rate, outstanding principal",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Jeux d'argent",
                    "Jeux d'argent : paris sportifs, poker, loterie, casino en ligne, relevé des gains",
                ),
                en: (
                    "Gambling",
                    "Gambling: sports betting, poker, lottery, online casino, winnings statements",
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
                    "Avis d'impôt sur le revenu, déclaration des revenus et accusé de réception, avis de situation déclarative, prélèvement à la source",
                ),
                en: (
                    "Income tax",
                    "Income tax notices, tax returns and acknowledgements, filing status notices, tax withheld at source",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Attestations fiscales",
                    "Imprimé fiscal unique (IFU), attestations fiscales annuelles à reporter sur la déclaration, attestations de services à la personne ouvrant droit à crédit d'impôt",
                ),
                en: (
                    "Tax certificates",
                    "Annual tax forms and certificates to report on the tax return, certificates for home services giving a tax credit",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Taxe foncière",
                    "Avis de taxe foncière : propriétés bâties ou non bâties, base d'imposition, taux communal, taxe d'enlèvement des ordures ménagères",
                ),
                en: (
                    "Property tax",
                    "Property tax notices: built or unbuilt property, tax base, local rate, household waste collection tax",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Taxe d'habitation",
                    "Avis de taxe d'habitation : résidence secondaire, valeur locative, taux voté par la commune, avis de dégrèvement",
                ),
                en: (
                    "Housing tax",
                    "Housing tax notices: second home, rental value, local rate, relief notices",
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
                    "Assurance maladie (CPAM, ameli) : relevés de remboursement de soins, attestation de droits, carte Vitale",
                ),
                en: (
                    "Health insurance",
                    "National health insurance: statements of reimbursed care, certificate of entitlement, health card",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Mutuelle",
                    "Mutuelle santé complémentaire : décomptes de la mutuelle, part complémentaire remboursée, cotisation, garanties du contrat",
                ),
                en: (
                    "Complementary health",
                    "Complementary health insurance: statements, top-up reimbursements, premiums, cover",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Soins",
                    "Soins médicaux : ordonnances, comptes rendus médicaux, factures de médecin, dentiste, pharmacie, hôpital, analyses",
                ),
                en: (
                    "Care",
                    "Medical care: prescriptions, medical reports, bills from doctors, dentists, pharmacies, hospitals, lab tests",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Prévoyance",
                    "Contrat de prévoyance : garantie décès, invalidité, incapacité de travail, capital versé aux bénéficiaires",
                ),
                en: (
                    "Death and disability cover",
                    "Death and disability cover: death, disability and incapacity benefits, lump sum paid to beneficiaries",
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
                    "Bulletins de salaire : salaire brut, cotisations sociales, net à payer, net imposable, employeur",
                ),
                en: (
                    "Payslips",
                    "Payslips: gross salary, social contributions, net pay, taxable pay, employer",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Contrats et attestations",
                    "Contrats de travail, attestations employeur, certificats de travail, soldes de tout compte",
                ),
                en: (
                    "Employment",
                    "Employment contracts, employer certificates, certificates of employment, final settlements",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Particulier employeur",
                    "Particulier employeur : CESU, Pajemploi, salaire d'un employé à domicile ou d'une assistante maternelle",
                ),
                en: (
                    "Household employer",
                    "Household employer: pay slips and declarations for a home employee or childminder",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Indépendant",
                    "Activité indépendante ou portage salarial : auto-entrepreneur, URSSAF, facturation de missions, chiffre d'affaires, comptes rendus d'activité",
                ),
                en: (
                    "Freelance",
                    "Self-employment or umbrella company: social contributions, invoicing missions, turnover, activity reports",
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
                    "Retraite : pension de base ou complémentaire, relevé de carrière, trimestres validés, caisse de retraite",
                ),
                en: (
                    "Retirement",
                    "Retirement: basic or supplementary pension, career statement, pension fund",
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
                    "Prestations familiales de la CAF : allocations familiales, aide au logement, prime d'activité, congé parental",
                ),
                en: (
                    "Benefits",
                    "Family benefits: child benefit, housing benefit, in-work bonus, parental leave",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Garde d'enfants",
                    "Garde d'enfant : crèche, halte-garderie, assistante maternelle, centre de loisirs, frais de garde",
                ),
                en: (
                    "Childcare",
                    "Childcare: nursery, day care, childminder, holiday club, childcare fees",
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
                    "Succession ou donation : notaire, héritiers, acte de donation, déclaration de succession, partage",
                ),
                en: (
                    "Inheritance and gifts",
                    "Inheritance or gift: notary, heirs, deed of gift, estate declaration, division of the estate",
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
                    "Procédure judiciaire : tribunal, jugement, avocat, commissaire de justice, assignation, convocation à une audience",
                ),
                en: (
                    "Justice",
                    "Legal proceedings: court, judgment, lawyer, bailiff, summons, hearing notice",
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
                    "Billets de train : trajet, gare de départ et d'arrivée, voiture, place",
                ),
                en: (
                    "Train",
                    "Train tickets: journey, departure and arrival stations, coach, seat",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Avion",
                    "Billets d'avion, cartes d'embarquement, réservations de vol",
                ),
                en: ("Flights", "Plane tickets, boarding passes, flight bookings"),
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
                    "Courses de VTC ou de taxi, trottinettes ou vélos en libre-service, autopartage",
                ),
                en: (
                    "Rides",
                    "Ride-hailing or taxi rides, shared scooters or bikes, car sharing",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Voyages",
                    "Voyages : réservations d'hôtel, locations de vacances, séjours, agence de voyage",
                ),
                en: (
                    "Travel",
                    "Travel: hotel bookings, holiday rentals, stays, travel agency",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Véhicule",
                    "Véhicule : carte grise, achat d'une voiture, entretien, révision, contrôle technique",
                ),
                en: (
                    "Vehicle",
                    "Vehicle: registration certificate, buying a car, servicing, roadworthiness test",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Assurance auto",
                    "Assurance automobile ou deux-roues : attestation d'assurance, carte verte, avis d'échéance, bonus-malus",
                ),
                en: (
                    "Vehicle insurance",
                    "Car or motorbike insurance: certificate of insurance, renewal notices, no-claims bonus",
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
                    "Vétérinaire : consultations, vaccins, soins pour chien ou chat, factures de la clinique",
                ),
                en: (
                    "Vet",
                    "Vet: consultations, vaccinations, care for dogs or cats, clinic bills",
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
                    "Assurance santé pour chien ou chat : cotisation, remboursement des frais vétérinaires",
                ),
                en: (
                    "Pet insurance",
                    "Health insurance for dogs or cats: premium, reimbursed vet fees",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Fournitures",
                    "Animalerie : nourriture, croquettes, litière, accessoires pour animaux",
                ),
                en: ("Pet supplies", "Pet shop: food, litter, accessories"),
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
                    "Courses alimentaires : tickets de caisse de supermarché, drive, épicerie",
                ),
                en: (
                    "Groceries",
                    "Groceries: supermarket receipts, click and collect, corner shop",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Repas livrés",
                    "Livraison de repas : commandes à un restaurant livrées à domicile",
                ),
                en: (
                    "Food delivery",
                    "Food delivery: restaurant orders delivered home",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Électronique",
                    "Achats d'appareils électroniques : ordinateur, smartphone, tablette, télévision, garantie",
                ),
                en: (
                    "Electronics",
                    "Electronics: computer, smartphone, tablet, TV, warranty",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Maison",
                    "Équipement de la maison : meubles, électroménager, literie, décoration",
                ),
                en: (
                    "Home equipment",
                    "Home equipment: furniture, appliances, bedding, decoration",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Vêtements",
                    "Vêtements et chaussures : achats en magasin ou en ligne, retours",
                ),
                en: (
                    "Clothing",
                    "Clothes and shoes: in-store or online purchases, returns",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Occasion",
                    "Achat ou vente d'occasion entre particuliers : Vinted, Leboncoin, bordereau d'envoi",
                ),
                en: (
                    "Second-hand",
                    "Second-hand buying or selling between individuals: marketplaces, shipping labels",
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
                    "Sport : licence, adhésion à un club, abonnement à une salle de sport, certificat médical",
                ),
                en: (
                    "Sports",
                    "Sports: licence, club membership, gym subscription, medical certificate",
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
                    "Adhésions à une association : cotisation annuelle, carte de membre",
                ),
                en: (
                    "Associations",
                    "Association memberships: annual fee, membership card",
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
                    "École, collège ou lycée : inscription, cantine, frais de scolarité, bulletins scolaires, périscolaire",
                ),
                en: (
                    "School",
                    "School: enrolment, canteen, school fees, school reports, after-school care",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Études supérieures",
                    "Études supérieures : inscription à l'université ou en école, frais de scolarité, certificat de scolarité",
                ),
                en: (
                    "Higher education",
                    "Higher education: university or college enrolment, tuition fees, certificate of enrolment",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Formation",
                    "Formation professionnelle : CPF, organisme de formation, convention de formation, attestation de fin de formation",
                ),
                en: (
                    "Training",
                    "Professional training: training provider, training agreement, certificate of completion",
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
                    "Abonnements média : streaming vidéo ou musique, plateformes de séries et de films",
                ),
                en: (
                    "Media",
                    "Media subscriptions: video or music streaming, series and film platforms",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Logiciels et cloud",
                    "Abonnements logiciel ou cloud : hébergement web, serveur, nom de domaine, stockage en ligne, licences",
                ),
                en: (
                    "Software and cloud",
                    "Software or cloud subscriptions: web hosting, servers, domain names, online storage, licences",
                ),
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: (
                    "Presse",
                    "Abonnements presse : journal, magazine, quotidien en ligne",
                ),
                en: (
                    "Press",
                    "Press subscriptions: newspapers, magazines, online news",
                ),
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: (
            "Dons",
            "Reçus fiscaux de dons à une association ou une fondation, ouvrant droit à réduction d'impôt",
        ),
        en: (
            "Donations",
            "Tax receipts for donations to a charity or foundation, giving a tax reduction",
        ),
        appearance: Some(FolderAppearance {
            color: FolderColor::Moss,
            icon: FolderIcon::Gift,
        }),
        children: &[],
    },
];
