// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Module and command metadata, registry and the `inventory`-based built-in
//! plugin discovery.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::tg::Msg;

// Security flags. They match the bitmask used by Heroku/Hikka, so existing
// database values keep working.
pub const OWNER: u32 = 1 << 0;
pub const SUDO: u32 = 1 << 1;
pub const SUPPORT: u32 = 1 << 2;
pub const GROUP_OWNER: u32 = 1 << 3;
pub const GROUP_ADMIN_ADD_ADMINS: u32 = 1 << 4;
pub const GROUP_ADMIN_CHANGE_INFO: u32 = 1 << 5;
pub const GROUP_ADMIN_BAN_USERS: u32 = 1 << 6;
pub const GROUP_ADMIN_DELETE_MESSAGES: u32 = 1 << 7;
pub const GROUP_ADMIN_PIN_MESSAGES: u32 = 1 << 8;
pub const GROUP_ADMIN_INVITE_USERS: u32 = 1 << 9;
pub const GROUP_ADMIN: u32 = 1 << 10;
pub const GROUP_MEMBER: u32 = 1 << 11;
pub const PM: u32 = 1 << 12;
pub const EVERYONE: u32 = 1 << 13;

pub const GROUP_ADMIN_ANY: u32 = GROUP_ADMIN_ADD_ADMINS
    | GROUP_ADMIN_CHANGE_INFO
    | GROUP_ADMIN_BAN_USERS
    | GROUP_ADMIN_DELETE_MESSAGES
    | GROUP_ADMIN_PIN_MESSAGES
    | GROUP_ADMIN_INVITE_USERS
    | GROUP_ADMIN;
pub const DEFAULT_PERMISSIONS: u32 = OWNER;
pub const ALL: u32 = (1 << 14) - 1;

pub const PERMISSION_NAMES: &[(&str, u32)] = &[
    ("owner", OWNER),
    ("sudo", SUDO),
    ("support", SUPPORT),
    ("group_owner", GROUP_OWNER),
    ("group_admin_add_admins", GROUP_ADMIN_ADD_ADMINS),
    ("group_admin_change_info", GROUP_ADMIN_CHANGE_INFO),
    ("group_admin_ban_users", GROUP_ADMIN_BAN_USERS),
    ("group_admin_delete_messages", GROUP_ADMIN_DELETE_MESSAGES),
    ("group_admin_pin_messages", GROUP_ADMIN_PIN_MESSAGES),
    ("group_admin_invite_users", GROUP_ADMIN_INVITE_USERS),
    ("group_admin", GROUP_ADMIN),
    ("group_member", GROUP_MEMBER),
    ("pm", PM),
    ("everyone", EVERYONE),
];

pub fn permission_bit(name: &str) -> Option<u32> {
    let name = name.trim().to_lowercase().replace('-', "_");
    PERMISSION_NAMES
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, bit)| *bit)
}

#[derive(Debug, Clone, Copy)]
pub struct CommandDef {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub usage: &'static str,
    pub security: u32,
    pub ratelimit: bool,
}

