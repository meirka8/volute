use cvc_core::vscode::ChatSession;

#[test]
fn test_parse_vscode_session() {
    let json_content = r#"{
      "version": 1,
      "requests": [
        {
          "requestId": "req-1",
          "message": {
            "text": "Implement todo list"
          },
          "response": [
            {
              "kind": "text",
              "value": "I'll help you implement a todo list."
            },
            {
              "kind": "toolInvocationSerialized",
              "toolName": "read_file",
              "pastTenseMessage": {
                  "value": "Read [](file:///path/to/file)"
              }
            },
            {
              "kind": "text",
              "value": "Created 3 todos for you."
            }
          ]
        }
      ]
    }"#;

    println!("Using embedded test fixture");
    let content = json_content;

    let session = ChatSession::parse(content).expect("Failed to parse session");

    assert!(!session.requests.is_empty());

    let req = &session.requests[0];
    let full_response = req.reconstruct_full_response();

    println!("---------- Reconstructed Response ----------");
    println!("{}", full_response);
    println!("--------------------------------------------");

    // Verification against embedded content
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
