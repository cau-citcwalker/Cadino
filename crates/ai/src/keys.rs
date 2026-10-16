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
fn entry(service: &str, provider: ProviderKind) -> Result<keyring::Entry, String> {
    keyring::Entry::new(service, provider.id()).map_err(|e| crate::text::KEYCHAIN_UNAVAILABLE.replace("{error}", &e.to_string()))
}

#[cfg(not(target_arch = "wasm32"))]
fn keychain(provider: ProviderKind) -> Option<String> {
    keychain_in(SERVICE, provider)
}

#[cfg(not(target_arch = "wasm32"))]
fn keychain_in(service: &str, provider: ProviderKind) -> Option<String> {
    let key = entry(service, provider).ok()?.get_password().ok()?;
    Some(key).filter(|k| !k.trim().is_empty())
}

/// Save the key in the keychain; an empty key deletes it.
#[cfg(not(target_arch = "wasm32"))]
pub fn store(provider: ProviderKind, key: &str) -> Result<(), String> {
    store_in(SERVICE, provider, key)
}

#[cfg(not(target_arch = "wasm32"))]
fn store_in(service: &str, provider: ProviderKind, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return delete_in(service, provider);
    }
    entry(service, provider)?.set_password(key).map_err(|e| crate::text::KEY_NOT_SAVED.replace("{error}", &e.to_string()))
}

/// Remove the key from the keychain (nothing to remove is fine).
#[cfg(not(target_arch = "wasm32"))]
pub fn delete(provider: ProviderKind) -> Result<(), String> {
    delete_in(SERVICE, provider)
}

#[cfg(not(target_arch = "wasm32"))]
fn delete_in(service: &str, provider: ProviderKind) -> Result<(), String> {
    match entry(service, provider)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(crate::text::KEY_NOT_REMOVED.replace("{error}", &e.to_string())),
    }
}

#[cfg(target_arch = "wasm32")]
fn keychain(_provider: ProviderKind) -> Option<String> {
    None
}

#[cfg(target_arch = "wasm32")]
pub fn store(_provider: ProviderKind, _key: &str) -> Result<(), String> {
    Err(crate::text::DESKTOP_ONLY.into())
}

#[cfg(target_arch = "wasm32")]
pub fn delete(_provider: ProviderKind) -> Result<(), String> {
    Err(crate::text::DESKTOP_ONLY.into())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// A local or custom server's key is its own (Options ▸ AI): never OpenAI's from the environment.
    #[test]
    fn a_custom_server_takes_no_environment_key() {
        assert_eq!(env(ProviderKind::Custom), None);
    }

    /// The system keychain round trip, under a service of its own: never the real "Cadino" keys.
    /// Run by hand (`cargo test -p cadcraft-ai -- --ignored keychain`); it leaves nothing behind.
    #[test]
    #[ignore = "uses the system keychain"]
    fn keychain_stores_loads_and_deletes_a_key() {
        const TEST: &str = "Cadino-selftest";
        let p = ProviderKind::OpenAi;
        delete_in(TEST, p).unwrap();
        assert_eq!(keychain_in(TEST, p), None);
        store_in(TEST, p, "  sk-selftest-123  ").unwrap();
        assert_eq!(keychain_in(TEST, p).as_deref(), Some("sk-selftest-123"), "stored trimmed");
        store_in(TEST, p, "sk-selftest-456").unwrap();
        assert_eq!(keychain_in(TEST, p).as_deref(), Some("sk-selftest-456"), "replaced");
        store_in(TEST, p, " ").unwrap();
        assert_eq!(keychain_in(TEST, p), None, "an empty key deletes it");
        store_in(TEST, p, "sk-selftest-789").unwrap();
        // On Windows, the entry is a real one in the Credential Manager.
        let listed = || {
            let out = std::process::Command::new("cmdkey").arg("/list").output().map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase());
            out.unwrap_or_default().contains("cadino-selftest")
        };
        assert!(!cfg!(windows) || listed());
        delete_in(TEST, p).unwrap();
        assert_eq!(keychain_in(TEST, p), None);
        assert!(!cfg!(windows) || !listed(), "nothing left behind");
        delete_in(TEST, p).unwrap();
    }
}
