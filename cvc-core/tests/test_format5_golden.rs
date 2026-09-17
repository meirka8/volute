use cvc_core::models::{DerivationEvent, RangeEvidence};

#[test]
fn rust_canonical_ids_match_format5_golden_fixture() -> anyhow::Result<()> {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../test-data/format5-golden.json"))?;
    let event: DerivationEvent = serde_json::from_value(fixture["event"].clone())?;
    let range: RangeEvidence = serde_json::from_value(fixture["range"].clone())?;
    let range_v2: RangeEvidence = serde_json::from_value(fixture["range_v2"].clone())?;
    assert_eq!(
        event.canonical_id(),
        "a5b182bd90e298bcb84948ce7d7d6caa1f9b4f2e993131267459776479556576"
    );
    assert_eq!(
        range.canonical_id(),
        "13e56effb5b051a74275009163cb3277f1188573b588474f5abb0d0feae2fd93"
    );
    // Identical in every other field, so this vector pins that `format` and
    // `version` alone separate the two bodies' IDs.
    assert_eq!(
        range_v2.canonical_id(),
        "883f7816cbe01fca8538335525af80e5ea663a7bd87543c8187d9276fc0af4d6"
    );
    assert!(event.verify_id());
    assert!(range.verify_id());
    assert!(range_v2.verify_id());
    assert_eq!(serde_json::to_value(&event)?, fixture["event"]);
    assert_eq!(serde_json::to_value(&range)?, fixture["range"]);
    assert_eq!(serde_json::to_value(&range_v2)?, fixture["range_v2"]);
    Ok(())
}

#[test]
fn range_bodies_must_pair_their_format_with_their_version() -> anyhow::Result<()> {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../test-data/format5-golden.json"))?;
    for (format, version) in [
        ("cvc.range-evidence/v1", 2),
        ("cvc.range-evidence/v2", 1),
        ("cvc.range-evidence/v3", 3),
    ] {
        let mut body = fixture["range"].clone();
        body["format"] = serde_json::json!(format);
        body["version"] = serde_json::json!(version);
        let mut range: RangeEvidence = serde_json::from_value(body)?;
        // Even re-derived, an unknown or mismatched body is refused: the ID is
        // self-consistent but the format itself is not one CVC accepts.
        range.range_id = range.canonical_id();
        assert!(!range.verify_id(), "{format} v{version} must not verify");
    }
    Ok(())
}
