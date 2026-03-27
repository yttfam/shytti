use std::path::PathBuf;

use serde::Deserialize;

use crate::error::Error;
use crate::shell::SpawnRequest;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    #[serde(default)]
    pub shells: Vec<ShellConfig>,
    /// Hermytt bootstrap writes [hermytt] with url/token. We merge it into daemon config.
    #[serde(default)]
    pub hermytt: Option<HermyttSection>,
    /// Hermytt bootstrap writes [shell] with default. We merge it into defaults.
    #[serde(default)]
    pub shell: Option<ShellSection>,
}

impl Config {
    pub fn new(daemon: DaemonConfig, defaults: DefaultsConfig, shells: Vec<ShellConfig>) -> Self {
        Self { daemon, defaults, shells, hermytt: None, shell: None }
    }
}

#[derive(Debug, Deserialize)]
pub struct HermyttSection {
    url: Option<String>,
    token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ShellSection {
    default: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Address to announce to Hermytt registry. If unset, uses listen address.
    pub advertise: Option<String>,
    #[serde(default = "default_hermytt_url")]
    pub hermytt_url: String,
    #[serde(default)]
    pub hermytt_key: String,
    pub max_shells: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct DefaultsConfig {
    #[serde(default = "default_shell")]
    pub shell: String,
    #[serde(default = "default_scrollback")]
    pub scrollback: usize,
}

#[derive(Debug, Deserialize)]
pub struct ShellConfig {
    pub name: String,
    pub shell: Option<String>,
    pub cwd: Option<String>,
    pub host: Option<String>,
    pub key: Option<String>,
    pub agent: Option<String>,
    pub project: Option<String>,
    pub cmd: Option<String>,
    #[serde(default)]
    pub autostart: bool,
}

fn default_listen() -> String { "127.0.0.1:7778".into() }
fn default_hermytt_url() -> String { "http://localhost:7777".into() }
fn default_shell() -> String { "/bin/zsh".into() }
fn default_scrollback() -> usize { 10000 }

impl Default for DaemonConfig {
    fn default() -> Self {
        Self { listen: default_listen(), advertise: None, hermytt_url: default_hermytt_url(), hermytt_key: String::new(), max_shells: None }
    }
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self { shell: default_shell(), scrollback: default_scrollback() }
    }
}

impl Config {
    pub fn load(path: Option<PathBuf>) -> Result<Self, Error> {
        let path = path.unwrap_or_else(|| config_dir().join("shytti.toml"));

        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            let mut config: Config = toml::from_str(&content)?;
            // Merge [hermytt] section into daemon config (bootstrap compat)
            if let Some(h) = config.hermytt.take() {
                if let Some(url) = h.url {
                    config.daemon.hermytt_url = url;
                }
                if let Some(token) = h.token {
                    config.daemon.hermytt_key = token;
                }
            }
            // Merge [shell] section into defaults (bootstrap compat)
            if let Some(s) = config.shell.take() {
                if let Some(default) = s.default {
                    config.defaults.shell = default;
                }
            }
            tracing::info!(path = %path.display(), "loaded config");
            Ok(config)
        } else {
            tracing::info!("no config file, using defaults");
            Ok(Config {
                daemon: DaemonConfig::default(),
                defaults: DefaultsConfig::default(),
                shells: vec![],
                hermytt: None,
                shell: None,
            })
        }
    }
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_else(|| "/etc".into());
            PathBuf::from(home).join(".config")
        })
        .join("shytti")
}

