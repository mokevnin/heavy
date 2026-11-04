import { existsSync, statSync } from "node:fs"
import { delimiter, join } from "node:path"

// OpenCode plugin for heavy: wraps known-heavy bash/shell commands in
// `heavy -- …` so builds, tests and linters take a machine-wide slot.
// Quick commands pass through. Nested heavy is safe (HEAVY_HELD).
//
// Dual-shape default export for OpenCode 2.x (`setup` → execute.before) and
// 1.x (`server` → tool.execute.before). See:
// https://opencode.ai/docs/plugins
// https://opencode.ai/v2/docs/build/plugins
// Requires OpenCode 1.18.29 or newer (the floor those docs give for this shape).
//
// PATH discovery uses node:fs only (no shell helpers). Missing heavy, or
// HEAVY_DISABLED=1, fails open and leaves the command unchanged.

let cachedHeavyPath: string | null = null

export function _resetCachedHeavyPath(): void {
  cachedHeavyPath = null
}

/**
 * Resolves the heavy binary from the system PATH (macOS/Linux).
 *
 * PATH only: OpenCode's shell tool uses OpenCode's own PATH and never sources
 * shell profiles, so a binary found outside PATH cannot be spawned as a bare
 * `heavy …`. A directory on PATH named `heavy` is skipped.
 */
export function resolveHeavyPath(): string | null {
  if (cachedHeavyPath && existsSync(cachedHeavyPath)) return cachedHeavyPath

  const dirs = (process.env.PATH ?? "").split(delimiter).filter(Boolean)
  for (const dir of dirs) {
    const fullPath = join(dir, "heavy")
    if (existsSync(fullPath) && !statSync(fullPath).isDirectory()) {
      return (cachedHeavyPath = fullPath)
    }
  }

  return (cachedHeavyPath = null)
}

const PASSTHROUGH_WRAPPERS = new Set(["command", "env", "nice", "nohup", "time"])

const CARGO_HEAVY = new Set(["bench", "build", "check", "clippy", "test"])
const GO_HEAVY = new Set(["build", "test", "vet"])
const JS_PACKAGE_MANAGERS = new Set(["bun", "npm", "pnpm", "yarn"])
const JS_RUN_HEAVY = new Set([
  "build",
  "check",
  "ci",
  "lint",
  "test",
  "type-check",
  "typecheck",
])
const MAKE_HEAVY_TARGETS = new Set(["all", "build", "check", "ci", "lint", "test"])
const STANDALONE_HEAVY = new Set([
  "biome",
  "eslint",
  "golangci-lint",
  "mypy",
  "ninja",
  "pytest",
  "py.test",
  "ruff",
  "tsc",
  "vue-tsc",
])
const BUILD_TOOL_HEAVY = new Set(["dotnet", "gradle", "gradlew", "mvn", "mvnw"])
const BUILD_TOOL_SUBS = new Set(["build", "check", "package", "test", "verify"])

/** True when the command already starts with heavy (after leading env assignments). */
export function isAlreadyHeavy(command: string): boolean {
  const trimmed = command.trim()
  if (!trimmed) return false
  return /^(?:\/\S*\/)?heavy(?:\s|$)/i.test(stripLeadingEnvAssignments(trimmed))
}

