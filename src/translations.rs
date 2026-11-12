// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Minimal YAML translation layer compatible with Heroku langpacks.

use std::collections::HashMap;
use std::sync::RwLock;

use serde_yaml::Value;

const EN: &str = include_str!("../langpacks/en.yml");
const RU: &str = include_str!("../langpacks/ru.yml");

pub struct Translator {
    lang: RwLock<String>,
    packs: HashMap<String, HashMap<String, String>>,
}

impl Translator {
    pub fn new(lang: impl Into<String>) -> Self {
        let mut packs = HashMap::new();
        packs.insert("en".to_owned(), load_pack(EN));
        packs.insert("ru".to_owned(), load_pack(RU));
        packs.insert("uk".to_owned(), load_pack(RU));
        Self {
            lang: RwLock::new(normalize_lang(&lang.into())),
            packs,
        }
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
        vec!["en", "ru"]
    }

    /// Translate a dotted key, e.g. `help.title`.
    pub fn t(&self, key: &str) -> String {
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

fn normalize_lang(lang: &str) -> String {
    let lang = lang.trim().to_lowercase();
    match lang.as_str() {
        "ru" | "uk" | "be" => "ru".to_owned(),
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
}
