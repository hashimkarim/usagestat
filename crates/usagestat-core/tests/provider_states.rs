use usagestat_core::{UsageSnapshot, model::ProviderState};

#[test]
fn old_snapshots_remain_readable_and_new_states_are_additive() {
    let old = serde_json::json!({"providerId": "fixture", "displayName": "Fixture",
        "metrics": [], "fetchedAt": "2026-01-01T00:00:00Z", "pace": null, "statusPageUrl": null});
    let snapshot: UsageSnapshot = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(snapshot.state, None);
    assert_eq!(serde_json::to_value(snapshot).unwrap(), old);
    for (message, expected) in [
        (
            "unsupported: source unavailable",
            ProviderState::Unsupported,
        ),
        ("missing-auth: sign in", ProviderState::MissingAuth),
        ("no-data: account has no usage", ProviderState::NoData),
        (
            "keychain read failed: credential-denied: OS refused access",
            ProviderState::CredentialDenied,
        ),
        (
            "keychain read failed: credential-unavailable: store locked",
            ProviderState::CredentialUnavailable,
        ),
        (
            "credential-account-mismatch: another account",
            ProviderState::CredentialAccountMismatch,
        ),
        (
            "credential-malformed: invalid encoding",
            ProviderState::CredentialMalformed,
        ),
        ("Probe timed out after 10 seconds", ProviderState::TimedOut),
        ("unclassified error", ProviderState::Failed),
    ] {
        let snapshot = UsageSnapshot::error("fixture", "Fixture", message);
        assert_eq!(snapshot.state, Some(expected), "{message}");
        assert_eq!(snapshot.source.as_deref(), Some("error"));
        assert_eq!(snapshot.metrics.len(), 1);
    }
}
