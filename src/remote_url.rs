//! A remote's URL, made fit to be shown: the remote a card names, the page
//! `o` opens, and what git said when a fetch failed, which names it.
//!
//! A URL can carry whoever it logs in as, a token included, and each of these
//! ends up somewhere that keeps it: on screen, in the log a `--json` run is
//! piped to, in a browser's history. So all of them go through [`login`], the
//! one place that says where a login ends, and whether that can be told; and
//! what git said loses the password of the repository's remotes as well,
//! which is known, wherever git put it ([`without_secrets`]).

use std::ops::Range;

/// What is shown where something cannot be: a login, or the host of a remote
/// that could be read as naming more than one.
pub const HIDDEN: &str = "•••";

/// A login in a remote's URL: from just after the scheme's `://`, or from the
/// start of a remote written as scp writes one (`who@host:path`), to just
/// after the `@` that ends it.
#[derive(Debug, PartialEq)]
enum Login {
    /// It ends at the last `@` before the first `/`, where git ends it, and
    /// what follows it is the host.
    Plain(Range<usize>),
    /// There is no telling where it ends: an `@` comes after the first `/`,
    /// and it is a login with a slash in it or a path with an `@` in it. It is
    /// taken to run to the last `@` of all, which may take the front of the
    /// path with it, that being the side to fail on. No page is named for it.
    Unsure(Range<usize>),
    /// It holds a `?`, `#`, `[` or `]`, at which git stops reading for a host,
    /// and so goes to a host other than the one that seems to follow the `@`:
    /// `https://evil.invalid#@other.invalid/x` reaches `evil.invalid`. Neither
    /// is named, nor a page.
    Misleading(Range<usize>),
}

impl Login {
    fn at(&self) -> Range<usize> {
        match self {
            Login::Plain(at) | Login::Unsure(at) | Login::Misleading(at) => at.clone(),
        }
    }
}

/// The login in `url`, if it has one.
///
/// Where git ends a login is at the last `@` before the first `/` that follows
/// the scheme, which is also where its address goes on to the path. That keeps
/// `https://me:p@ss@host/x` and `oauth2:token@gitlab.com:group/repo` from
/// showing half of their secret, and leaves `git@host.example:acme/w@v2` its
/// host and its path.
///
/// An `@` after the first `/` as well makes two readings of it. `@` before the
/// slash, and what lies between that `@` and the slash is not something that
/// can be a host, as in `me:p@ss/word@host/x`, and the login is taken to run
/// to the last `@` (unsure); no `@` before the slash, and there is no telling
/// either (unsure).
///
/// A path on disk has no login. git takes a remote for one when it has no
/// colon, or a slash before the first.
fn login(url: &str) -> Option<Login> {
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
    let rest = &url[start..];
    let last = rest.rfind('@')?;
    let slash = rest.find('/').unwrap_or(rest.len());
    let to = |at: usize| start..start + at + 1;
    match rest[..slash].rfind('@') {
        Some(at) if at == last || can_be_a_host(&rest[at + 1..slash]) => {
            if rest[..at].contains(['?', '#', '[', ']']) {
                Some(Login::Misleading(to(at)))
            } else {
                Some(Login::Plain(to(at)))
            }
        }
        _ => Some(Login::Unsure(to(last))),
    }
}

/// Whether what follows a login, up to the first `/`, can be the host: a name
/// with a dot in it, or one followed by a port, or, as scp writes it, by a
/// colon and the path.
fn can_be_a_host(text: &str) -> bool {
    text.contains(['.', ':'])
}

/// `url` without whoever it logs in as: see [`login`].
fn without_login(url: &str) -> String {
    match login(url) {
        Some(login) => {
            let at = login.at();
            format!("{}{}", &url[..at.start], &url[at.end..])
        }
        None => url.to_string(),
    }
}

/// The password in `url`: what follows the first `:` of its login.
fn password(url: &str) -> Option<&str> {
    let url = url.trim();
    let at = login(url)?.at();
    let login = &url[at.start..at.end - 1];
    login.split_once(':').map(|(_, password)| password)
}

/// How long a password, or a piece of one, has to be to be looked for in
/// what git said. Anything shorter is common enough to be found inside words
/// that are nobody's, and taken out of them.
const LOOKED_FOR: usize = 4;

