//! Generic verification of a checkout: run declared commands and collect their checks.
//!
//! A caller decides *what* must hold (which commands, whether the tree must have changed, which
//! files the work declared); this module owns the mechanics that are the same every time: one
//! bounded, timed child per command, a machine-readable [`Check`] per command, and the bounded
//! output tail kept for evidence. It also carries the porcelain-status parser the tree-change
//! decision needs.
//!
//! Nothing here runs a shell: a command is an argv array, and the child environment is the
//! caller's scrubbed map ([`crate::launch::EnvPolicy`] decides it). The caller wraps the argv
//! directly in the checkout, with the caller responsible for any boundary; this
//! module only spawns it through [`crate::process`], so capture and timeout are bounded for every
//! command alike.

use std::time::Instant;

use crate::process;
use crate::types::{
    AbsolutePath, Argv, ByteLimit, CheckId, CheckName, DurationMillis, Env, ExitCode, RelativePath,
    Timeout,
};

/// One check's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The check, as a reader recognizes it.
    pub name: CheckName,
    /// Whether it passed.
    pub ok: bool,
    /// What happened, with the failing output when it failed.
    pub detail: String,
    /// Wall-clock milliseconds spent running this command (zero for synthetic checks).
    pub duration_ms: DurationMillis,
    /// Process exit code; absent for timeout, signal, or failure to start.
    pub exit: Option<ExitCode>,
    /// Whether the deadline expired.
    pub timed_out: bool,
    /// Captured stdout followed by stderr, bounded for persistence.
    pub output: String,
    /// A configured command rather than a synthetic repository check.
    pub is_command: bool,
    /// The caller's configured name for a command, when one was assigned.
    pub logical_id: Option<CheckId>,
}

impl Default for Check {
    fn default() -> Self {
        Self {
            name: CheckName::synthetic(String::new()),
            ok: false,
            detail: String::new(),
            duration_ms: DurationMillis::default(),
            exit: None,
            timed_out: false,
            output: String::new(),
            is_command: false,
            logical_id: None,
        }
    }
}

/// The verification result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    /// Whether every required check passed.
    pub ok: bool,
    /// The checks, in the order they ran.
    pub checks: Vec<Check>,
    /// The caller's advisory warning line, when there is one.
    pub warning: Option<String>,
    /// The changed paths the caller's tree comparison reported.
    pub changed_paths: Vec<RelativePath>,
}

impl Verification {
    /// Total command time, saturating rather than overflowing for synthetic inputs.
    #[must_use]
    pub fn total_duration_ms(&self) -> u64 {
        self.checks
            .iter()
            .filter(|check| check.is_command)
            .fold(0_u64, |sum, check| {
                sum.saturating_add(check.duration_ms.get())
            })
    }

