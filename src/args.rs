// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Command argument and flag parsing.

use std::collections::HashMap;

/// Flags that consume the next token as their value when written as `--flag value`
/// (or `-p value` for the short form).
const VALUE_FLAGS: &[&str] = &[
    "user", "p", "prefix", "time", "duration", "reason", "lang", "limit", "count", "id", "chat",
    "target", "period", "format", "module", "command", "expires", "name", "text",
];

#[derive(Debug, Clone, Default)]
pub struct Args {
    /// Everything after the command name, as typed by the user.
    pub raw: String,
    /// Positional (non-flag) tokens.
    pub list: Vec<String>,
    /// Flags, e.g. `-f`, `--force`, `--user 123`.
    pub flags: HashMap<String, Option<String>>,
}

impl Args {
    pub fn parse(raw: impl Into<String>) -> Self {
        let raw = raw.into();
        let tokens = split_quotes(&raw);
        let mut list = Vec::new();
        let mut flags = HashMap::new();
        let mut index = 0;
        let mut no_more_flags = false;

        while index < tokens.len() {
            let token = &tokens[index];
            if no_more_flags {
                list.push(token.clone());
                index += 1;
                continue;
            }

            if token == "--" {
                no_more_flags = true;
                index += 1;
                continue;
            }

            if let Some(rest) = token.strip_prefix("--") {
                if rest.is_empty() {
                    index += 1;
                    continue;
                }
                if let Some((key, value)) = rest.split_once('=') {
                    flags.insert(key.to_lowercase(), Some(value.to_owned()));
                } else {
                    let key = rest.to_lowercase();
                    let next = tokens.get(index + 1);
                    if VALUE_FLAGS.contains(&key.as_str())
                        && next.is_some_and(|value| !value.starts_with('-'))
                    {
                        flags.insert(key, Some(next.unwrap().clone()));
                        index += 1;
                    } else {
                        flags.insert(key, None);
                    }
                }
                index += 1;
                continue;
            }

            if token.starts_with('-') && token.len() > 1 {
                let short = &token[1..];
                let next = tokens.get(index + 1);
                if short.len() == 1
                    && VALUE_FLAGS.contains(&short)
                    && next.is_some_and(|value| !value.starts_with('-'))
                {
                    flags.insert(short.to_owned(), Some(next.unwrap().clone()));
                    index += 2;
                    continue;
                }
                for ch in short.chars() {
                    flags.insert(ch.to_string(), None);
                }
                index += 1;
                continue;
            }

            list.push(token.clone());
            index += 1;
        }

        Self { raw, list, flags }
    }

    pub fn get(&self, index: usize) -> Option<&str> {
        self.list.get(index).map(String::as_str)
    }

    pub fn join(&self, range: std::ops::RangeFrom<usize>) -> String {
        self.list.get(range.start..).unwrap_or_default().join(" ")
    }

    pub fn flag(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.flags.get(name).and_then(|value| value.as_deref())
    }

    pub fn is_empty(&self) -> bool {
        self.raw.trim().is_empty()
    }
}

/// Split a string into tokens, respecting single and double quotes and
/// backslash escapes.
pub fn split_quotes(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Split text into lines, keeping an empty trailing line out.
pub fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n').map(str::trim_end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags_and_quotes() {
        let args = Args::parse(r#"hello "big world" --force --user 42 -n"#);
        assert_eq!(args.list, ["hello", "big world"]);
        assert!(args.flag("force"));
        assert!(args.flag("n"));
        assert_eq!(args.value("user"), Some("42"));
    }
}
