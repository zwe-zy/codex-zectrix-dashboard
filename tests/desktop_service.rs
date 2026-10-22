#![cfg(windows)]
use codex_zectrix_dashboard::{desktop::*, *};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use zeroize::Zeroizing;
struct NoCredentials;
impl CredentialStore for NoCredentials {
    fn get(&self) -> Result<Option<Zeroizing<String>>, CredentialError> {
        panic!("Refresh must not read Key")
    }
    fn set(&self, _: &str) -> Result<(), CredentialError> {
        panic!("not authorized")
    }
    fn delete(&self) -> Result<(), CredentialError> {
        panic!("not authorized")
    }
}
struct NoCloud(Arc<AtomicUsize>);
impl SetupCloud for NoCloud {
    fn discover(&mut self, _: &str) -> Result<Vec<SetupDevice>, SetupCloudError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("Refresh must stay local")
    }
    fn upload_png(&mut self, _: &str, _: &str, _: u8, _: &[u8]) -> Result<(), SetupCloudError> {
        panic!("not authorized")
    }
}
#[test]
fn custom_single_card_uses_its_page_and_theme_without_changing_device_credentials() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("settings.json"),
        br#"{"deviceId":"fixture","pageId":1}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("inkcards.json"),
        br#"{"mode":"single","pages":[{"page":4,"theme":"eva"}]}"#,
    )
    .unwrap();
    let mut backend = WindowsDesktopBackend::with_services(
        dir.path().into(),
        dir.path().into(),
        NoCloud(Arc::new(AtomicUsize::new(0))),
        NoCredentials,
    );
    let DesktopReply::Status(status) =
        backend.execute(DesktopCommand::Refresh, &DraftCancellation::default())
    else {
        panic!("custom card must be readable locally")
    };
    assert_eq!(status.pages.len(), 1);
    assert_eq!(status.pages[0].page, 4);
    assert_eq!(status.pages[0].theme, status_board::BoardTheme::Eva);
    assert_eq!(status.configured_page, 4);
    assert_eq!(status.theme, status_board::BoardTheme::Eva);
}

#[test]
fn saving_custom_cards_is_local_atomic_and_refreshes_the_saved_routes() {
    use inkcards::{InkcardPage, InkcardSettings};
    use status_board::BoardTheme::*;
    let dir = tempfile::tempdir().unwrap();
    let device = br#"{"deviceId":"fixture","pageId":3}"#;
    std::fs::write(dir.path().join("settings.json"), device).unwrap();
    std::fs::write(dir.path().join("installation.json"), serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"activeVersion":env!("CARGO_PKG_VERSION"),"previousVersion":null})).unwrap()).unwrap();
    let mut backend = WindowsDesktopBackend::with_services(
        dir.path().into(),
        dir.path().into(),
        NoCloud(Arc::new(AtomicUsize::new(0))),
        NoCredentials,
    );
    for layout in [
        vec![
            InkcardPage {
                page: 1,
                theme: Eva,
            },
            InkcardPage {
                page: 2,
                theme: PopCut,
            },
        ],
        vec![InkcardPage {
            page: 5,
            theme: CyberHud,
        }],
    ] {
        let config = InkcardSettings::from_pages(layout.clone()).unwrap();
        let result = backend.execute(
            DesktopCommand::SetCards(config.clone()),
            &DraftCancellation::default(),
        );
        assert!(matches!(result, DesktopReply::Completed(_)), "{result:?}");
        assert_eq!(
            InkcardSettings::load(dir.path().join("inkcards.json")).unwrap(),
            config
        );
        assert_eq!(
            std::fs::read(dir.path().join("settings.json")).unwrap(),
            device
        );
        let DesktopReply::Status(status) =
            backend.execute(DesktopCommand::Refresh, &DraftCancellation::default())
        else {
            panic!("status after save")
        };
        assert_eq!(
            status
                .pages
                .iter()
                .map(|p| InkcardPage {
                    page: p.page,
                    theme: p.theme
                })
                .collect::<Vec<_>>(),
            layout
        );
        assert_eq!(status.background, DesktopBackground::Stopped);
        assert!(status.pages.iter().all(|p| p.uploaded_png.is_none()));
    }
    let before = std::fs::read(dir.path().join("inkcards.json")).unwrap();
    let invalid = InkcardSettings {
        mode: inkcards::InkcardMode::All,
        pages: Some(vec![]),
    };
    assert!(matches!(
        backend.execute(
            DesktopCommand::SetCards(invalid),
            &DraftCancellation::default()
        ),
        DesktopReply::Failed(_)
    ));
    assert_eq!(
        std::fs::read(dir.path().join("inkcards.json")).unwrap(),
        before
    );
}
#[test]
fn single_page_reports_both_durable_key_rejection_phases() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("settings.json"),
        br#"{"deviceId":"fixture","pageId":1}"#,
    )
    .unwrap();
    let mut backend = WindowsDesktopBackend::with_services(
        dir.path().into(),
        dir.path().into(),
        NoCloud(Arc::new(AtomicUsize::new(0))),
        NoCredentials,
    );
    for (encoded, expected) in [
        (0, false),
        (4, false),
        (0x7fff_ffff, true),
        (0xffff_ffff, true),
    ] {
        let record = StatusFramePublisherState {
            failure_count: encoded,
            ..Default::default()
        };
        std::fs::write(
            dir.path().join("status-frame-state.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        let DesktopReply::Status(status) =
            backend.execute(DesktopCommand::Refresh, &DraftCancellation::default())
        else {
            panic!("expected local status")
        };
        assert_eq!(status.credentials_rejected, expected, "encoded {encoded}");
    }
}
#[test]
fn refresh_does_not_create_codex_session() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut backend = WindowsDesktopBackend::with_services(
        dir.path().into(),
        dir.path().into(),
        NoCloud(calls.clone()),
        NoCredentials,
    );
    for _ in 0..3 {
        let reply = backend.execute(DesktopCommand::Refresh, &DraftCancellation::default());
        let DesktopReply::Status(status) = reply else {
            panic!("expected local status")
        };
        assert!(!status.configured);
        assert!(status.snapshot.is_none());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!dir.path().join("settings.json").exists());
}
#[test]
fn old_version_cannot_modify_new_install() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("installation.json"),
        br#"{"schemaVersion":1,"activeVersion":"9.0.0","previousVersion":null}"#,
    )
    .unwrap();
    let mut backend = WindowsDesktopBackend::with_services(
        dir.path().into(),
        dir.path().into(),
        NoCloud(Arc::new(AtomicUsize::new(0))),
        NoCredentials,
    );
    let reply = backend.execute(
        DesktopCommand::SetStartup(false),
        &DraftCancellation::default(),
    );
    assert!(
        matches!(
            reply,
            DesktopReply::Failed(DesktopError::InstallationChanged)
        ),
        "{reply:?}"
    );
    assert!(!dir.path().join("appearance.json").exists());
}
