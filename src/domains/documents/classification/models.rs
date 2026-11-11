use super::super::{Category, DocumentType, Subcategory};

/// What [`super::Service::classify`] guessed about a document. A field is
/// `None` when no choice clearly beat the others; `subcategory` can also be
/// `None` under a `category` because that's where such documents belong
/// (local taxes are [`Category::Taxes`] alone).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Classification {
    /// Unused by uploads since folders replaced categories (they only keep
    /// the type).
    pub(crate) category: Option<Category>,
    /// Always one of `category`'s, and `None` without one.
    pub(crate) subcategory: Option<Subcategory>,
    pub(crate) r#type: Option<DocumentType>,
}

/// Builds a [`Classification`] filled with random-but-plausible data,
/// overridable field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeClassification {
    classification: Classification,
}

#[cfg(test)]
impl FakeClassification {
    pub(crate) fn new() -> Self {
        use crate::tools::testing::random_u64;

        let subcategory = Subcategory::ALL[random_u64(Subcategory::ALL.len() as u64) as usize];
        Self {
            classification: Classification {
                category: Some(subcategory.category()),
                subcategory: Some(subcategory),
                r#type: Some(
                    DocumentType::ALL[random_u64(DocumentType::ALL.len() as u64) as usize],
                ),
            },
        }
    }

    pub(crate) fn with_category(mut self, category: impl Into<Option<Category>>) -> Self {
        self.classification.category = category.into();
        self
    }

    pub(crate) fn with_subcategory(mut self, subcategory: impl Into<Option<Subcategory>>) -> Self {
        self.classification.subcategory = subcategory.into();
        self
    }

    pub(crate) fn with_type(mut self, r#type: impl Into<Option<DocumentType>>) -> Self {
        self.classification.r#type = r#type.into();
        self
    }

    pub(crate) fn build(self) -> Classification {
        self.classification
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_classification_builds_a_subcategory_of_its_category() {
        let classification = FakeClassification::new().build();

        assert_eq!(
            classification
                .subcategory
                .map(|subcategory| subcategory.category()),
            classification.category
        );
        assert!(classification.r#type.is_some());
    }

    #[test]
    fn fake_classification_with_methods_override_defaults() {
        let classification = FakeClassification::new()
            .with_category(Category::Taxes)
            .with_subcategory(None)
            .with_type(DocumentType::TaxNotice)
            .build();

        assert_eq!(
            classification,
            Classification {
                category: Some(Category::Taxes),
                subcategory: None,
                r#type: Some(DocumentType::TaxNotice),
            }
        );
    }
}
