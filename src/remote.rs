use std::process::Command;
use url::{Host, Url};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteRepository {
    pub api_base: String,
    pub repository: String,
    /// Original git remote URL with credentials stripped. Used by phase 2
    /// to register the Redmine project repository when no explicit override
    /// is configured; the value never contains credentials from the input.
    pub repository_url: String,
}

pub fn resolve_origin() -> Result<RemoteRepository, String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(|error| format!("could not inspect git remote: {error}"))?;
    if !output.status.success() {
        return Err("origin git remote is not configured".to_owned());
    }
    let remote = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    parse_remote(&remote)
}

/// The origin's host, deployment prefix and repository as evidence for the
/// trusted Forgejo instance. The derived API base is never used for trust or
/// as the request endpoint; only these normalized parts are compared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OriginRepositoryEvidence {
    pub host: String,
    pub prefix: String,
    pub repository: String,
}

/// Read the current checkout's `origin` as Forgejo attestation evidence.
/// Every configured origin URL is enumerated: zero or several URLs are an
/// error, so an unbound selection inside Git fails closed instead of guessing
/// the first one.
pub(crate) fn forgejo_origin_evidence() -> Result<OriginRepositoryEvidence, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    forgejo_origin_evidence_at(&cwd)
}

/// The same evidence read from one named checkout.
///
/// The candidate's own repository is proven, never the process working
/// directory, so a prune candidate in another repository is judged by its own
/// origin.
pub(crate) fn forgejo_origin_evidence_at(
    path: &std::path::Path,
) -> Result<OriginRepositoryEvidence, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["remote", "get-url", "--all", "origin"])
        .output()
        .map_err(|error| format!("could not inspect git remote: {error}"))?;
    if !output.status.success() {
        return Err("origin git remote is not configured".to_owned());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let urls: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    match urls.as_slice() {
        [] => Err("origin git remote has no URL".to_owned()),
        [remote] => parse_origin_evidence(remote),
        _ => Err(format!(
            "origin git remote has {} URLs; provide --repository to name a repository on the \
             trusted instance",
            urls.len()
        )),
    }
}

fn parse_origin_evidence(remote: &str) -> Result<OriginRepositoryEvidence, String> {
    let (host, path) = if let Some(value) = remote.strip_prefix("git@") {
        let (host, path) = value
            .split_once(':')
            .ok_or_else(|| "invalid scp-style git remote".to_owned())?;
        (host.to_ascii_lowercase(), path.to_owned())
    } else {
        let url = Url::parse(remote).map_err(|error| format!("invalid git remote: {error}"))?;
        if !matches!(
            url.scheme(),
            "http" | "https" | "ssh" | "git+ssh" | "ssh+git"
        ) {
            return Err(format!("unsupported git remote scheme '{}'", url.scheme()));
        }
        let host = host_authority(&url)?;
        (host.to_ascii_lowercase(), url.path().to_owned())
    };
    let repository = repository_from_path(&path)?;
    let prefix = deployment_prefix(&path)?;
    Ok(OriginRepositoryEvidence {
        host,
        prefix,
        repository,
    })
}

/// The origin's host and full path as evidence for a trusted GitLab instance.
/// The deployment prefix is not split off here because only the trusted
/// instance root can say where a prefix ends and a subgroup begins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GitlabOriginEvidence {
    pub host: String,
    pub path: String,
}

pub(crate) fn gitlab_origin_evidence() -> Result<GitlabOriginEvidence, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    gitlab_origin_evidence_at(&cwd)
}

pub(crate) fn gitlab_origin_evidence_at(
    path: &std::path::Path,
) -> Result<GitlabOriginEvidence, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["remote", "get-url", "--all", "origin"])
        .output()
        .map_err(|error| format!("could not inspect git remote: {error}"))?;
    if !output.status.success() {
        return Err("origin git remote is not configured".to_owned());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let urls: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    match urls.as_slice() {
        [] => Err("origin git remote has no URL".to_owned()),
        [remote] => parse_gitlab_origin(remote),
        _ => Err(format!(
            "origin git remote has {} URLs; provide --repository to name a project path on the \
             trusted instance",
            urls.len()
        )),
    }
}

