use cvc_core::vscode::ChatSession;
use std::fs;
use std::path::PathBuf;

#[test]
fn test_parse_vscode_session() {
    // Navigate up from cvc-core/tests/ to project root, then to common_data
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop(); // cvc-core
    d.push("common_data/chatSessions/47083254-cdd2-4c61-be6c-5e3a7fd17d33.json");

    println!("Reading file from: {:?}", d);
    let content = fs::read_to_string(&d).expect("Failed to read test file");

    let session = ChatSession::parse(&content).expect("Failed to parse session");

    assert!(!session.requests.is_empty());

    let req = &session.requests[0];
    let full_response = req.reconstruct_full_response();

    println!("---------- Reconstructed Response ----------");
    println!("{}", full_response);
    println!("--------------------------------------------");

    // Verification against known content in 470.md
    assert!(
        full_response.contains("I'll help you implement"),
        "Missing introduction text"
    );
    assert!(
        full_response.contains("Read [](file://"),
        "Missing tool output"
    );
    assert!(
        full_response.contains("Created 3 todos"),
        "Missing todo list creation"
    );
}
