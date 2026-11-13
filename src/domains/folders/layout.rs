use super::{FolderAppearance, FolderColor, FolderIcon};

/// A folder of [`PROPOSED_LAYOUT`], named in French and English.
pub(super) struct ProposedFolder {
    pub(super) fr: &'static str,
    pub(super) en: &'static str,
    /// Set on top-level folders only.
    pub(super) appearance: Option<FolderAppearance>,
    pub(super) children: &'static [ProposedFolder],
}

/// A realistic vault's folders, for tests: one per part of life, with a
/// subfolder per kind of paper (the classifier's real-model test fills some
/// with synthetic documents, then files others by them).
pub(super) const PROPOSED_LAYOUT: &[ProposedFolder] = &[
    ProposedFolder {
        fr: "Logement",
        en: "Housing",
        appearance: Some(FolderAppearance {
            color: FolderColor::Sand,
            icon: FolderIcon::Home,
        }),
        children: &[
            ProposedFolder {
                fr: "Énergie",
                en: "Energy",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Eau",
                en: "Water",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Assurance habitation",
                en: "Home insurance",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Services à domicile",
                en: "Home services",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Loyer et charges",
                en: "Rent and charges",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Achat et vente",
                en: "Buying and selling",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Travaux",
                en: "Works",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Télécom",
        en: "Telecom",
        appearance: Some(FolderAppearance {
            color: FolderColor::Blue,
            icon: FolderIcon::Phone,
        }),
        children: &[
            ProposedFolder {
                fr: "Mobile",
                en: "Mobile",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Internet",
                en: "Internet",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Argent",
        en: "Money",
        appearance: Some(FolderAppearance {
            color: FolderColor::Green,
            icon: FolderIcon::Wallet,
        }),
        children: &[
            ProposedFolder {
                fr: "Banque",
                en: "Banking",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Épargne",
                en: "Savings",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Placements",
                en: "Investments",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Crédits",
                en: "Loans",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Jeux d'argent",
                en: "Gambling",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Impôts",
        en: "Taxes",
        appearance: Some(FolderAppearance {
            color: FolderColor::Brick,
            icon: FolderIcon::Landmark,
        }),
        children: &[
            ProposedFolder {
                fr: "Impôt sur le revenu",
                en: "Income tax",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Attestations fiscales",
                en: "Tax certificates",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Taxe foncière",
                en: "Property tax",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Taxe d'habitation",
                en: "Housing tax",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Santé",
        en: "Health",
        appearance: Some(FolderAppearance {
            color: FolderColor::Rose,
            icon: FolderIcon::Heart,
        }),
        children: &[
            ProposedFolder {
                fr: "Assurance maladie",
                en: "Health insurance",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Mutuelle",
                en: "Complementary health",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Soins",
                en: "Care",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Prévoyance",
                en: "Death and disability cover",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Travail",
        en: "Work",
        appearance: Some(FolderAppearance {
            color: FolderColor::Teal,
            icon: FolderIcon::Briefcase,
        }),
        children: &[
            ProposedFolder {
                fr: "Bulletins de paie",
                en: "Payslips",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Contrats et attestations",
                en: "Employment",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Particulier employeur",
                en: "Household employer",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Indépendant",
                en: "Freelance",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Chômage",
                en: "Unemployment",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Retraite",
                en: "Retirement",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Famille",
        en: "Family",
        appearance: Some(FolderAppearance {
            color: FolderColor::Ochre,
            icon: FolderIcon::Users,
        }),
        children: &[
            ProposedFolder {
                fr: "Prestations",
                en: "Benefits",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Garde d'enfants",
                en: "Childcare",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Identité",
        en: "Identity",
        appearance: Some(FolderAppearance {
            color: FolderColor::Cyan,
            icon: FolderIcon::IdCard,
        }),
        children: &[
            ProposedFolder {
                fr: "Pièces d'identité",
                en: "ID documents",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "État civil",
                en: "Civil status",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Juridique",
        en: "Legal",
        appearance: Some(FolderAppearance {
            color: FolderColor::Slate,
            icon: FolderIcon::Scale,
        }),
        children: &[
            ProposedFolder {
                fr: "Successions et donations",
                en: "Inheritance and gifts",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Société",
                en: "Company",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Justice",
                en: "Justice",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Transport",
        en: "Transport",
        appearance: Some(FolderAppearance {
            color: FolderColor::Indigo,
            icon: FolderIcon::Train,
        }),
        children: &[
            ProposedFolder {
                fr: "Train",
                en: "Train",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Avion",
                en: "Flights",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Route",
                en: "Road",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Courses et partage",
                en: "Rides",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Voyages",
                en: "Travel",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Véhicule",
                en: "Vehicle",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Assurance auto",
                en: "Vehicle insurance",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Animaux",
        en: "Pets",
        appearance: Some(FolderAppearance {
            color: FolderColor::Rust,
            icon: FolderIcon::Paw,
        }),
        children: &[
            ProposedFolder {
                fr: "Vétérinaire",
                en: "Vet",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Garde",
                en: "Pet care",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Assurance animaux",
                en: "Pet insurance",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Fournitures",
                en: "Pet supplies",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Achats",
        en: "Shopping",
        appearance: Some(FolderAppearance {
            color: FolderColor::Amber,
            icon: FolderIcon::Bag,
        }),
        children: &[
            ProposedFolder {
                fr: "En ligne",
                en: "Online",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Courses",
                en: "Groceries",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Repas livrés",
                en: "Food delivery",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Électronique",
                en: "Electronics",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Maison",
                en: "Home equipment",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Vêtements",
                en: "Clothing",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Occasion",
                en: "Second-hand",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Loisirs",
        en: "Leisure",
        appearance: Some(FolderAppearance {
            color: FolderColor::Plum,
            icon: FolderIcon::Ticket,
        }),
        children: &[
            ProposedFolder {
                fr: "Sport",
                en: "Sports",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Sorties",
                en: "Events",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Associations",
                en: "Associations",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Éducation",
        en: "Education",
        appearance: Some(FolderAppearance {
            color: FolderColor::Leaf,
            icon: FolderIcon::GraduationCap,
        }),
        children: &[
            ProposedFolder {
                fr: "École",
                en: "School",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Études supérieures",
                en: "Higher education",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Formation",
                en: "Training",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Abonnements",
        en: "Subscriptions",
        appearance: Some(FolderAppearance {
            color: FolderColor::Violet,
            icon: FolderIcon::Repeat,
        }),
        children: &[
            ProposedFolder {
                fr: "Médias",
                en: "Media",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Logiciels et cloud",
                en: "Software and cloud",
                appearance: None,
                children: &[],
            },
            ProposedFolder {
                fr: "Presse",
                en: "Press",
                appearance: None,
                children: &[],
            },
        ],
    },
    ProposedFolder {
        fr: "Dons",
        en: "Donations",
        appearance: Some(FolderAppearance {
            color: FolderColor::Moss,
            icon: FolderIcon::Gift,
        }),
        children: &[],
    },
];
