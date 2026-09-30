//! Localization of Babel-owned diagnostics at the presentation boundary.
//!
//! The core emits stable English diagnostic strings. Catalog keys join their
//! translations, and legacy Portuguese diagnostics remain recognized. Only whole
//! known messages or known context prefixes are matched. `{{value}}` captures are
//! copied verbatim, and `{{error}}` explicitly marks a nested diagnostic. Never use this function
//! on transcript text, user names, prompts, or arbitrary JSON values.
//!
//! To add a language, add its JSON catalog here with the same keys. A template may
//! contain one `{{value}}` or `{{error}}` slot; literal single braces are preserved
//! (for example filename pattern tokens). Missing translations fall back to en.

use std::{collections::BTreeMap, sync::OnceLock};

const SOURCES: &[(&str, &str)] = &[
    ("en", include_str!("../locales/messages/en.json")),
    ("pt", include_str!("../locales/messages/pt.json")),
];
const MAX_CHAIN_DEPTH: usize = 16;
const MAX_MESSAGE_BYTES: usize = 65_536;

struct Template {
    key: String,
    prefix: String,
    suffix: String,
    slot: &'static str,
}

impl Template {
    fn capture<'a>(&self, message: &'a str) -> Option<&'a str> {
        message
            .strip_prefix(&self.prefix)?
            .strip_suffix(&self.suffix)
    }
}

struct Catalogs {
    translations: BTreeMap<&'static str, BTreeMap<String, String>>,
    exact: BTreeMap<String, String>,
    templates: Vec<Template>,
}

fn catalogs() -> &'static Catalogs {
    static CATALOGS: OnceLock<Catalogs> = OnceLock::new();
    CATALOGS.get_or_init(|| {
        let translations: BTreeMap<_, BTreeMap<String, String>> = SOURCES
            .iter()
            .map(|(language, json)| {
                (
                    *language,
                    serde_json::from_str(json).expect("valid embedded message catalog"),
                )
            })
            .collect();
        let mut exact = BTreeMap::new();
        let mut templates = Vec::new();
        for catalog in translations.values() {
            for (key, text) in catalog {
                if let Some((slot, index)) = slot(text) {
                    templates.push(Template {
                        key: key.clone(),
                        prefix: text[..index].into(),
                        suffix: text[index + slot.len()..].into(),
                        slot,
                    });
                } else {
                    exact.insert(text.clone(), key.clone());
                }
            }
        }
        // Match the most specific template first, independent of catalog order.
        templates.sort_by_key(|template| {
            std::cmp::Reverse(template.prefix.len() + template.suffix.len())
        });
        Catalogs {
            translations,
            exact,
            templates,
        }
    })
}

fn slot(text: &str) -> Option<(&'static str, usize)> {
    ["{{value}}", "{{error}}"]
        .into_iter()
        .find_map(|slot| text.find(slot).map(|index| (slot, index)))
}

impl Catalogs {
    fn translation(&self, language: &str, key: &str) -> Option<&str> {
        self.translations
            .get(language)
            .and_then(|catalog| catalog.get(key))
            .or_else(|| {
                self.translations
                    .get("en")
                    .and_then(|catalog| catalog.get(key))
            })
            .map(String::as_str)
    }

    fn exact(&self, language: &str, message: &str) -> Option<&str> {
        self.exact
            .get(message)
            .and_then(|key| self.translation(language, key))
    }

    fn localize(&self, language: &str, message: &str, depth: usize) -> String {
        if depth >= MAX_CHAIN_DEPTH || message.len() > MAX_MESSAGE_BYTES {
            return message.to_owned();
        }
        if let Some(translated) = self.exact(language, message) {
            return translated.to_owned();
        }

        // Only recurse past a recognized context. An external error containing
        // Portuguese words, paths, quotes, or newlines is otherwise untouched.
        for separator in [": ", ":\n"] {
            for (index, _) in message.match_indices(separator) {
                if let Some(context) = self.exact(language, &message[..index]) {
                    let cause =
                        self.localize(language, &message[index + separator.len()..], depth + 1);
                    return format!("{context}{separator}{cause}");
                }
            }
        }
        for template in &self.templates {
            let Some(value) = template.capture(message) else {
                continue;
            };
            let Some(translated) = self.translation(language, &template.key) else {
                continue;
            };
            let Some((target_slot, index)) = slot(translated) else {
                continue;
            };
            let value = if template.slot == "{{error}}" {
                self.localize(language, value, depth + 1)
            } else {
                value.to_owned()
            };
            // Render in parts instead of replacing repeatedly: captured braces
            // or words must never become placeholders or translated fragments.
            return format!(
                "{}{}{}",
                &translated[..index],
                value,
                &translated[index + target_slot.len()..]
            );
        }
        message.to_owned()
    }
}

