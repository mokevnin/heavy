import { existsSync, statSync } from "node:fs"
import { delimiter, join } from "node:path"

// Heavy OpenCode plugin — wraps bash/shell commands in `heavy -- bash -c …`
// so CPU/IO-bound agent work takes a machine-wide slot instead of starving
// other checkouts and agents.
//
// Unlike RTK, rewrite decisions live here (heavy has no `hook opencode`
// subcommand). Light-only commands pass through; everything else is wrapped.
// Nested `heavy` is safe: the binary skips the queue when HEAVY_HELD matches.
//
// Transport mirrors RTK's OpenCode plugin: node:fs PATH discovery (no Bun `$`,
// no `which`), dual-shape default export for OpenCode 2.x (`setup`) and 1.x
// (`server`), mutate command in place, fail open when heavy is missing.

let cachedHeavyPath: string | null = null

export function _resetCachedHeavyPath(): void {
  cachedHeavyPath = null
}

/**
 * Resolves the heavy binary from the system PATH.
 *
 * PATH only: OpenCode's shell tool uses OpenCode's own PATH and never sources
 * shell profiles, so a binary found outside PATH cannot be spawned as a bare
 * `heavy …`. A directory on PATH named `heavy` is skipped.
 */
export function resolveHeavyPath(): string | null {
  if (cachedHeavyPath && existsSync(cachedHeavyPath)) return cachedHeavyPath

  const dirs = (process.env.PATH ?? "").split(delimiter).filter(Boolean)
  const exts =
    process.platform === "win32"
      ? (process.env.PATHEXT ?? ".EXE;.CMD;.BAT;.COM").split(";")
      : [""]

  for (const dir of dirs) {
    for (const ext of exts) {
      const fullPath = join(dir, `heavy${ext}`)
      if (existsSync(fullPath) && !statSync(fullPath).isDirectory()) {
        return (cachedHeavyPath = fullPath)
      }
    }
  }

  return (cachedHeavyPath = null)
}

const LIGHT_COMMANDS = new Set([
  ":",
  "alias",
  "basename",
  "cat",
  "cd",
  "dirs",
  "dirname",
  "echo",
  "export",
  "false",
  "file",
  "hash",
  "head",
  "ls",
  "popd",
  "printf",
  "pushd",
  "pwd",
  "readlink",
  "realpath",
  "stat",
  "tail",
  "true",
  "type",
  "unset",
  "wc",
  "which",
])

const LIGHT_GIT_SUBCOMMANDS = new Set([
  "branch",
  "config",
  "describe",
  "diff",
  "log",
  "remote",
  "rev-parse",
  "show",
  "status",
])

const PASSTHROUGH_WRAPPERS = new Set(["command", "env", "nice", "nohup", "time"])

const HEAVY_LOOKAHEAD = /(?:^|[\s;&|])heavy(?:\s|$)/
const DOUBTFUL = /\$\(|`|\$\{|[<>]/

/** Single-quote a string for `bash -c '…'`. */
export function shellSingleQuote(value: string): string {
  return `'${value.replace(/'/g, `'\\''`)}'`
}

/** True when the command is already queued through heavy as the outer wrapper. */
export function isAlreadyHeavy(command: string): boolean {
  const trimmed = command.trim()
  if (!trimmed) return false
  // Leading env assignments then `heavy`.
  const withoutEnv = stripLeadingEnvAssignments(trimmed)
  return /^(?:\/\S*\/)?heavy(?:\.exe)?(?:\s|$)/i.test(withoutEnv)
}

function stripLeadingEnvAssignments(segment: string): string {
  let rest = segment.trim()
  // FOO=bar BAZ=qux cmd …
  while (/^[A-Za-z_][A-Za-z0-9_]*=/.test(rest)) {
    const match = rest.match(/^[A-Za-z_][A-Za-z0-9_]*=(?:'[^']*'|"[^"]*"|\S+)\s+/)
    if (!match) break
    rest = rest.slice(match[0].length)
  }
  return rest.trim()
}

function tokenize(segment: string): string[] {
  const tokens: string[] = []
  const re = /'([^']*)'|"([^"]*)"|(\S+)/g
  let match: RegExpExecArray | null
  while ((match = re.exec(segment)) !== null) {
    tokens.push(match[1] ?? match[2] ?? match[3] ?? "")
  }
  return tokens
}

