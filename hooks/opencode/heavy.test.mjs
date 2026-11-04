import test, { after, before } from "node:test"
import assert from "node:assert/strict"
import { copyFileSync, mkdirSync, mkdtempSync, rmSync } from "node:fs"
import { tmpdir } from "node:os"
import { delimiter, join } from "node:path"
import HeavyOpenCodePlugin, {
  _resetCachedHeavyPath,
  isAlreadyHeavy,
  isHeavySegment,
  resolveHeavyPath,
  rewriteCommand,
  tryRewriteCommand,
  wrapSegment,
} from "./heavy.ts"

const BIN = "heavy"
let mockDir = ""
let emptyDir = ""
let savedPath = ""

const v2Event = (command) => ({
  tool: "shell",
  sessionID: "ses_1",
  agent: "build",
  messageID: "msg_1",
  id: "call_1",
  input: { command },
})
const v1Input = { tool: "bash", sessionID: "ses_1", callID: "call_1" }
const v1Output = (command) => ({ args: { command, description: "run" } })

async function captureSetup() {
  const names = []
  let hook = null
  await HeavyOpenCodePlugin.setup({
    tool: {
      hook(name, callback) {
        names.push(name)
        if (name === "execute.before") hook = callback
      },
    },
  })
  return { names, hook }
}

async function captureServer() {
  const hooks = await HeavyOpenCodePlugin.server()
  return { names: Object.keys(hooks), hooks }
}

function mockBin() {
  return join(mockDir, BIN)
}

before(() => {
  savedPath = process.env.PATH ?? ""
  mockDir = mkdtempSync(join(tmpdir(), "heavy-opencode-mock-"))
  emptyDir = mkdtempSync(join(tmpdir(), "heavy-opencode-empty-"))
  copyFileSync(process.execPath, mockBin())
  process.env.PATH = `${mockDir}${delimiter}${savedPath}`
  _resetCachedHeavyPath()
})

after(() => {
  process.env.PATH = savedPath
  delete process.env.HEAVY_DISABLED
  _resetCachedHeavyPath()
  rmSync(mockDir, { recursive: true, force: true })
  rmSync(emptyDir, { recursive: true, force: true })
})

test("quick commands are not wrapped", () => {
  for (const command of [
    "ls",
    "ls -la",
    "cd /tmp",
    "pwd",
    "echo hello",
    "cat README.md",
    "rg foo src",
    "grep -rn x .",
    "find . -name '*.rs'",
    "sed -n 1,50p file",
    "gh pr view 6",
    "jq . a.json",
    "python -c 'print(1)'",
    "git status",
    "git add -A",
    "git commit -m 'x'",
    "git fetch origin",
    "git clone https://example.com/repo.git",
    "mkdir -p a",
    "sleep 5",
    "npm run dev",
    "pnpm run start",
    "yarn run serve",
    "bun run watch",
    "cargo fmt",
    "go fmt ./...",
    "make help",
  ]) {
    assert.equal(isHeavySegment(command), false, command)
    assert.equal(rewriteCommand(command), null, command)
  }
})

test("known heavy commands are wrapped with heavy --", () => {
  const cases = [
    ["cargo test", "heavy -- cargo test"],
    ["cargo +nightly clippy", "heavy -- cargo +nightly clippy"],
    ["cargo build --release", "heavy -- cargo build --release"],
    ["go test ./...", "heavy -- go test ./..."],
    ["go build ./cmd/app", "heavy -- go build ./cmd/app"],
    ["tsc --noEmit", "heavy -- tsc --noEmit"],
    ["pytest -q", "heavy -- pytest -q"],
    ["python -m pytest", "heavy -- python -m pytest"],
    ["python3 -m mypy src", "heavy -- python3 -m mypy src"],
    ["npm test", "heavy -- npm test"],
    ["pnpm run build", "heavy -- pnpm run build"],
    ["yarn run lint", "heavy -- yarn run lint"],
    ["bun run typecheck", "heavy -- bun run typecheck"],
    ["make", "heavy -- make"],
    ["make test", "heavy -- make test"],
    ["eslint src", "heavy -- eslint src"],
    ["golangci-lint run", "heavy -- golangci-lint run"],
    ["cmake --build build", "heavy -- cmake --build build"],
    ["FOO=1 cargo test", "FOO=1 heavy -- cargo test"],
    ["env FOO=1 cargo test", "env FOO=1 heavy -- cargo test"],
  ]
  for (const [command, expected] of cases) {
    assert.equal(isHeavySegment(command), true, command)
    assert.equal(rewriteCommand(command), expected, command)
  }
})