fn parse_gitlab_origin(remote: &str) -> Result<GitlabOriginEvidence, String> {
    let (host, path) = if let Some(value) = remote.strip_prefix("git@") {
        let (host, path) = value
            .split_once(':')
            .ok_or_else(|| "invalid scp-style git remote".to_owned())?;
        require_ssh_user("git")?;
        (host.to_ascii_lowercase(), path.to_owned())
    } else {
        let url = Url::parse(remote).map_err(|error| format!("invalid git remote: {error}"))?;
        let scheme = url.scheme();
        if !matches!(scheme, "http" | "https" | "ssh" | "git+ssh" | "ssh+git") {
            return Err(format!("unsupported git remote scheme '{scheme}'"));
        }
        if is_ssh_like_scheme(scheme) {
            require_ssh_user(url.username())?;
        } else if !url.username().is_empty() || url.password().is_some() {
            return Err("GitLab HTTPS git remote must not contain credentials".to_owned());
        }
        if url.password().is_some() {
            return Err("GitLab git remote must not contain a password".to_owned());
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err("GitLab git remote must not contain a query or fragment".to_owned());
        }
        let host = host_authority(&url)?;
        (host.to_ascii_lowercase(), url.path().to_owned())
    };
    let path = normalize_gitlab_origin_path(&path)?;
    if path.split('/').count() < 2 {
        return Err("git remote must identify a namespace and project".to_owned());
    }
    Ok(GitlabOriginEvidence { host, path })
}

fn require_ssh_user(user: &str) -> Result<(), String> {
    if user.is_empty() || user.eq_ignore_ascii_case("git") {
        Ok(())
    } else {
        Err("GitLab SSH git remote user must be 'git'".to_owned())
    }
}

fn normalize_gitlab_origin_path(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim_matches('/');
    let without_git = if trimmed
        .as_bytes()
        .get(trimmed.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(b".git"))
    {
        &trimmed[..trimmed.len() - 4]
    } else {
        trimmed
    };
    let without_git = without_git.trim_matches('/');
    let parts: Vec<&str> = without_git.split('/').collect();
    if parts.is_empty()
        || parts.iter().any(|part| {
            part.is_empty()
                || *part == "."
                || *part == ".."
                || part.starts_with('.')
                || part.ends_with('.')
                || !part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
                })
        })
    {
        return Err("git remote path is not a canonical project path".to_owned());
    }
    Ok(parts.join("/"))
}

pub fn parse_remote(remote: &str) -> Result<RemoteRepository, String> {
    let (host, path): (String, String) = if let Some(value) = remote.strip_prefix("git@") {
        value
            .split_once(':')
            .map(|(host, path)| (host.to_owned(), path.to_owned()))
            .ok_or_else(|| "invalid scp-style git remote".to_owned())?
    } else {
        let url = Url::parse(remote).map_err(|error| format!("invalid git remote: {error}"))?;
        url.host_str()
            .ok_or_else(|| "git remote has no host".to_owned())?;
        let repository = repository_from_path(url.path())?;
        let api_base = api_base_from_url(&url, url.path())?;
        let repository_url = credential_free_remote_url(&url, remote)?;
        return Ok(RemoteRepository {
            api_base,
            repository,
            repository_url,
        });
    };

    let repository = repository_from_path(&path)?;
    let prefix = deployment_prefix(&path)?;
    Ok(RemoteRepository {
        api_base: format!("https://{host}{prefix}/api/v1"),
        repository,
        repository_url: format!("ssh://git@{host}/{path}"),
    })
}

fn api_base_from_url(url: &Url, path: &str) -> Result<String, String> {
    let authority = host_authority(url)?;
    let prefix = deployment_prefix(path)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Ok(format!("https://{authority}{prefix}/api/v1"));
    }
    let mut api = url.clone();
    api.set_username("")
        .map_err(|_| "could not remove git remote username".to_owned())?;
    api.set_password(None)
        .map_err(|_| "could not remove git remote password".to_owned())?;
    api.set_path(&format!("{prefix}/api/v1"));
    api.set_query(None);
    api.set_fragment(None);
    match url.scheme() {
        "http" | "https" => {}
        _ => unreachable!("non-HTTP schemes returned above"),
    }
    Ok(api.to_string().trim_end_matches('/').to_owned())
}

fn host_authority(url: &Url) -> Result<String, String> {
    match url.host() {
        Some(Host::Domain(host)) => Ok(host.to_owned()),
        Some(Host::Ipv4(host)) => Ok(host.to_string()),
        Some(Host::Ipv6(host)) => Ok(format!("[{host}]")),
        None => Err("git remote has no host".to_owned()),
    }
}

