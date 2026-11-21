//
//       user/sudo/src/config.rs
//       passwd/group/shadow and sudoers/doas.conf policy parsing
//
//       2026/11/21 By JiTianYu391
//       Copyright (C) 2026 ViudiraTech.
//

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use crate::paths::Paths;
use crate::util::{check_secure_file, hostname, read_to_string};

pub const DEFAULT_SECURE_PATH: &str =
    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

#[derive(Clone, Debug)]
pub struct User {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
    pub shell: String,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub name: String,
    pub gid: u32,
    pub members: Vec<String>,
}

pub fn load_users(paths: &Paths) -> Result<Vec<User>, String> {
    let text = read_to_string(&paths.passwd()).map_err(|error| error.to_string())?;
    let mut users = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 7 {
            continue;
        }
        let uid = fields[2]
            .parse()
            .map_err(|_| format!("invalid uid in '{}'", line))?;
        let gid = fields[3]
            .parse()
            .map_err(|_| format!("invalid gid in '{}'", line))?;
        users.push(User {
            name: fields[0].to_string(),
            uid,
            gid,
            home: fields[5].to_string(),
            shell: if fields[6].is_empty() {
                "/bin/sh".to_string()
            } else {
                fields[6].to_string()
            },
        });
    }
    Ok(users)
}

pub fn load_groups(paths: &Paths) -> Result<Vec<Group>, String> {
    let text = read_to_string(&paths.group()).map_err(|error| error.to_string())?;
    let mut groups = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 4 {
            continue;
        }
        let gid = fields[2]
            .parse()
            .map_err(|_| format!("invalid gid in '{}'", line))?;
        groups.push(Group {
            name: fields[0].to_string(),
            gid,
            members: fields[3]
                .split(',')
                .filter(|member| !member.is_empty())
                .map(str::to_string)
                .collect(),
        });
    }
    Ok(groups)
}

pub fn find_user<'a>(users: &'a [User], name: &str) -> Option<&'a User> {
    if let Some(uid) = name
        .strip_prefix('#')
        .and_then(|uid| uid.parse::<u32>().ok())
    {
        return users.iter().find(|user| user.uid == uid);
    }
    users.iter().find(|user| user.name == name)
}

pub fn find_user_by_uid(users: &[User], uid: u32) -> Option<&User> {
    users.iter().find(|user| user.uid == uid)
}

pub fn find_group<'a>(groups: &'a [Group], name: &str) -> Option<&'a Group> {
    if let Some(gid) = name
        .strip_prefix('#')
        .and_then(|gid| gid.parse::<u32>().ok())
    {
        return groups.iter().find(|group| group.gid == gid);
    }
    groups.iter().find(|group| group.name == name)
}

pub fn user_group_names(user: &User, groups: &[Group]) -> Vec<String> {
    let mut names = Vec::new();
    for group in groups {
        if group.gid == user.gid || group.members.iter().any(|member| member == &user.name) {
            names.push(group.name.clone());
        }
    }
    names
}

pub fn user_group_ids(user: &User, groups: &[Group]) -> Vec<u32> {
    let mut ids = vec![user.gid];
    for group in groups {
        if group.members.iter().any(|member| member == &user.name) && !ids.contains(&group.gid) {
            ids.push(group.gid);
        }
    }
    ids
}

pub fn shadow_hash(paths: &Paths, name: &str) -> Result<Option<String>, String> {
    let text = read_to_string(&paths.shadow()).map_err(|error| error.to_string())?;
    for line in text.lines() {
        let mut fields = line.split(':');
        if fields.next() == Some(name) {
            return Ok(fields.next().map(str::to_string));
        }
    }
    Ok(None)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LectureMode {
    Once,
    Always,
    Never,
}

#[derive(Clone, Debug)]
pub struct Defaults {
    pub lecture: LectureMode,
    /// Seconds; 0 disables the ticket.
    pub timestamp_timeout: u64,
    pub secure_path: String,
    pub env_keep: BTreeSet<String>,
    pub passwd_tries: u32,
    pub timestamp_type_global: bool,
    pub lecture_file: Option<String>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            lecture: LectureMode::Once,
            timestamp_timeout: 15 * 60,
            secure_path: DEFAULT_SECURE_PATH.to_string(),
            env_keep: BTreeSet::new(),
            passwd_tries: 3,
            timestamp_type_global: false,
            lecture_file: None,
        }
    }
}