impl CommandDef {
    pub const fn new(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            aliases: &[],
            description,
            usage: "",
            security: 0,
            ratelimit: false,
        }
    }

    pub const fn aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.aliases = aliases;
        self
    }

    pub const fn usage(mut self, usage: &'static str) -> Self {
        self.usage = usage;
        self
    }

    pub const fn security(mut self, security: u32) -> Self {
        self.security = security;
        self
    }

    pub const fn ratelimit(mut self) -> Self {
        self.ratelimit = true;
        self
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ConfigDefault {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(&'static str),
    List(&'static [&'static str]),
}

impl ConfigDefault {
    pub fn to_value(self) -> Value {
        match self {
            Self::Bool(value) => Value::Bool(value),
            Self::Int(value) => Value::from(value),
            Self::Float(value) => Value::from(value),
            Self::Text(value) => Value::String(value.to_owned()),
            Self::List(values) => Value::Array(
                values
                    .iter()
                    .map(|v| Value::String((*v).to_owned()))
                    .collect(),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ConfigKind {
    Bool,
    Int,
    Float,
    Text,
    Choice(&'static [&'static str]),
    List,
    Hidden,
}

#[derive(Debug, Clone, Copy)]
pub struct ConfigSpec {
    pub key: &'static str,
    pub description: &'static str,
    pub default: ConfigDefault,
    pub kind: ConfigKind,
    pub folder: Option<&'static str>,
}

impl ConfigSpec {
    pub const fn new(
        key: &'static str,
        description: &'static str,
        default: ConfigDefault,
        kind: ConfigKind,
    ) -> Self {
        Self {
            key,
            description,
            default,
            kind,
            folder: None,
        }
    }

    pub const fn folder(mut self, folder: &'static str) -> Self {
        self.folder = Some(folder);
        self
    }
}

#[async_trait]
pub trait Module: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str {
        ""
    }
    fn author(&self) -> &'static str {
        "Heroku-userbot"
    }
    fn version(&self) -> &'static str {
        crate::config::VERSION
    }
    fn commands(&self) -> Vec<CommandDef> {
        Vec::new()
    }
    fn config_spec(&self) -> Vec<ConfigSpec> {
        Vec::new()
    }
    fn core(&self) -> bool {
        true
    }

    async fn on_load(&self, _ctx: &Arc<Ctx>) -> Result<()> {
        Ok(())
    }

    async fn on_unload(&self, _ctx: &Arc<Ctx>) -> Result<()> {
        Ok(())
    }

    /// Passive watcher. Return `true` if the message was handled and command
    /// dispatch should stop.
    async fn on_message(&self, _ctx: &Arc<Ctx>, _message: &Msg) -> Result<bool> {
        Ok(false)
    }

    async fn handle(&self, ctx: &Arc<Ctx>, command: &str, message: &Msg, args: &Args)
        -> Result<()>;
}

pub struct ModuleFactory(pub fn() -> Box<dyn Module>);

inventory::collect!(ModuleFactory);

pub struct Modules {
    modules: Vec<Arc<dyn Module>>,
    by_name: HashMap<String, usize>,
    commands: HashMap<String, Vec<usize>>,
}

impl Modules {
    pub fn discover() -> Self {
        let mut modules: Vec<Arc<dyn Module>> = inventory::iter::<ModuleFactory>
            .into_iter()
            .map(|factory| Arc::from((factory.0)()) as Arc<dyn Module>)
            .collect();
        modules.sort_by_key(|module| module.name());

        let mut by_name = HashMap::new();
        let mut commands: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, module) in modules.iter().enumerate() {
            by_name.insert(module.name().to_lowercase(), index);
            let short = module
                .name()
                .strip_suffix("Mod")
                .unwrap_or(module.name())
                .to_lowercase();
            by_name.entry(short).or_insert(index);

            for command in module.commands() {
                commands
                    .entry(command.name.to_lowercase())
                    .or_default()
                    .push(index);
                for alias in command.aliases {
                    commands
                        .entry((*alias).to_lowercase())
                        .or_default()
                        .push(index);
                }
            }
        }

        Self {
            modules,
            by_name,
            commands,
        }
    }

    pub fn all(&self) -> &[Arc<dyn Module>] {
        &self.modules
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Module>> {
        let name = name.trim().trim_start_matches('.').to_lowercase();
        self.by_name
            .get(&name)
            .and_then(|index| self.modules.get(*index))
            .cloned()
    }

    /// All commands (including aliases) with the module that owns them.
    pub fn find_command(&self, name: &str) -> Vec<(Arc<dyn Module>, CommandDef)> {
        let name = name.trim().trim_start_matches('.').to_lowercase();
        let mut out = Vec::new();
        if let Some(indexes) = self.commands.get(&name) {
            for index in indexes {
                let module = self.modules[*index].clone();
                if let Some(command) = module.commands().into_iter().find(|command| {
                    command.name.eq_ignore_ascii_case(&name)
                        || command
                            .aliases
                            .iter()
                            .any(|alias| alias.eq_ignore_ascii_case(&name))
                }) {
                    out.push((module, command));
                }
            }
        }
        out
    }

    pub fn command_exists(&self, name: &str) -> bool {
        self.commands
            .contains_key(&name.trim().trim_start_matches('.').to_lowercase())
    }

    pub fn commands_of(&self, module: &str) -> Vec<CommandDef> {
        self.get(module)
            .map(|module| module.commands())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_bits_match_heroku() {
        assert_eq!(permission_bit("owner"), Some(1));
        assert_eq!(permission_bit("sudo"), Some(2));
        assert_eq!(permission_bit("Group-Admin"), Some(1 << 10));
        assert_eq!(permission_bit("everyone"), Some(1 << 13));
        assert_eq!(permission_bit("nonsense"), None);
    }

    #[test]
    fn every_known_command_is_discoverable() {
        let modules = Modules::discover();
        assert!(!modules.all().is_empty());
        for module in modules.all() {
            for command in module.commands() {
                assert!(
                    modules.command_exists(command.name),
                    "missing command {} from {}",
                    command.name,
                    module.name()
                );
                for alias in command.aliases {
                    assert!(
                        modules.command_exists(alias),
                        "missing alias {alias} from {}",
                        module.name()
                    );
                }
            }
        }
    }

    #[test]
    fn module_lookup_is_case_insensitive_and_short() {
        let modules = Modules::discover();
        assert!(modules.get("help").is_some());
        assert!(modules.get("Help").is_some());
        assert!(modules.get("CoreMod").is_some());
        assert!(modules.get("core").is_some());
        assert!(modules.get("nope").is_none());
    }
}
