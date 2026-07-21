use cvc_core::models::{DerivationEvent, RangeEvidence};

#[test]
fn rust_canonical_ids_match_format5_golden_fixture() -> anyhow::Result<()> {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../test-data/format5-golden.json"))?;
    let event: DerivationEvent = serde_json::from_value(fixture["event"].clone())?;
    let range: RangeEvidence = serde_json::from_value(fixture["range"].clone())?;
    assert_eq!(
        event.canonical_id(),
        "a5b182bd90e298bcb84948ce7d7d6caa1f9b4f2e993131267459776479556576"
    );
    assert_eq!(
        range.canonical_id(),
        "13e56effb5b051a74275009163cb3277f1188573b588474f5abb0d0feae2fd93"
    );
    assert!(event.verify_id());
    assert!(range.verify_id());
    assert_eq!(serde_json::to_value(&event)?, fixture["event"]);
    assert_eq!(serde_json::to_value(&range)?, fixture["range"]);
    Ok(())
}