#[derive(Clone, Debug)]
struct Rule {
    users: Vec<String>,
    hosts: Vec<String>,
    runas: Vec<String>,
    commands: Vec<String>,
    nopass: bool,
}

#[derive(Clone, Debug)]
pub struct Sudoers {
    pub defaults: Defaults,
    rules: Vec<Rule>,
    user_aliases: BTreeMap<String, Vec<String>>,
    host_aliases: BTreeMap<String, Vec<String>>,
    cmnd_aliases: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allowed { nopass: bool },
    Denied,
}

fn split_list(text: &str) -> Vec<String> {
    text.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_defaults(text: &str, defaults: &mut Defaults) {
    for item in text.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (key, value) = match item.split_once('=') {
            Some((key, value)) => (key.trim(), value.trim().trim_matches('"')),
            None => (item, ""),
        };
        match key {
            "lecture" => {
                defaults.lecture = match value {
                    "always" => LectureMode::Always,
                    "never" => LectureMode::Never,
                    _ => LectureMode::Once,
                }
            }
            "timestamp_timeout" => {
                if let Ok(minutes) = value.parse::<i64>() {
                    defaults.timestamp_timeout = if minutes <= 0 {
                        0
                    } else {
                        (minutes as u64).saturating_mul(60)
                    };
                }
            }
            "secure_path" => defaults.secure_path = value.to_string(),
            "env_keep" => {
                for name in value.split_whitespace() {
                    defaults.env_keep.insert(name.to_string());
                }
            }
            "passwd_tries" => {
                if let Ok(tries) = value.parse::<u32>() {
                    defaults.passwd_tries = tries.max(1);
                }
            }
            "timestamp_type" => defaults.timestamp_type_global = value == "global",
            "lecture_file" => {
                defaults.lecture_file = (!value.is_empty()).then(|| value.to_string())
            }
            _ => {}
        }
    }
}

fn parse_alias(rest: &str) -> Result<(String, Vec<String>), String> {
    let (name, values) = rest
        .split_once('=')
        .ok_or_else(|| format!("malformed alias '{rest}'"))?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("empty alias name".into());
    }
    Ok((name, split_list(values)))
}

impl Sudoers {
    pub fn builtin() -> Self {
        Self {
            defaults: Defaults::default(),
            rules: vec![
                Rule {
                    users: vec!["%wheel".into()],
                    hosts: vec!["ALL".into()],
                    runas: vec!["ALL".into()],
                    commands: vec!["ALL".into()],
                    nopass: false,
                },
                Rule {
                    users: vec!["%sudo".into()],
                    hosts: vec!["ALL".into()],
                    runas: vec!["ALL".into()],
                    commands: vec!["ALL".into()],
                    nopass: false,
                },
            ],
            user_aliases: BTreeMap::new(),
            host_aliases: BTreeMap::new(),
            cmnd_aliases: BTreeMap::new(),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut sudoers = Self {
            defaults: Defaults::default(),
            rules: Vec::new(),
            user_aliases: BTreeMap::new(),
            host_aliases: BTreeMap::new(),
            cmnd_aliases: BTreeMap::new(),
        };
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix("Defaults") {
                parse_defaults(rest.trim_start_matches(':').trim(), &mut sudoers.defaults);
                continue;
            }
            if let Some(rest) = line.strip_prefix("User_Alias") {
                let (name, values) = parse_alias(rest)?;
                sudoers.user_aliases.insert(name, values);
                continue;
            }
            if let Some(rest) = line.strip_prefix("Host_Alias") {
                let (name, values) = parse_alias(rest)?;
                sudoers.host_aliases.insert(name, values);
                continue;
            }
            if let Some(rest) = line.strip_prefix("Cmnd_Alias") {
                let (name, values) = parse_alias(rest)?;
                sudoers.cmnd_aliases.insert(name, values);
                continue;
            }
            let (left, right) = line
                .split_once('=')
                .ok_or_else(|| format!("line {}: missing '='", index + 1))?;
            let left = left.trim();
            let split = left
                .rfind(char::is_whitespace)
                .ok_or_else(|| format!("line {}: missing host list", index + 1))?;
            let users = split_list(&left[..split]);
            let hosts = split_list(left[split..].trim());
            if users.is_empty() || hosts.is_empty() {
                return Err(format!("line {}: empty user/host list", index + 1));
            }
            let mut right = right.trim();
            let mut runas = vec!["root".to_string()];
            if right.starts_with('(') {
                let end = right
                    .find(')')
                    .ok_or_else(|| format!("line {}: unterminated runas list", index + 1))?;
                runas = split_list(&right[1..end]);
                right = right[end + 1..].trim();
            }
            let mut nopass = false;
            if let Some(rest) = right.strip_prefix("NOPASSWD:") {
                nopass = true;
                right = rest.trim();
            } else if let Some(rest) = right.strip_prefix("PASSWD:") {
                right = rest.trim();
            }
            let commands = split_list(right);
            if commands.is_empty() {
                return Err(format!("line {}: empty command list", index + 1));
            }
            sudoers.rules.push(Rule {
                users,
                hosts,
                runas,
                commands,
                nopass,
            });
        }
        Ok(sudoers)
    }