function basenameCommand(token: string): string {
  const cleaned = token.replace(/^['"]|['"]$/g, "")
  const parts = cleaned.split(/[/\\]/)
  const base = parts[parts.length - 1] ?? cleaned
  return base.replace(/\.exe$/i, "").toLowerCase()
}

/**
 * Strip thin wrappers (`env`, `command`, `time`, `nice`, `nohup`) and leading
 * env assignments, then return the effective argv.
 */
export function effectiveArgv(segment: string): string[] {
  let tokens = tokenize(stripLeadingEnvAssignments(segment))
  while (tokens.length > 0) {
    const head = basenameCommand(tokens[0]!)
    if (PASSTHROUGH_WRAPPERS.has(head)) {
      tokens = tokens.slice(1)
      // `env` / `command` may still carry flags or NAME=value before the program.
      while (tokens.length > 0) {
        const next = tokens[0]!
        if (next.startsWith("-")) {
          tokens = tokens.slice(1)
          continue
        }
        if (/^[A-Za-z_][A-Za-z0-9_]*=/.test(next)) {
          tokens = tokens.slice(1)
          continue
        }
        break
      }
      continue
    }
    break
  }
  return tokens
}

function isLightGit(argv: string[]): boolean {
  if (argv.length < 2) return false
  if (basenameCommand(argv[0]!) !== "git") return false
  let i = 1
  while (i < argv.length && argv[i]!.startsWith("-")) {
    // git -C path … — skip the path argument too
    if (argv[i] === "-C" || argv[i] === "-c") {
      i += 2
      continue
    }
    i += 1
  }
  const sub = argv[i]
  if (!sub) return false
  return LIGHT_GIT_SUBCOMMANDS.has(sub.toLowerCase())
}

function isLightSegment(segment: string): boolean {
  const trimmed = segment.trim()
  if (!trimmed) return true
  if (DOUBTFUL.test(trimmed)) return false
  if (HEAVY_LOOKAHEAD.test(trimmed) && isAlreadyHeavy(trimmed)) return true

  const argv = effectiveArgv(trimmed)
  if (argv.length === 0) return true

  const cmd = basenameCommand(argv[0]!)
  if (cmd === "heavy") return true
  if (cmd === "git") return isLightGit(argv)
  return LIGHT_COMMANDS.has(cmd)
}

/** Split a shell string into rough pipeline/list segments. */
export function splitSegments(command: string): string[] {
  const segments: string[] = []
  let current = ""
  let quote: "'" | '"' | null = null

  for (let i = 0; i < command.length; i++) {
    const ch = command[i]!
    const next = command[i + 1]

    if (quote) {
      current += ch
      if (ch === quote) quote = null
      continue
    }

    if (ch === "'" || ch === '"') {
      quote = ch
      current += ch
      continue
    }

    if (ch === "\\" && next !== undefined) {
      current += ch + next
      i += 1
      continue
    }

    if ((ch === "&" && next === "&") || (ch === "|" && next === "|")) {
      segments.push(current)
      current = ""
      i += 1
      continue
    }

    if (ch === ";" || ch === "|") {
      segments.push(current)
      current = ""
      continue
    }

    current += ch
  }

  segments.push(current)
  return segments
}

/**
 * Whether the command should be wrapped in `heavy -- bash -c …`.
 * Light-only chains and commands already under heavy are left alone.
 */
export function shouldWrapCommand(command: string): boolean {
  const trimmed = command.trim()
  if (!trimmed) return false
  if (isAlreadyHeavy(trimmed)) return false
  return !splitSegments(trimmed).every(isLightSegment)
}

/**
 * Build the rewritten shell command, or null when nothing should change.
 */
export function rewriteCommand(command: string): string | null {
  if (!shouldWrapCommand(command)) return null
  return `heavy -- bash -c ${shellSingleQuote(command.trim())}`
}

export function tryRewriteCommand(toolName: string, command: unknown): string | null {
  const tool = String(toolName ?? "").toLowerCase()
  if ((tool !== "bash" && tool !== "shell") || typeof command !== "string" || !command.trim()) {
    return null
  }

  if (process.env.HEAVY_DISABLED === "1") return null

  if (!resolveHeavyPath()) return null

  try {
    return rewriteCommand(command)
  } catch {
    return null
  }
}

async function handleToolHook(tool: unknown, container: any) {
  if (!container || typeof container !== "object") return
  const rewritten = tryRewriteCommand(String(tool ?? ""), container.command)
  if (rewritten) container.command = rewritten
}

async function ensureHeavyUsable(): Promise<boolean> {
  const heavyBin = resolveHeavyPath()
  if (!heavyBin) {
    console.warn("[heavy] no heavy on OpenCode's PATH — plugin disabled")
    return false
  }
  return true
}

/**
 * OpenCode 2.x and 1.x (>= 1.18.29) plugin for heavy.
 *
 * Plain object: the 2.x loader schema-validates `default` against
 * `typeof === "object"`. The `server()` method is the 1.x dual-shape entrypoint.
 */
const HeavyOpenCodePlugin = {
  id: "heavy",

  async setup(ctx: any) {
    if (!(await ensureHeavyUsable())) return
    await ctx?.tool?.hook?.("execute.before", (e: any) => handleToolHook(e?.tool, e?.input))
  },

  async server() {
    if (!(await ensureHeavyUsable())) return {}
    return {
      "tool.execute.before": (input: any, output: any) => handleToolHook(input?.tool, output?.args),
    }
  },
}

export default HeavyOpenCodePlugin
export { HeavyOpenCodePlugin }
