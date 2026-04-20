import { defineConfig } from '@playwright/test'

import { projectProfiles } from './project-profiles'

const baseURL = process.env.E2E_BASE_URL ?? 'http://127.0.0.1:4100'

export default defineConfig({
  testDir: './tests',
  testMatch: 'view-validators.spec.ts',
  outputDir: './.tmp/test-results-view-validators',
  timeout: 900_000,
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
      use: { ...projectProfiles.chromium },
    },
  ],
})
