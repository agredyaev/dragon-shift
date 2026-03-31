import { defineConfig } from '@playwright/test'

import { projectProfiles } from './project-profiles'

const baseURL = process.env.E2E_BASE_URL ?? 'http://127.0.0.1:32000'

export default defineConfig({
  testDir: './tests',
  outputDir: './.tmp/test-results',
  timeout: 60_000,
  expect: {
    timeout: 10_000,
  },
  fullyParallel: false,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  reporter: 'list',
  use: {
    baseURL,
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
  },
  projects: [
    ...Object.entries(projectProfiles).map(([name, profile]) => ({
      name,
      use: { ...profile },
    })),
  ],
})
