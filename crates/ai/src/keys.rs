//! API keys in the system keychain (Windows Credential Manager, macOS Keychain, the Secret
//! Service on Linux and the BSDs), service "Cadino", one entry per provider. Keys are never logged.

use crate::ProviderKind;

#[cfg(not(target_arch = "wasm32"))]
const SERVICE: &str = "Cadino";

/// Where a provider's key comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    Keychain,
    /// `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`.
    Environment,
    Missing,
}

/// The provider's key: the keychain's, else the environment's.
pub fn load(provider: ProviderKind) -> Option<String> {
    keychain(provider).or_else(|| env(provider))
}

pub fn source(provider: ProviderKind) -> KeySource {
    if keychain(provider).is_some() {
        KeySource::Keychain
    } else if env(provider).is_some() {
        KeySource::Environment
    } else {
        KeySource::Missing
    }
}

fn env(provider: ProviderKind) -> Option<String> {
    let var = match provider {
        ProviderKind::Anthropic => "ANTHROPIC_API_KEY",
        ProviderKind::OpenAi => "OPENAI_API_KEY",
        ProviderKind::Custom => return None,
    };
    std::env::var(var).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

#[cfg(not(target_arch = "wasm32"))]
fn entry(provider: ProviderKind) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, provider.id()).map_err(|e| format!("The system keychain is not available: {e}"))
}

#[cfg(not(target_arch = "wasm32"))]
fn keychain(provider: ProviderKind) -> Option<String> {
    let key = entry(provider).ok()?.get_password().ok()?;
    Some(key).filter(|k| !k.trim().is_empty())
}

/// Save the key in the keychain; an empty key deletes it.
#[cfg(not(target_arch = "wasm32"))]
pub fn store(provider: ProviderKind, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return delete(provider);
    }
    entry(provider)?.set_password(key).map_err(|e| format!("Could not save the key in the system keychain: {e}"))
}

/// Remove the key from the keychain (nothing to remove is fine).
#[cfg(not(target_arch = "wasm32"))]
pub fn delete(provider: ProviderKind) -> Result<(), String> {
    match entry(provider)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Could not remove the key from the system keychain: {e}")),
    }
}

#[cfg(target_arch = "wasm32")]
fn keychain(_provider: ProviderKind) -> Option<String> {
    None
}

#[cfg(target_arch = "wasm32")]
pub fn store(_provider: ProviderKind, _key: &str) -> Result<(), String> {
    Err(crate::http::DESKTOP_ONLY.into())
}

#[cfg(target_arch = "wasm32")]
pub fn delete(_provider: ProviderKind) -> Result<(), String> {
    Err(crate::http::DESKTOP_ONLY.into())
}
