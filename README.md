# workcell

A Git-backed workspace substrate for coding agents: a hardened Git runner, worktrees, conservative GC,
one child process with timeout/stall/group-kill semantics, bounded instruction and file reads, launch
templates, and verification helpers.

Callers launch their own harnesses and are responsible for any permission boundary they need. The
Git runner disables hooks, the process layer kills a child as a group, and worktree/GC operations
remain conservative.

## Modules

| Module | Holds |
|---|---|
| `git` | A typed Git runner and the primitives a coding workspace needs |
| `worktree` | `RepoSpec` and `Worktree`: one isolated checkout per unit of work |
| `gc` | Conservative garbage collection for stale worktrees and branches |
| `process` | One child process: spawn, timeout, stall watchdog, two-stage kill |
| `instructions` | Bounded, root-confined reads of a checkout's instruction files |
| `launch` | An argv template with per-argument placeholders, and the child-environment policy |
| `verify` | Running declared commands with bounded capture and status parsing |
| `fs` | Bounded reads and atomic/no-clobber writes |

## Runtime requirements

A Tokio runtime and `git` on `PATH`.

## License

MIT.