/// What git said when a fetch failed, fit to be shown: the password of each
/// of `urls`, the repository's remotes, taken out of it wherever it is, and
/// then whatever login is still in an address in it (see [`without_logins_in`]).
///
/// git leaves a remote's login out of most of what it prints, and not out of
/// an address it cannot make sense of. That one it prints whole, inside
/// quotes, and a password with a quote, a space or a `<` in it is then no one
/// word that could be found by looking for addresses
/// (`'https://me:it's/x@host'`). And curl names what it took for the host,
/// which can be the front of a password: `me:p@SECR/ET@host` comes out as
/// "Could not resolve host: SECR". The password is known, being in the
/// remote's URL, so it is taken out as it is, and so are the pieces it breaks
/// into at an `@`, `/`, `?` or `#`, where they are long enough to look for.
/// Each gives way to [`HIDDEN`], so that it is plain something was there.
///
/// A user name with no password is not looked for: `git` and `deploy` are
/// nobody's secret, and would be taken out of every sentence they are in.
pub fn without_secrets(said: &str, urls: &[String]) -> String {
    let mut said = said.to_string();
    for password in urls.iter().filter_map(|url| password(url)) {
        let mut secrets: Vec<&str> = std::iter::once(password)
            .chain(password.split(['@', '/', '?', '#']))
            .filter(|secret| secret.chars().count() >= LOOKED_FOR)
            .collect();
        // Longest first: a piece taken out ahead of the whole would leave
        // the rest of the whole behind.
        secrets.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        secrets.dedup();
        for secret in secrets {
            said = said.replace(secret, HIDDEN);
        }
    }
    without_logins_in(&said)
}

/// `text` with the login of every address in it put out of sight: a word
/// that names a scheme (`https://`, `ssh://`) and has an `@` after it. What
/// the address logs in as gives way to [`HIDDEN`].
///
/// The net under [`without_secrets`], for a password nobody knows: in a
/// record of failed fetches written before passwords were taken out of what
/// git said, or one whose remote has since changed. A word runs from one
/// space or quote to the next, so a password with one in it gets through this
/// net, and only the known one is sure to go.
///
/// Nothing without a scheme is touched. Taken for logins, `contact
/// admin@x.invalid:`, `stash@{0}:` and `deploy@host: Permission denied` came
/// out without their front; a token in a remote written as scp writes one is
/// known, and goes by name.
pub fn without_logins_in(text: &str) -> String {
    let ends = |c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '<' | '>');
    let mut out = String::with_capacity(text.len());
    for piece in text.split_inclusive(ends) {
        let word = piece.strip_suffix(ends).unwrap_or(piece);
        let login = if word.contains("://") { login(word) } else { None };
        match login {
            Some(login) => {
                let at = login.at();
                out.push_str(&word[..at.start]);
                out.push_str(HIDDEN);
                // From the `@` on, which says that a login was there.
                out.push_str(&piece[at.end - 1..]);
            }
            None => out.push_str(piece),
        }
    }
    out
}