    pub fn load(paths: &Paths) -> Result<(Self, bool), String> {
        let path = paths.sudoers();
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                check_secure_file(&path, 0o440).map_err(|error| error.to_string())?;
                let text = read_to_string(&path).map_err(|error| error.to_string())?;
                Ok((Self::parse(&text)?, true))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok((Self::builtin(), false))
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn user_matches(&self, spec: &str, user: &User, groups: &[String]) -> bool {
        if let Some(items) = self.user_aliases.get(spec) {
            return items
                .iter()
                .any(|item| self.user_matches(item, user, groups));
        }
        spec == "ALL"
            || spec == user.name
            || spec
                .strip_prefix('#')
                .is_some_and(|uid| uid.parse::<u32>() == Ok(user.uid))
            || spec
                .strip_prefix('%')
                .is_some_and(|group| groups.iter().any(|name| name == group))
    }

    fn host_matches(&self, spec: &str, host: &str) -> bool {
        if let Some(items) = self.host_aliases.get(spec) {
            return items.iter().any(|item| self.host_matches(item, host));
        }
        spec == "ALL" || spec == host || (spec == "localhost" && host == "localhost")
    }

    fn runas_matches(&self, spec: &str, target: &User, target_groups: &[String]) -> bool {
        spec == "ALL"
            || spec == target.name
            || spec
                .strip_prefix('#')
                .is_some_and(|uid| uid.parse::<u32>() == Ok(target.uid))
            || spec
                .strip_prefix('%')
                .is_some_and(|group| target_groups.iter().any(|name| name == group))
    }

    fn command_matches(&self, spec: &str, program: &str) -> bool {
        if spec == "sudoedit" && program == "/usr/bin/sudoedit" {
            return true;
        }
        if let Some(items) = self.cmnd_aliases.get(spec) {
            return items.iter().any(|item| self.command_matches(item, program));
        }
        if spec == "ALL" {
            return true;
        }
        if let Some(prefix) = spec.strip_suffix('*') {
            return program.starts_with(prefix);
        }
        spec == program
    }

    pub fn check(
        &self,
        user: &User,
        groups: &[String],
        target: &User,
        target_groups: &[String],
        program: &str,
    ) -> Decision {
        let host = hostname();
        for rule in &self.rules {
            if !rule
                .users
                .iter()
                .any(|spec| self.user_matches(spec, user, groups))
                || !rule.hosts.iter().any(|spec| self.host_matches(spec, &host))
                || !rule
                    .runas
                    .iter()
                    .any(|spec| self.runas_matches(spec, target, target_groups))
            {
                continue;
            }
            if rule
                .commands
                .iter()
                .any(|spec| self.command_matches(spec, program))
            {
                return Decision::Allowed {
                    nopass: rule.nopass,
                };
            }
        }
        Decision::Denied
    }

    pub fn list(&self, user: &User, groups: &[String]) -> Vec<String> {
        let host = hostname();
        let mut lines = Vec::new();
        for rule in &self.rules {
            if rule
                .users
                .iter()
                .any(|spec| self.user_matches(spec, user, groups))
                && rule.hosts.iter().any(|spec| self.host_matches(spec, &host))
            {
                lines.push(format!(
                    "({}) {}{}",
                    rule.runas.join(", "),
                    if rule.nopass { "NOPASSWD: " } else { "" },
                    rule.commands.join(", ")
                ));
            }
        }
        lines
    }
}

#[derive(Clone, Debug)]
struct DoasRule {
    users: Vec<String>,
    target: Option<String>,
    command: String,
    nopass: bool,
    deny: bool,
}

#[derive(Clone, Debug)]
pub struct Doas {
    pub defaults: Defaults,
    rules: Vec<DoasRule>,
}

impl Doas {
    pub fn builtin() -> Self {
        Self {
            defaults: Defaults::default(),
            rules: vec![DoasRule {
                users: vec![":wheel".into(), ":sudo".into()],
                target: None,
                command: "*".into(),
                nopass: false,
                deny: false,
            }],
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut doas = Self {
            defaults: Defaults::default(),
            rules: Vec::new(),
        };
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut tokens = line.split_whitespace();
            let action = tokens.next().unwrap_or_default();
            if action != "permit" && action != "deny" {
                return Err(format!("line {}: expected permit/deny", index + 1));
            }
            let deny = action == "deny";
            let mut nopass = false;
            let mut users = Vec::new();
            let mut target = None;
            let mut command = "*".to_string();
            let rest: Vec<&str> = tokens.collect();
            let mut cursor = 0;
            if rest.get(cursor) == Some(&"nopass") {
                nopass = true;
                cursor += 1;
            }
            while cursor < rest.len() && rest[cursor] != "as" && rest[cursor] != "cmd" {
                users.push(rest[cursor].to_string());
                cursor += 1;
            }
            if users.is_empty() {
                users.push("all".into());
            }
            if rest.get(cursor) == Some(&"as") {
                cursor += 1;
                target = rest.get(cursor).map(|value| value.to_string());
                cursor += 1;
            }
            if rest.get(cursor) == Some(&"cmd") {
                cursor += 1;
                command = rest.get(cursor).unwrap_or(&"*").to_string();
            }
            doas.rules.push(DoasRule {
                users,
                target,
                command,
                nopass,
                deny,
            });
        }
        Ok(doas)
    }

