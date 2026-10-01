//! Independent temporary and saved secrets; temporary values always take precedence.
use anyhow::{Result, ensure};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};
use zeroize::Zeroizing;

pub(crate) type SavedKeys = BTreeMap<String, Zeroizing<String>>;
static SAVED: OnceLock<Mutex<SavedKeys>> = OnceLock::new();
fn saved() -> &'static Mutex<SavedKeys> {
    SAVED.get_or_init(Default::default)
}

static KEYS: OnceLock<Mutex<BTreeMap<String, Zeroizing<String>>>> = OnceLock::new();
fn keys() -> &'static Mutex<BTreeMap<String, Zeroizing<String>>> {
    KEYS.get_or_init(Default::default)
}
pub(crate) fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "Invalid credential name"
    );
    Ok(())
}
pub fn set(env: &str, key: String) -> Result<()> {
    let key = Zeroizing::new(key);
    validate_name(env)?;
    validate_key(&key)?;
    keys()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(env.into(), key);
    Ok(())
}
pub(crate) fn validate_key(key: &str) -> Result<()> {
    ensure!(
        !key.trim().is_empty() && key.len() <= 4096 && !key.contains(['\n', '\r', '\0']),
        "Invalid key"
    );
    Ok(())
}
pub fn clear(env: &str) -> Result<()> {
    validate_name(env)?;
    keys().lock().unwrap_or_else(|e| e.into_inner()).remove(env);
    Ok(())
}
pub fn get(env: &str) -> Result<Zeroizing<String>> {
    resolve(env, |name| std::env::var(name).map(Zeroizing::new))
}
fn resolve(
    env: &str,
    environment: impl FnOnce(&str) -> std::result::Result<Zeroizing<String>, std::env::VarError>,
) -> Result<Zeroizing<String>> {
    validate_name(env)?;
    if let Some(key) = keys().lock().unwrap_or_else(|e| e.into_inner()).get(env) {
        return Ok(Zeroizing::new(key.to_string()));
    }
    if let Some(key) = saved().lock().unwrap_or_else(|e| e.into_inner()).get(env) {
        return Ok(Zeroizing::new(key.to_string()));
    }
    let value = environment(env).map_err(|_| {
        anyhow::anyhow!(
            "Credential {env} is missing; set the key in the dashboard or environment variable"
        )
    })?;
    ensure!(!value.trim().is_empty(), "Credential {env} is empty");
    Ok(value)
}
pub fn configured(env: &str) -> bool {
    get(env).is_ok()
}

/// Cache saved credentials once, outside audio and inference executors.
pub fn load_saved(path: &std::path::Path) -> Result<()> {
    let entries = crate::config::saved_credentials(path)?;
    saved()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend(entries);
    Ok(())
}

/// Commit to disk before updating the cache; a failed write preserves both layers.
pub fn save(
    config: &crate::config::AppConfig,
    path: &std::path::Path,
    name: &str,
    key: Option<String>,
) -> Result<()> {
    let key = key.map(Zeroizing::new);
    validate_name(name)?;
    if let Some(key) = &key {
        validate_key(key)?;
    }
    config.save_credential(path, name, key.as_deref().map(String::as_str))?;
    let mut values = saved().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(key) = key {
        values.insert(name.into(), key);
    } else {
        values.remove(name);
    }
    Ok(())
}

