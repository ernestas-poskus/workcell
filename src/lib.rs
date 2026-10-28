//! A sandboxed, git-backed workspace for running untrusted coding agents.
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
//! | [`sandbox`] | The OS sandbox around a coding agent: profile, launch, preflight |
//!
//! The sandbox profiles are reviewed policy compiled into the crate. The one value that differs
//! between callers is the in-checkout directory where a caller keeps its run-control state; it is
//! named by [`sandbox::SandboxOptions::state_dir`] (default
//! [`sandbox::DEFAULT_STATE_DIR`]) and rendered into the six state-path denials before the
//! profiles are content-addressed, published, and verified.
//! | [`instructions`] | Bounded, root-confined reads of a checkout's instruction files |
//! | [`fs`] | Bounded reads and atomic/no-clobber writes |
//!
//! # Guarantees
//!
//! - **Git runs without hooks.** Every invocation the crate performs passes
//!   `-c core.hooksPath=/dev/null`, so a repository cannot execute code in the caller's process
//!   tree through a planted hook or configuration file.
//! - **A child is killed as a group.** A timeout sends `SIGTERM` to the process group, waits a
//!   short grace period, then sends `SIGKILL`; a coding agent routinely starts children of its own.
//! - **The sandbox is a prerequisite, not an option.** [`sandbox::Sandbox::resolve`] fails when the
//!   sandbox binary is missing, and [`sandbox::Sandbox::preflight`] fails when this host cannot
//!   enforce a sandbox. There is no "run without it" path in the API.
//! - **I/O on the runtime is asynchronous.** Filesystem work uses Tokio's blocking pool and child
//!   processes use Tokio's process driver; nothing on an async path calls a blocking
//!   `std::fs`/`std::process` operation. [`sandbox::Sandbox::resolve`] is a synchronous `PATH`
//!   lookup with no side effects; publishing the profiles and probing the binary belong to the
//!   async [`sandbox::Sandbox::preflight`].
//! - **Reads are bounded and root-confined.** Instruction files and diffs are capped, and a path
//!   that resolves outside the checkout (including through a symlink) is refused.
//!
//! # Runtime requirements
//!
//! A Tokio runtime, and `git` on `PATH`. The sandbox module additionally requires the `nono`
//! binary (version [`sandbox::MIN_NONO_VERSION`] or newer) on `PATH`; the sandbox is the
//! operating-system boundary the crate launches coding agents behind.
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod error;
pub mod fs;
pub mod gc;
pub mod git;
pub mod instructions;
pub mod process;
pub mod sandbox;
pub mod worktree;