impl From<&ShellConfig> for SpawnRequest {
    fn from(cfg: &ShellConfig) -> Self {
        SpawnRequest {
            name: Some(cfg.name.clone()),
            shell: cfg.shell.clone(),
            cwd: cfg.cwd.clone(),
            host: cfg.host.clone(),
            agent: cfg.agent.clone(),
            cmd: cfg.cmd.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/shytti_test_{name}_{}.toml", std::process::id()))
    }

    fn write_tmp(name: &str, content: &str) -> PathBuf {
        let p = tmp_path(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn load_none_no_file_returns_defaults() {
        // Use a path that doesn't exist
        let cfg = Config::load(Some(PathBuf::from("/tmp/shytti_nonexistent_config.toml"))).unwrap();
        assert_eq!(cfg.daemon.listen, "127.0.0.1:7778");
        assert_eq!(cfg.daemon.hermytt_url, "http://localhost:7777");
        assert!(cfg.shells.is_empty());
    }

    #[test]
    fn load_valid_toml_full() {
        let p = write_tmp("full", r#"
[daemon]
listen = "0.0.0.0:9999"
hermytt_url = "http://example.com:7777"
hermytt_key = "secret"

[defaults]
shell = "/bin/bash"
scrollback = 5000

[[shells]]
name = "infra"
cwd = "~/Developer/perso/infra"
autostart = true

[[shells]]
name = "polnareff"
host = "cali@10.10.0.7"
key = "~/.ssh/cali_net_rsa"
autostart = false
"#);
        let cfg = Config::load(Some(p.clone())).unwrap();
        std::fs::remove_file(&p).ok();

        assert_eq!(cfg.daemon.listen, "0.0.0.0:9999");
        assert_eq!(cfg.daemon.hermytt_url, "http://example.com:7777");
        assert_eq!(cfg.daemon.hermytt_key, "secret");
        assert_eq!(cfg.defaults.shell, "/bin/bash");
        assert_eq!(cfg.defaults.scrollback, 5000);
        assert_eq!(cfg.shells.len(), 2);
        assert_eq!(cfg.shells[0].name, "infra");
        assert!(cfg.shells[0].autostart);
        assert_eq!(cfg.shells[1].name, "polnareff");
        assert!(!cfg.shells[1].autostart);
    }

    #[test]
    fn load_minimal_toml_fills_defaults() {
        let p = write_tmp("minimal", "[daemon]\n");
        let cfg = Config::load(Some(p.clone())).unwrap();
        std::fs::remove_file(&p).ok();

        assert_eq!(cfg.daemon.listen, "127.0.0.1:7778");
        assert_eq!(cfg.daemon.hermytt_url, "http://localhost:7777");
        assert_eq!(cfg.defaults.shell, "/bin/zsh");
        assert_eq!(cfg.defaults.scrollback, 10000);
        assert!(cfg.shells.is_empty());
    }

    #[test]
    fn load_invalid_toml_returns_error() {
        let p = write_tmp("invalid", "this is not [[[valid toml");
        let result = Config::load(Some(p.clone()));
        std::fs::remove_file(&p).ok();
        assert!(result.is_err());
    }

    #[test]
    fn shell_config_to_spawn_request_preserves_fields() {
        let cfg = ShellConfig {
            name: "test".into(),
            shell: Some("/bin/bash".into()),
            cwd: Some("~/work".into()),
            host: Some("user@host".into()),
            key: Some("~/.ssh/id_rsa".into()),
            agent: Some("infra".into()),
            project: Some("~/proj".into()),
            cmd: Some("ls -la".into()),
            autostart: true,
        };
        let req = SpawnRequest::from(&cfg);
        assert_eq!(req.name, Some("test".into()));
        assert_eq!(req.shell, Some("/bin/bash".into()));
        assert_eq!(req.cwd, Some("~/work".into()));
        assert_eq!(req.host, Some("user@host".into()));
        assert_eq!(req.agent, Some("infra".into()));
        assert_eq!(req.cmd, Some("ls -la".into()));
    }

    #[test]
    fn hermytt_section_overrides_daemon() {
        let p = write_tmp("hermytt_compat", r#"
[hermytt]
url = "http://10.10.0.3:7777"
token = "d83c76d70e0847cf9bc6db0720e8faed"
"#);
        let cfg = Config::load(Some(p.clone())).unwrap();
        std::fs::remove_file(&p).ok();

        assert_eq!(cfg.daemon.hermytt_url, "http://10.10.0.3:7777");
        assert_eq!(cfg.daemon.hermytt_key, "d83c76d70e0847cf9bc6db0720e8faed");
    }

    #[test]
    fn hermytt_section_with_daemon_section() {
        let p = write_tmp("both_sections", r#"
[daemon]
listen = "0.0.0.0:7778"

[hermytt]
url = "http://10.10.0.3:7777"
token = "mytoken"
"#);
        let cfg = Config::load(Some(p.clone())).unwrap();
        std::fs::remove_file(&p).ok();

        assert_eq!(cfg.daemon.listen, "0.0.0.0:7778");
        assert_eq!(cfg.daemon.hermytt_url, "http://10.10.0.3:7777");
        assert_eq!(cfg.daemon.hermytt_key, "mytoken");
    }

    #[test]
    fn shell_section_overrides_default_shell() {
        let p = write_tmp("shell_section", r#"
[hermytt]
url = "http://10.10.0.3:7777"
token = "abc"

[shell]
default = "/bin/fish"
"#);
        let cfg = Config::load(Some(p.clone())).unwrap();
        std::fs::remove_file(&p).ok();

        assert_eq!(cfg.defaults.shell, "/bin/fish");
    }

    #[test]
    fn shell_section_bootstrap_compat() {
        // Exact format Hermytt bootstrap writes
        let p = write_tmp("bootstrap_real", r#"
[hermytt]
url = "http://10.10.0.3:7777"
token = "d83c76d70e0847cf9bc6db0720e8faed"

[shell]
default = "/bin/zsh"
"#);
        let cfg = Config::load(Some(p.clone())).unwrap();
        std::fs::remove_file(&p).ok();

        assert_eq!(cfg.defaults.shell, "/bin/zsh");
        assert_eq!(cfg.daemon.hermytt_url, "http://10.10.0.3:7777");
    }

    #[test]
    fn shells_entries_parse() {
        let p = write_tmp("shells", r#"
[[shells]]
name = "one"
cmd = "echo 1"
autostart = true

[[shells]]
name = "two"
host = "user@host"
autostart = false
"#);
        let cfg = Config::load(Some(p.clone())).unwrap();
        std::fs::remove_file(&p).ok();

        assert_eq!(cfg.shells.len(), 2);
        assert_eq!(cfg.shells[0].cmd.as_deref(), Some("echo 1"));
        assert_eq!(cfg.shells[1].host.as_deref(), Some("user@host"));
    }
}
