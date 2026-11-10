//! Turning a declared argv template and an environment policy into a child launch.
//!
//! A caller that launches an agent or a check owns two decisions this module carries because they
//! are easy to get wrong and expensive to get wrong twice:
//!
//! - **Placeholder substitution inside one argument.** A template element such as
//!   `--prompt-file={prompt}` is substituted inside that element, so a path with a space stays one
//!   argv element. An unknown `{name}` is left alone rather than emptied: silently dropping it
//!   would turn a template typo into a launch with a missing argument.
//! - **The child environment allowlist.** A child that can read the deployment's credentials has
//!   the deployment's authority. [`EnvPolicy`] is the deny-list mechanism; the caller supplies
//!   the exact names, prefixes, suffixes, and substrings its domain requires, so this crate never
//!   learns what a credential or a tracker is.
//!
//! Nothing here spawns a process: the output of [`ArgvTemplate::render`] and [`EnvPolicy::scrub`]
//! is passed to [`crate::process`], which owns spawn, timeout, and capture.

use std::collections::{BTreeMap, BTreeSet};

use crate::types::{Arg, Argv, Env, EnvName};

/// An argv template whose arguments may contain `{name}` placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgvTemplate {
    arguments: Argv,
}

impl ArgvTemplate {
    /// Build a template from its declared, already-validated arguments.
    #[must_use]
    pub fn new(arguments: Argv) -> Self {
        Self { arguments }
    }

    /// The declared arguments, unrendered.
    #[must_use]
    pub fn arguments(&self) -> &[Arg] {
        self.arguments.as_slice()
    }

    /// Render every argument with `values`.
    ///
    /// Each entry replaces `{name}` inside an argument; a value may itself contain braces. An
    /// unknown placeholder is left in place. A substituted value carrying NUL cannot be an argv
    /// element, so such a template renders unsubstituted rather than shifting or dropping an
    /// argument.
    #[must_use]
    pub fn render(&self, values: &BTreeMap<&str, &str>) -> Argv {
        let rendered = self
            .arguments
            .as_slice()
            .iter()
            .map(|argument| render_argument(argument.as_str(), values));
        match Argv::new(rendered) {
            Ok(argv) => argv,
            Err(_) => self.arguments.clone(),
        }
    }
}

/// Substitute `{name}` placeholders inside one argument.
///
/// A placeholder is matched literally on the string `{name}` (braces included), so a lone brace in
/// a value is ordinary text. Unknown placeholders are left in place.
#[must_use]
pub fn render_argument(argument: &str, values: &BTreeMap<&str, &str>) -> String {
    let mut rendered = argument.to_owned();
    for (name, value) in values {
        let placeholder = format!("{{{name}}}");
        rendered = rendered.replace(&placeholder, value);
    }
    rendered
}

/// A case-insensitive name pattern a deny rule matches against.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvPattern(String);

impl EnvPattern {
    fn new(value: impl Into<String>) -> Self {
        Self(value.into().to_ascii_uppercase())
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// The child-environment deny rules a caller owns.
///
/// Matching is ASCII-case-insensitive for prefixes, suffixes, and substrings; an exact name is
/// matched as the environment spells it. Rules only ever remove a variable: there is no allow
/// override, because the policy exists to fail closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvPolicy {
    deny_exact: BTreeSet<EnvName>,
    deny_prefixes: Vec<EnvPattern>,
    deny_suffixes: Vec<EnvPattern>,
    deny_contains: Vec<EnvPattern>,
}

impl EnvPolicy {
    /// An empty policy: every variable is allowed until a rule denies it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Deny a variable whose name is exactly `name` (case-sensitive, as the environment spells it).
    #[must_use]
    pub fn deny_exact(mut self, name: EnvName) -> Self {
        self.deny_exact.insert(name);
        self
    }