#[derive(serde::Serialize)]
pub struct Status {
    pub configured: bool,
    pub temporary: bool,
    pub permanent: bool,
    pub environment: bool,
    pub source: Option<&'static str>,
}
pub fn status(name: &str) -> Status {
    let temporary = keys()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(name);
    let permanent = saved()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains_key(name);
    let environment = validate_name(name).is_ok()
        && std::env::var(name)
            .map(Zeroizing::new)
            .is_ok_and(|v| !v.trim().is_empty());
    let source = if temporary {
        Some("temporary")
    } else if permanent {
        Some("permanent")
    } else if environment {
        Some("environment")
    } else {
        None
    };
    Status {
        configured: source.is_some(),
        temporary,
        permanent,
        environment,
        source,
    }
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
    #[test]
    fn saved_and_temporary_values_are_independent_and_survive_settings_saves() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private/config.toml");
        let name = "BABEL_TEST_LAYERED_593814";
        let other = "BABEL_TEST_LAYERED_OTHER_593814";
        let mut config = crate::config::AppConfig::default();
        let environment =
            || resolve(name, |_| Ok(Zeroizing::new("environment-fixture".into()))).unwrap();
        assert_eq!(environment().as_str(), "environment-fixture");
        save(&config, &path, name, Some("saved-fixture".into())).unwrap();
        save(&config, &path, other, Some("other-fixture".into())).unwrap();
        assert_eq!(environment().as_str(), "saved-fixture");
        set(name, "temporary-fixture".into()).unwrap();
        assert_eq!(environment().as_str(), "temporary-fixture");
        assert!(status(name).temporary && status(name).permanent);
        config.interface.language = "pt".into();
        config.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("saved-fixture") && text.contains("other-fixture"));
        assert!(!text.contains("temporary-fixture"));
        let public = crate::config::AppConfig::load(&path).unwrap();
        assert_eq!(public.interface.language, "pt");
        assert!(
            !serde_json::to_string(&public)
                .unwrap()
                .contains("saved-fixture")
        );
        saved().lock().unwrap().remove(name);
        load_saved(&path).unwrap();
        clear(name).unwrap();
        assert_eq!(environment().as_str(), "saved-fixture");
        set(name, "temporary-fixture".into()).unwrap();
        save(&config, &path, name, None).unwrap();
        assert!(status(name).temporary && !status(name).permanent);
        assert_eq!(environment().as_str(), "temporary-fixture");
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("saved-fixture")
        );
        assert_eq!(get(other).unwrap().as_str(), "other-fixture");
        clear(name).unwrap();
        assert_eq!(environment().as_str(), "environment-fixture");
        save(&config, &path, other, None).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }
    #[test]
    fn failed_persistence_does_not_replace_either_value_and_errors_do_not_echo_keys() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let name = "BABEL_TEST_FAILED_PERSIST_597144";
        let config = crate::config::AppConfig::default();
        save(&config, &path, name, Some("original-saved-fixture".into())).unwrap();
        set(name, "temporary-fixture".into()).unwrap();
        assert!(
            save(
                &config,
                directory.path(),
                name,
                Some("replacement-fixture".into())
            )
            .is_err()
        );
        assert!(save(&config, directory.path(), name, None).is_err());
        assert_eq!(get(name).unwrap().as_str(), "temporary-fixture");
        clear(name).unwrap();
        assert_eq!(get(name).unwrap().as_str(), "original-saved-fixture");
        std::fs::write(
            &path,
            "[credentials]\nKEY = \"private-fixture-do-not-echo\n",
        )
        .unwrap();
        let error = crate::config::AppConfig::load(&path).unwrap_err();
        assert!(!format!("{error:#}").contains("private-fixture"));
        saved().lock().unwrap().remove(name);
    }
    #[test]
    fn legacy_configuration_migration_preserves_saved_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.toml");
        let mut document = toml::Value::try_from(crate::config::AppConfig::default()).unwrap();
        document["files"]["base_path"] = toml::Value::String(".".into());
        document.as_table_mut().unwrap().insert(
            "credentials".into(),
            toml::Value::Table(
                [(
                    "SYNTHETIC_MIGRATION_KEY".into(),
                    toml::Value::String("synthetic-migration-value".into()),
                )]
                .into_iter()
                .collect(),
            ),
        );
        std::fs::write(&path, toml::to_string(&document).unwrap()).unwrap();
        let public = crate::config::AppConfig::load(&path).unwrap();
        assert!(std::path::Path::new(&public.files.base_path).is_absolute());
        assert!(
            std::fs::read_to_string(path)
                .unwrap()
                .contains("synthetic-migration-value")
        );
        assert!(
            !serde_json::to_string(&public)
                .unwrap()
                .contains("synthetic-migration-value")
        );
    }
}