/// Build a representation of the remote URL with credentials removed. The
/// returned value is suitable for passing to Redmine as `repository[url]`
/// without leaking any embedded password or HTTP(S) username. If the source
/// URL carries no credentials, query, or fragment, the original URL is
/// returned unchanged.
///
/// SSH-like schemes (`ssh://`, `git+ssh://`, `ssh+git://`) keep their
/// username — the `git@host` segment is required by the SSH transport and
/// is not a credential — but any embedded password, query string, or
/// fragment is still dropped so the mirror plugin receives a clean URL.
fn credential_free_remote_url(url: &Url, raw: &str) -> Result<String, String> {
    let ssh_like = is_ssh_like_scheme(url.scheme());
    let has_password = url.password().is_some();
    let has_username = !url.username().is_empty();
    let has_query = url.query().is_some();
    let has_fragment = url.fragment().is_some();

    if !has_password && !has_username && !has_query && !has_fragment {
        return Ok(raw.trim().to_owned());
    }

    let mut sanitized = url.clone();
    if ssh_like {
        if has_password {
            sanitized
                .set_password(None)
                .map_err(|_| "could not strip git remote password".to_owned())?;
        }
    } else {
        sanitized
            .set_username("")
            .map_err(|_| "could not strip git remote username".to_owned())?;
        sanitized
            .set_password(None)
            .map_err(|_| "could not strip git remote password".to_owned())?;
    }
    sanitized.set_query(None);
    sanitized.set_fragment(None);
    Ok(sanitized.to_string())
}

fn is_ssh_like_scheme(scheme: &str) -> bool {
    matches!(scheme, "ssh" | "git+ssh" | "ssh+git")
}

fn repository_from_path(path: &str) -> Result<String, String> {
    let parts = path_parts(path)?;
    let owner = parts[parts.len() - 2];
    let repository = parts[parts.len() - 1];
    Ok(format!("{owner}/{repository}"))
}

fn deployment_prefix(path: &str) -> Result<String, String> {
    let parts = path_parts(path)?;
    let prefix = &parts[..parts.len() - 2];
    if prefix.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("/{}", prefix.join("/")))
    }
}

fn path_parts(path: &str) -> Result<Vec<&str>, String> {
    let parts: Vec<_> = path
        .trim_matches('/')
        .trim_end_matches(".git")
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() < 2 || parts.iter().any(|part| part.is_empty()) {
        return Err("git remote must identify an owner and repository".to_owned());
    }
    Ok(parts)
}

pub fn validate_repository(value: &str) -> Result<String, String> {
    let parts: Vec<_> = value.split('/').collect();
    if parts.len() != 2
        || parts
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
    {
        return Err("repository must use OWNER/REPOSITORY form".to_owned());
    }
    Ok(value.to_owned())
}

pub fn redmine_identifier(repository: &str) -> Result<String, String> {
    let repository = validate_repository(repository)?;
    let mut components = repository.split('/').map(normalize_identifier_component);
    let owner = components.next().unwrap_or_default();
    let repository = components.next().unwrap_or_default();
    let owner = if owner.is_empty() { "owner" } else { &owner };
    let repository = if repository.is_empty() {
        "repository"
    } else {
        &repository
    };
    let mut identifier = format!("{owner}-{repository}");
    if !identifier
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_lowercase)
    {
        identifier.insert_str(0, "wf-");
    }
    if identifier.len() > 100 {
        identifier.truncate(100);
        while identifier.ends_with('-') || identifier.ends_with('_') {
            identifier.pop();
        }
    }
    Ok(identifier)
}

fn normalize_identifier_component(component: &str) -> String {
    let mut normalized = String::new();
    let mut previous_separator = false;
    for character in component.chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
            previous_separator = false;
        } else if !previous_separator {
            normalized.push(if character == '_' { '_' } else { '-' });
            previous_separator = true;
        }
    }
    while normalized.ends_with('-') || normalized.ends_with('_') {
        normalized.pop();
    }
    while normalized.starts_with('-') || normalized.starts_with('_') {
        normalized.remove(0);
    }
    normalized
}

