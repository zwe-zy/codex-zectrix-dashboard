use std::fs;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

use codex_zectrix_dashboard::{CredentialError, CredentialStore, Settings};
use zeroize::Zeroizing;

#[derive(Default)]
struct MemoryCredentialStore {
    secret: Mutex<Option<Zeroizing<String>>>,
}

impl CredentialStore for MemoryCredentialStore {
    fn get(&self) -> Result<Option<Zeroizing<String>>, CredentialError> {
        Ok(self
            .secret
            .lock()
            .map_err(|_| CredentialError::Unavailable)?
            .clone())
    }

    fn set(&self, secret: &str) -> Result<(), CredentialError> {
        *self
            .secret
            .lock()
            .map_err(|_| CredentialError::Unavailable)? = Some(Zeroizing::new(secret.to_owned()));
        Ok(())
    }

    fn delete(&self) -> Result<(), CredentialError> {
        *self
            .secret
            .lock()
            .map_err(|_| CredentialError::Unavailable)? = None;
        Ok(())
    }
}

#[test]
fn credential_round_trip_never_enters_settings() {
    let store = MemoryCredentialStore::default();
    store.set("secret-value").unwrap();
    assert_eq!(
        store.get().unwrap().as_deref().map(String::as_str),
        Some("secret-value")
    );
    store.delete().unwrap();
    assert!(store.get().unwrap().is_none());
}

#[test]
fn credential_errors_do_not_echo_secret_material() {
    let error = codex_zectrix_dashboard::CredentialError::Unavailable;
    assert!(!error.to_string().contains("secret-value"));
}

#[test]
fn settings_round_trip_contains_only_device_and_page() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let settings = Settings {
        device_id: "AA:BB:CC:DD:EE:FF".into(),
        page_id: 3,
    };

    settings.save_atomically(&path).unwrap();

    let serialized = fs::read_to_string(&path).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&serialized).unwrap(),
        serde_json::json!({"deviceId": "AA:BB:CC:DD:EE:FF", "pageId": 3})
    );
    assert!(!serialized.contains("apiKey"));
    assert!(!serialized.contains("title"));
    assert!(!serialized.contains("task"));
    let loaded = Settings::load(&path).unwrap().unwrap();
    assert_eq!(loaded.device_id, settings.device_id);
    assert_eq!(loaded.page_id, settings.page_id);

    Settings::remove(&path).unwrap();
    assert!(!path.exists());
}

#[test]
fn settings_rejects_page_ids_outside_the_device_range() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let invalid = Settings {
        device_id: "AA:BB:CC:DD:EE:FF".into(),
        page_id: 0,
    };

    assert!(invalid.save_atomically(&path).is_err());
    fs::write(&path, r#"{"deviceId":"AA:BB:CC:DD:EE:FF","pageId":6}"#).unwrap();
    assert!(Settings::load(&path).is_err());
}

#[test]
fn settings_rejects_empty_whitespace_unsafe_and_unreasonable_routing_ids() {
    // Catches treating merely valid JSON as an authoritative cloud routing identifier.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let cases = [
        String::new(),
        "   ".into(),
        "AA BB CC".into(),
        "..".into(),
        ".hidden-route".into(),
        "../unsafe-route".into(),
        "route/child".into(),
        "路由".into(),
        "A".repeat(129),
    ];

    for device_id in cases {
        let invalid = Settings {
            device_id: device_id.clone(),
            page_id: 3,
        };
        assert!(invalid.save_atomically(&path).is_err(), "{device_id:?}");

        fs::write(
            &path,
            serde_json::json!({"deviceId": device_id, "pageId": 3}).to_string(),
        )
        .unwrap();
        assert!(Settings::load(&path).is_err());
    }
}

#[test]
fn settings_debug_redacts_the_routing_identifier() {
    // Catches an otherwise private routing ID escaping through public Debug diagnostics.
    let sentinel = "PRIVATE_DEVICE_ROUTE_SENTINEL";
    let settings = Settings {
        device_id: sentinel.into(),
        page_id: 3,
    };

    let debug = format!("{settings:?}");

    assert!(!debug.contains(sentinel));
    assert!(debug.contains("[redacted]"));
}

#[test]
fn settings_replacement_preserves_the_new_value() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("settings.json");
    let initial = Settings {
        device_id: "AA:BB:CC:DD:EE:FF".into(),
        page_id: 1,
    };
    let replacement = Settings {
        device_id: "AA:BB:CC:DD:EE:FF".into(),
        page_id: 5,
    };

    initial.save_atomically(&path).unwrap();
    replacement.save_atomically(&path).unwrap();

    let loaded = Settings::load(&path).unwrap().unwrap();
    assert_eq!(loaded.page_id, 5);
}

#[test]
fn settings_load_rejects_unknown_fields() {
    let directory = tempfile::tempdir().unwrap();
    for field in ["apiKey", "task", "title", "prompt", "plan", "privacyMode"] {
        let path = directory.path().join(format!("{field}.json"));
        fs::write(
            &path,
            serde_json::json!({
                "deviceId": "AA:BB:CC:DD:EE:FF",
                "pageId": 3,
                field: "unwanted-value",
            })
            .to_string(),
        )
        .unwrap();

        assert!(Settings::load(&path).is_err(), "field {field} was accepted");
    }
}

#[test]
fn simultaneous_settings_saves_do_not_collide_on_staging_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = Arc::new(directory.path().join("settings.json"));
    let barrier = Arc::new(Barrier::new(2));
    let first_path = Arc::clone(&path);
    let first_barrier = Arc::clone(&barrier);
    let first = thread::spawn(move || {
        first_barrier.wait();
        Settings {
            device_id: "A".repeat(128),
            page_id: 1,
        }
        .save_atomically(first_path.as_ref())
    });
    let second_path = Arc::clone(&path);
    let second = thread::spawn(move || {
        barrier.wait();
        Settings {
            device_id: "B".repeat(128),
            page_id: 2,
        }
        .save_atomically(second_path.as_ref())
    });

    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    assert!(matches!(
        Settings::load(path.as_ref()).unwrap().unwrap().page_id,
        1 | 2
    ));
}
