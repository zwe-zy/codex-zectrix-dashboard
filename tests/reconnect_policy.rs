use std::time::Duration;

use codex_zectrix_dashboard::ReconnectPolicy;

#[test]
fn reconnect_policy_caps_failures_and_resets_only_after_ten_healthy_minutes() {
    let mut policy = ReconnectPolicy::default();

    assert_eq!(
        policy.record_transport_failure(Duration::ZERO),
        Duration::from_secs(5)
    );
    assert_eq!(
        policy.retry_after(Duration::from_secs(4)),
        Some(Duration::from_secs(1))
    );
    assert_eq!(policy.retry_after(Duration::from_secs(5)), None);
    assert_eq!(
        policy.record_transport_failure(Duration::from_secs(5)),
        Duration::from_secs(30)
    );
    assert_eq!(
        policy.record_transport_failure(Duration::from_secs(35)),
        Duration::from_secs(300)
    );
    assert_eq!(
        policy.record_transport_failure(Duration::from_secs(335)),
        Duration::from_secs(3_600)
    );
    assert_eq!(
        policy.record_transport_failure(Duration::from_secs(3_935)),
        Duration::from_secs(3_600)
    );

    policy.record_session_started(Duration::from_secs(7_535));
    policy.record_transport_healthy(Duration::from_secs(8_134));
    assert_eq!(
        policy.record_transport_failure(Duration::from_secs(8_134)),
        Duration::from_secs(3_600)
    );

    policy.record_session_started(Duration::from_secs(11_734));
    policy.record_transport_healthy(Duration::from_secs(12_334));
    assert_eq!(
        policy.record_transport_failure(Duration::from_secs(12_334)),
        Duration::from_secs(5)
    );
}
