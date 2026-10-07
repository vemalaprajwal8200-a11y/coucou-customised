// API keys live in the OS credential store, never in settings files. The UI can
// retrieve an OpenRouter key only through its explicit reveal action.

use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const SERVICE: &str = "fr.louisraille.coucou";
const OPENROUTER_META: &str = "openrouter-accounts.json";
const LEGACY_OPENROUTER_KEY: &str = "openrouter-api-key";
static ACCOUNT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRouterAccount {
    pub id: String,
    pub name: String,
}

/// Every key Coucou may store. Anything outside this list is refused.
pub const KNOWN_KEYS: &[&str] = &[
    "openrouter-api-key",
    "anthropic-api-key",
    "n8n-url",
    "n8n-api-key",
    "vercel-token",
    "github-token",
    "stripe-api-key",
    "resend-api-key",
    "notion-api-key",
    "calcom-api-key",
];

fn entry(key: &str) -> Option<Entry> {
    if !KNOWN_KEYS.contains(&key) {
        return None;
    }
    Entry::new(SERVICE, key).ok()
}

fn account_entry(id: &str) -> Result<Entry, String> {
    if id.is_empty() || id.len() > 80 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("Invalid OpenRouter account ID.".into());
    }
    Entry::new(SERVICE, &format!("openrouter-account-{id}")).map_err(|e| e.to_string())
}

fn accounts_path() -> PathBuf {
    crate::settings::config_dir().join(OPENROUTER_META)
}

fn load_openrouter_accounts() -> Result<Vec<OpenRouterAccount>, String> {
    let path = accounts_path();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(format!("Could not read OpenRouter accounts: {error}")),
    };
    let mut accounts: Vec<OpenRouterAccount> = if bytes.is_empty() {
        Vec::new()
    } else {
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("OpenRouter account list is invalid: {error}"))?
    };
    if accounts.iter().any(|account| {
        account.id.is_empty()
            || account.id.len() > 80
            || !account
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || account.name.trim().is_empty()
            || account.name.chars().count() > 80
    }) {
        return Err("OpenRouter account list contains invalid entries.".into());
    }
    for (index, account) in accounts.iter().enumerate() {
        if accounts[index + 1..]
            .iter()
            .any(|other| other.id == account.id)
        {
            return Err("OpenRouter account list contains duplicate IDs.".into());
        }
    }
    if get(LEGACY_OPENROUTER_KEY).is_some()
        && !accounts.iter().any(|account| account.id == "legacy")
    {
        accounts.insert(
            0,
            OpenRouterAccount {
                id: "legacy".into(),
                name: "Existing OpenRouter key".into(),
            },
        );
    }
    Ok(accounts)
}

fn save_openrouter_accounts(accounts: &[OpenRouterAccount]) -> Result<(), String> {
    let dir = crate::settings::config_dir();
    crate::platform::ensure_private_dir(&dir).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(accounts).map_err(|e| e.to_string())?;
    std::fs::write(accounts_path(), bytes).map_err(|e| format!("Could not save account list: {e}"))
}

pub fn openrouter_accounts() -> Result<Vec<OpenRouterAccount>, String> {
    load_openrouter_accounts()
}

pub fn add_openrouter_account(name: &str, key: &str) -> Result<OpenRouterAccount, String> {
    let name = name.trim();
    let key = key.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("Account name must be between 1 and 80 characters.".into());
    }
    if key.is_empty() {
        return Err("OpenRouter API key cannot be empty.".into());
    }
    let mut accounts = load_openrouter_accounts()?;
    let id = format!(
        "account-{}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos(),
        ACCOUNT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let account = OpenRouterAccount {
        id: id.clone(),
        name: name.to_string(),
    };
    account_entry(&id)?
        .set_password(key)
        .map_err(|e| e.to_string())?;
    accounts.push(account.clone());
    if let Err(error) = save_openrouter_accounts(&accounts) {
        return match account_entry(&id)
            .and_then(|entry| entry.delete_credential().map_err(|e| e.to_string()))
        {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(format!(
                "{error}; also could not remove the unsaved credential: {cleanup_error}"
            )),
        };
    }
    Ok(account)
}

pub fn remove_openrouter_account(id: &str) -> Result<(), String> {
    let mut accounts = load_openrouter_accounts()?;
    let Some(index) = accounts.iter().position(|account| account.id == id) else {
        return Err("OpenRouter account was not found.".into());
    };
    let removed = accounts.remove(index);
    save_openrouter_accounts(&accounts)?;
    let remove_secret = if id == "legacy" {
        clear(LEGACY_OPENROUTER_KEY)
    } else {
        match account_entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    };
    if let Err(error) = remove_secret {
        accounts.insert(index, removed);
        return match save_openrouter_accounts(&accounts) {
            Ok(()) => Err(error),
            Err(restore_error) => Err(format!(
                "{error}; also could not restore the account list: {restore_error}"
            )),
        };
    }
    Ok(())
}

pub fn reveal_openrouter_key(id: &str) -> Result<String, String> {
    if !load_openrouter_accounts()?
        .iter()
        .any(|account| account.id == id)
    {
        return Err("OpenRouter account was not found.".into());
    }
    if id == "legacy" {
        return get(LEGACY_OPENROUTER_KEY)
            .ok_or_else(|| "The saved OpenRouter key is not available.".into());
    }
    account_entry(id)?
        .get_password()
        .map_err(|e| format!("Could not read OpenRouter key: {e}"))
}

pub fn openrouter_key_for_account(id: &str) -> Result<String, String> {
    reveal_openrouter_key(id)
}

pub fn get(key: &str) -> Option<String> {
    entry(key)?.get_password().ok().filter(|v| !v.is_empty())
}

pub fn set(key: &str, value: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    if value.is_empty() {
        let _ = entry.delete_credential();
        return Ok(());
    }
    entry.set_password(value).map_err(|e| e.to_string())
}

pub fn clear(key: &str) -> Result<(), String> {
    let entry = entry(key).ok_or_else(|| format!("unknown key {key}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub fn present(key: &str) -> bool {
    get(key).is_some()
}
