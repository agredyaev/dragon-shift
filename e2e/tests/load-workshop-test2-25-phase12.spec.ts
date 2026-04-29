import { expect, test } from '@playwright/test'

test.describe('load harness configuration for load2-load25 on external workshop', () => {
  test('documents required env for manual phase1/phase2 coverage run', async () => {
    expect({
      baseUrl: process.env.E2E_BASE_URL ?? 'http://127.0.0.1:4100',
      externalWorkshopCode: process.env.E2E_EXTERNAL_WORKSHOP_CODE ?? '988875',
      clientCount: Number(process.env.E2E_CLIENT_COUNT ?? 24),
      clientNamePrefix: process.env.E2E_CLIENT_NAME_PREFIX ?? 'load',
      clientNameOffset: Number(process.env.E2E_CLIENT_NAME_OFFSET ?? 2),
      clientPassword: process.env.E2E_CLIENT_PASSWORD ?? '<set-me>',
      allowAccountCreation: process.env.E2E_ALLOW_ACCOUNT_CREATION ?? 'false',
      allowStarterFallback: process.env.E2E_ALLOW_STARTER_FALLBACK ?? 'true',
      hostAccountName: process.env.E2E_HOST_ACCOUNT_NAME ?? 'test1',
      coverageTarget: process.env.E2E_COVERAGE_TARGET ?? 'phase2',
    }).toEqual({
      baseUrl: process.env.E2E_BASE_URL ?? 'http://127.0.0.1:4100',
      externalWorkshopCode: process.env.E2E_EXTERNAL_WORKSHOP_CODE ?? '988875',
      clientCount: Number(process.env.E2E_CLIENT_COUNT ?? 24),
      clientNamePrefix: process.env.E2E_CLIENT_NAME_PREFIX ?? 'load',
      clientNameOffset: Number(process.env.E2E_CLIENT_NAME_OFFSET ?? 2),
      clientPassword: process.env.E2E_CLIENT_PASSWORD ?? '<set-me>',
      allowAccountCreation: process.env.E2E_ALLOW_ACCOUNT_CREATION ?? 'false',
      allowStarterFallback: process.env.E2E_ALLOW_STARTER_FALLBACK ?? 'true',
      hostAccountName: process.env.E2E_HOST_ACCOUNT_NAME ?? 'test1',
      coverageTarget: process.env.E2E_COVERAGE_TARGET ?? 'phase2',
    })
  })
})
