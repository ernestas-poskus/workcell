//! A git-backed workspace for running coding agents.
//!
//! `workcell` owns the parts of a coding-agent runtime that do not depend on a task tracker, a
//! model provider, or a review workflow:
//!
//! | Module | Holds |
//! |---|---|
//! | [`git`] | A typed Git runner and the primitives a coding workspace needs |
//! | [`worktree`] | [`worktree::RepoSpec`] and [`worktree::Worktree`]: one isolated checkout per unit of work |
//! | [`gc`] | Conservative garbage collection for stale worktrees and branches |
//! | [`process`] | One child process: spawn, timeout, stall watchdog, two-stage kill |
//! //!
//! | [`instructions`] | Bounded, root-confined reads of a checkout's instruction files |
//! | [`launch`] | An argv template with per-argument placeholders, and the child-environment policy |
//! | [`verify`] | Running declared commands with bounded capture, the check record, and the status parser |
//! | [`types`] | Validated paths, Git identities, process arguments, environment, limits, and operation labels |
//!
//! # Guarantees
//!
//! - **Git runs without hooks.** Every invocation the crate performs passes
//!   `-c core.hooksPath=/dev/null`, so a repository cannot execute code in the caller's process
//!   tree through a planted hook or configuration file.
//! - **A child is killed as a group.** A timeout sends `SIGTERM` to the process group, waits a
//!   short grace period, then sends `SIGKILL`; a coding agent routinely starts children of its own.
//! - **I/O on the runtime is asynchronous.** Filesystem work uses Tokio's blocking pool and child
//!   processes use Tokio's process driver; nothing on an async path calls a blocking
//! - **Reads are bounded and root-confined.** Instruction files and diffs are capped, and a path
//!   that resolves outside the checkout (including through a symlink) is refused.
//!
//! # Runtime requirements
//!
//! A Tokio runtime and `git` on `PATH`.
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod error;
pub mod gc;
pub mod git;
pub mod instructions;
pub mod launch;
pub mod process;
pub mod types;
pub mod verify;
pub mod worktree;
