# Changelog

## 0.3.0

- Added the `types` module of validated primitives (`RepoRoot`, `RelativePath`,
  `BranchName`, `CommitId`, `Argv`, `Env`, `Timeout`, `ByteLimit`, and related types).
- `Git::diff` now takes `ByteLimit` and returns `CommitId`, `RevisionRange`,
  `RelativePath`, and `ByteSize`-typed diff fields.
- `ProcessSpec`/`Outcome`, `Check`/`Verification`, `InstructionSpec`/`InstructionFile`/`Bundle`,
  `ArgvTemplate`/`EnvPolicy`, `RepoSpec`/`Worktree`, and `GcPolicy`/`GcEntry` use the typed
  primitives.
- `Git` takes and returns the typed primitives for every operation: `new(RepoRoot)`,
  `with_env(Env)`, `root() -> &RepoRoot`, branch/reference/remote/revision/commit/digest/path
  arguments and results, and `GitOperation` labels in `run`/`git_error`.
- `Error::Git` now carries a typed `GitOperation` instead of a free-form command string.
- Removed the unused `fs` module and `Git::config_get`.
- Confirmed production Git operations remain in-process through `git2`; the only Git child
  processes are deliberate test/example fixtures and negative controls.
