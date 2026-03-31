import { mkdtempSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { defineConfig } from '@playwright/test'

import { projectProfiles } from './project-profiles'

const baseURL = process.env.E2E_BASE_URL ?? 'http://127.0.0.1:4101'
const workspaceRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const statePath = process.env.E2E_MANAGED_SERVER_STATE_PATH
  ?? join(mkdtempSync(join(tmpdir(), 'dragon-switch-e2e-state-')), 'managed-server-state.json')

process.env.E2E_BASE_URL = baseURL
process.env.E2E_MANAGED_SERVER_STATE_PATH = statePath

export default defineConfig({
  testDir: './tests',
  testMatch: /restart-reconnect\.spec\.ts/,
  outputDir: './.tmp/test-results-local-restart',
  timeout: 120_000,
  expect: {
    timeout: 15_000,
  },
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: 'list',
  use: {
    baseURL,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  webServer: {
    command: `node ./e2e/local-managed-server.mjs`,
    cwd: workspaceRoot,
    url: `${baseURL}/api/ready`,
    reuseExistingServer: false,
    timeout: 240_000,
    env: {
      ...process.env,
      E2E_BASE_URL: baseURL,
      E2E_MANAGED_SERVER_STATE_PATH: statePath,
      E2E_MANAGED_BIND_ADDR: baseURL.replace(/^https?:\/\//, ''),
      TEST_DATABASE_URL: process.env.TEST_DATABASE_URL ?? process.env.DATABASE_URL ?? '',
    },
  },
  projects: [
    {
      name: 'chromium',
      use: { ...projectProfiles.chromium },
    },
  ],
  metadata: {
    managedServerStatePath: statePath,
  },
})
