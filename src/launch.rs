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

/// An argv template whose arguments may contain `{name}` placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArgvTemplate {
    arguments: Vec<String>,
}

impl ArgvTemplate {
    /// Build a template from its declared arguments.
    #[must_use]
    pub fn new(arguments: impl IntoIterator<Item = String>) -> Self {
        Self {
            arguments: arguments.into_iter().collect(),
        }
    }

    /// The declared arguments, unrendered.
    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    /// Render every argument with `values`.
    ///
    /// Each entry replaces `{name}` inside an argument; a value may itself contain braces. An
    /// unknown placeholder is left in place.
    #[must_use]
    pub fn render(&self, values: &BTreeMap<&str, &str>) -> Vec<String> {
        self.arguments
            .iter()
            .map(|argument| render_argument(argument, values))
            .collect()
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

/// The child-environment deny rules a caller owns.
///
/// Matching is ASCII-case-insensitive for prefixes, suffixes, and substrings; an exact name is
/// matched as the environment spells it. Rules only ever remove a variable: there is no allow
/// override, because the policy exists to fail closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvPolicy {
    deny_exact: BTreeSet<String>,
    deny_prefixes: Vec<String>,
    deny_suffixes: Vec<String>,
    deny_contains: Vec<String>,
}

impl EnvPolicy {
    /// An empty policy: every variable is allowed until a rule denies it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Deny a variable whose name is exactly `name` (case-sensitive, as the environment spells it).
    #[must_use]
    pub fn deny_exact(mut self, name: impl Into<String>) -> Self {
        self.deny_exact.insert(name.into());
        self
    }

    /// Deny every variable whose name starts with `prefix`, case-insensitively.
    #[must_use]
    pub fn deny_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.deny_prefixes.push(prefix.into().to_ascii_uppercase());
        self
    }

    /// Deny every variable whose name ends with `suffix`, case-insensitively.
    #[must_use]
    pub fn deny_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.deny_suffixes.push(suffix.into().to_ascii_uppercase());
        self
    }

    /// Deny every variable whose name contains `needle`, case-insensitively.
    #[must_use]
    pub fn deny_contains(mut self, needle: impl Into<String>) -> Self {
        self.deny_contains.push(needle.into().to_ascii_uppercase());
        self
    }

    /// Whether `key` survives the policy.
    #[must_use]
    pub fn allows(&self, key: &str) -> bool {
        if self.deny_exact.contains(key) {
            return false;
        }
        let upper = key.to_ascii_uppercase();
        !self
            .deny_prefixes
            .iter()
            .any(|prefix| upper.starts_with(prefix))
            && !self
                .deny_suffixes
                .iter()
                .any(|suffix| upper.ends_with(suffix))
            && !self
                .deny_contains
                .iter()
                .any(|needle| upper.contains(needle))
    }

    /// Remove every denied variable from `env`.
    #[must_use]
    pub fn scrub(&self, env: BTreeMap<String, String>) -> BTreeMap<String, String> {
        env.into_iter()
            .filter(|(key, _)| self.allows(key))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_substituted_inside_one_argument() {
        let values = BTreeMap::from([("prompt", "/tmp/a brief.md"), ("model", "m1")]);
        let template = ArgvTemplate::new(vec![
            "--prompt-file={prompt}".to_owned(),
            "{model}".to_owned(),
            "plain".to_owned(),
        ]);
        assert_eq!(
            template.render(&values),
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
            .deny_exact("CARGO_HOME")
            .deny_prefix("TRACKER_")
            .deny_suffix("_TOKEN")
            .deny_contains("API_KEY");
        assert!(!policy.allows("CARGO_HOME"));
        assert!(!policy.allows("tracker_secret"));
        assert!(!policy.allows("MY_TOKEN"));
        assert!(!policy.allows("OPENAI_API_KEY"));
        assert!(policy.allows("PATH"));
        assert!(policy.allows("CARGO_TARGET_DIR"));
    }

    #[test]
    fn scrub_removes_only_the_denied_variables() {
        let policy = EnvPolicy::new().deny_suffix("_TOKEN");
        let env = BTreeMap::from([
            ("PATH".to_owned(), "/bin".to_owned()),
            ("GH_TOKEN".to_owned(), "secret".to_owned()),
            ("HOME".to_owned(), "/home/a".to_owned()),
        ]);
        let scrubbed = policy.scrub(env);
        assert_eq!(scrubbed.len(), 2);
        assert!(scrubbed.contains_key("PATH"));
        assert!(!scrubbed.contains_key("GH_TOKEN"));
    }
}