    pub fn load(paths: &Paths) -> Result<(Self, bool), String> {
        let path = paths.doas_conf();
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                check_secure_file(&path, 0o440).map_err(|error| error.to_string())?;
                let text = read_to_string(&path).map_err(|error| error.to_string())?;
                Ok((Self::parse(&text)?, true))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok((Self::builtin(), false))
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn user_matches(spec: &str, user: &User, groups: &[String]) -> bool {
        spec == "all"
            || spec == "ALL"
            || spec == user.name
            || spec
                .strip_prefix(':')
                .is_some_and(|group| groups.iter().any(|name| name == group))
    }

    fn command_matches(spec: &str, program: &str) -> bool {
        if spec == "*" || spec == "ALL" || spec == "all" {
            return true;
        }
        if let Some(prefix) = spec.strip_suffix('*') {
            return program.starts_with(prefix);
        }
        spec == program
    }

    pub fn list(&self, user: &User, groups: &[String]) -> Vec<String> {
        self.rules
            .iter()
            .filter(|rule| {
                rule.users
                    .iter()
                    .any(|spec| Self::user_matches(spec, user, groups))
            })
            .map(|rule| {
                format!(
                    "{} {}{} {}",
                    if rule.deny { "deny" } else { "permit" },
                    if rule.nopass { "nopass " } else { "" },
                    rule.users.join(" "),
                    rule.command
                )
            })
            .collect()
    }

    pub fn check(&self, user: &User, groups: &[String], target: &User, program: &str) -> Decision {
        for rule in &self.rules {
            if !rule
                .users
                .iter()
                .any(|spec| Self::user_matches(spec, user, groups))
            {
                continue;
            }
            if let Some(wanted) = &rule.target {
                if wanted != "ALL" && wanted != &target.name {
                    continue;
                }
            }
            if Self::command_matches(&rule.command, program) {
                return if rule.deny {
                    Decision::Denied
                } else {
                    Decision::Allowed {
                        nopass: rule.nopass,
                    }
                };
            }
        }
        Decision::Denied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> User {
        User {
            name: "alice".into(),
            uid: 1000,
            gid: 1000,
            home: "/home/alice".into(),
            shell: "/bin/sh".into(),
        }
    }

    fn root_user() -> User {
        User {
            name: "root".into(),
            uid: 0,
            gid: 0,
            home: "/root".into(),
            shell: "/bin/sh".into(),
        }
    }

    #[test]
    fn sudoers_subset_matches() {
        let policy = Sudoers::parse(
            "User_Alias ADMINS = alice\n\
             Cmnd_Alias PKG = /usr/bin/spk\n\
             Defaults lecture=never,timestamp_timeout=2\n\
             ADMINS ALL=(root) NOPASSWD: PKG\n",
        )
        .unwrap();
        assert_eq!(policy.defaults.lecture, LectureMode::Never);
        assert_eq!(policy.defaults.timestamp_timeout, 120);
        let decision = policy.check(
            &user(),
            &["alice".into()],
            &root_user(),
            &[],
            "/usr/bin/spk",
        );
        assert_eq!(decision, Decision::Allowed { nopass: true });
        assert_eq!(
            policy.check(&user(), &[], &root_user(), &[], "/usr/bin/other"),
            Decision::Denied
        );
    }

    #[test]
    fn sudoers_groups_and_hosts() {
        let policy = Sudoers::parse("%wheel ALL=(ALL) ALL\n").unwrap();
        assert_eq!(
            policy.check(
                &user(),
                &["wheel".into()],
                &root_user(),
                &[],
                "/usr/bin/spk"
            ),
            Decision::Allowed { nopass: false }
        );
        assert_eq!(
            policy.check(
                &user(),
                &["users".into()],
                &root_user(),
                &[],
                "/usr/bin/spk"
            ),
            Decision::Denied
        );
    }

    #[test]
    fn doas_subset_matches() {
        let policy = Doas::parse(
            "permit nopass :wheel as root cmd /usr/bin/spk\ndeny :guest cmd /usr/bin/sh\n",
        )
        .unwrap();
        assert_eq!(
            policy.check(&user(), &["wheel".into()], &root_user(), "/usr/bin/spk"),
            Decision::Allowed { nopass: true }
        );
        assert_eq!(
            policy.check(&user(), &["guest".into()], &root_user(), "/usr/bin/sh"),
            Decision::Denied
        );
        assert_eq!(
            policy.check(&user(), &["users".into()], &root_user(), "/usr/bin/spk"),
            Decision::Denied
        );
    }

    #[test]
    fn user_and_group_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::rooted(dir.path());
        fs::create_dir_all(&paths.etc).unwrap();
        fs::write(
            paths.passwd(),
            "root:x:0:0:root:/root:/bin/sh\nalice:x:1000:1000:Alice:/home/alice:/bin/sh\n",
        )
        .unwrap();
        fs::write(
            paths.group(),
            "root:x:0:\nwheel:x:10:alice\nusers:x:1000:\n",
        )
        .unwrap();
        let users = load_users(&paths).unwrap();
        let groups = load_groups(&paths).unwrap();
        let alice = find_user(&users, "alice").unwrap();
        assert_eq!(alice.uid, 1000);
        let names = user_group_names(alice, &groups);
        assert!(names.contains(&"wheel".to_string()));
        assert!(names.contains(&"users".to_string()));
    }
}
