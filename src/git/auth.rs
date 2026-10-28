//! Credential acquisition for an in-process fetch.
//!
//! libgit2 has no interactive prompt and no `~/.ssh/config` reader. This module supplies the
//! credentials a headless fetch can actually use:
//!
//! - SSH: the ssh-agent, then an unencrypted default key in `~/.ssh`.
//! - HTTPS: URL userinfo, then the operator's **global/XDG/system** `credential.helper` chain.
//!
//! The helper chain is deliberately not libgit2's own: this implementation reads helpers only
//! from the global levels (a repository-local `credential.helper` would let a hostile checkout
//! execute a program in the caller's process tree), and it runs each helper with the same scrubbed
//! environment the caller supplied to [`super::Git`], never the parent process environment.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use git2::{Config, ConfigLevel, Cred, CredentialType};
use url::Url;

/// How long one credential helper may take.
const HELPER_TIMEOUT: Duration = Duration::from_secs(30);

/// The most helper output that is read.
const HELPER_OUTPUT_LIMIT: usize = 64 * 1024;

/// The credentials libgit2 may offer for one fetch attempt.
pub(super) fn credentials(
    env: &BTreeMap<String, String>,
    root: &Path,
    url: &str,
    username: Option<&str>,
    allowed: CredentialType,
) -> std::result::Result<Cred, git2::Error> {
    if allowed.contains(CredentialType::SSH_KEY) {
        return ssh(env, username);
    }
    if allowed.contains(CredentialType::USER_PASS_PLAINTEXT)
        && let Some((user, password)) = https(env, root, url, username)
    {
        return Cred::userpass_plaintext(&user, &password);
    }
    if allowed.contains(CredentialType::DEFAULT) {
        return Cred::default();
    }
    if allowed.contains(CredentialType::USERNAME) {
        return Cred::username(username.unwrap_or("git"));
    }
    Err(git2::Error::from_str(
        "the remote offered no credential type this fetch can answer",
    ))
}

/// SSH credentials: the agent first, then an unencrypted default key.
fn ssh(
    env: &BTreeMap<String, String>,
    username: Option<&str>,
) -> std::result::Result<Cred, git2::Error> {
    let user = username.unwrap_or("git");
    if let Ok(credential) = Cred::ssh_key_from_agent(user) {
        return Ok(credential);
    }
    let home = env
        .get("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
    if let Some(home) = home {
        for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
            let key = home.join(".ssh").join(name);
            if key.is_file()
                && let Ok(credential) = Cred::ssh_key(user, None, &key, None)
            {
                return Ok(credential);
            }
        }
    }
    Err(git2::Error::from_str(
        "SSH authentication failed: no ssh-agent and no usable unencrypted key in ~/.ssh; start an agent (ssh-add) or use an HTTPS remote",
    ))
}

/// HTTPS credentials: URL userinfo, then the global credential-helper chain.
fn https(
    env: &BTreeMap<String, String>,
    root: &Path,
    url: &str,
    username: Option<&str>,
) -> Option<(String, String)> {
    let parsed = Url::parse(url).ok()?;
    let mut user = username.map(str::to_owned);
    let mut password = None;
    if !parsed.username().is_empty() && parsed.password().is_some() {
        user = Some(parsed.username().to_owned());
        password = parsed.password().map(str::to_owned);
    }
    if let (Some(user), Some(password)) = (user.clone(), password) {
        return Some((user, password));
    }
    let request = helper_request(&parsed, user.as_deref())?;
    let config = Config::open_default().ok()?;
    for helper in helper_entries(&config, url) {
        if let Some((helper_user, helper_password)) = run_helper(env, root, &helper, &request) {
            return Some((helper_user, helper_password));
        }
    }
    None
}

