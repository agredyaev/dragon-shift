import { defineConfig } from '@playwright/test'

import { projectProfiles } from './project-profiles'

const baseURL = process.env.E2E_BASE_URL ?? 'https://dragon-shift.34.54.200.112.nip.io'

export default defineConfig({
  testDir: './tests',
  testMatch: 'e2e-scenario.spec.ts',
  outputDir: './.tmp/test-results',
  timeout: 480_000,
  expect: {
    timeout: 30_000,
  },
  fullyParallel: false,
  retries: 0,
  reporter: 'list',
  use: {
    baseURL,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: { ...projectProfiles['chromium'] },
    },
  ],
})
