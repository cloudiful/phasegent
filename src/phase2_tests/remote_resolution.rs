use super::*;

/// `remote::parse_remote` normalisation contract, parameterised over the
/// HTTPS, URL-form SSH, and scp-style SSH remotes that reach it: HTTPS keeps
/// its non-default port in `api_base` and drops embedded credentials, SSH
/// keeps the transport user (required to clone) and its non-default port, an
/// SSH port never leaks into `api_base`, and query/fragment are always
/// stripped without ever leaking a credential.
#[test]
fn remote_resolution_normalises_https_and_ssh_forms_without_credentials() {
    let cases = [
        (
            "https://forgejo.example:8443/owner/widgets.git",
            "owner/widgets",
            "https://forgejo.example:8443/api/v1",
            "https://forgejo.example:8443/owner/widgets.git",
        ),
        (
            "ssh://git@forgejo.example:2222/owner/widgets.git",
            "owner/widgets",
            "https://forgejo.example/api/v1",
            "ssh://git@forgejo.example:2222/owner/widgets.git",
        ),
        (
            "https://forgejo.example/forgejo/owner/widgets.git",
            "owner/widgets",
            "https://forgejo.example/forgejo/api/v1",
            "https://forgejo.example/forgejo/owner/widgets.git",
        ),
        (
            "git@forgejo.example:owner/widgets.git",
            "owner/widgets",
            "https://forgejo.example/api/v1",
            "ssh://git@forgejo.example/owner/widgets.git",
        ),
        (
            "https://deploy:supersecret@forgejo.example/owner/widgets.git",
            "owner/widgets",
            "https://forgejo.example/api/v1",
            "https://forgejo.example/owner/widgets.git",
        ),
        (
            "ssh://git@forgejo.example.com:2222/owner/repo.git",
            "owner/repo",
            "https://forgejo.example.com/api/v1",
            "ssh://git@forgejo.example.com:2222/owner/repo.git",
        ),
        (
            "ssh://deploy@git.example.com/owner/repo.git",
            "owner/repo",
            "https://git.example.com/api/v1",
            "ssh://deploy@git.example.com/owner/repo.git",
        ),
        (
            "ssh://git@git.example.com/owner/repo.git?ref=main#frag",
            "owner/repo",
            "https://git.example.com/api/v1",
            "ssh://git@git.example.com/owner/repo.git",
        ),
        (
            "https://deploy:supersecret@forgejo.example/owner/repo.git",
            "owner/repo",
            "https://forgejo.example/api/v1",
            "https://forgejo.example/owner/repo.git",
        ),
    ];
    for (input, repository, api_base, repository_url) in cases {
        let parsed = remote::parse_remote(input).unwrap_or_else(|error| panic!("{input}: {error}"));
        assert_eq!(parsed.repository, repository, "{input}");
        assert_eq!(parsed.api_base, api_base, "{input}");
        assert_eq!(parsed.repository_url, repository_url, "{input}");
        assert!(
            !parsed.repository_url.contains("supersecret"),
            "credentials must never survive normalisation: {input}"
        );
    }
}

#[test]
fn canonical_git_url_strips_credentials_query_fragment_and_git_suffix() {
    // Credentials, query, fragment, and trailing .git must not affect
    // the canonical identity so the same repository behind different
    // transports still matches.
    let a = crate::remote::canonical_git_url(
        "https://user:secret@git.example.com/owner/repo.git?ref=main#frag",
    )
    .unwrap();
    let b = crate::remote::canonical_git_url("https://git.example.com/owner/repo").unwrap();
    assert_eq!(a, b);
    assert_eq!(a, "git.example.com/owner/repo");
    assert!(crate::remote::git_urls_match(
        "https://user:secret@git.example.com/owner/repo.git?ref=main#frag",
        "https://git.example.com/owner/repo"
    ));
}

#[test]
fn canonical_git_url_supports_ssh_https_equivalence_and_preserves_port_and_case() {
    // SSH and HTTPS forms for the same host/path must be equivalent
    // (scheme ignored), but non-default ports and case-sensitive paths
    // are preserved and distinguish repositories.
    assert!(crate::remote::git_urls_match(
        "ssh://git@git.example.com/owner/repo.git",
        "https://git.example.com/owner/repo.git"
    ));
    assert!(crate::remote::git_urls_match(
        "git@git.example.com:owner/repo.git",
        "https://git.example.com/owner/repo"
    ));
    // Non-default port must be preserved: different ports are not equal.
    let with_port =
        crate::remote::canonical_git_url("https://git.example.com:8443/owner/repo.git").unwrap();
    let without_port =
        crate::remote::canonical_git_url("https://git.example.com/owner/repo.git").unwrap();
    assert_ne!(with_port, without_port);
    assert!(with_port.contains(":8443"));
    // Same non-default port on different schemes still matches.
    assert!(crate::remote::git_urls_match(
        "https://git.example.com:8443/owner/repo.git",
        "ssh://git@git.example.com:8443/owner/repo.git"
    ));
    // Host is case-insensitive, path is case-sensitive.
    assert!(crate::remote::git_urls_match(
        "https://GIT.EXAMPLE.COM/owner/repo.git",
        "https://git.example.com/owner/repo.git"
    ));
    assert!(!crate::remote::git_urls_match(
        "https://git.example.com/Owner/Repo.git",
        "https://git.example.com/owner/repo.git"
    ));
    // Deployment prefix in the path is part of the identity.
    let prefixed =
        crate::remote::canonical_git_url("https://git.example.com/prefix/owner/repo.git").unwrap();
    assert_eq!(prefixed, "git.example.com/prefix/owner/repo");
    assert!(!crate::remote::git_urls_match(
        "https://git.example.com/prefix/owner/repo.git",
        "https://git.example.com/owner/repo.git"
    ));
}