    /// The one-line rendering of every check and its status.
    #[must_use]
    pub fn summary_line(&self) -> String {
        self.checks
            .iter()
            .map(|check| {
                let status = if check.ok { "ok" } else { "FAILED" };
                format!("`{}` {status}", check.name)
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The failing checks.
    #[must_use]
    pub fn failed_checks(&self) -> Vec<&Check> {
        self.checks.iter().filter(|check| !check.ok).collect()
    }
}

/// Run one declared command with bounded capture and a timeout.
///
/// The argv must already be substituted and, when the command must run behind a boundary,
/// already wrapped. An empty argv is a failed check rather than an error: it means the caller's
/// configuration declared an empty command, and the record must say so.
pub async fn run_command(
    argv: &Argv,
    root: &AbsolutePath,
    env: &Env,
    timeout: Timeout,
    output_tail: ByteLimit,
    logical_id: Option<CheckId>,
) -> Check {
    let mut check = Check {
        name: CheckName::synthetic(argv.to_strings().join(" ")),
        is_command: true,
        logical_id,
        ..Check::default()
    };
    if argv.is_empty() {
        check.name = CheckName::synthetic("(empty command)");
        check.detail = "an empty command was declared".to_owned();
        return check;
    }
    let started = Instant::now();
    let outcome = match process::capture(argv.clone(), root.clone(), env.clone(), timeout).await {
        Ok(outcome) => outcome,
        Err(error) => {
            check.duration_ms = DurationMillis::from_duration(started.elapsed());
            check.detail = format!("could not start: {error}");
            return check;
        }
    };
    check.duration_ms = DurationMillis::from_duration(started.elapsed());
    if outcome.timed_out {
        check.timed_out = true;
        check.detail = format!("timed out after {}s", timeout.as_duration().as_secs());
        return check;
    }
    check.exit = outcome.exit_code;
    check.ok = outcome.exit_code == Some(ExitCode::new(0));
    let stderr = &outcome.stderr;
    let stdout = &outcome.stdout;
    check.output = tail_bounded(&format!("{stdout}\n{stderr}"), output_tail.get());
    check.detail = if check.ok {
        "exit 0".to_owned()
    } else {
        let code = check
            .exit
            .map_or_else(|| "signal".to_owned(), |code| code.to_string());
        let tail = tail_characters(&format!("{stdout}\n{stderr}"), output_tail.get());
        format!("exit {code}\n{tail}")
    };
    check
}

/// The last at most `max` bytes, starting at a UTF-8 character boundary.
#[must_use]
pub fn tail_bounded(text: &str, max: usize) -> String {
    let mut start = text.len().saturating_sub(max);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

/// The last at most `max` characters, trimmed at both ends.
#[must_use]
pub fn tail_characters(text: &str, max: usize) -> String {
    let characters: Vec<char> = text.chars().collect();
    if characters.len() <= max {
        return text.trim_end().to_owned();
    }
    characters[characters.len() - max..]
        .iter()
        .collect::<String>()
        .trim_start()
        .to_owned()
}

/// The changed paths in a porcelain v1 status.
///
/// A rename is two paths on one line (`R  old -> new`); the new path is what changed. A quoted
/// path (a name with non-ASCII bytes) is unquoted rather than dropped. A status path that is not a
/// usable relative path is skipped: a changed path the caller cannot name safely is not a path the
/// caller can act on.
#[must_use]
pub fn parse_status(status: &str) -> Vec<RelativePath> {
    status
        .lines()
        .filter(|line| line.len() > 3)
        .filter_map(|line| {
            let path = &line[3..];
            let path = path.rsplit(" -> ").next().unwrap_or(path);
            RelativePath::new(path.trim_matches('"')).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(values: &[&str]) -> Argv {
        Argv::new(values.iter().copied()).expect("argv")
    }

    fn root() -> AbsolutePath {
        AbsolutePath::new(std::env::current_dir().expect("cwd")).expect("absolute cwd")
    }

    fn env() -> Env {
        Env::empty()
    }

    #[test]
    fn an_empty_command_is_a_failed_check_not_a_panic() {
        let check = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(run_command(
                &Argv::default(),
                &root(),
                &env(),
                Timeout::new(std::time::Duration::from_secs(1)),
                ByteLimit::new(100),
                Some(CheckId::new("lint").expect("id")),
            ));
        assert!(!check.ok);
        assert!(check.is_command);
        assert_eq!(check.name.as_str(), "(empty command)");
        assert_eq!(check.detail, "an empty command was declared");
        assert_eq!(check.logical_id.as_ref().map(CheckId::as_str), Some("lint"));
    }

    #[test]
    fn a_missing_program_is_a_failed_check_with_the_reason() {
        let check = tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(run_command(
                &argv(&["/definitely/not/a/program"]),
                &root(),
                &env(),
                Timeout::new(std::time::Duration::from_secs(1)),
                ByteLimit::new(100),
                None,
            ));
        assert!(!check.ok);
        assert!(
            check.detail.starts_with("could not start:"),
            "{}",
            check.detail
        );
    }

    #[test]
    fn tail_bounded_starts_at_a_character_boundary() {
        let text = "αβγδε";
        assert_eq!(tail_bounded(text, 4), "δε");
        assert_eq!(tail_bounded("short", 100), "short");
    }

    #[test]
    fn tail_characters_counts_characters_not_bytes() {
        assert_eq!(tail_characters("αβγδε", 3), "γδε");
        assert_eq!(tail_characters("short\n", 100), "short");
    }

    #[test]
    fn a_rename_reports_the_new_path_and_a_quoted_path_is_unquoted() {
        let status = " M src/lib.rs\nR  old/name.rs -> new/name.rs\n?? \"spaced name.rs\"\n";
        let paths: Vec<String> = parse_status(status)
            .iter()
            .map(RelativePath::to_string)
            .collect();
        assert_eq!(paths, vec!["src/lib.rs", "new/name.rs", "spaced name.rs"]);
    }

    #[test]
    fn a_short_or_empty_status_line_is_ignored() {
        assert_eq!(parse_status(""), Vec::<RelativePath>::new());
        assert_eq!(parse_status(" M\n"), Vec::<RelativePath>::new());
    }
}