test("only heavy segments of a chain are wrapped", () => {
  assert.equal(rewriteCommand("cd /tmp && cargo test"), "cd /tmp && heavy -- cargo test")
  assert.equal(
    rewriteCommand("git status && cargo test && ls"),
    "git status && heavy -- cargo test && ls"
  )
  assert.equal(rewriteCommand("rg foo || true"), null)
  assert.equal(wrapSegment("  cargo test  "), "  heavy -- cargo test  ")
})

test("already-heavy commands pass through", () => {
  assert.equal(isAlreadyHeavy("heavy -- cargo test"), true)
  assert.equal(isAlreadyHeavy("heavy --status"), true)
  assert.equal(isAlreadyHeavy("HEAVY_SLOTS=2 heavy -- make"), true)
  assert.equal(rewriteCommand("heavy -- cargo test"), null)
  assert.equal(
    rewriteCommand("cd /tmp && heavy -- cargo test"),
    null
  )
})

test("redirects and substitutions on quick commands stay unwrapped", () => {
  assert.equal(rewriteCommand("ls > out"), null)
  assert.equal(rewriteCommand("echo $(pwd)"), null)
})

test("2.x execute.before mutates event.input.command", async () => {
  assert.equal(HeavyOpenCodePlugin.id, "heavy")
  const { names, hook } = await captureSetup()
  assert.deepEqual(names, ["execute.before"])
  assert.equal(typeof hook, "function")

  const rewritten = v2Event("cargo test")
  await hook(rewritten)
  assert.equal(rewritten.input.command, "heavy -- cargo test")

  const kept = v2Event("git status")
  await hook(kept)
  assert.equal(kept.input.command, "git status")
})

test("1.x tool.execute.before mutates output.args.command", async () => {
  const { names, hooks } = await captureServer()
  assert.deepEqual(names, ["tool.execute.before"])

  const rewritten = v1Output("go test ./...")
  await hooks["tool.execute.before"](v1Input, rewritten)
  assert.equal(rewritten.args.command, "heavy -- go test ./...")
  assert.equal(rewritten.args.description, "run")

  const kept = v1Output("rg foo src")
  await hooks["tool.execute.before"](v1Input, kept)
  assert.equal(kept.args.command, "rg foo src")
})

test("missing heavy on PATH registers nothing", async () => {
  const warnings = []
  const realWarn = console.warn
  console.warn = (message) => warnings.push(String(message))
  try {
    process.env.PATH = emptyDir
    _resetCachedHeavyPath()
    assert.equal(resolveHeavyPath(), null)
    assert.deepEqual(await captureSetup().then((c) => c.names), [])
    assert.deepEqual((await captureServer()).names, [])
    assert.match(warnings.join("\n"), /no heavy on OpenCode's PATH/)
  } finally {
    process.env.PATH = `${mockDir}${delimiter}${savedPath}`
    console.warn = realWarn
    _resetCachedHeavyPath()
  }
})

test("resolveHeavyPath skips a directory named heavy", () => {
  const decoy = mkdtempSync(join(tmpdir(), "heavy-opencode-decoy-"))
  mkdirSync(join(decoy, BIN))
  process.env.PATH = `${decoy}${delimiter}${mockDir}${delimiter}${savedPath}`
  _resetCachedHeavyPath()
  try {
    assert.equal(resolveHeavyPath(), mockBin())
  } finally {
    process.env.PATH = `${mockDir}${delimiter}${savedPath}`
    _resetCachedHeavyPath()
    rmSync(decoy, { recursive: true, force: true })
  }
})

test("HEAVY_DISABLED=1 passes commands through", async () => {
  delete process.env.HEAVY_DISABLED
  const { hooks } = await captureServer()
  const live = v1Output("cargo test")
  await hooks["tool.execute.before"](v1Input, live)
  assert.equal(live.args.command, "heavy -- cargo test")

  try {
    for (const value of ["0", "true"]) {
      process.env.HEAVY_DISABLED = value
      const probed = v1Output("cargo test")
      await hooks["tool.execute.before"](v1Input, probed)
      assert.equal(probed.args.command, "heavy -- cargo test")
    }

    process.env.HEAVY_DISABLED = "1"
    const off = v1Output("cargo test")
    await hooks["tool.execute.before"](v1Input, off)
    assert.equal(off.args.command, "cargo test")
  } finally {
    delete process.env.HEAVY_DISABLED
  }
})

test("tryRewriteCommand guards the tool name and the command shape", async () => {
  assert.equal(await tryRewriteCommand("read", "cargo test"), null)
  assert.equal(await tryRewriteCommand("BASH", 42), null)
  assert.equal(await tryRewriteCommand("shell", " "), null)

  const sneaky = {
    toString() {
      return "cargo test"
    },
  }
  assert.equal(await tryRewriteCommand("shell", sneaky), null)
  assert.equal(await tryRewriteCommand("SHELL", "cargo test"), "heavy -- cargo test")
})
