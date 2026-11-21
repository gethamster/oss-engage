use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use engage_client::{Client, TaskClaim};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Profile {
    pub url: String,
    pub token: Option<String>,
    pub actor: Option<String>,
}

impl fmt::Debug for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Profile")
            .field("url", &self.url)
            .field("token", &self.token.as_ref().map(|_| "[redacted]"))
            .field("actor", &self.actor)
            .finish()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub active: Option<String>,
    pub profiles: BTreeMap<String, Profile>,
}

pub fn dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("ENGAGE_CONFIG_DIR") {
        return Ok(path.into());
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("engage"));
    }
    Ok(PathBuf::from(
        std::env::var_os("HOME")
            .context("HOME or XDG_CONFIG_HOME must name your configuration directory")?,
    )
    .join(".config/engage"))
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("reading {}", path.display())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        private_write(path, &serde_json::to_vec_pretty(self)?)
    }

    pub fn redacted(&self) -> Value {
        let profiles: BTreeMap<_, _> = self.profiles.iter().map(|(name, profile)| (name, json!({"url":profile.url,"actor":profile.actor,"authenticated":profile.token.is_some()}))).collect();
        json!({"active":self.active,"profiles":profiles})
    }

    pub fn client(
        &self,
        explicit_url: Option<&str>,
        profile: Option<&str>,
        actor: Option<&str>,
    ) -> Result<Client> {
        let selected = profile.or(self.active.as_deref());
        let configured = selected.and_then(|name| self.profiles.get(name));
        if let Some(profile) = profile
            && configured.is_none()
        {
            bail!("unknown profile {profile}; use engage config add");
        }
        let environment_url = std::env::var("ENGAGE_URL")
            .or_else(|_| std::env::var("BOARD_URL"))
            .ok();
        let url = explicit_url
            .or(environment_url.as_deref())
            .or(configured.map(|p| p.url.as_str()))
            .unwrap_or("http://127.0.0.1:4100");
        let mut client = Client::new(url)?;
        let environment_token = std::env::var("ENGAGE_TOKEN")
            .or_else(|_| std::env::var("BOARD_TOKEN"))
            .ok();
        // An explicit URL never inherits an unrelated stored credential.
        let configured_token = configured
            .filter(|p| Client::new(&p.url).is_ok_and(|c| c.base_url() == client.base_url()))
            .and_then(|p| p.token.as_deref());
        if let Some(token) = environment_token.as_deref().or(configured_token) {
            client = client.with_token(token)?;
        }
        let environment_actor = std::env::var("ENGAGE_PROFILE")
            .or_else(|_| std::env::var("BOARD_PROFILE"))
            .ok();
        if let Some(actor) = actor
            .or(environment_actor.as_deref())
            .or(configured.and_then(|p| p.actor.as_deref()))
        {
            client = client.with_profile(actor)?;
        }
        Ok(client)
    }
}

pub fn private_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("configuration file requires a parent directory")?;
    fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[derive(Debug)]
pub struct Claims {
    directory: PathBuf,
    scope: String,
}

impl Claims {
    pub fn new(directory: PathBuf, board: &str, actor: Option<&str>) -> Self {
        Self {
            directory,
            scope: serde_json::to_string(&(board, actor)).expect("string tuple serializes"),
        }
    }
    fn path(&self, task: &str) -> PathBuf {
        let digest = Sha256::digest(format!("{}\0{task}", self.scope));
        self.directory.join(format!("{digest:x}.json"))
    }
    pub fn get(&self, task: &str) -> Result<TaskClaim> {
        let path = self.path(task);
        let bytes = fs::read(&path).with_context(|| {
            format!("no saved claim for task {task}; run engage tasks claim first")
        })?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    pub fn put(&self, task: &str, claim: &TaskClaim) -> Result<()> {
        private_write(&self.path(task), &serde_json::to_vec(claim)?)
    }
    pub fn drop(&self, task: &str) -> Result<()> {
        match fs::remove_file(self.path(task)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claims_are_scoped_to_board_and_actor_and_private() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let alice = Claims::new(
            dir.path().join("claims"),
            "https://board.example/api/",
            Some("alice"),
        );
        let bob = Claims::new(
            dir.path().join("claims"),
            "https://board.example/api/",
            Some("bob"),
        );
        let claim = TaskClaim {
            epoch: 2,
            token: "secret".into(),
            until: "2027-01-01T00:00:00Z".into(),
        };
        alice.put("task/opaque", &claim)?;
        assert_eq!(alice.get("task/opaque")?, claim);
        assert!(bob.get("task/opaque").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(alice.path("task/opaque"))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        alice.drop("task/opaque")?;
        assert!(alice.get("task/opaque").is_err());
        Ok(())
    }
    #[test]
    fn configuration_display_redacts_credentials() {
        let config = Config {
            active: Some("work".into()),
            profiles: BTreeMap::from([(
                "work".into(),
                Profile {
                    url: "https://board.example".into(),
                    token: Some("secret-never-show".into()),
                    actor: None,
                },
            )]),
        };
        assert!(!config.redacted().to_string().contains("secret-never-show"));
        assert!(!format!("{config:?}").contains("secret-never-show"));
    }
}