function stripLeadingEnvAssignments(segment: string): string {
  let rest = segment.trim()
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
  return base.toLowerCase()
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

function skipFlags(argv: string[], start: number): number {
  let i = start
  while (i < argv.length && argv[i]!.startsWith("-")) i += 1
  return i
}

function cargoSubcommand(argv: string[]): string | null {
  // cargo [+toolchain] [flags] <subcommand>
  let i = 1
  if (i < argv.length && argv[i]!.startsWith("+")) i += 1
  i = skipFlags(argv, i)
  return argv[i]?.toLowerCase() ?? null
}

function isHeavyJsPackage(argv: string[]): boolean {
  const cmd = basenameCommand(argv[0]!)
  if (!JS_PACKAGE_MANAGERS.has(cmd)) return false
  let i = skipFlags(argv, 1)
  const sub = argv[i]?.toLowerCase()
  if (!sub) return false
  if (sub === "test") return true
  if (sub !== "run") return false
  i = skipFlags(argv, i + 1)
  const script = argv[i]?.toLowerCase()
  return script !== undefined && JS_RUN_HEAVY.has(script)
}

function isHeavyMake(argv: string[]): boolean {
  const cmd = basenameCommand(argv[0]!)
  if (cmd !== "make" && cmd !== "gmake") return false
  let i = skipFlags(argv, 1)
  // make VAR=value target — skip assignments
  while (i < argv.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(argv[i]!)) i += 1
  const target = argv[i]
  if (!target) return true
  return MAKE_HEAVY_TARGETS.has(target.toLowerCase())
}

function isHeavyPythonModule(argv: string[]): boolean {
  const cmd = basenameCommand(argv[0]!)
  if (cmd !== "python" && cmd !== "python3") return false
  for (let i = 1; i < argv.length; i++) {
    if (argv[i] === "-m") {
      const mod = argv[i + 1]?.toLowerCase()
      return mod === "pytest" || mod === "mypy"
    }
  }
  return false
}

function isHeavyCmake(argv: string[]): boolean {
  if (basenameCommand(argv[0]!) !== "cmake") return false
  return argv.some((token) => token === "--build")
}

function isHeavyBuildTool(argv: string[]): boolean {
  if (!BUILD_TOOL_HEAVY.has(basenameCommand(argv[0]!))) return false
  let i = skipFlags(argv, 1)
  const sub = argv[i]?.toLowerCase()
  return sub !== undefined && BUILD_TOOL_SUBS.has(sub)
}

/** Whether this shell segment is a known heavy command. */
export function isHeavySegment(segment: string): boolean {
  const trimmed = segment.trim()
  if (!trimmed) return false
  if (isAlreadyHeavy(trimmed)) return false

  const argv = effectiveArgv(trimmed)
  if (argv.length === 0) return false

  const cmd = basenameCommand(argv[0]!)
  if (cmd === "heavy") return false
  if (STANDALONE_HEAVY.has(cmd)) return true
  if (cmd === "cargo") {
    const sub = cargoSubcommand(argv)
    return sub !== null && CARGO_HEAVY.has(sub)
  }
  if (cmd === "go") {
    const sub = argv[skipFlags(argv, 1)]?.toLowerCase()
    return sub !== undefined && GO_HEAVY.has(sub)
  }
  if (isHeavyJsPackage(argv)) return true
  if (isHeavyMake(argv)) return true
  if (isHeavyPythonModule(argv)) return true
  if (isHeavyCmake(argv)) return true
  if (isHeavyBuildTool(argv)) return true
  return false
}

type Piece =
  | { kind: "segment"; text: string }
  | { kind: "sep"; text: string }

/** Split a shell string into segments and the separators between them. */
export function splitPieces(command: string): Piece[] {
  const pieces: Piece[] = []
  let current = ""
  let quote: "'" | '"' | null = null

  const pushSegment = () => {
    pieces.push({ kind: "segment", text: current })
    current = ""
  }

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
      pushSegment()
      pieces.push({ kind: "sep", text: ch + next })
      i += 1
      continue
    }

    if (ch === ";" || ch === "|") {
      pushSegment()
      pieces.push({ kind: "sep", text: ch })
      continue
    }

    current += ch
  }

  pushSegment()
  return pieces
}

/**
 * Insert `heavy -- ` before the effective binary of a segment, keeping leading
 * env assignments and thin wrappers in place.
 */
