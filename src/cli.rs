use std::path::PathBuf;

pub enum Command {
    Daemon { config: Option<PathBuf> },
    Spawn {
        name: Option<String>,
        shell: Option<String>,
        cwd: Option<String>,
        host: Option<String>,
        agent: Option<String>,
        cmd: Option<String>,
    },
    List,
    Kill { id: String },
    Pair { config: Option<PathBuf> },
}

pub fn parse() -> Command {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(|s| s.as_str()) {
        Some("daemon") => {
            let config = flag(&args, "--config").or_else(|| flag(&args, "-c")).map(PathBuf::from);
            Command::Daemon { config }
        }
        Some("spawn") => Command::Spawn {
            name: flag(&args, "--name").or_else(|| flag(&args, "-n")),
            shell: flag(&args, "--shell"),
            cwd: flag(&args, "--cwd"),
            host: flag(&args, "--host"),
            agent: flag(&args, "--agent"),
            cmd: flag(&args, "--cmd"),
        },
        Some("list" | "ls") => Command::List,
        Some("pair") => {
            let config = flag(&args, "--config").or_else(|| flag(&args, "-c")).map(PathBuf::from);
            Command::Pair { config }
        }
        Some("kill") => {
            let id = args.get(1).cloned().unwrap_or_else(|| {
                eprintln!("usage: shytti kill <id>");
                std::process::exit(1);
            });
            Command::Kill { id }
        }
        // No subcommand → daemon
        None => {
            let config = flag(&args, "--config").or_else(|| flag(&args, "-c")).map(PathBuf::from);
            Command::Daemon { config }
        }
        // First arg is a flag (not a subcommand) → daemon with flags
        Some(other) if other.starts_with('-') => {
            let config = flag(&args, "--config").or_else(|| flag(&args, "-c")).map(PathBuf::from);
            Command::Daemon { config }
        }
        Some(other) => {
            eprintln!("unknown command: {other}");
            eprintln!("usage: shytti [daemon|spawn|list|kill|pair]");
            std::process::exit(1);
        }
    }
}

pub(crate) fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn flag_finds_value() {
        let a = args(&["spawn", "--name", "foo", "--shell", "/bin/bash"]);
        assert_eq!(flag(&a, "--name"), Some("foo".into()));
    }

    #[test]
    fn flag_missing_returns_none() {
        let a = args(&["spawn", "--shell", "/bin/bash"]);
        assert_eq!(flag(&a, "--name"), None);
    }

    #[test]
    fn flag_at_end_no_value_returns_none() {
        let a = args(&["spawn", "--name"]);
        assert_eq!(flag(&a, "--name"), None);
    }
}
