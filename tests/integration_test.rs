use sdk::documents::GetAllCmd;
use sdk::{Sdk, SdkConfig, StorageType};
use std::path::Path;

#[test]
fn run_integration_tests() {
    let sdk = Sdk::init(&SdkConfig {
        storage_type: StorageType::Memory,
    })
    .unwrap();

    let file_path = Path::new("./fixtures/basic-text.pdf");

    // Check the save content
    let saved = sdk.documents.save_file_from_path(file_path).unwrap();
    assert_eq!("basic-text.pdf", saved.metadata.name);
    assert_eq!(
        "382ac9ea60bb0ba40c9eb0815f4ec7f60e248fc136d3055c411d09709e1b9b31",
        saved.metadata.checksum
    );
    assert_eq!("application/pdf", saved.metadata.detected_type);
    assert_eq!(74656, saved.metadata.size);
    assert_eq!(
        "Sample Document for PDF Testing\nIntroduction\nThis is a simple document created to test basic PDF functionality. It includes various text formatting\noptions to ensure proper rendering in PDF readers.\nText Formatting Examples\n1. \nBold text\n is used for emphasis.\n2. \nItalic text\n can be used for titles or subtle emphasis.\n3. \nStrikethrough\n is used to show deleted text.\nLists\nHere's an example of an unordered list:\nItem 1\nItem 2\nItem 3\nAnd here's an ordered list:\n1. \nFirst item\n2. \nSecond item\n3. \nThird item\nQuote\nThis is an example of a block quote. It can be used to highlight important information or\ncitations.\nTable\nHeader 1\nHeader 2\nHeader 3\nRow 1, Col 1\nRow 1, Col 2\nRow 1, Col 3\nRow 2, Col 1\nRow 2, Col 2\nRow 2, Col 3\nThis document demonstrates various formatting options that should translate well to PDF format.\nThis sample PDF file is provided by \nSample-Files.com\n. Visit us for more sample files and resources.\n",
        saved.metadata.transcript.as_ref().unwrap()
    );
    assert_ne!("", saved.metadata.id.to_string());
    assert_ne!(0, saved.file_preview.len());
    assert_ne!(0, saved.file_content.len());

    // Check get_all result
    let docs = sdk
        .documents
        .get_all(&GetAllCmd {
            after: None,
            limit: None,
        })
        .unwrap();

    assert_eq!(1, docs.len());
    assert_eq!(&saved.metadata, docs.first().unwrap());

    // Check get_preview
    let preview = sdk.documents.get_preview(&saved.metadata.id).unwrap();
    assert_eq!(saved.file_preview, preview);

    // Check get_content
    let content = sdk.documents.get_content(&saved.metadata.id).unwrap();
    assert_eq!(saved.file_content, content);

    // Check get_by_id
    let res = sdk.documents.get_by_id(&saved.metadata.id).unwrap();
    assert_eq!(saved.metadata, res);
}
