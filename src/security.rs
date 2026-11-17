// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Security manager: owners, sudo, security groups, temporary rules and
//! per-command permission masks.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::ctx::Ctx;
use crate::database::Database;
use crate::module::*;
use crate::tg::Msg;

pub const NS: &str = "heroku.security";

pub struct Security {
    db: Arc<Database>,
}

impl Security {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn ensure_owner(&self, user_id: i64) {
        let mut owners = self.db.get_i64_list(NS, "owner");
        if !owners.contains(&user_id) {
            owners.push(user_id);
            self.db.set(
                NS,
                "owner",
                Value::Array(owners.into_iter().map(Value::from).collect()),
            );
        }
    }

    pub fn owners(&self) -> Vec<i64> {
        self.db.get_i64_list(NS, "owner")
    }

    pub fn sudo(&self) -> Vec<i64> {
        self.db.get_i64_list(NS, "sudo")
    }

    /// Every user that is allowed to use inline mode by default: owners, sudo,
    /// security-group members and temporary user rules. This mirrors Heroku's
    /// `security.all_users` cache.
    pub fn all_users(&self) -> Vec<i64> {
        let mut users = self.owners();
        users.extend(self.sudo());
        for group in self.sgroups().values() {
            if let Some(members) = group.get("users").and_then(Value::as_array) {
                users.extend(members.iter().filter_map(Value::as_i64));
            }
        }
        for rule in self.db.get_array(NS, "tsec_user") {
            if let Some(target) = rule.get("target").and_then(Value::as_i64) {
                users.push(target);
            }
        }
        users.sort_unstable();
        users.dedup();
        users
    }

    pub fn is_owner(&self, user_id: i64) -> bool {
        self.owners().contains(&user_id)
    }

    pub fn is_sudo(&self, user_id: i64) -> bool {
        self.sudo().contains(&user_id)
    }

    pub fn add_owner(&self, user_id: i64) -> bool {
        let mut owners = self.owners();
        if owners.contains(&user_id) {
            return false;
        }
        owners.push(user_id);
        self.set_id_list("owner", &owners)
    }

    pub fn remove_owner(&self, user_id: i64, self_id: i64) -> bool {
        if user_id == self_id {
            return false;
        }
        let mut owners = self.owners();
        let before = owners.len();
        owners.retain(|id| *id != user_id);
        if owners.len() == before {
            return false;
        }
        self.set_id_list("owner", &owners)
    }

    pub fn add_sudo(&self, user_id: i64) -> bool {
        let mut users = self.sudo();
        if users.contains(&user_id) {
            return false;
        }
        users.push(user_id);
        self.set_id_list("sudo", &users)
    }

    pub fn remove_sudo(&self, user_id: i64) -> bool {
        let mut users = self.sudo();
        let before = users.len();
        users.retain(|id| *id != user_id);
        users.len() != before && self.set_id_list("sudo", &users)
    }

    fn set_id_list(&self, key: &str, ids: &[i64]) -> bool {
        self.db.set(
            NS,
            key,
            Value::Array(ids.iter().copied().map(Value::from).collect()),
        )
    }

    pub fn default_mask(&self) -> u32 {
        self.db.get_i64(NS, "default", DEFAULT_PERMISSIONS as i64) as u32
    }

    pub fn bounding_mask(&self) -> u32 {
        self.db
            .get_i64(NS, "bounding_mask", DEFAULT_PERMISSIONS as i64) as u32
    }

    pub fn command_mask(&self, module: &str, command: &str, fallback: u32) -> u32 {
        let masks = self.db.get_map(NS, "masks");
        masks
            .get(&format!("{module}.{command}"))
            .and_then(Value::as_i64)
            .map(|mask| mask as u32)
            .unwrap_or_else(|| {
                if fallback == 0 {
                    self.default_mask()
                } else {
                    fallback
                }
            })
    }

    pub fn set_command_mask(&self, module: &str, command: &str, mask: u32) {
        let mut masks = self.db.get_map(NS, "masks");
        masks.insert(format!("{module}.{command}"), Value::from(mask));
        self.db.set(NS, "masks", Value::Object(masks));
    }

