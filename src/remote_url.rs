//! A remote's URL, made fit to be shown: the remote a card names, the page
//! `o` opens, and an address in what git said when a fetch failed.
//!
//! A URL can carry whoever it logs in as, a token included, and each of these
//! ends up somewhere that keeps it: on screen, in the log a `--json` run is
//! piped to, in a browser's history. So all of them go through [`login`], the
//! one place that says where a login ends. It errs on the side of taking out
//! too much.

/// Where the login in `url` is: from just after the scheme's `://`, or from
/// the start of a remote written as scp writes one (`who@host:path`), to just
/// after the last `@` there is.
///
/// The last `@`, and not the first, nor the last before the first `/`. A login
/// is not supposed to hold either unencoded, and one that does is configured
/// all the same: `https://me:p@ss@host/x` and `https://me:a/b@host/x` both
/// reach a remote that way, or fail to with the URL in git's error. Cut at the
/// first `@`, or ended at the first `/`, what was shown held the rest of the
/// password. An `@` in the path is taken for the end of a login as well, and
/// the path loses what comes before it. That is the side to fail on.
///
/// A path on disk has no login. git takes a remote for one when it has no
/// colon, or a slash before the first.
fn login(url: &str) -> Option<std::ops::Range<usize>> {
    let start = match url.find("://") {
        Some(at) => at + "://".len(),
        None => {
            let colon = url.find(':')?;
            if url[..colon].contains('/') {
                return None;
            }
            0
        }
    };
    let end = start + url[start..].rfind('@')? + 1;
    Some(start..end)
}

/// `url` without whoever it logs in as: see [`login`].
fn without_login(url: &str) -> String {
    match login(url) {
        Some(login) => format!("{}{}", &url[..login.start], &url[login.end..]),
        None => url.to_string(),
    }
}

/// `text` with the login taken out of every address in it.
///
/// For what git said when a fetch failed, which names the remote. git leaves
/// the login out of most of what it prints, and not out of an address it
/// cannot make sense of, which a password with a `/` in it makes of one. An
/// address is taken to run from one space or quote to the next, and every
/// word with an `@` in it is held to [`login`]. One that is not an address,
/// such as an email, is left alone, having no colon; `git@host:` in "git@host:
/// Permission denied" loses its `git@`, which is nobody's secret.
pub fn without_logins_in(text: &str) -> String {
    let ends = |c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '<' | '>');
    let mut out = String::with_capacity(text.len());
    for piece in text.split_inclusive(ends) {
        let word = piece.strip_suffix(ends).unwrap_or(piece);
        if word.contains('@') {
            out.push_str(&without_login(word));
            out.push_str(&piece[word.len()..]);
        } else {
            out.push_str(piece);
        }
    }
    out
}

/// What a card calls a remote: `git@github.com:owner/repo.git` and
/// `https://me:token@host/owner/repo.git` both come to `host/owner/repo`.
pub fn tidy(url: &str) -> String {
    let url = without_login(url.trim());
    let url = url.trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    if let Some((_, rest)) = url.split_once("://") {
        return rest.to_string();
    }
    match url.split_once(':') {
        // `host:path`, as scp writes it. A colon after a slash is part of a
        // directory's name.
        Some((host, path)) if !host.contains('/') => format!("{host}/{path}"),
        _ => url.to_string(),
    }
}

/// Hosts that take git over ssh under one name and serve their pages under
/// another.
const SSH_ONLY_HOSTS: &[(&str, &str)] = &[
    ("ssh.github.com", "github.com"),
    ("altssh.gitlab.com", "gitlab.com"),
    ("altssh.bitbucket.org", "bitbucket.org"),
];