    /// Deny every variable whose name starts with `prefix`, case-insensitively.
    #[must_use]
    pub fn deny_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.deny_prefixes.push(EnvPattern::new(prefix));
        self
    }

    /// Deny every variable whose name ends with `suffix`, case-insensitively.
    #[must_use]
    pub fn deny_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.deny_suffixes.push(EnvPattern::new(suffix));
        self
    }

    /// Deny every variable whose name contains `needle`, case-insensitively.
    #[must_use]
    pub fn deny_contains(mut self, needle: impl Into<String>) -> Self {
        self.deny_contains.push(EnvPattern::new(needle));
        self
    }

    /// Whether `name` survives the policy.
    #[must_use]
    pub fn allows(&self, name: &EnvName) -> bool {
        if self.deny_exact.contains(name) {
            return false;
        }
        let upper = name.as_str().to_ascii_uppercase();
        !self
            .deny_prefixes
            .iter()
            .any(|prefix| upper.starts_with(prefix.as_str()))
            && !self
                .deny_suffixes
                .iter()
                .any(|suffix| upper.ends_with(suffix.as_str()))
            && !self
                .deny_contains
                .iter()
                .any(|needle| upper.contains(needle.as_str()))
    }

    /// Remove every denied variable from `env`.
    #[must_use]
    pub fn scrub(&self, env: Env) -> Env {
        let mut scrubbed = Env::empty();
        for (name, value) in env.iter() {
            if self.allows(name) {
                scrubbed.insert(name.clone(), value.clone());
            }
        }
        scrubbed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EnvValue;

    fn name(value: &str) -> EnvName {
        EnvName::new(value).expect("env name")
    }

    #[test]
    fn placeholders_are_substituted_inside_one_argument() {
        let values = BTreeMap::from([("prompt", "/tmp/a brief.md"), ("model", "m1")]);
        let template = ArgvTemplate::new(
            Argv::new(["--prompt-file={prompt}", "{model}", "plain"]).expect("argv"),
        );
        assert_eq!(
            template.render(&values).to_strings(),
            vec!["--prompt-file=/tmp/a brief.md", "m1", "plain"]
        );
    }

    #[test]
    fn unknown_placeholders_are_left_alone() {
        let values = BTreeMap::from([("known", "value")]);
        assert_eq!(
            render_argument("{known} {unknown}", &values),
            "value {unknown}"
        );
    }

    #[test]
    fn a_value_containing_braces_is_literal_text() {
        let values = BTreeMap::from([("k", "{v}")]);
        assert_eq!(render_argument("x{k}y", &values), "x{v}y");
    }

    #[test]
    fn the_policy_denies_by_exact_name_prefix_suffix_and_substring() {
        let policy = EnvPolicy::new()
            .deny_exact(name("CARGO_HOME"))
            .deny_prefix("TRACKER_")
            .deny_suffix("_TOKEN")
            .deny_contains("API_KEY");
        assert!(!policy.allows(&name("CARGO_HOME")));
        assert!(!policy.allows(&name("tracker_secret")));
        assert!(!policy.allows(&name("MY_TOKEN")));
        assert!(!policy.allows(&name("OPENAI_API_KEY")));
        assert!(policy.allows(&name("PATH")));
        assert!(policy.allows(&name("CARGO_TARGET_DIR")));
    }

    #[test]
    fn scrub_removes_only_the_denied_variables() {
        let policy = EnvPolicy::new().deny_suffix("_TOKEN");
        let env = Env::new([
            ("PATH", "/bin"),
            ("GH_TOKEN", "secret"),
            ("HOME", "/home/a"),
        ])
        .expect("env");
        let scrubbed = policy.scrub(env);
        assert_eq!(scrubbed.len(), 2);
        assert_eq!(
            scrubbed.get("PATH").map(EnvValue::as_str),
            Some("/bin"),
            "the allowed variable keeps its value"
        );
        assert!(!scrubbed.get("GH_TOKEN").is_some_and(|_| true));
    }
}
