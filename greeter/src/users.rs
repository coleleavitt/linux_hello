//! The account on the sign-in screen: name, display name, picture, session command.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

#[derive(Debug, Clone)]
pub struct Account {
    pub name: String,
    /// The GECOS full name, else the login name.
    pub display_name: String,
    pub home: PathBuf,
}

impl Account {
    /// `name`, or the first login account in /etc/passwd (uid 1000–59999 with a login shell).
    pub fn find(name: Option<&str>) -> Result<Account> {
        let passwd = std::fs::read_to_string("/etc/passwd")?;
        let mut accounts = passwd.lines().filter_map(parse_passwd_line);
        match name {
            Some(name) => accounts.find(|a| a.0 == name).map(|a| a.1).ok_or_else(|| anyhow!("no such user: {name}")),
            None => accounts
                .find(|(_, _, uid, shell)| {
                    (1000..60000).contains(uid) && !shell.ends_with("nologin") && shell != "/bin/false"
                })
                .map(|a| a.1)
                .ok_or_else(|| anyhow!("no login account in /etc/passwd; set `user` in the config")),
        }
    }

    /// The user's picture, where desktops keep it: `~/.face`, `~/.face.icon`, AccountsService.
    pub fn picture(&self) -> Option<PathBuf> {
        [
            self.home.join(".face"),
            self.home.join(".face.icon"),
            PathBuf::from("/var/lib/AccountsService/icons").join(&self.name),
        ]
        .into_iter()
        .find(|p| p.is_file())
    }

    /// Up to two initials for the picture-less avatar.
    pub fn initials(&self) -> String {
        self.display_name
            .split_whitespace()
            .filter_map(|word| word.chars().next())
            .take(2)
            .flat_map(char::to_uppercase)
            .collect()
    }

    /// The session command: `command` from the config, else `dbus-run-session
    /// ~/.local/bin/start-niri.sh` when that exists, else the first wayland session's `Exec`.
    pub fn session_command(&self, configured: Option<&str>) -> Result<Vec<String>> {
        if let Some(command) = configured {
            return Ok(command.split_whitespace().map(str::to_string).collect());
        }
        let script = self.home.join(".local/bin/start-niri.sh");
        if script.is_file() {
            return Ok(vec!["dbus-run-session".into(), script.to_string_lossy().into_owned()]);
        }
        first_wayland_session(Path::new("/usr/share/wayland-sessions"))
            .ok_or_else(|| anyhow!("no session command: set `command` in the config"))
    }
}

/// `(name, account, uid, shell)` from one /etc/passwd line.
fn parse_passwd_line(line: &str) -> Option<(String, Account, u32, String)> {
    let fields: Vec<&str> = line.split(':').collect();
    if fields.len() < 7 {
        return None;
    }
    let name = fields[0].to_string();
    let full_name = fields[4].split(',').next().unwrap_or("").trim();
    let account = Account {
        name: name.clone(),
        display_name: if full_name.is_empty() { name.clone() } else { full_name.to_string() },
        home: PathBuf::from(fields[5]),
    };
    Some((name, account, fields[2].parse().ok()?, fields[6].to_string()))
}

/// The `Exec=` of the alphabetically first `*.desktop` in `dir`.
fn first_wayland_session(dir: &Path) -> Option<Vec<String>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
        .collect();
    entries.sort();
    entries.into_iter().find_map(|path| {
        let text = std::fs::read_to_string(path).ok()?;
        let exec = text.lines().find_map(|l| l.strip_prefix("Exec="))?;
        Some(exec.split_whitespace().map(str::to_string).collect())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_and_display_name() {
        let (_, account, uid, shell) =
            parse_passwd_line("cole:x:1000:1000:Cole Leavitt,,,:/home/cole:/bin/zsh").unwrap();
        assert_eq!((uid, shell.as_str()), (1000, "/bin/zsh"));
        assert_eq!(account.display_name, "Cole Leavitt");
        assert_eq!(account.initials(), "CL");
        let (_, plain, ..) = parse_passwd_line("bob:x:1001:1001::/home/bob:/bin/sh").unwrap();
        assert_eq!(plain.display_name, "bob");
        assert_eq!(plain.initials(), "B");
    }

    #[test]
    fn configured_command_is_split() {
        let account = Account { name: "a".into(), display_name: "a".into(), home: "/nonexistent".into() };
        assert_eq!(account.session_command(Some("dbus-run-session niri")).unwrap(), vec!["dbus-run-session", "niri"]);
    }
}
