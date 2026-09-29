//! Interface language resolution and embedded tray catalogs.
//!
//! Add a locale to `CATALOGS` and `SUPPORTED_LANGUAGES`, then add its dashboard
//! catalog. Missing translations fall back to English, never the host locale.
use std::{collections::BTreeMap, sync::OnceLock};

pub const SUPPORTED_LANGUAGES: &[(&str, &str)] = &[("en", "English"), ("pt", "Português")];
const CATALOGS: &[(&str, &str)] = &[
    ("en", include_str!("../locales/tray/en.json")),
    ("pt", include_str!("../locales/tray/pt.json")),
];
type Catalog = BTreeMap<String, String>;
static TRANSLATIONS: OnceLock<BTreeMap<&'static str, Catalog>> = OnceLock::new();

pub fn is_supported_language(language: &str) -> bool {
    SUPPORTED_LANGUAGES
        .iter()
        .any(|(code, _)| *code == language)
}

/// Native user locale from the operating system (not the audio language).
pub fn system_locale() -> Option<String> {
    sys_locale::get_locale()
}

pub fn resolve_language(preference: &str) -> String {
    resolve_language_with_locale(preference, system_locale().as_deref())
}

/// Pure resolver so tests never mutate process locale or user desktop settings.
pub fn resolve_language_with_locale(preference: &str, locale: Option<&str>) -> String {
    let preference = preference.trim();
    let requested = if preference.eq_ignore_ascii_case("system") {
        locale.unwrap_or("en")
    } else {
        preference
    };
    let base = requested
        .trim()
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if is_supported_language(&base) {
        base
    } else {
        "en".into()
    }
}

fn catalog_text<'a>(catalog: Option<&'a Catalog>, english: &'a Catalog, key: &'a str) -> &'a str {
    catalog
        .and_then(|catalog| catalog.get(key))
        .or_else(|| english.get(key))
        .map(String::as_str)
        .unwrap_or(key)
}

/// Get a label with per-key English fallback; unknown keys remain diagnosable.
pub fn text(language: &str, key: &str) -> String {
    let catalogs = TRANSLATIONS.get_or_init(|| {
        CATALOGS
            .iter()
            .map(|(code, json)| {
                (
                    *code,
                    serde_json::from_str(json).expect("valid embedded tray catalog"),
                )
            })
            .collect()
    });
    catalog_text(catalogs.get(language), &catalogs["en"], key).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_locale_resolves_regions_case_and_platform_formats() {
        for locale in ["pt", "pt-BR", "pt_PT.UTF-8", "PT-br", " pt_BR@latin "] {
            assert_eq!(resolve_language_with_locale("system", Some(locale)), "pt");
        }
        for locale in ["en", "en-US", "EN_gb.UTF-8"] {
            assert_eq!(resolve_language_with_locale("system", Some(locale)), "en");
        }
    }

    #[test]
    fn unavailable_unsupported_and_c_locales_use_english() {
        for locale in [None, Some(""), Some("de-DE"), Some("C"), Some("POSIX")] {
            assert_eq!(resolve_language_with_locale("system", locale), "en");
        }
        assert_eq!(resolve_language_with_locale("fr", Some("pt-BR")), "en");
        assert_eq!(resolve_language_with_locale("pt", Some("en-US")), "pt");
        assert_eq!(resolve_language_with_locale("en", Some("pt-BR")), "en");
    }

    #[test]
    fn catalogs_match_registered_languages_and_english_keys() {
        let english: Catalog = serde_json::from_str(CATALOGS[0].1).unwrap();
        assert!(!english.is_empty());
        assert_eq!(CATALOGS.len(), SUPPORTED_LANGUAGES.len());
        for (code, json) in CATALOGS {
            assert!(is_supported_language(code));
            let catalog: Catalog = serde_json::from_str(json).unwrap();
            assert_eq!(
                catalog.keys().collect::<Vec<_>>(),
                english.keys().collect::<Vec<_>>()
            );
            assert!(catalog.values().all(|label| !label.trim().is_empty()));
        }
    }

    #[test]
    fn missing_translation_falls_back_per_key_to_english() {
        let english = BTreeMap::from([("start".into(), "Start session".into())]);
        let partial = Catalog::new();
        assert_eq!(
            catalog_text(Some(&partial), &english, "start"),
            "Start session"
        );
        assert_eq!(text("unknown", "start"), "Start session");
        assert_eq!(text("pt", "start"), "Iniciar sessão");
        assert_eq!(text("pt", "future.key"), "future.key");
    }
}