/// What a card calls a remote: `git@github.com:owner/repo.git` and
/// `https://me:token@host/owner/repo.git` both come to `host/owner/repo`.
///
/// A remote whose login is [misleading](Login::Misleading) is called
/// [`HIDDEN`]. The host it seems to name is not where git goes, and the one
/// git does go to can be the front of a password.
pub fn tidy(url: &str) -> String {
    let url = url.trim();
    if matches!(login(url), Some(Login::Misleading(_))) {
        return HIDDEN.to_string();
    }
    let url = without_login(url);
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
    match login(url) {
        // In a remote written as scp writes one, git takes whatever comes
        // before the first colon for the host, and a login with a colon in it
        // puts the host somewhere else than it seems to be.
        Some(Login::Plain(at)) if scp && url[at.clone()].contains(':') => return None,
        Some(Login::Plain(_)) | None => {}
        // No page, rather than the wrong one, or one with half a password in
        // its path.
        Some(Login::Unsure(_) | Login::Misleading(_)) => return None,
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

    /// A remote, what a card calls it, and the page `o` opens for it.
    type Shown = (&'static str, &'static str, Option<&'static str>);

    fn shown(cases: &[Shown]) {
        for (remote, named, page) in cases {
            assert_eq!((tidy(remote).as_str(), web_url(remote).as_deref()), (*named, *page), "{remote}");
        }
    }

    /// Every way two reviews found a login written into a remote: properly,
    /// and the ways that are not proper and are configured all the same. None
    /// of the secret may reach the card or the page.
    const LOGINS: &[Shown] = &[
        (
            "https://jm:ghp_SECRET0123456789@github.com/acme/widgets.git",
            "github.com/acme/widgets",
            Some("https://github.com/acme/widgets"),
        ),
        (
            "ssh://deploy:SECRET@git.example.org:2222/acme/widgets.git",
            "git.example.org:2222/acme/widgets",
            Some("https://git.example.org/acme/widgets"),
        ),
        // An `@` in the password.
        ("https://jm:p@SECRET@github.com/acme/widgets.git", "github.com/acme/widgets", Some("https://github.com/acme/widgets")),
        // A token in front of a remote written as scp writes one, which git
        // reads as a host called `oauth2`.
        ("oauth2:glpat-SECRET@gitlab.com:group/repo.git", "gitlab.com/group/repo", None),
        // A `/` in the password, alone and with a quote or a space beside it,
        // which cannot be told from a path with an `@` in it.
        ("https://deploy:abc/SECRET@example.invalid/x.git", "example.invalid/x", None),
        ("https://user:it's/SECRET@example.invalid/x.git", "example.invalid/x", None),
        ("https://user:pa ss/SECRET@example.invalid/x.git", "example.invalid/x", None),
        // An `@` and then a `/`, with no host between them.
        ("https://me:p@SE/CRET@example.invalid/x.git", "example.invalid/x", None),
        // A `?` in the password, at which git stops reading for the host.
        ("https://me:p@SE?CRET@example.invalid/x.git", HIDDEN, None),
    ];

    #[test]
    fn no_login_reaches_the_card_or_the_page() {
        shown(LOGINS);
        for (remote, ..) in LOGINS {
            let (named, page) = (tidy(remote), web_url(remote).unwrap_or_default());
            assert!(!named.contains("CRET") && !page.contains("CRET"), "{remote}: {named} {page}");
        }
    }

    /// Working remotes with an `@` in the path, and a login in front of the
    /// host. Taking the login to the last `@` of all made `git@host:acme/w@v2`
    /// a remote called `v2` with no page.
    #[test]
    fn a_remote_with_an_at_in_its_path_keeps_its_name_and_its_page() {
        shown(&[
            ("git@host.example:acme/w@v2.git", "host.example/acme/w@v2", Some("https://host.example/acme/w@v2")),
            ("ssh://git@host.example/acme/w@v2.git", "host.example/acme/w@v2", Some("https://host.example/acme/w@v2")),
            (
                "https://jm:ghp_SECRET@host.example/acme/w@v2.git",
                "host.example/acme/w@v2",
                Some("https://host.example/acme/w@v2"),
            ),
            // With no login there is no telling it from a login with a `/` in
            // it, and the path pays for that.
            ("https://git.example.org/team@2024/widgets.git", "2024/widgets", None),
        ]);
    }

    /// A login with a `?`, `#`, `[` or `]` in it. git stops reading for the
    /// host there, and goes somewhere other than the host that seems to
    /// follow the `@`, which was named on the card and opened by `o`.
    #[test]
    fn a_login_that_hides_where_git_goes_names_no_host() {
        shown(&[
            ("https://evil.invalid#@other.invalid/acme/w", HIDDEN, None),
            ("https://evil.invalid?@other.invalid/acme/w", HIDDEN, None),
            ("[git@host.example:2222]:acme/w.git", HIDDEN, None),
        ]);
    }

    #[test]
    fn a_card_names_a_remote_by_its_host_and_path() {
        for (remote, named) in [
            ("https://github.com/acme/widgets.git", "github.com/acme/widgets"),
            ("git@github.com:acme/widgets.git", "github.com/acme/widgets"),
            ("ssh://git@github.com/acme/widgets.git", "github.com/acme/widgets"),
            ("https://acme@dev.azure.com/acme/Shop/_git/widgets", "dev.azure.com/acme/Shop/_git/widgets"),
            // A directory, which logs in as nobody, whatever is in its name.
            ("/srv/git/widgets.git", "/srv/git/widgets"),
            ("/srv/backups@2024/widgets.git", "/srv/backups@2024/widgets"),
            ("./with:colon/widgets", "./with:colon/widgets"),
        ] {
            assert_eq!(tidy(remote), named, "{remote}");
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
    /// git prints it, and no password known: the login of every address with
    /// a scheme gives way to the mark, where git could tell where it ends and
    /// where it could not.
    #[test]
    fn the_login_of_an_address_in_what_git_said_is_put_out_of_sight() {
        for (said, shown) in [
            (
                "fatal: unable to access 'https://deploy:abc/SECRET@example.invalid/x.git/': \
                 URL rejected: Port number was not a decimal number between 0 and 65535",
                "fatal: unable to access 'https://•••@example.invalid/x.git/': \
                 URL rejected: Port number was not a decimal number between 0 and 65535",
            ),
            (
                "fatal: unable to access 'https://SECRET@github.com/acme/widgets.git/': URL rejected: Bad hostname",
                "fatal: unable to access 'https://•••@github.com/acme/widgets.git/': URL rejected: Bad hostname",
            ),
            (
                "fatal: unable to access 'https://me:tok@en@host.example/x.git/': error: 403",
                "fatal: unable to access 'https://•••@host.example/x.git/': error: 403",
            ),
            ("ssh://deploy:hunter2@host.example:2222/x.git: refused", "ssh://•••@host.example:2222/x.git: refused"),
            ("could not reach https://ghp_token@host.example", "could not reach https://•••@host.example"),
        ] {
            assert_eq!(without_logins_in(said), shown, "{said}");
            assert_eq!(without_logins_in(shown), shown, "and once is as good as twice");
        }
    }

    /// Text that is nobody's login comes through whole, with a password
    /// known and with none: no scheme, no address. Each of these used to be
    /// taken for a login written as scp writes one, and lose its front.
    #[test]
    fn what_is_not_a_login_comes_through_whole() {
        let urls = ["https://deploy:hunter2-SECRET@host.example/x.git".to_string()];
        for said in [
            "error: contact admin@x.invalid: the repository has moved",
            "stash@{0}: WIP on main: 1234567 first",
            "deploy@host.example: Permission denied (publickey).",
            "git@github.com: Permission denied (publickey).",
            "fatal: 'oauth2:glpat-ABCDEFGH@gitlab.com:group/repo.git' does not appear to be a git repository",
            "pushed by Fixture <fixture@example.invalid>",
            "fatal: repository 'https://example.invalid/acme/billing.git/' not found",
            "git fetch gave no answer in 60 seconds",
            "",
        ] {
            assert_eq!(without_logins_in(said), said);
            assert_eq!(without_secrets(said, &urls), said);
        }
        // A user name with no password is no secret, and is not looked for.
        let unsecret = ["git@host.example:acme/w.git".to_string(), "https://deploy@host.example/x".to_string()];
        let said = "deploy: git said nothing to deploy";
        assert_eq!(without_secrets(said, &unsecret), said);
    }

    /// The password of the repository's own remote, found and taken out
    /// wherever git or curl put it, in the ways a review confirmed them
    /// against git 2.56: whole, inside quotes, with a quote or a space in it;
    /// and cut up, with curl naming its front for the host.
    #[test]
    fn a_known_password_is_taken_out_of_what_git_said_wherever_it_is() {
        let rejected = "URL rejected: Port number was not a decimal number between 0 and 65535";
        for (url, said, shown) in [
            (
                "https://user:it's/SECRET@example.invalid/x.git",
                format!("fatal: unable to access 'https://user:it's/SECRET@example.invalid/x.git/': {rejected}"),
                format!("fatal: unable to access 'https://•••@example.invalid/x.git/': {rejected}"),
            ),
            (
                "https://user:pa ss/SECRET@example.invalid/x.git",
                format!("fatal: unable to access 'https://user:pa ss/SECRET@example.invalid/x.git/': {rejected}"),
                format!("fatal: unable to access 'https://•••@example.invalid/x.git/': {rejected}"),
            ),
            (
                "https://me:p@SECR/ET@example.invalid/x.git",
                "fatal: unable to access 'https://SECR/ET@example.invalid/x.git/': Could not resolve host: SECR"
                    .to_string(),
                "fatal: unable to access 'https://•••@example.invalid/x.git/': Could not resolve host: •••".to_string(),
            ),
            (
                "https://me:p@SECR?ET@example.invalid/x.git",
                "fatal: unable to access 'https://SECR?ET@example.invalid/x.git/': Could not resolve host: SECR"
                    .to_string(),
                "fatal: unable to access 'https://•••@example.invalid/x.git/': Could not resolve host: •••".to_string(),
            ),
            (
                "oauth2:glpat-SECRET@gitlab.com:group/repo.git",
                "fatal: 'oauth2:glpat-SECRET@gitlab.com:group/repo.git' does not appear to be a git repository"
                    .to_string(),
                "fatal: 'oauth2:•••@gitlab.com:group/repo.git' does not appear to be a git repository".to_string(),
            ),
            // A piece shorter than four is left, being too common to look for:
            // here the review's own `p@SE/CRET`, whose `SE` curl names.
            (
                "https://me:p@SE/CRET@example.invalid/x.git",
                "fatal: unable to access 'https://SE/CRET@example.invalid/x.git/': Could not resolve host: SE"
                    .to_string(),
                "fatal: unable to access 'https://•••@example.invalid/x.git/': Could not resolve host: SE".to_string(),
            ),
        ] {
            let urls = [url.to_string()];
            assert_eq!(without_secrets(&said, &urls), shown, "{url}");
            assert_eq!(without_secrets(&shown, &urls), shown, "and once is as good as twice: {url}");
        }
    }
}
