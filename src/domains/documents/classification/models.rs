use uuid::Uuid;

use super::super::DocumentType;

/// What [`super::Service::classify`] guessed about a document. A field is
/// `None` when no choice clearly beat the others (or, for the folder, none
/// holds documents close enough to it, or none holds any).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Classification {
    pub(crate) r#type: Option<DocumentType>,
    /// The folder whose documents the document is the closest to.
    pub(crate) folder_id: Option<Uuid>,
}

/// A document already filed directly in a folder, as
/// [`super::Service::classify`] files a new one by.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FiledDocument {
    pub(crate) folder_id: Uuid,
    /// Its cosine similarity to the new document: its best chunk's to the
    /// mean of the new document's chunks, as
    /// [`super::super::Service::similar`] compares documents.
    pub(crate) similarity: f32,
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

        Self {
            classification: Classification {
                r#type: Some(
                    DocumentType::ALL[random_u64(DocumentType::ALL.len() as u64) as usize],
                ),
                folder_id: Some(Uuid::now_v7()),
            },
        }
    }

    pub(crate) fn with_type(mut self, r#type: impl Into<Option<DocumentType>>) -> Self {
        self.classification.r#type = r#type.into();
        self
    }

    pub(crate) fn with_folder(mut self, folder_id: impl Into<Option<Uuid>>) -> Self {
        self.classification.folder_id = folder_id.into();
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
    fn fake_classification_with_methods_override_defaults() {
        let folder_id = Uuid::now_v7();
        let classification = FakeClassification::new()
            .with_type(DocumentType::TaxNotice)
            .with_folder(folder_id)
            .build();

        assert_eq!(
            classification,
            Classification {
                r#type: Some(DocumentType::TaxNotice),
                folder_id: Some(folder_id),
            }
        );
    }
}
