# OpenCode plugin for heavy

TypeScript plugin that wraps known-heavy OpenCode `bash` / `shell` tool
commands in `heavy -- …` so builds, tests and linters take a machine-wide
slot. Quick commands (`ls`, `rg`, `git status`, `npm run dev`, …) pass
through unchanged.

## Install

Download the plugin into the project's OpenCode plugins directory (users
install heavy via brew, mise, cargo or the installer and usually have no
checkout), then restart OpenCode:

```sh
mkdir -p .opencode/plugins
curl -fsSL https://raw.githubusercontent.com/mokevnin/heavy/main/hooks/opencode/heavy.ts \
  -o .opencode/plugins/heavy.ts
```

For a machine-wide install, use `~/.config/opencode/plugins/heavy.ts`
instead.

`heavy` must be on the PATH that OpenCode itself uses (the shell tool does
not source `.bashrc` / `.profile`).

Requires OpenCode **1.18.29 or newer**, the floor documented for the
dual-shape plugin export:
https://opencode.ai/v2/docs/build/plugins

Plugin loading in general: https://opencode.ai/docs/plugins

## Behaviour

- Hooks OpenCode 2.x (`setup` → `execute.before`) and OpenCode 1.x
  (`server` → `tool.execute.before`). See the docs linked above.
- Rewrites only known heavy commands to `heavy -- <command>`, including
  heavy segments inside `&&` / `||` / `;` / `|` chains
  (`cd /tmp && cargo test` → `cd /tmp && heavy -- cargo test`).
- Leaves everything else alone (search, git, editors, `npm run dev`, …).
- Leaves commands that already start with `heavy` alone.
- Nested heavy is safe: the binary skips the queue when `HEAVY_HELD` matches.
- Missing `heavy` on PATH, or `HEAVY_DISABLED=1` in OpenCode's process
  environment, disables rewriting for the session (pass-through).
- Fail-open: any unexpected error leaves the command unchanged.

## Tests

Needs Node.js 22+ (TypeScript import via native strip-types):

```sh
node --test hooks/opencode/heavy.test.mjs
```
