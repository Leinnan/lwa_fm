//! Where servers' passwords are kept: the system's keychain (Windows
//! Credential Manager, the macOS Keychain, the Secret Service elsewhere),
//! under the app's name and the connection's id.
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

/// The name the keychain files the app's passwords under.
const SERVICE: &str = "io.github.leinnan.dirfleet";

/// Something that keeps one secret for each connection.
pub trait Secrets: Send + Sync {
    fn get(&self, id: &str) -> Option<String>;
    fn set(&self, id: &str, secret: &str);
    fn forget(&self, id: &str);
}

/// The system's keychain. What it refuses is logged and otherwise goes
/// unsaid: the password is then asked for again next time.
pub struct Keychain;

impl Secrets for Keychain {
    fn get(&self, id: &str) -> Option<String> {
        keyring::Entry::new(SERVICE, id).ok()?.get_password().ok()
    }

    fn set(&self, id: &str, secret: &str) {
        let kept = keyring::Entry::new(SERVICE, id).and_then(|entry| entry.set_password(secret));
        if let Err(error) = kept {
            log::warn!("the keychain did not keep a password: {error}");
        }
    }

    fn forget(&self, id: &str) {
        if let Ok(entry) = keyring::Entry::new(SERVICE, id) {
            let _ = entry.delete_credential();
        }
    }
}

/// Secrets kept while the process runs: for tests, and for runs that keep
/// no files.
#[derive(Default)]
pub struct Memory(Mutex<HashMap<String, String>>);

impl Secrets for Memory {
    fn get(&self, id: &str) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    fn set(&self, id: &str, secret: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned(), secret.to_owned());
    }

    fn forget(&self, id: &str) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
    }
}
