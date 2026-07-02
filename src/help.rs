use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_HELP_FILE_BYTES: u64 = 2 * 1024 * 1024;
const BUILTIN_RUSSIAN: &str = include_str!("../help/ru.json");
const BUILTIN_ENGLISH: &str = include_str!("../help/en.json");

#[derive(Debug, Clone, Deserialize)]
pub struct HelpCatalog {
    pub id: String,
    pub tab_label: String,
    pub title: String,
    pub intro: String,
    pub sections: Vec<HelpSection>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HelpSection {
    pub title: String,
    pub items: Vec<HelpItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HelpItem {
    pub name: String,
    pub description: String,
}

#[derive(Debug)]
pub struct HelpLibrary {
    catalogs: Vec<HelpCatalog>,
    warnings: Vec<String>,
}

impl HelpLibrary {
    pub fn load() -> Self {
        let directory = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|parent| parent.join("help")))
            .unwrap_or_else(|| PathBuf::from("help"));
        Self::load_from_directory(&directory)
    }

    pub fn catalogs(&self) -> &[HelpCatalog] {
        &self.catalogs
    }

    pub fn catalog(&self, id: &str) -> Option<&HelpCatalog> {
        self.catalogs.iter().find(|catalog| catalog.id == id)
    }

    pub fn preferred_language_id(&self) -> &str {
        self.catalog("ru")
            .or_else(|| self.catalogs.first())
            .map(|catalog| catalog.id.as_str())
            .unwrap_or("en")
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    fn load_from_directory(directory: &Path) -> Self {
        let mut by_id = BTreeMap::new();
        let russian = parse_catalog(BUILTIN_RUSSIAN, "built-in ru")
            .expect("built-in Russian Help catalog must be valid");
        let english = parse_catalog(BUILTIN_ENGLISH, "built-in en")
            .expect("built-in English Help catalog must be valid");
        by_id.insert(russian.id.clone(), russian);
        by_id.insert(english.id.clone(), english);

        let mut warnings = Vec::new();
        if directory.is_dir() {
            match help_json_paths(directory) {
                Ok(paths) => {
                    for path in paths {
                        match load_catalog_file(&path) {
                            Ok(catalog) => {
                                by_id.insert(catalog.id.clone(), catalog);
                            }
                            Err(error) => warnings.push(format!("{}: {error:#}", path.display())),
                        }
                    }
                }
                Err(error) => warnings.push(format!("{}: {error:#}", directory.display())),
            }
        }

        let mut catalogs = Vec::with_capacity(by_id.len());
        if let Some(russian) = by_id.remove("ru") {
            catalogs.push(russian);
        }
        if let Some(english) = by_id.remove("en") {
            catalogs.push(english);
        }
        let mut additional: Vec<_> = by_id.into_values().collect();
        additional.sort_by_cached_key(|catalog| catalog.tab_label.to_lowercase());
        catalogs.extend(additional);

        Self { catalogs, warnings }
    }
}

fn help_json_paths(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read Help directory {}", directory.display()))?
    {
        let entry = entry.context("failed to read a Help directory entry")?;
        let path = entry.path();
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn load_catalog_file(path: &Path) -> Result<HelpCatalog> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect Help catalog {}", path.display()))?;
    if metadata.len() > MAX_HELP_FILE_BYTES {
        bail!(
            "Help catalog is {} bytes; maximum is {MAX_HELP_FILE_BYTES}",
            metadata.len()
        );
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read Help catalog {}", path.display()))?;
    parse_catalog(&contents, &path.display().to_string())
}

fn parse_catalog(contents: &str, source: &str) -> Result<HelpCatalog> {
    let catalog: HelpCatalog = serde_json::from_str(contents)
        .with_context(|| format!("invalid Help catalog JSON in {source}"))?;
    validate_catalog(&catalog).with_context(|| format!("invalid Help catalog in {source}"))?;
    Ok(catalog)
}

fn validate_catalog(catalog: &HelpCatalog) -> Result<()> {
    if catalog.id.is_empty()
        || catalog.id.len() > 32
        || !catalog
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("id must be 1..32 ASCII letters, digits, hyphens, or underscores");
    }
    validate_text("tab_label", &catalog.tab_label, 64)?;
    validate_text("title", &catalog.title, 128)?;
    validate_text("intro", &catalog.intro, 2_048)?;
    if catalog.sections.is_empty() || catalog.sections.len() > 64 {
        bail!("sections must contain 1..64 entries");
    }
    for (section_index, section) in catalog.sections.iter().enumerate() {
        validate_text(
            &format!("sections[{section_index}].title"),
            &section.title,
            128,
        )?;
        if section.items.is_empty() || section.items.len() > 128 {
            bail!("sections[{section_index}].items must contain 1..128 entries");
        }
        for (item_index, item) in section.items.iter().enumerate() {
            validate_text(
                &format!("sections[{section_index}].items[{item_index}].name"),
                &item.name,
                128,
            )?;
            validate_text(
                &format!("sections[{section_index}].items[{item_index}].description"),
                &item.description,
                4_096,
            )?;
        }
    }
    Ok(())
}

fn validate_text(field: &str, text: &str, max_chars: usize) -> Result<()> {
    let chars = text.chars().count();
    if text.trim().is_empty() || chars > max_chars {
        bail!("{field} must contain 1..{max_chars} characters");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{HelpLibrary, parse_catalog};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn built_in_catalogs_are_valid_and_ordered_ru_then_en() {
        let directory = tempdir().expect("temporary directory");
        let library = HelpLibrary::load_from_directory(directory.path());
        let ids: Vec<_> = library
            .catalogs()
            .iter()
            .map(|catalog| catalog.id.as_str())
            .collect();

        assert_eq!(ids, ["ru", "en"]);
        assert_eq!(library.preferred_language_id(), "ru");
        assert!(library.warnings().is_empty());
    }

    #[test]
    fn external_catalog_replaces_builtin_and_adds_language_tab() {
        let directory = tempdir().expect("temporary directory");
        fs::write(
            directory.path().join("ru.json"),
            test_catalog("ru", "Русский внешний", "Внешняя справка"),
        )
        .expect("write Russian override");
        fs::write(
            directory.path().join("de.json"),
            test_catalog("de", "Deutsch", "Hilfe"),
        )
        .expect("write German catalog");

        let library = HelpLibrary::load_from_directory(directory.path());
        let ids: Vec<_> = library
            .catalogs()
            .iter()
            .map(|catalog| catalog.id.as_str())
            .collect();

        assert_eq!(ids, ["ru", "en", "de"]);
        assert_eq!(
            library.catalog("ru").expect("Russian catalog").tab_label,
            "Русский внешний"
        );
        assert!(library.warnings().is_empty());
    }

    #[test]
    fn malformed_external_catalog_keeps_builtin_fallback() {
        let directory = tempdir().expect("temporary directory");
        fs::write(directory.path().join("ru.json"), "{not valid json")
            .expect("write malformed catalog");

        let library = HelpLibrary::load_from_directory(directory.path());

        assert_eq!(
            library.catalog("ru").expect("Russian fallback").tab_label,
            "Русский"
        );
        assert_eq!(library.warnings().len(), 1);
    }

    #[test]
    fn catalog_validation_rejects_empty_descriptions() {
        let contents = test_catalog("fr", "Français", "");

        assert!(parse_catalog(&contents, "test").is_err());
    }

    fn test_catalog(id: &str, tab_label: &str, description: &str) -> String {
        serde_json::json!({
            "id": id,
            "tab_label": tab_label,
            "title": "Title",
            "intro": "Intro",
            "sections": [{
                "title": "Section",
                "items": [{
                    "name": "Item",
                    "description": description
                }]
            }]
        })
        .to_string()
    }
}