/// The credential-helper request for one URL.
fn helper_request(parsed: &Url, username: Option<&str>) -> Option<String> {
    let mut host = parsed.host_str()?.to_owned();
    if let Some(port) = parsed.port() {
        host = format!("{host}:{port}");
    }
    let mut request = format!("protocol={}\nhost={}\n", parsed.scheme(), host);
    // git sends `path` only when `credential.useHttpPath` is true (or the URL-scoped helper asks
    // for it); the default keeps the helper keyed by host, which is what keychain helpers expect.
    if use_http_path(parsed) {
        let path = parsed.path().trim_start_matches('/');
        if !path.is_empty() {
            request.push_str(&format!("path={path}\n"));
        }
    }
    if let Some(username) = username {
        request.push_str(&format!("username={username}\n"));
    }
    request.push('\n');
    Some(request)
}

/// Whether the operator configured `credential.useHttpPath`.
fn use_http_path(parsed: &Url) -> bool {
    Config::open_default()
        .ok()
        .and_then(|config| config.get_bool("credential.usehttppath").ok())
        .unwrap_or(false)
        || parsed.path().len() > 1
}

/// Every helper that applies to `url`, most specific first, from global levels only.
fn helper_entries(config: &Config, url: &str) -> Vec<String> {
    let mut matched: Vec<(usize, usize, String)> = Vec::new();
    let mut order = 0usize;
    if let Ok(mut entries) = config.entries(None) {
        while let Some(entry) = entries.next() {
            let Ok(entry) = entry else {
                continue;
            };
            if entry.level() == ConfigLevel::Local {
                continue;
            }
            let Ok(name) = entry.name() else {
                continue;
            };
            let Some(rest) = name.strip_prefix("credential.") else {
                continue;
            };
            let specificity = if rest == "helper" {
                0
            } else if let Some(pattern) = rest.strip_suffix(".helper") {
                if pattern.is_empty() || !url_matches(pattern, url) {
                    continue;
                }
                pattern.len()
            } else {
                continue;
            };
            let Ok(value) = entry.value() else {
                continue;
            };
            matched.push((specificity, order, value.to_owned()));
            order += 1;
        }
    }
    matched.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    let mut helpers = Vec::new();
    for (_, _, value) in matched {
        // git treats an empty helper value as "clear the previous list".
        if value.trim().is_empty() {
            helpers.clear();
        } else {
            helpers.push(value);
        }
    }
    helpers
}

/// Whether a `credential.<pattern>.helper` pattern matches the URL.
fn url_matches(pattern: &str, url: &str) -> bool {
    let pattern = pattern.trim_end_matches('/');
    if pattern.contains("://") {
        return url.starts_with(pattern);
    }
    // A bare host (or host/path) pattern.
    url.contains(pattern)
}

/// Run one helper with the `get` operation.
fn run_helper(
    env: &BTreeMap<String, String>,
    root: &Path,
    helper: &str,
    request: &str,
) -> Option<(String, String)> {
    let mut command = helper_command(helper)?;
    command
        .current_dir(root)
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(request.as_bytes()).ok()?;
    }
    if !wait_bounded(&mut child) {
        return None;
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() || output.stdout.len() > HELPER_OUTPUT_LIMIT {
        return None;
    }
    parse_helper_output(&output.stdout)
}

/// Build the command for one helper value.
fn helper_command(helper: &str) -> Option<Command> {
    if let Some(shell) = helper.strip_prefix('!') {
        let mut command = Command::new("/bin/sh");
        // `git` runs `sh -c '<value> "$@"' - get`.
        command
            .arg("-c")
            .arg(format!("{shell} \"$@\""))
            .arg("git-credential")
            .arg("get");
        return Some(command);
    }
    if helper.contains('/') {
        let mut command = Command::new(helper);
        command.arg("get");
        return Some(command);
    }
    let mut command = Command::new(format!("git-credential-{helper}"));
    command.arg("get");
    Some(command)
}

/// Wait for the helper, killing it after [`HELPER_TIMEOUT`].
fn wait_bounded(child: &mut Child) -> bool {
    let deadline = Instant::now() + HELPER_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Err(_) => return false,
        }
    }
}