pub fn normalize_redmine_api_base(value: &str) -> Result<String, String> {
    let mut url =
        Url::parse(value).map_err(|error| format!("invalid Redmine API base URL: {error}"))?;
    if url.host_str().is_none() || !matches!(url.scheme(), "http" | "https") {
        return Err("Redmine API base URL must use http or https".to_owned());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("Redmine API base URL cannot contain a query or fragment".to_owned());
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

/// Canonical Git URL identity for cross-transport comparison.
///
/// Returns a normalized `host[:port]/path` string where host is lower-cased,
/// port is preserved only when non-default, and credentials, query,
/// fragment, and trailing `.git` are removed. Scheme is ignored so
/// `ssh://git@host/owner/repo.git` and `https://host/owner/repo.git` match
/// while meaningful non-default ports and case-sensitive repository paths are
/// preserved. Used by Redmine project discovery to match the local origin
/// against the plugin's `remote_url` without leaking secrets. The original
/// credential-free URL is kept separately for later mirror registration.
pub fn canonical_git_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("git remote URL cannot be empty".to_owned());
    }
    if let Some(rest) = trimmed.strip_prefix("git@") {
        let (host, path) = rest
            .split_once(':')
            .ok_or_else(|| "invalid scp-style git remote".to_owned())?;
        if host.trim().is_empty() || path.trim().is_empty() {
            return Err("invalid scp-style git remote".to_owned());
        }
        let host = host.trim().to_ascii_lowercase();
        let normalized = normalize_git_path(path)?;
        return Ok(format!("{host}/{normalized}"));
    }
    let parsed = Url::parse(trimmed).map_err(|error| format!("invalid git remote: {error}"))?;
    let host = parsed
        .host()
        .ok_or_else(|| "git remote has no host".to_owned())?;
    let host_str = match host {
        Host::Domain(domain) => domain.to_ascii_lowercase(),
        Host::Ipv4(addr) => addr.to_string(),
        Host::Ipv6(addr) => format!("[{addr}]").to_ascii_lowercase(),
    };
    let mut port = parsed.port();
    if matches!(parsed.scheme(), "ssh" | "git+ssh" | "ssh+git") && port == Some(22) {
        port = None;
    }
    let normalized = normalize_git_path(parsed.path())?;
    if let Some(port) = port {
        Ok(format!("{host_str}:{port}/{normalized}"))
    } else {
        Ok(format!("{host_str}/{normalized}"))
    }
}

fn normalize_git_path(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim_matches('/');
    if trimmed.is_empty() {
        return Err("git remote path is empty".to_owned());
    }
    let without_git =
        if trimmed.len() >= 4 && trimmed[trimmed.len() - 4..].eq_ignore_ascii_case(".git") {
            &trimmed[..trimmed.len() - 4]
        } else {
            trimmed
        };
    let without_git = without_git.trim_matches('/');
    if without_git.is_empty() {
        return Err("git remote path is empty after stripping .git".to_owned());
    }
    let parts: Vec<&str> = without_git
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return Err("git remote path is empty".to_owned());
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod gitlab_origin_tests {
    use super::parse_gitlab_origin;

    #[test]
    fn gitlab_origin_invalid_unicode_path_returns_error_without_panicking() {
        assert!(
            std::panic::catch_unwind(|| parse_gitlab_origin("git@gitlab.example.com:a/中中"))
                .is_ok()
        );
    }

    #[test]
    fn gitlab_origin_accepts_safe_transports_and_refuses_credentials() {
        for (remote, host, path) in [
            (
                "git@GitLab.Example.com:group/sub/project.git",
                "gitlab.example.com",
                "group/sub/project",
            ),
            (
                "https://gitlab.example.com/group/sub/project.git",
                "gitlab.example.com",
                "group/sub/project",
            ),
            (
                "ssh://git@gitlab.example.com:2222/group/project.git",
                "gitlab.example.com",
                "group/project",
            ),
            (
                "https://gitlab.example.com/deploy/group/project",
                "gitlab.example.com",
                "deploy/group/project",
            ),
        ] {
            let evidence = parse_gitlab_origin(remote).unwrap();
            assert_eq!(evidence.host, host, "{remote}");
            assert_eq!(evidence.path, path, "{remote}");
        }

        for remote in [
            "https://user:secret@gitlab.example.com/group/project.git",
            "https://user@gitlab.example.com/group/project.git",
            "ssh://deploy@gitlab.example.com/group/project.git",
            "https://gitlab.example.com/group/project?private=1",
            "https://gitlab.example.com/group/project#fragment",
            "https://gitlab.example.com/project.git",
            "https://gitlab.example.com/group/../project.git",
            "ftp://gitlab.example.com/group/project.git",
            "git@gitlab.example.com:project.git",
        ] {
            assert!(parse_gitlab_origin(remote).is_err(), "{remote}");
        }
    }
}
