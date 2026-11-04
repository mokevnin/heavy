# OpenCode plugin for heavy

TypeScript plugin that wraps OpenCode `bash` / `shell` tool commands in
`heavy -- bash -c '…'` so CPU- and IO-bound agent work takes a machine-wide
slot instead of starving other checkouts and agents.

## Install

Copy or symlink this plugin into OpenCode's plugins directory, then restart
OpenCode:

```sh
# machine-wide
mkdir -p ~/.config/opencode/plugins
ln -sf /path/to/heavy/hooks/opencode/heavy.ts ~/.config/opencode/plugins/heavy.ts

# or project-local
mkdir -p .opencode/plugins
ln -sf /path/to/heavy/hooks/opencode/heavy.ts .opencode/plugins/heavy.ts
```

`heavy` must be on the PATH that OpenCode itself uses (the shell tool does not
source `.bashrc` / `.profile`).

Requires OpenCode **1.18.29 or newer** (the floor OpenCode documents for the
dual-shape `{ id, setup, server }` plugin export).

## Behaviour

- Hooks OpenCode 2.x (`setup` → `execute.before`) and OpenCode 1.x
  (`server` → `tool.execute.before`).
- Rewrites non-light commands to `heavy -- bash -c '<command>'` so pipelines
  and `&&` chains stay intact (`heavy` itself does not run a shell).
- Leaves light-only commands alone (`ls`, `cat`, `cd`, `git status` / `diff` /
  `log` / …, and chains made only of those).
- Leaves commands that already start with `heavy` alone.
- Nested heavy is safe: the binary skips the queue when `HEAVY_HELD` matches.
- Missing `heavy` on PATH, or `HEAVY_DISABLED=1` in OpenCode's process
  environment, disables rewriting for the session (pass-through).
- Fail-open: any unexpected error leaves the command unchanged.

## Tests

Needs Node.js 22+ (TypeScript import via native strip-types). With nvm:

```sh
nvm use 24
node --test hooks/opencode/heavy.test.mjs
```
