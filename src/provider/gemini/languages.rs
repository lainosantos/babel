//! Map application locales to the language targets supported by Live Translate.
//! https://ai.google.dev/gemini-api/docs/live-api/live-translate#supported-languages
//! Checked 2026-09-30. This is deliberately narrower than arbitrary BCP-47:
//! the provider does not advertise every region, script, variant or extension.

use anyhow::{Result, bail, ensure};

// These targets have one documented code. Regional application locales map to
// that code; Portuguese and Chinese are handled separately below.
const BASE_LANGUAGES: &[&str] = &[
    "af", "ak", "sq", "am", "ar", "hy", "az", "eu", "be", "bn", "bg", "my", "ca", "hr", "cs", "da",
    "nl", "en", "et", "fil", "fi", "fr", "gl", "ka", "de", "el", "gu", "ha", "he", "hi", "hu",
    "is", "id", "it", "ja", "jv", "kn", "kk", "km", "rw", "ko", "lo", "lv", "lt", "mk", "ms", "ml",
    "mr", "mn", "ne", "no", "nb", "fa", "pl", "pa", "ro", "ru", "sr", "sd", "si", "sk", "sl", "es",
    "su", "sw", "sv", "ta", "te", "th", "tr", "uk", "ur", "uz", "vi", "zu",
];

/// Return a canonical wire code without modifying the application's saved locale.
/// Unknown variants fail locally rather than becoming a WebSocket format error.
pub(crate) fn target_language_code(language: &str) -> Result<&'static str> {
    let mut parts = language.split('-');
    let primary = parts.next().unwrap_or_default();
    ensure!(
        (2..=3).contains(&primary.len()) && primary.bytes().all(|byte| byte.is_ascii_alphabetic()),
        "Gemini Live Translate target must be a supported language code"
    );
    let first = parts.next();
    let second = parts.next();
    ensure!(
        parts.next().is_none(),
        "Gemini Live Translate does not support target language variants or extensions"
    );
    let (script, region) = match (first, second) {
        (None, None) => (None, None),
        (Some(region), None) if is_region(region) => (None, Some(region)),
        (Some(script), None) if is_script(script) => (Some(script), None),
        (Some(script), Some(region)) if is_script(script) && is_region(region) => {
            (Some(script), Some(region))
        }
        _ => bail!(
            "Gemini Live Translate target must use language-region or language-script-region syntax"
        ),
    };

    if primary.eq_ignore_ascii_case("zh") {
        // An explicit script takes precedence: zh-Hant-CN still means Hant.
        if let Some(script) = script {
            return if script.eq_ignore_ascii_case("Hans") {
                Ok("zh-Hans")
            } else if script.eq_ignore_ascii_case("Hant") {
                Ok("zh-Hant")
            } else {
                bail!("For Chinese translation, choose zh-Hans or zh-Hant")
            };
        }
        return match region {
            Some(region)
                if ["CN", "SG"]
                    .iter()
                    .any(|value| region.eq_ignore_ascii_case(value)) =>
            {
                Ok("zh-Hans")
            }
            Some(region)
                if ["TW", "HK", "MO"]
                    .iter()
                    .any(|value| region.eq_ignore_ascii_case(value)) =>
            {
                Ok("zh-Hant")
            }
            _ => bail!("For Chinese translation, choose zh-Hans or zh-Hant"),
        };
    }

    if primary.eq_ignore_ascii_case("pt") {
        ensure!(
            script.is_none(),
            "For Portuguese translation, choose pt-BR or pt-PT without a script subtag"
        );
        return match region {
            Some(region) if region.eq_ignore_ascii_case("BR") => Ok("pt-BR"),
            Some(region) if region.eq_ignore_ascii_case("PT") => Ok("pt-PT"),
            _ => bail!("For Portuguese translation, choose pt-BR or pt-PT"),
        };
    }

    ensure!(
        script.is_none(),
        "Gemini Live Translate does not support this target script; use its documented language code"
    );
    BASE_LANGUAGES
        .iter()
        .copied()
        .find(|code| primary.eq_ignore_ascii_case(code))
        .ok_or_else(|| {
            anyhow::anyhow!("Gemini Live Translate does not support this target language")
        })
}

fn is_script(value: &str) -> bool {
    value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_alphabetic())
}

fn is_region(value: &str) -> bool {
    (value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_alphabetic()))
        || (value.len() == 3 && value.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_base_language_keeps_its_canonical_code() {
        for code in BASE_LANGUAGES {
            assert_eq!(target_language_code(code).unwrap(), *code);
        }
    }

    #[test]
    fn supported_base_languages_accept_regional_application_locales() {
        for (locale, expected) in [
            ("en-US", "en"),
            ("en-GB", "en"),
            ("EN-au", "en"),
            ("es-MX", "es"),
            ("es-419", "es"),
            ("fr-CA", "fr"),
            ("fil-PH", "fil"),
            ("nb-NO", "nb"),
        ] {
            assert_eq!(target_language_code(locale).unwrap(), expected);
        }
    }

    #[test]
    fn chinese_and_portuguese_keep_their_documented_distinctions() {
        for (locale, expected) in [
            ("pt-BR", "pt-BR"),
            ("PT-pt", "pt-PT"),
            ("zh-Hans", "zh-Hans"),
            ("ZH-hant", "zh-Hant"),
            ("zh-CN", "zh-Hans"),
            ("zh-SG", "zh-Hans"),
            ("zh-TW", "zh-Hant"),
            ("zh-HK", "zh-Hant"),
            ("zh-MO", "zh-Hant"),
            ("zh-Hant-CN", "zh-Hant"),
            ("zh-Hans-TW", "zh-Hans"),
        ] {
            assert_eq!(target_language_code(locale).unwrap(), expected);
        }
    }

    #[test]
    fn ambiguous_or_unsupported_targets_fail_with_actionable_messages() {
        for code in ["pt", "pt-AO", "pt-419"] {
            assert!(
                target_language_code(code)
                    .unwrap_err()
                    .to_string()
                    .contains("pt-BR or pt-PT")
            );
        }
        for code in ["zh", "zh-US", "zh-Latn-CN"] {
            assert!(
                target_language_code(code)
                    .unwrap_err()
                    .to_string()
                    .contains("zh-Hans or zh-Hant")
            );
        }
        for code in ["auto", "xx", "zz-US", "English", "sr-Latn", "en-Cyrl-US"] {
            assert!(target_language_code(code).is_err(), "{code}");
        }
    }

    #[test]
    fn malformed_tags_and_extra_subtags_are_not_silently_discarded() {
        for code in [
            "",
            " en-US",
            "en-US ",
            "en_US",
            "en--US",
            "en-",
            "en-USA",
            "en-1234",
            "e-US",
            "en-US-extra",
            "en-u-ca",
            "en-US-x-test",
            "zh-Hans-CN-extra",
            "zh-CN-Hans",
            "zh-漢字",
        ] {
            assert!(target_language_code(code).is_err(), "{code}");
        }
    }
}
