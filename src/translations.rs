// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Minimal YAML translation layer compatible with Heroku langpacks.

use std::collections::HashMap;
use std::sync::RwLock;

use serde_yaml::Value;

const EN: &str = include_str!("../langpacks/en.yml");
const RU: &str = include_str!("../langpacks/ru.yml");
const UK: &str = include_str!("../langpacks/ua.yml");
const DE: &str = include_str!("../langpacks/de.yml");
const JA: &str = include_str!("../langpacks/jp.yml");
const LEET: &str = include_str!("../langpacks/leet.yml");
const UWU: &str = include_str!("../langpacks/uwu.yml");
const TIKTOK: &str = include_str!("../langpacks/tiktok.yml");
const NEOFIT: &str = include_str!("../langpacks/neofit.yml");

/// Canonical language codes shown by `.setlang` and the language picker.
pub const LANGUAGES: &[&str] = &[
    "en", "ru", "uk", "de", "ja", "leet", "uwu", "tiktok", "neofit",
];

pub struct Translator {
    lang: RwLock<String>,
    packs: HashMap<String, HashMap<String, String>>,
    external: RwLock<Vec<HashMap<String, String>>>,
}

impl Translator {
    pub fn new(lang: impl Into<String>) -> Self {
        let mut packs = HashMap::new();
        for (code, raw) in [
            ("en", EN),
            ("ru", RU),
            ("uk", UK),
            ("de", DE),
            ("ja", JA),
            ("leet", LEET),
            ("uwu", UWU),
            ("tiktok", TIKTOK),
            ("neofit", NEOFIT),
        ] {
            packs.insert(code.to_owned(), load_pack(raw));
        }
        Self {
            lang: RwLock::new(normalize_lang(&lang.into())),
            packs,
            external: RwLock::new(Vec::new()),
        }
    }

    /// Merge an external YAML/JSON language pack. Later packs take precedence.
    /// Returns the number of loaded keys.
    pub fn merge_external(&self, raw: &str) -> usize {
        let mut pack = HashMap::new();
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
            flatten_json("", &value, &mut pack);
        } else if let Ok(value) = serde_yaml::from_str::<Value>(raw) {
            flatten("", &value, &mut pack);
        }
        let count = pack.len();
        if count > 0 {
            if let Ok(mut external) = self.external.write() {
                external.push(pack);
            }
        }
        count
    }

    pub fn clear_external(&self) {
        if let Ok(mut external) = self.external.write() {
            external.clear();
        }
    }

    pub fn external_count(&self) -> usize {
        self.external
            .read()
            .map(|external| external.len())
            .unwrap_or(0)
    }

    pub fn lang(&self) -> String {
        self.lang
            .read()
            .map(|lang| lang.clone())
            .unwrap_or_else(|_| "en".to_owned())
    }

    pub fn set_lang(&self, lang: &str) {
        let lang = normalize_lang(lang);
        if let Ok(mut current) = self.lang.write() {
            *current = lang;
        }
    }

    pub fn available(&self) -> Vec<&'static str> {
        LANGUAGES.to_vec()
    }

    /// Translate a dotted key, e.g. `help.title`.
    pub fn t(&self, key: &str) -> String {
        if let Ok(external) = self.external.read() {
            if let Some(value) = external
                .iter()
                .rev()
                .find_map(|pack| pack.get(key).cloned())
            {
                return value;
            }
        }
        let lang = self.lang();
        if let Some(value) = self.packs.get(&lang).and_then(|pack| pack.get(key)) {
            return value.clone();
        }
        if let Some(value) = self.packs.get("en").and_then(|pack| pack.get(key)) {
            return value.clone();
        }
        key.to_owned()
    }

    /// Translate and replace `{}` placeholders in order.
    pub fn tf(&self, key: &str, values: &[&str]) -> String {
        let mut text = self.t(key);
        for value in values {
            if let Some(position) = text.find("{}") {
                text.replace_range(position..position + 2, value);
            } else {
                break;
            }
        }
        text
    }

    /// Translate and replace named `{name}` placeholders.
    pub fn tn(&self, key: &str, values: &[(&str, &str)]) -> String {
        let mut text = self.t(key);
        for (name, value) in values {
            text = text.replace(&format!("{{{name}}}"), value);
        }
        text
    }
}

/// Resolve a user-provided language code to its canonical form.
pub fn canonical_lang(lang: &str) -> String {
    normalize_lang(lang)
}

fn normalize_lang(lang: &str) -> String {
    let lang = lang.trim().to_lowercase();
    match lang.as_str() {
        "ru" | "be" => "ru".to_owned(),
        "ua" | "uk" => "uk".to_owned(),
        "de" => "de".to_owned(),
        "jp" | "ja" => "ja".to_owned(),
        "leet" => "leet".to_owned(),
        "uwu" => "uwu".to_owned(),
        "tiktok" => "tiktok".to_owned(),
        "neofit" => "neofit".to_owned(),
        _ => "en".to_owned(),
    }
}

fn load_pack(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Ok(value) = serde_yaml::from_str::<Value>(raw) {
        flatten("", &value, &mut out);
    }
    out
}

fn flatten(prefix: &str, value: &Value, out: &mut HashMap<String, String>) {
    match value {
        Value::Mapping(map) => {
            for (key, value) in map {
                let key = key.as_str().unwrap_or_default();
                let next = if prefix.is_empty() {
                    key.to_owned()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&next, value, out);
            }
        }
        Value::String(text) => {
            out.insert(prefix.to_owned(), text.clone());
        }
        Value::Number(number) => {
            out.insert(prefix.to_owned(), number.to_string());
        }
        Value::Bool(boolean) => {
            out.insert(prefix.to_owned(), boolean.to_string());
        }
        _ => {}
    }
}

fn flatten_json(prefix: &str, value: &serde_json::Value, out: &mut HashMap<String, String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                let next = if prefix.is_empty() {
                    key.to_owned()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_json(&next, value, out);
            }
        }
        serde_json::Value::String(text) => {
            out.insert(prefix.to_owned(), text.clone());
        }
        serde_json::Value::Number(number) => {
            out.insert(prefix.to_owned(), number.to_string());
        }
        serde_json::Value::Bool(boolean) => {
            out.insert(prefix.to_owned(), boolean.to_string());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flattens_and_formats() {
        let translator = Translator::new("en");
        assert!(translator.t("help.title").contains("Heroku"));
        assert_eq!(
            translator.tf("info.uptime", &["5m"]),
            "▫️ Uptime: <b>5m</b>"
        );
    }

    #[test]
    fn falls_back_to_english() {
        let translator = Translator::new("klingon");
        assert_eq!(translator.lang(), "en");
        assert!(!translator.t("help.title").starts_with("help."));
        translator.set_lang("ru");
        assert_eq!(translator.lang(), "ru");
        assert!(translator.t("help.title").contains("Модули"));
    }

    #[test]
    fn keeps_unknown_keys() {
        let translator = Translator::new("en");
        assert_eq!(translator.t("does.not.exist"), "does.not.exist");
    }

    #[test]
    fn merges_external_packs() {
        let translator = Translator::new("en");
        assert_eq!(
            translator.merge_external("help:\n  title: External title\n"),
            1
        );
        assert_eq!(translator.t("help.title"), "External title");
        assert_eq!(translator.external_count(), 1);
        translator.clear_external();
        assert_ne!(translator.t("help.title"), "External title");
    }
}