/// Translate a known Babel diagnostic into `en` or `pt` (regional tags accepted).
/// Unknown language tags fall back to English; unknown diagnostics stay verbatim.
pub fn localize(language: &str, message: &str) -> String {
    let language = language
        .split(['-', '_'])
        .next()
        .unwrap_or("en")
        .to_ascii_lowercase();
    catalogs().localize(&language, message, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn all_catalogs_have_identical_keys_and_compatible_placeholders() {
        let catalogs = catalogs();
        let english = &catalogs.translations["en"];
        let keys: BTreeSet<_> = english.keys().collect();
        for (language, catalog) in &catalogs.translations {
            assert_eq!(catalog.keys().collect::<BTreeSet<_>>(), keys, "{language}");
            for (key, text) in catalog {
                assert!(!text.is_empty(), "{language}/{key}");
                assert_eq!(
                    slot(text).map(|(slot, _)| slot),
                    slot(&english[key]).map(|(slot, _)| slot),
                    "{language}/{key}"
                );
                let count = text.matches("{{").count();
                assert!(
                    count <= 1,
                    "Only one capture per template: {language}/{key}"
                );
                assert_eq!(count, usize::from(slot(text).is_some()), "{language}/{key}");
                assert_eq!(text.matches("}}").count(), count, "{language}/{key}");
            }
        }
    }

    #[test]
    fn every_catalog_template_renders_in_both_directions() {
        let catalogs = catalogs();
        for (source_language, catalog) in &catalogs.translations {
            for (key, source) in catalog {
                for (target_language, target) in &catalogs.translations {
                    let sample = |text: &str| {
                        text.replace("{{value}}", "João: user {data}")
                            .replace("{{error}}", "External error details")
                    };
                    assert_eq!(
                        localize(target_language, &sample(source)),
                        sample(&target[key]),
                        "{key}: {source_language} -> {target_language}"
                    );
                }
            }
        }
    }

    #[test]
    fn primary_validation_and_language_fallback_are_localized() {
        assert_eq!(localize("en-US", "Chave inválida"), "Invalid key");
        assert_eq!(localize("pt_BR", "Invalid key"), "Chave inválida");
        assert_eq!(localize("PT-PT", "Invalid key"), "Chave inválida");
        assert_eq!(localize("ja-JP", "Chave inválida"), "Invalid key");
        assert_eq!(
            localize("en", "Selecione captura e reprodução de saída"),
            "Select capture and playback devices for speaker"
        );
        assert_eq!(
            localize("pt", "invalid OpenAI transcription model"),
            "Modelo de transcrição OpenAI inválido"
        );
    }

    #[test]
    fn captures_preserve_names_unicode_paths_braces_and_colons() {
        let device = "João / Chave inválida: saída {{value}}";
        assert_eq!(
            localize(
                "en",
                &format!(
                    "Captura do fluxo microfone não encontrada: {device}. Atualize a lista de dispositivos"
                )
            ),
            format!("Capture device for microphone not found: {device}. Refresh the device list")
        );
        let path = "C:\\Usuários\\João\\Chave inválida.txt";
        assert_eq!(
            localize(
                "en",
                &format!("Não foi possível criar a transcrição em {path}")
            ),
            format!("Could not create the transcript at {path}")
        );
        assert_eq!(
            localize(
                "en",
                "Inclua {id} no padrão para identificar sessões distintas"
            ),
            "Include {id} in the pattern to identify distinct sessions"
        );
    }

    #[test]
    fn known_context_chains_translate_but_external_details_are_verbatim() {
        let external = "HTTP 429: quota exceeded\nrequest='Chave inválida'; /saída";
        assert_eq!(localize("pt", external), external);
        assert_eq!(localize("en", external), external);
        assert_eq!(
            localize(
                "en",
                &format!("Roteamento original indisponível: Captura:\n{external}")
            ),
            format!("Original audio routing unavailable: Capture:\n{external}")
        );
        assert_eq!(
            localize("en", "Menu de dispositivos: Nome da credencial inválido"),
            "Device menu: Invalid credential name"
        );
        assert_eq!(
            localize("en", "Captura: Chave inválida; reconnect budget exhausted"),
            "Capture: Invalid key; reconnect budget exhausted"
        );
        assert_eq!(
            localize(
                "pt",
                "Provider: HTTP 503\nupstream says Chave inválida; reconnect budget exhausted"
            ),
            "Provider: HTTP 503\nupstream says Chave inválida; limite de reconexões esgotado"
        );
    }

    #[test]
    fn exact_templates_are_anchored_and_empty_unknowns_stay_unchanged() {
        for message in [
            "",
            "Minha mensagem: Chave inválida",
            "Chave inválida mencionada pelo participante",
            "External error: Nome da credencial inválido",
        ] {
            assert_eq!(localize("en", message), message);
        }
        assert_eq!(
            localize("en", "Credencial GEMINI_API_KEY está vazia"),
            "Credential GEMINI_API_KEY is empty"
        );
    }
}
