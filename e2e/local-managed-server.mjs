import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { randomBytes } from 'node:crypto'

const currentDir = dirname(fileURLToPath(import.meta.url))
const workspaceRoot = resolve(currentDir, '..')
const platformDir = join(workspaceRoot, 'platform')
const staticDir = join(platformDir, 'app-web', 'dist')
const bindAddr = process.env.E2E_MANAGED_BIND_ADDR ?? '127.0.0.1:4101'
const baseUrl = process.env.E2E_BASE_URL ?? `http://${bindAddr}`
const databaseUrl = process.env.TEST_DATABASE_URL ?? process.env.DATABASE_URL
const statePath = process.env.E2E_MANAGED_SERVER_STATE_PATH
  ?? join(mkdtempSync(join(tmpdir(), 'dragon-switch-e2e-')), 'managed-server-state.json')

if (!databaseUrl) {
  throw new Error('Managed restart e2e requires TEST_DATABASE_URL or DATABASE_URL.')
}

let child = null
let restarting = false
let shuttingDown = false
const sessionCookieKey = process.env.SESSION_COOKIE_KEY ?? randomBytes(64).toString('base64')

function ensureParentDir(filePath) {
  mkdirSync(dirname(filePath), { recursive: true })
}

function writeState() {
  ensureParentDir(statePath)
  const payload = {
    managerPid: process.pid,
    childPid: child?.pid ?? null,
    bindAddr,
    baseUrl,
    databaseUrl,
  }
  writeFileSync(statePath, `${JSON.stringify(payload)}\n`, 'utf8')
}

function spawnServer() {
  child = spawn('cargo', ['run', '-p', 'app-server'], {
    cwd: platformDir,
    env: {
      ...process.env,
      APP_SERVER_BIND_ADDR: bindAddr,
      APP_SERVER_STATIC_DIR: staticDir,
      ALLOWED_ORIGINS: baseUrl,
      VITE_APP_URL: baseUrl,
      NODE_ENV: 'development',
      DATABASE_URL: databaseUrl,
      SESSION_COOKIE_KEY: sessionCookieKey,
    },
    stdio: 'inherit',
  })

  child.once('exit', code => {
    writeState()
    if (!restarting && !shuttingDown) {
      process.exit(code ?? 0)
    }
  })

  writeState()
}

function stopChild(signal = 'SIGTERM') {
  if (!child || child.exitCode !== null || child.signalCode !== null) {
    child = null
    writeState()
    return Promise.resolve()
  }

  return new Promise(resolvePromise => {
    const current = child
    current.once('exit', () => {
      child = null
      writeState()
      resolvePromise()
    })
    current.kill(signal)
  })
}

async function restartServer() {
  if (restarting || shuttingDown) {
    return
  }
  restarting = true
  try {
    await stopChild('SIGTERM')
    spawnServer()
  } finally {
    restarting = false
  }
}

async function shutdown(exitCode) {
  if (shuttingDown) {
    return
  }
  shuttingDown = true
  await stopChild('SIGTERM')
  rmSync(statePath, { force: true })
  process.exit(exitCode)
}

process.on('SIGHUP', () => {
  void restartServer()
})

process.on('SIGINT', () => {
  void shutdown(130)
})

process.on('SIGTERM', () => {
  void shutdown(143)
})

process.on('uncaughtException', error => {
  process.stderr.write(`${error.stack ?? String(error)}\n`)
  void shutdown(1)
})

process.on('unhandledRejection', reason => {
  process.stderr.write(`${String(reason)}\n`)
  void shutdown(1)
})

spawnServer()
process.stdout.write(`${readFileSync(statePath, 'utf8').trim()}\n`)
