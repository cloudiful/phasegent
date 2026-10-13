use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
pub struct Repo {
    pub root: PathBuf,
}
impl Repo {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "phasegent-hooks-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let repo = Self { root };
        repo.git(&["init", "-b", "main"]);
        repo.git(&["config", "user.name", "Hook Test"]);
        repo.git(&["config", "user.email", "hook@example.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.git(&["commit", "--allow-empty", "-m", "baseline"]);
        repo.git(&["checkout", "-b", "feat/9"]);
        repo
    }
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .current_dir(&self.root)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    Path::new(env!("CARGO_BIN_EXE_phasegent"))
                        .parent()
                        .unwrap()
                        .display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("PHASEGENT_DB_PATH", self.root.join("runtime.sqlite3"))
            .env("PHASEGENT_LOCAL_DB_PATH", self.root.join("local.sqlite3"))
            .env("PHASEGENT_INDEX_DB_PATH", self.root.join("index.sqlite3"))
            .env("PHASEGENT_CONFIG_PATH", self.root.join("phasegent.toml"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no");
        cmd
    }
    pub fn git_output(&self, args: &[&str]) -> Output {
        self.command("git").args(args).output().unwrap()
    }
    pub fn git(&self, args: &[&str]) -> Output {
        let out = self.git_output(args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
    pub fn cli(&self, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_phasegent"))
            .args(args)
            .output()
            .unwrap()
    }
    pub fn install(&self) {
        let out = self.cli(&["hooks", "install"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    pub fn write(&self, path: &str, bytes: &[u8]) {
        std::fs::write(self.root.join(path), bytes).unwrap();
    }
    pub fn read(&self, path: &str) -> Vec<u8> {
        std::fs::read(self.root.join(path)).unwrap()
    }
    pub fn executable(&self, path: &str, bytes: &[u8]) {
        use std::os::unix::fs::PermissionsExt;
        self.write(path, bytes);
        std::fs::set_permissions(self.root.join(path), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    pub fn hook(&self, hook: &str, source: Option<&str>) -> Output {
        let mut args = vec!["hooks", "run", hook, "message"];
        if let Some(source) = source {
            args.push(source);
        }
        self.cli(&args)
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
