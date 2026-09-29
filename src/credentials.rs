//! Session-only secrets, zeroized on replacement/drop. Config files store names only.
use anyhow::{Result, ensure};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};
use zeroize::Zeroizing;

static KEYS: OnceLock<Mutex<BTreeMap<String, Zeroizing<String>>>> = OnceLock::new();
fn keys() -> &'static Mutex<BTreeMap<String, Zeroizing<String>>> {
    KEYS.get_or_init(Default::default)
}
fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "Nome da credencial inválido"
    );
    Ok(())
}
pub fn set(env: &str, key: String) -> Result<()> {
    let key = Zeroizing::new(key);
    validate_name(env)?;
    ensure!(
        !key.trim().is_empty() && key.len() <= 4096 && !key.contains(['\n', '\r', '\0']),
        "Chave inválida"
    );
    keys()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(env.into(), key);
    Ok(())
}
pub fn clear(env: &str) -> Result<()> {
    validate_name(env)?;
    keys().lock().unwrap_or_else(|e| e.into_inner()).remove(env);
    Ok(())
}
pub fn get(env: &str) -> Result<Zeroizing<String>> {
    validate_name(env)?;
    if let Some(key) = keys().lock().unwrap_or_else(|e| e.into_inner()).get(env) {
        return Ok(Zeroizing::new(key.to_string()));
    }
    let value = Zeroizing::new(std::env::var(env).map_err(|_| {
        anyhow::anyhow!(
            "Credencial {env} ausente; configure a chave no painel ou na variável de ambiente"
        )
    })?);
    ensure!(!value.trim().is_empty(), "Credencial {env} está vazia");
    Ok(value)
}
pub fn configured(env: &str) -> bool {
    get(env).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_can_be_replaced_and_cleared_without_environment_mutation() {
        let name = "BABEL_TEST_EPHEMERAL_SECRET_7638";
        set(name, "first".into()).unwrap();
        assert!(configured(name));
        set(name, "second".into()).unwrap();
        assert_eq!(get(name).unwrap().as_str(), "second");
        clear(name).unwrap();
        assert!(!configured(name));
        assert!(set(name, "\n".into()).is_err());
    }
}