export function wrapSegment(segment: string): string {
  const leadingWs = segment.match(/^\s*/)?.[0] ?? ""
  const trailingWs = segment.match(/\s*$/)?.[0] ?? ""
  const body = segment.slice(leadingWs.length, segment.length - trailingWs.length)
  if (!body || isAlreadyHeavy(body)) return segment

  // Find where the effective command starts inside the original body by walking
  // the same prefixes effectiveArgv strips, but on the raw string.
  let rest = body
  let prefix = ""

  // Leading FOO=bar …
  while (/^[A-Za-z_][A-Za-z0-9_]*=/.test(rest)) {
    const match = rest.match(/^[A-Za-z_][A-Za-z0-9_]*=(?:'[^']*'|"[^"]*"|\S+)(\s+)/)
    if (!match) break
    prefix += match[0]
    rest = rest.slice(match[0].length)
  }

  // Thin wrappers and their flags / env assignments
  while (true) {
    const tokens = tokenize(rest)
    if (tokens.length === 0) break
    const head = basenameCommand(tokens[0]!)
    if (!PASSTHROUGH_WRAPPERS.has(head)) break

    const headMatch = rest.match(/^\S+(\s*)/)
    if (!headMatch) break
    prefix += headMatch[0]
    rest = rest.slice(headMatch[0].length)

    while (rest.length > 0) {
      if (rest[0] === "-") {
        const flagMatch = rest.match(/^\S+(\s*)/)
        if (!flagMatch) break
        prefix += flagMatch[0]
        rest = rest.slice(flagMatch[0].length)
        continue
      }
      if (/^[A-Za-z_][A-Za-z0-9_]*=/.test(rest)) {
        const envMatch = rest.match(/^[A-Za-z_][A-Za-z0-9_]*=(?:'[^']*'|"[^"]*"|\S+)(\s*)/)
        if (!envMatch) break
        prefix += envMatch[0]
        rest = rest.slice(envMatch[0].length)
        continue
      }
      break
    }
  }

  if (!rest.trim()) return segment
  return `${leadingWs}${prefix}heavy -- ${rest}${trailingWs}`
}

/**
 * Build the rewritten shell command, or null when nothing should change.
 * Only known-heavy segments are prefixed with `heavy --`.
 */
export function rewriteCommand(command: string): string | null {
  const trimmed = command.trim()
  if (!trimmed) return null
  if (isAlreadyHeavy(trimmed)) return null

  const pieces = splitPieces(command)
  let changed = false
  const out: string[] = []

  for (const piece of pieces) {
    if (piece.kind === "sep") {
      out.push(piece.text)
      continue
    }
    if (isHeavySegment(piece.text)) {
      out.push(wrapSegment(piece.text))
      changed = true
    } else {
      out.push(piece.text)
    }
  }

  return changed ? out.join("") : null
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

function handleToolHook(tool: unknown, container: any) {
  if (!container || typeof container !== "object") return
  const rewritten = tryRewriteCommand(String(tool ?? ""), container.command)
  if (rewritten) container.command = rewritten
}

function ensureHeavyUsable(): boolean {
  if (resolveHeavyPath()) return true
  console.warn("[heavy] no heavy on OpenCode's PATH — plugin disabled")
  return false
}

/**
 * OpenCode 2.x and 1.x (>= 1.18.29) plugin for heavy.
 *
 * Plain object: the 2.x loader schema-validates default against
 * typeof === "object". The server() method is the 1.x dual-shape entrypoint.
 * https://opencode.ai/v2/docs/build/plugins
 */
const HeavyOpenCodePlugin = {
  id: "heavy",

  async setup(ctx: any) {
    if (!ensureHeavyUsable()) return
    await ctx?.tool?.hook?.("execute.before", (e: any) => handleToolHook(e?.tool, e?.input))
  },

  async server() {
    if (!ensureHeavyUsable()) return {}
    return {
      "tool.execute.before": (input: any, output: any) => handleToolHook(input?.tool, output?.args),
    }
  },
}

export default HeavyOpenCodePlugin
export { HeavyOpenCodePlugin }