/// Where a remote's repository is on the web, for a browser to be sent to, or
/// nothing when the remote does not say.
///
/// A remote's URL is an address for git. That the page for people is at the
/// same host and path is only a convention, but it is one GitHub, GitLab,
/// Bitbucket, Codeberg, Gitea and sourcehut all keep, so that is what this
/// builds: `https://`, the host, the path, without `.git`.
///
/// What it leaves out matters as much:
///
/// - Whoever the URL logs in as, and above all a token written into an
///   `https://` remote. A browser keeps every address it is handed.
/// - The port of an ssh remote, which no web server is listening on. An
///   `http://` remote keeps its port and its `http`: there the remote is a
///   web address already, and the server's own.
///
/// And it answers nothing at all for a remote that is a directory or a
/// `file://` URL, for one that names no repository, and for an ssh remote
/// whose host has no dot in it. That last is a name out of `~/.ssh/config`:
/// which host it stands for is ssh's to know, and a guess would open somebody
/// else's page. Nor for one whose login cannot be told from the rest of it;
/// see below.
pub fn web_url(remote: &str) -> Option<String> {
    let url = remote.trim();
    // Nothing a browser, or whatever starts it, could read as two things.
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let scp = !url.contains("://");
    if let Some(login) = login(url) {
        let login = &url[login];
        // A slash in what is taken for the login is a password with one in
        // it, or a path with an `@` in it, and there is no telling which. In
        // a remote written as scp writes one, git takes whatever comes before
        // the first colon for the host, and a login with a colon in it puts
        // the host somewhere else than it seems to be. No page, rather than
        // the wrong one, or one with half a password in its path.
        if login.contains('/') || (scp && login.contains(':')) {
            return None;
        }
    }
    let url = without_login(url);
    let url = url.trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);

    // The scheme, where it is the web's already; host:port; the path.
    let (scheme, host, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            match scheme.to_ascii_lowercase().as_str() {
                "https" => (Some("https"), host, path),
                "http" => (Some("http"), host, path),
                "ssh" | "git" | "git+ssh" | "ssh+git" => (None, host, path),
                // `file://`, and anything else nobody browses.
                _ => return None,
            }
        }
        // `host:path`, the way scp writes it. Git takes a colon that comes
        // after a slash to be part of a directory's name, and so does this.
        None => {
            let (host, path) = url.split_once(':')?;
            if host.contains('/') {
                return None;
            }
            (None, host, path)
        }
    };
    let host = host.to_ascii_lowercase();
    let path = path.trim_matches('/');
    if host.is_empty() || path.is_empty() || host.starts_with("git-codecommit.") {
        return None;
    }

    let Some(scheme) = scheme else {
        let host = host.split_once(':').map_or(host.as_str(), |(host, _port)| host);
        let named = host.contains('.')
            && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        if !named {
            return None;
        }
        // Azure DevOps writes an ssh remote as `v3/organisation/project/
        // repository` and the page as something else again.
        if host == "ssh.dev.azure.com" || host == "vs-ssh.visualstudio.com" {
            let parts: Vec<&str> = path.split('/').collect();
            return match parts.as_slice() {
                ["v3", organisation, project, repository] => Some(format!(
                    "https://dev.azure.com/{organisation}/{project}/_git/{repository}"
                )),
                _ => None,
            };
        }
        let host = SSH_ONLY_HOSTS
            .iter()
            .find(|(ssh, _)| *ssh == host)
            .map_or(host, |(_, web)| web);
        return Some(format!("https://{host}/{path}"));
    };
    Some(format!("{scheme}://{host}/{path}"))
}

