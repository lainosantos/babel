//! User-facing session names are separate from safe, unique transcript identifiers.
use anyhow::{Result, ensure};

#[derive(Clone, Debug)]
pub(crate) struct SessionIdentity {
    pub name: String,
    pub id: String,
    created_at: chrono::DateTime<chrono::Utc>,
    nonce: String,
}

pub(crate) fn validate_pattern(pattern: &str) -> Result<()> {
    ensure!(
        !pattern.is_empty() && pattern.len() <= 128,
        "Padrão de nomes: 1 a 128 bytes"
    );
    ensure!(
        pattern.contains("{id}"),
        "Inclua {{id}} no padrão para identificar sessões distintas"
    );
    ensure!(
        !pattern
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c)),
        "Padrão de nomes não pode conter caminhos ou caracteres reservados"
    );
    let mut remaining = pattern.to_owned();
    for token in ["{date}", "{time}", "{session}", "{id}"] {
        remaining = remaining.replace(token, "");
    }
    ensure!(
        !remaining.contains(['{', '}']),
        "Campos do padrão: {{date}}, {{time}}, {{session}}, {{id}}"
    );
    let longest = pattern
        .replace("{date}", "20000101")
        .replace("{time}", "120000")
        .replace("{session}", &"s".repeat(64))
        .replace("{id}", "abcdef12");
    validate_file_stem(&longest)?;
    Ok(())
}

pub(crate) fn validate_file_stem(stem: &str) -> Result<()> {
    ensure!(
        !stem.is_empty()
            && stem.len() <= 240
            && !stem
                .chars()
                .any(|c| c.is_control() || "/\\:*?\"<>|{}".contains(c)),
        "Nome de arquivo gerado inválido ou maior que 240 bytes"
    );
    ensure!(
        !stem.ends_with(['.', ' ']) && !matches!(stem, "." | ".."),
        "Nome de arquivo não pode terminar em ponto ou espaço"
    );
    let base = stem.split('.').next().unwrap_or(stem).to_ascii_uppercase();
    let numbered_device = ["COM", "LPT"].into_iter().any(|prefix| {
        base.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    ensure!(
        !matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL") && !numbered_device,
        "Nome reservado pelo Windows; escolha outro padrão"
    );
    Ok(())
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    ensure!(
        name.chars().count() <= 100,
        "Nome da sessão: no máximo 100 caracteres"
    );
    ensure!(
        !name.chars().any(char::is_control),
        "Nome da sessão não pode conter quebras de linha ou caracteres de controle"
    );
    Ok(())
}

impl SessionIdentity {
    #[cfg(test)]
    pub fn new(name: Option<&str>) -> Result<Self> {
        Self::new_with_language(name, "pt")
    }
    pub fn new_with_language(name: Option<&str>, language: &str) -> Result<Self> {
        let supplied = name.unwrap_or_default();
        validate_name(supplied)?;
        let now = chrono::Utc::now();
        let name = if supplied.trim().is_empty() {
            crate::i18n::text(language, "session.default")
                .replace("{timestamp}", &now.format("%Y-%m-%d %H:%M:%S").to_string())
        } else {
            supplied.trim().to_owned()
        };
        let nonce = format!("{:08x}", rand::random::<u32>());
        let unique = format!("{}-{nonce}", now.format("%Y%m%dT%H%M%SZ"));
        let slug = slug(&name);
        Ok(Self {
            name,
            id: format!("{unique}-{slug}"),
            created_at: now,
            nonce,
        })
    }
    pub fn file_stem(&self, pattern: &str) -> Result<String> {
        validate_pattern(pattern)?;
        let stem = pattern
            .replace("{date}", &self.created_at.format("%Y%m%d").to_string())
            .replace("{time}", &self.created_at.format("%H%M%S").to_string())
            .replace("{session}", &slug(&self.name))
            .replace("{id}", &self.nonce);
        validate_file_stem(&stem)?;
        Ok(stem)
    }
}

fn slug(name: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for c in name.chars().flat_map(char::to_lowercase) {
        if !c.is_alphanumeric() {
            separator = !slug.is_empty();
            continue;
        }
        if slug.len() + usize::from(separator) + c.len_utf8() > 64 {
            break;
        }
        if separator {
            slug.push('-');
            separator = false;
        }
        slug.push(c);
    }
    if slug.is_empty() {
        "sessao".into()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configurable_filename_patterns_are_bounded_and_cannot_escape_directory() {
        let session = SessionIdentity::new(Some("Equipe / João")).unwrap();
        let stem = session.file_stem("cliente_{date}_{session}_{id}").unwrap();
        assert!(stem.starts_with("cliente_"));
        assert!(stem.contains("_equipe-joão_"));
        assert_eq!(
            stem,
            session.file_stem("cliente_{date}_{session}_{id}").unwrap()
        );
        for pattern in [
            "../{id}",
            "C:\\{id}",
            "{unknown}-{id}",
            "{session}",
            "{id}\n",
            "{id}?",
        ] {
            assert!(session.file_stem(pattern).is_err(), "{pattern:?}");
        }
    }

    #[test]
    fn names_are_preserved_but_paths_are_safe_and_unique() {
        let first = SessionIdentity::new(Some("  Reunião João / ../../cliente  ")).unwrap();
        let second = SessionIdentity::new(Some(&first.name)).unwrap();
        assert_eq!(first.name, "Reunião João / ../../cliente");
        assert!(first.id.ends_with("reunião-joão-cliente"));
        assert_ne!(first.id, second.id);
        assert!(first.id.chars().all(|c| c.is_alphanumeric() || c == '-'));
        assert!(first.id.len() <= 128);
    }
    #[test]
    fn automatic_names_and_unicode_length_are_bounded() {
        assert!(
            SessionIdentity::new(Some("   "))
                .unwrap()
                .name
                .starts_with("Sessão ")
        );
        assert!(
            SessionIdentity::new(Some(&"界".repeat(100)))
                .unwrap()
                .id
                .len()
                <= 128
        );
        assert!(SessionIdentity::new(Some(&"a".repeat(101))).is_err());
        assert!(SessionIdentity::new(Some("header\ninjection")).is_err());
        assert!(SessionIdentity::new(Some("\0")).is_err());
        assert!(
            SessionIdentity::new(Some("../../"))
                .unwrap()
                .id
                .ends_with("sessao")
        );
    }
    #[test]
    fn only_automatic_session_names_follow_interface_language() {
        assert!(
            SessionIdentity::new_with_language(None, "en")
                .unwrap()
                .name
                .starts_with("Session ")
        );
        assert!(
            SessionIdentity::new_with_language(None, "pt")
                .unwrap()
                .name
                .starts_with("Sessão ")
        );
        assert_eq!(
            SessionIdentity::new_with_language(Some("Sessão do cliente"), "en")
                .unwrap()
                .name,
            "Sessão do cliente"
        );
    }
}