/// Parse a helper's `username=`/`password=` output.
fn parse_helper_output(stdout: &[u8]) -> Option<(String, String)> {
    let text = String::from_utf8_lossy(stdout);
    let mut username = None;
    let mut password = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("username=") {
            username = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("password=") {
            password = Some(value.to_owned());
        }
    }
    username.zip(password)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("helper.sh");
        std::fs::write(&path, body).expect("write helper");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod helper");
        }
        path
    }

    #[test]
    fn a_shell_helper_runs_the_value_with_the_operation_argument() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = script(
            dir.path(),
            "#!/bin/sh\ncase \"$1\" in get) printf 'username=u\\npassword=p\\n' ;; esac\n",
        );
        let env = BTreeMap::new();
        let helper = format!("!{}", path.display());
        let request = "protocol=https\nhost=example.com\n\n";
        assert_eq!(
            run_helper(&env, dir.path(), &helper, request),
            Some(("u".to_owned(), "p".to_owned()))
        );
    }

    #[test]
    fn a_helper_receives_only_the_supplied_environment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dump = dir.path().join("env.txt");
        let path = script(
            dir.path(),
            &format!(
                "#!/bin/sh\n/usr/bin/env > {}\nprintf 'username=u\\npassword=p\\n'\n",
                dump.display()
            ),
        );
        let mut env = BTreeMap::new();
        env.insert("FAKE_MARKER".to_owned(), "present".to_owned());
        let helper = format!("!{}", path.display());
        assert!(run_helper(&env, dir.path(), &helper, "protocol=https\n\n").is_some());
        let text = std::fs::read_to_string(&dump).expect("env dump");
        assert!(text.contains("FAKE_MARKER=present"), "{text}");
        // HOME is in the parent process but was not supplied, so it must not reach the helper.
        assert!(!text.contains("HOME="), "{text}");
    }

    #[test]
    fn helper_values_come_from_global_config_most_specific_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config");
        std::fs::write(
            &config_path,
            "[credential]\n\thelper = global\n[credential \"https://github.com\"]\n\thelper = scoped\n",
        )
        .expect("write config");
        let mut config = Config::new().expect("config");
        config
            .add_file(&config_path, ConfigLevel::Global, false)
            .expect("add config");
        let helpers = helper_entries(&config, "https://github.com/acme/repo.git");
        assert_eq!(helpers, vec!["scoped".to_owned(), "global".to_owned()]);
    }

    #[test]
    fn a_repository_local_helper_is_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config");
        std::fs::write(&config_path, "[credential]\n\thelper = local\n").expect("write config");
        let mut config = Config::new().expect("config");
        config
            .add_file(&config_path, ConfigLevel::Local, false)
            .expect("add config");
        assert!(helper_entries(&config, "https://example.com/x").is_empty());
    }

    #[test]
    fn an_empty_helper_value_clears_the_previous_list() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("config");
        std::fs::write(
            &config_path,
            "[credential]\n\thelper = first\n\thelper =\n\thelper = last\n",
        )
        .expect("write config");
        let mut config = Config::new().expect("config");
        config
            .add_file(&config_path, ConfigLevel::Global, false)
            .expect("add config");
        assert_eq!(
            helper_entries(&config, "https://example.com/x"),
            vec!["last".to_owned()]
        );
    }

    #[test]
    fn a_bare_helper_name_becomes_git_credential_name() {
        let command = helper_command("osxkeychain").expect("command");
        assert_eq!(command.get_program(), "git-credential-osxkeychain");
        assert_eq!(command.get_args().collect::<Vec<_>>(), vec!["get"]);
    }

    #[test]
    fn helper_output_requires_both_fields() {
        assert_eq!(
            parse_helper_output(b"username=u\npassword=p\n"),
            Some(("u".to_owned(), "p".to_owned()))
        );
        assert_eq!(parse_helper_output(b"username=u\n"), None);
        assert_eq!(parse_helper_output(b""), None);
    }
}