/// What to call the site a page from [`web_url`] is on: the name it goes by
/// where everybody knows it by one, and its host otherwise. A company's own
/// GitHub or GitLab gets its host, because "Open on GitHub" would name the
/// wrong place.
pub fn web_site(page: &str) -> &str {
    let rest = page.split_once("://").map_or(page, |(_, rest)| rest);
    let host = rest.split('/').next().unwrap_or(rest);
    let host = host
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(host, |(host, _port)| host);
    match host {
        "github.com" | "gist.github.com" => "GitHub",
        "gitlab.com" => "GitLab",
        "bitbucket.org" => "Bitbucket",
        "codeberg.org" => "Codeberg",
        "dev.azure.com" => "Azure DevOps",
        _ => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Remotes with a login in them, as they have been configured: written
    /// properly, and three ways that are not proper and still configured. A
    /// password with an `@` in it, a token put in front of a remote written
    /// as scp writes one, and a password with a `/` in it each showed some of
    /// the secret on the card, and the last in the page `o` opens.
    const LOGINS: &[&str] = &[
        "https://jm:ghp_SECRET0123456789@github.com/acme/widgets.git",
        "https://jm:p@SECRET@github.com/acme/widgets.git",
        "oauth2:glpat-SECRET@gitlab.com:group/repo.git",
        "https://deploy:abc/SECRET@example.invalid/x.git",
        "ssh://deploy:SECRET@git.example.org:2222/acme/widgets.git",
    ];

    #[test]
    fn a_card_never_names_a_remote_with_its_login() {
        for (remote, named) in [
            ("https://jm:ghp_SECRET0123456789@github.com/acme/widgets.git", "github.com/acme/widgets"),
            ("https://jm:p@SECRET@github.com/acme/widgets.git", "github.com/acme/widgets"),
            ("oauth2:glpat-SECRET@gitlab.com:group/repo.git", "gitlab.com/group/repo"),
            ("https://deploy:abc/SECRET@example.invalid/x.git", "example.invalid/x"),
            ("ssh://deploy:SECRET@git.example.org:2222/acme/widgets.git", "git.example.org:2222/acme/widgets"),
            // The everyday spellings, which never had a secret in them.
            ("https://github.com/acme/widgets.git", "github.com/acme/widgets"),
            ("git@github.com:acme/widgets.git", "github.com/acme/widgets"),
            ("ssh://git@github.com/acme/widgets.git", "github.com/acme/widgets"),
            ("https://acme@dev.azure.com/acme/Shop/_git/widgets", "dev.azure.com/acme/Shop/_git/widgets"),
            // A directory, which logs in as nobody, whatever is in its name.
            ("/srv/git/widgets.git", "/srv/git/widgets"),
            ("/srv/backups@2024/widgets.git", "/srv/backups@2024/widgets"),
            ("./with:colon/widgets", "./with:colon/widgets"),
            // An `@` in the path is taken for the end of a login, and costs
            // the path its front. Never the other way round.
            ("https://git.example.org/team@2024/widgets.git", "2024/widgets"),
        ] {
            assert_eq!(tidy(remote), named, "{remote}");
        }
        for remote in LOGINS {
            assert!(!tidy(remote).contains("SECRET"), "{remote}: {}", tidy(remote));
        }
    }

    #[test]
    fn a_remote_becomes_the_page_a_browser_can_open() {
        let cases: &[(&str, Option<&str>)] = &[
            // The everyday spellings of one repository.
            ("https://github.com/acme/widgets.git", Some("https://github.com/acme/widgets")),
            ("https://github.com/acme/widgets", Some("https://github.com/acme/widgets")),
            ("https://github.com/acme/widgets.git/", Some("https://github.com/acme/widgets")),
            ("git@github.com:acme/widgets.git", Some("https://github.com/acme/widgets")),
            ("ssh://git@github.com/acme/widgets.git", Some("https://github.com/acme/widgets")),
            ("git+ssh://git@github.com/acme/widgets.git", Some("https://github.com/acme/widgets")),
            ("git://git.example.org/acme/widgets.git", Some("https://git.example.org/acme/widgets")),
            ("  git@github.com:acme/widgets.git\n", Some("https://github.com/acme/widgets")),
            ("HTTPS://github.com/acme/widgets.git", Some("https://github.com/acme/widgets")),
            // A host is the same host however it is capitalised. A path is not.
            ("git@GitHub.com:Acme/Widgets.git", Some("https://github.com/Acme/Widgets")),
            // An ssh port is ssh's, and no web server is listening on it. An
            // http remote's port is the web server's own, and so is its
            // choice of http.
            (
                "ssh://git@git.example.org:2222/acme/widgets.git",
                Some("https://git.example.org/acme/widgets"),
            ),
            (
                "http://git.example.org:3000/acme/widgets.git",
                Some("http://git.example.org:3000/acme/widgets"),
            ),
            ("http://localhost:3000/acme/widgets.git", Some("http://localhost:3000/acme/widgets")),
            // Whoever the remote logs in as is not the browser's to be told,
            // a token least of all: a browser keeps what it is handed.
            (
                "https://jm:ghp_0123456789abcdef@github.com/acme/widgets.git",
                Some("https://github.com/acme/widgets"),
            ),
            (
                "https://jm:p@ss@github.com/acme/widgets.git",
                Some("https://github.com/acme/widgets"),
            ),
            (
                "https://acme@dev.azure.com/acme/Shop/_git/widgets",
                Some("https://dev.azure.com/acme/Shop/_git/widgets"),
            ),
            // A login that cannot be told from the rest: a slash in it, which
            // is a password with one or a path with an `@` in it; and a colon
            // in front of the host of a remote written as scp writes one,
            // where git looks for the host before that colon.
            ("https://deploy:abc/SECRET@example.invalid/x.git", None),
            ("https://git.example.org/team@2024/widgets.git", None),
            ("oauth2:glpat-SECRET@gitlab.com:group/repo.git", None),
            // Subgroups, a namespace with a tilde, a path from the root.
            (
                "git@gitlab.com:acme/platform/widgets.git",
                Some("https://gitlab.com/acme/platform/widgets"),
            ),
            ("git@git.sr.ht:~jm/widgets", Some("https://git.sr.ht/~jm/widgets")),
            ("git@git.example.org:/acme/widgets.git", Some("https://git.example.org/acme/widgets")),
            // Hosts that take ssh under one name and serve pages under another.
            (
                "git@ssh.dev.azure.com:v3/acme/Shop/widgets",
                Some("https://dev.azure.com/acme/Shop/_git/widgets"),
            ),
            (
                "acme@vs-ssh.visualstudio.com:v3/acme/Shop/widgets",
                Some("https://dev.azure.com/acme/Shop/_git/widgets"),
            ),
            ("git@ssh.dev.azure.com:acme/widgets", None),
            (
                "ssh://git@ssh.github.com:443/acme/widgets.git",
                Some("https://github.com/acme/widgets"),
            ),
            (
                "ssh://git@altssh.gitlab.com:443/acme/widgets.git",
                Some("https://gitlab.com/acme/widgets"),
            ),
            // A directory is not on the web, whatever it is called.
            ("/srv/git/widgets.git", None),
            ("../widgets", None),
            ("./with:colon/widgets", None),
            ("file:///srv/git/widgets.git", None),
            // A name out of ~/.ssh/config. Which host it stands for is ssh's
            // to know, and a guess would open somebody else's page.
            ("work:acme/widgets.git", None),
            ("git@github-work:acme/widgets.git", None),
            ("ssh://git@forge/acme/widgets.git", None),
            // Git is served here and nothing else is.
            ("https://git-codecommit.eu-north-1.amazonaws.com/v1/repos/widgets", None),
            ("ssh://git-codecommit.eu-north-1.amazonaws.com/v1/repos/widgets", None),
            // No repository named, no host named, a transport nobody browses.
            ("", None),
            ("https://github.com", None),
            ("https://github.com/", None),
            ("git@github.com:", None),
            ("https:///acme/widgets", None),
            ("ftp://git.example.org/acme/widgets.git", None),
            // Nothing a browser would read as more than one thing.
            ("https://github.com/acme/wid gets", None),
            ("https://github.com/acme/wid\tgets", None),
            ("git@git hub.com:acme/widgets", None),
        ];
        for (remote, page) in cases {
            assert_eq!(web_url(remote).as_deref(), *page, "{remote:?}");
        }
        for remote in LOGINS {
            let page = web_url(remote);
            assert!(!page.as_deref().unwrap_or("").contains("SECRET"), "{remote}: {page:?}");
        }
    }

    #[test]
    fn a_page_is_called_by_the_name_its_site_goes_by() {
        for (page, site) in [
            ("https://github.com/acme/widgets", "GitHub"),
            ("https://gist.github.com/0123abcd", "GitHub"),
            ("https://gitlab.com/acme/platform/widgets", "GitLab"),
            ("https://bitbucket.org/acme/widgets", "Bitbucket"),
            ("https://codeberg.org/acme/widgets", "Codeberg"),
            ("https://dev.azure.com/acme/Shop/_git/widgets", "Azure DevOps"),
            // A company's own GitHub is not GitHub the site: say which.
            ("https://github.example.org/acme/widgets", "github.example.org"),
            ("http://git.example.org:3000/acme/widgets", "git.example.org"),
            ("http://localhost:3000/acme/widgets", "localhost"),
        ] {
            assert_eq!(web_site(page), site, "{page}");
        }
    }

    /// What git says when a fetch fails, with the remote's address in it as
    /// git prints it: its login left out where git could tell where that
    /// ends, and whole where it could not.
    #[test]
    fn whoever_an_address_logs_in_as_is_taken_out_of_what_git_said() {
        for (said, shown) in [
            (
                "fatal: unable to access 'https://deploy:abc/SECRET@example.invalid/x.git/': \
                 URL rejected: Port number was not a decimal number between 0 and 65535",
                "fatal: unable to access 'https://example.invalid/x.git/': \
                 URL rejected: Port number was not a decimal number between 0 and 65535",
            ),
            (
                "fatal: unable to access 'https://SECRET@github.com/acme/widgets.git/': URL rejected: Bad hostname",
                "fatal: unable to access 'https://github.com/acme/widgets.git/': URL rejected: Bad hostname",
            ),
            (
                "fatal: unable to access 'https://me:tok@en@host.example/x.git/': error: 403",
                "fatal: unable to access 'https://host.example/x.git/': error: 403",
            ),
            ("ssh://deploy:hunter2@host.example:2222/x.git: refused", "ssh://host.example:2222/x.git: refused"),
            ("could not reach https://ghp_token@host.example", "could not reach https://host.example"),
            (
                "fatal: 'oauth2:glpat-SECRET@gitlab.com:group/repo.git' does not appear to be a git repository",
                "fatal: 'gitlab.com:group/repo.git' does not appear to be a git repository",
            ),
            // Nothing secret where there is no login: an address without one,
            // an email, and whom an ssh remote logs in as, which goes anyway.
            (
                "fatal: repository 'https://example.invalid/acme/billing.git/' not found",
                "fatal: repository 'https://example.invalid/acme/billing.git/' not found",
            ),
            ("pushed by Fixture <fixture@example.invalid>", "pushed by Fixture <fixture@example.invalid>"),
            ("git@github.com: Permission denied (publickey).", "github.com: Permission denied (publickey)."),
            ("git fetch gave no answer in 60 seconds", "git fetch gave no answer in 60 seconds"),
            ("", ""),
        ] {
            assert_eq!(without_logins_in(said), shown, "{said}");
        }
    }
}