    pub fn clear_command_mask(&self, module: &str, command: &str) {
        let mut masks = self.db.get_map(NS, "masks");
        masks.remove(&format!("{module}.{command}"));
        self.db.set(NS, "masks", Value::Object(masks));
    }

    pub fn set_default_mask(&self, mask: u32) -> bool {
        self.db.set(NS, "default", Value::from(mask))
    }

    pub fn set_bounding_mask(&self, mask: u32) -> bool {
        self.db.set(NS, "bounding_mask", Value::from(mask))
    }

    pub fn has_rule(&self, user_id: i64, chat_id: i64, module: &str, command: &str) -> bool {
        let now = chrono::Utc::now().timestamp();
        self.rule_matches("sgroups", user_id, chat_id, module, command, now, true)
            || self.rule_matches("tsec_user", user_id, chat_id, module, command, now, true)
            || self.rule_matches("tsec_chat", user_id, chat_id, module, command, now, false)
    }

    // The rule lookup mirrors Heroku's matcher and needs all four selectors.
    #[allow(clippy::too_many_arguments)]
    fn rule_matches(
        &self,
        which: &str,
        user_id: i64,
        chat_id: i64,
        module: &str,
        command: &str,
        now: i64,
        by_user: bool,
    ) -> bool {
        let value = self.db.get_value(NS, which).unwrap_or(Value::Null);
        match which {
            "sgroups" => {
                let Some(groups) = value.as_object() else {
                    return false;
                };
                for group in groups.values() {
                    let users = group
                        .get("users")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    if !users.iter().any(|id| id.as_i64() == Some(user_id)) {
                        continue;
                    }
                    for rule in group
                        .get("permissions")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                    {
                        if rule_matches(&rule, module, command) {
                            return true;
                        }
                    }
                }
                false
            }
            "tsec_user" | "tsec_chat" => {
                let Some(rules) = value.as_array() else {
                    return false;
                };
                for rule in rules {
                    let target = rule
                        .get("target")
                        .and_then(Value::as_i64)
                        .unwrap_or_default();
                    if by_user && target != user_id {
                        continue;
                    }
                    if !by_user && target != chat_id {
                        continue;
                    }
                    let expires = rule.get("expires").and_then(Value::as_i64).unwrap_or(0);
                    if expires != 0 && expires < now {
                        continue;
                    }
                    if rule_matches(rule, module, command) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Core permission check.
    pub async fn check(
        &self,
        ctx: &Arc<Ctx>,
        message: &Msg,
        module: &str,
        command: &str,
        fallback_mask: u32,
    ) -> bool {
        let mask = self.command_mask(module, command, fallback_mask) & self.bounding_mask();
        if mask == 0 {
            return false;
        }

        let user_id = message.sender_id().unwrap_or_else(crate::tg::self_id);
        if user_id == ctx.me_id || message.outgoing() {
            return true;
        }

        if self
            .db
            .get_i64_list("heroku.main", "blacklist_users")
            .contains(&user_id)
        {
            return false;
        }

        if self.is_owner(user_id) || self.is_sudo(user_id) {
            return true;
        }

        if self.has_rule(user_id, message.chat_id(), module, command) {
            return true;
        }

        if message.is_private() && mask & PM != 0 {
            return true;
        }

        if !message.is_private() {
            if mask & GROUP_MEMBER != 0 {
                return true;
            }
            if mask & (GROUP_OWNER | GROUP_ADMIN_ANY) != 0 {
                let Some(rights) = ctx.chat_rights(message).await else {
                    return false;
                };
                if rights & mask != 0 {
                    return true;
                }
            }
        }

        false
    }

    /// Whether `user_id` may see and run `module.command` from an inline card,
    /// where there is no chat to evaluate. Mirrors Heroku's
    /// `security.check(message=None)`: owners, sudo and temporary rules pass;
    /// everyone else needs the `everyone` bit.
    pub fn inline_allowed(
        &self,
        me_id: i64,
        user_id: i64,
        module: &str,
        command: &str,
        fallback: u32,
    ) -> bool {
        let mask = self.command_mask(module, command, fallback) & self.bounding_mask();
        if mask == 0 {
            return false;
        }
        if user_id == me_id || self.is_owner(user_id) || self.is_sudo(user_id) {
            return true;
        }
        if self
            .db
            .get_i64_list("heroku.main", "blacklist_users")
            .contains(&user_id)
        {
            return false;
        }
        if mask & EVERYONE != 0 {
            return true;
        }
        self.has_rule(user_id, 0, module, command)
    }

    pub fn sgroups(&self) -> Map<String, Value> {
        self.db.get_map(NS, "sgroups")
    }

    pub fn save_sgroups(&self, groups: Map<String, Value>) -> bool {
        self.db.set(NS, "sgroups", Value::Object(groups))
    }

    pub fn add_sgroup(&self, name: &str, user_id: i64) -> bool {
        let mut groups = self.sgroups();
        let mut group = groups
            .remove(name)
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        let mut users = group
            .remove("users")
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default();
        if users.iter().any(|id| id.as_i64() == Some(user_id)) {
            groups.insert(name.to_owned(), Value::Object(group));
            return self.save_sgroups(groups);
        }
        users.push(Value::from(user_id));
        group.insert("users".to_owned(), Value::Array(users));
        group
            .entry("permissions".to_owned())
            .or_insert_with(|| Value::Array(Vec::new()));
        groups.insert(name.to_owned(), Value::Object(group));
        self.save_sgroups(groups)
    }

    pub fn remove_sgroup_user(&self, name: &str, user_id: i64) -> bool {
        let mut groups = self.sgroups();
        let Some(mut group) = groups
            .remove(name)
            .and_then(|value| value.as_object().cloned())
        else {
            return false;
        };
        let mut users = group
            .remove("users")
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default();
        users.retain(|id| id.as_i64() != Some(user_id));
        group.insert("users".to_owned(), Value::Array(users));
        groups.insert(name.to_owned(), Value::Object(group));
        self.save_sgroups(groups)
    }

    pub fn delete_sgroup(&self, name: &str) -> bool {
        let mut groups = self.sgroups();
        groups.remove(name).is_some() && self.save_sgroups(groups)
    }

    pub fn add_group_rule(&self, name: &str, rule_type: &str, rule: &str) -> bool {
        let mut groups = self.sgroups();
        let Some(mut group) = groups
            .remove(name)
            .and_then(|value| value.as_object().cloned())
        else {
            return false;
        };
        let mut permissions = group
            .remove("permissions")
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default();
        permissions.push(json!({"rule_type": rule_type, "rule": rule}));
        group.insert("permissions".to_owned(), Value::Array(permissions));
        groups.insert(name.to_owned(), Value::Object(group));
        self.save_sgroups(groups)
    }

    pub fn add_tsec(
        &self,
        target_type: &str,
        target: i64,
        rule_type: &str,
        rule: &str,
        duration: i64,
        name: &str,
    ) -> bool {
        let key = if target_type == "chat" {
            "tsec_chat"
        } else {
            "tsec_user"
        };
        let mut rules = self.db.get_array(NS, key);
        let expires = if duration > 0 {
            chrono::Utc::now().timestamp() + duration
        } else {
            0
        };
        rules.push(json!({
            "target": target,
            "rule_type": rule_type,
            "rule": rule,
            "expires": expires,
            "entity_name": name,
        }));
        self.db.set(NS, key, Value::Array(rules))
    }

    pub fn clear_tsec(&self) -> bool {
        self.db.set(NS, "tsec_user", Value::Array(Vec::new()))
            && self.db.set(NS, "tsec_chat", Value::Array(Vec::new()))
    }

    pub fn remove_tsec(&self, target_type: &str, target: i64) -> bool {
        let key = if target_type == "chat" {
            "tsec_chat"
        } else {
            "tsec_user"
        };
        let mut rules = self.db.get_array(NS, key);
        let before = rules.len();
        rules.retain(|rule| rule.get("target").and_then(Value::as_i64) != Some(target));
        rules.len() != before && self.db.set(NS, key, Value::Array(rules))
    }

    pub fn remove_tsec_rule(&self, target_type: &str, target: i64, rule: &str) -> bool {
        let key = if target_type == "chat" {
            "tsec_chat"
        } else {
            "tsec_user"
        };
        let mut rules = self.db.get_array(NS, key);
        let before = rules.len();
        rules.retain(|item| {
            item.get("target").and_then(Value::as_i64) != Some(target)
                || item.get("rule").and_then(Value::as_str) != Some(rule)
        });
        rules.len() != before && self.db.set(NS, key, Value::Array(rules))
    }

    pub fn clear_tsec_target(&self, target_type: &str, target: i64) -> bool {
        let key = if target_type == "chat" {
            "tsec_chat"
        } else {
            "tsec_user"
        };
        let mut rules = self.db.get_array(NS, key);
        let before = rules.len();
        rules.retain(|item| item.get("target").and_then(Value::as_i64) != Some(target));
        rules.len() != before && self.db.set(NS, key, Value::Array(rules))
    }

    pub fn remove_group_rule(&self, name: &str, rule: &str) -> bool {
        let mut groups = self.sgroups();
        let Some(mut group) = groups
            .remove(name)
            .and_then(|value| value.as_object().cloned())
        else {
            return false;
        };
        let mut permissions = group
            .remove("permissions")
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default();
        let before = permissions.len();
        permissions.retain(|item| item.get("rule").and_then(Value::as_str) != Some(rule));
        group.insert("permissions".to_owned(), Value::Array(permissions));
        groups.insert(name.to_owned(), Value::Object(group));
        self.save_sgroups(groups) && before != 0
    }

    pub fn clear_group_rules(&self, name: &str) -> bool {
        let mut groups = self.sgroups();
        let Some(mut group) = groups
            .remove(name)
            .and_then(|value| value.as_object().cloned())
        else {
            return false;
        };
        group.insert("permissions".to_owned(), Value::Array(Vec::new()));
        groups.insert(name.to_owned(), Value::Object(group));
        self.save_sgroups(groups)
    }

    pub fn tsec_rules(&self) -> Vec<Value> {
        let mut out = self.db.get_array(NS, "tsec_user");
        out.extend(self.db.get_array(NS, "tsec_chat"));
        out
    }
}

fn rule_matches(rule: &Value, module: &str, command: &str) -> bool {
    let rule_type = rule
        .get("rule_type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let target = rule.get("rule").and_then(Value::as_str).unwrap_or_default();
    match rule_type {
        "command" => target == command,
        "module" => target == module,
        _ => false,
    }
}

pub fn mask_to_names(mask: u32) -> Vec<&'static str> {
    PERMISSION_NAMES
        .iter()
        .filter(|(_, bit)| mask & bit != 0)
        .map(|(name, _)| *name)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn temp_security() -> (Security, Arc<Database>, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "heroku-security-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let db = Arc::new(Database::open(&path).expect("open database"));
        (Security::new(db.clone()), db, path)
    }

    #[test]
    fn inline_allowed_respects_masks_and_everyone() {
        let (security, db, path) = temp_security();
        let me = 1;
        let stranger = 2;

        // Default mask is OWNER and the bounding mask is OWNER, so a stranger
        // cannot see owner-only commands.
        assert!(security.inline_allowed(me, me, "Any", "cmd", 0));
        assert!(!security.inline_allowed(me, stranger, "Any", "cmd", 0));

        // Explicitly open the command and the bounding mask.
        db.set(NS, "bounding_mask", Value::from(ALL));
        security.set_command_mask("Any", "cmd", EVERYONE);
        assert!(security.inline_allowed(me, stranger, "Any", "cmd", 0));

        // Blacklist wins over EVERYONE, like in `Security::check`.
        db.set(
            "heroku.main",
            "blacklist_users",
            Value::Array(vec![Value::from(stranger)]),
        );
        assert!(!security.inline_allowed(me, stranger, "Any", "cmd", 0));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn zero_bounding_mask_blocks_even_the_owner() {
        let (security, db, path) = temp_security();
        db.set(NS, "bounding_mask", Value::from(0u32));
        assert!(!security.inline_allowed(1, 1, "Any", "cmd", 0));
        let _ = std::fs::remove_file(path);
    }
}
