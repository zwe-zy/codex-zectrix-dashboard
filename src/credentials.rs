use thiserror::Error;
use zeroize::Zeroizing;

use crate::{KEYCHAIN_ACCOUNT, KEYCHAIN_SERVICE};

pub trait CredentialStore: Send + Sync {
    fn get(&self) -> Result<Option<Zeroizing<String>>, CredentialError>;
    fn set(&self, secret: &str) -> Result<(), CredentialError>;
    fn delete(&self) -> Result<(), CredentialError>;
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("secure credential storage is unavailable")]
    Unavailable,
    #[error("unable to read secure credential")]
    Read,
    #[error("unable to write secure credential")]
    Write,
    #[error("unable to remove secure credential")]
    Delete,
}

pub struct OsCredentialStore {
    entry: keyring::Entry,
}

impl OsCredentialStore {
    pub fn zectrix_api_key() -> Result<Self, CredentialError> {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)
            .map_err(|_| CredentialError::Unavailable)?;
        Ok(Self { entry })
    }
}

impl CredentialStore for OsCredentialStore {
    fn get(&self) -> Result<Option<Zeroizing<String>>, CredentialError> {
        match self.entry.get_password() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(CredentialError::Read),
        }
    }

    fn set(&self, secret: &str) -> Result<(), CredentialError> {
        self.entry
            .set_password(secret)
            .map_err(|_| CredentialError::Write)
    }

    fn delete(&self) -> Result<(), CredentialError> {
        match self.entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(CredentialError::Delete),
        }
    }
}
