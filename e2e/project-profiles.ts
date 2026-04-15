import { devices, type BrowserContextOptions } from '@playwright/test'

type DeviceProfile = (typeof devices)[keyof typeof devices]

export const projectProfiles: Record<string, DeviceProfile> = {
  chromium: devices['Desktop Chrome'],
  'mobile-safari': devices['iPhone 13'],
}

const defaultBaseURL = process.env.E2E_BASE_URL ?? 'http://127.0.0.1:32000'

export function getProjectContextOptions(
  projectName: string,
  baseURL?: string,
): BrowserContextOptions {
  const resolvedBaseURL = baseURL ?? defaultBaseURL

  const profile = projectProfiles[projectName]
  if (!profile) {
    return { baseURL: resolvedBaseURL }
  }

  const { defaultBrowserType: _defaultBrowserType, ...contextOptions } = profile
  return { ...contextOptions, baseURL: resolvedBaseURL }
}
