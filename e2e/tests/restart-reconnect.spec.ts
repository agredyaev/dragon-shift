import { existsSync, readFileSync } from 'node:fs'

import { expect, test, type APIRequestContext } from '@playwright/test'

import {
  cloneSignedInSession,
  createCharacter,
  createWorkshopAndJoinAsHost,
  joinWorkshop,
  newPlayerContext,
  readSessionSnapshot,
  saveHandoverTags,
  signInAccount,
  voteForVisibleDragon,
  waitForNotice,
} from './gameplay-helpers'

type ManagedServerState = {
  managerPid: number
  childPid: number | null
  bindAddr: string
  baseUrl: string
  databaseUrl: string
}

function managedServerStatePath() {
  const configured = process.env.E2E_MANAGED_SERVER_STATE_PATH
  if (!configured) {
    throw new Error('E2E_MANAGED_SERVER_STATE_PATH is required for restart proof.')
  }
  return configured
}

function readManagedState(): ManagedServerState {
  const statePath = managedServerStatePath()
  if (!existsSync(statePath)) {
    throw new Error(`managed server state file is missing: ${statePath}`)
  }
  return JSON.parse(readFileSync(statePath, 'utf8')) as ManagedServerState
}

async function waitForManagedState(
  predicate: (state: ManagedServerState) => boolean,
  timeoutMs: number,
) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    try {
      const state = readManagedState()
      if (predicate(state)) {
        return state
      }
    } catch {
      // manager rewrites the file during restart; retry until settled
    }
    await new Promise(resolve => setTimeout(resolve, 200))
  }
  throw new Error('timed out waiting for managed server state update')
}

async function waitForReady(request: APIRequestContext, baseUrl: string) {
  await expect
    .poll(
      async () => {
        try {
          const response = await request.get(`${baseUrl}/api/ready`)
          return response.ok()
        } catch {
          return false
        }
      },
      { timeout: 30_000, intervals: [250, 500, 1000] },
    )
    .toBe(true)
}

test.describe('browser restart reconnect proof', () => {
  test.skip(!process.env.E2E_MANAGED_SERVER_STATE_PATH, 'managed local restart harness only')

  test('restart -> storage bootstrap reconnect -> continued realtime correctness end-to-end', async ({ browser, request }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)
    const reconnect = await newPlayerContext(browser)

    try {
      await signInAccount(host.page, 'Alice')
      await createCharacter(host.page, 'A lantern-scaled dragon with ember whiskers.')
      await signInAccount(guest.page, 'Bob')
      await createCharacter(guest.page, 'A mint dragon with ribbon fins and calm eyes.')

      const workshopCode = await createWorkshopAndJoinAsHost(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await host.page.getByTestId('start-phase1-button').click()
      await waitForNotice(host.page, 'Phase 1 started.')
      await expect(host.page.getByTestId('session-panel')).toContainText('Phase 1: Discovery')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Phase 1: Discovery')

      await host.page.getByTestId('start-handover-button').click()
      await waitForNotice(host.page, 'Handover started.')
      await expect(host.page.locator('body')).toContainText('Shift Change!')
      await expect(guest.page.locator('body')).toContainText('Shift Change!')

      await saveHandoverTags(host.page, 'calm,dusk,berries')
      await expect(host.page.locator('body')).toContainText('berries')

      const snapshot = await readSessionSnapshot(host.page)
      const beforeRestart = readManagedState()

      process.kill(beforeRestart.managerPid, 'SIGHUP')

      await waitForManagedState(
        state => state.childPid !== null && state.childPid !== beforeRestart.childPid,
        30_000,
      )
      await waitForReady(request, beforeRestart.baseUrl)

      await expect(host.page.getByTestId('connection-badge')).toContainText('Offline')

      await cloneSignedInSession(host.page, reconnect.context, reconnect.page, snapshot)

      await expect(reconnect.page.getByTestId('connection-badge')).toContainText('Connected')
      await waitForNotice(reconnect.page, 'Session synced.')
      await expect(reconnect.page.locator('body')).toContainText('Shift Change!')
      await expect(reconnect.page.locator('body')).toContainText('berries')

      await expect(guest.page.getByTestId('connection-badge')).toContainText('Offline')
      await guest.page.reload()
      await waitForNotice(guest.page, 'Session synced.')
      await expect(guest.page.getByTestId('connection-badge')).toContainText('Connected')
      await expect(guest.page.locator('body')).toContainText('Shift Change!')

      await saveHandoverTags(guest.page, 'music,night,playful')
      await expect(reconnect.page.locator('body')).toContainText('Shift Change!')

      await reconnect.page.getByTestId('start-phase2-button').click()
      await waitForNotice(reconnect.page, 'Phase 2 started.')
      await expect(reconnect.page.locator('body')).toContainText('Phase 2: New Shift')
      await expect(guest.page.locator('body')).toContainText('Phase 2: New Shift')

      await reconnect.page.getByTestId('end-game-button').click()
      await waitForNotice(reconnect.page, 'Scoring opened.')
      await expect(reconnect.page.locator('body')).toContainText('Vote for the most creative dragon design')
      await expect(guest.page.locator('body')).toContainText('Vote for the most creative dragon design')

      await expect(reconnect.page.locator('body')).toContainText('0 / 2 votes submitted')

      await voteForVisibleDragon(guest.page)
      await expect(reconnect.page.locator('body')).toContainText('1 / 2 votes submitted')
      await expect(guest.page.locator('body')).toContainText('1 / 2 votes submitted')

      await voteForVisibleDragon(reconnect.page)
      await expect(reconnect.page.locator('body')).toContainText('2 / 2 votes submitted')
      await expect(guest.page.locator('body')).toContainText('2 / 2 votes submitted')

      await reconnect.page.getByTestId('reveal-results-button').click()
      await waitForNotice(reconnect.page, 'Voting finished.')
      await expect(reconnect.page.locator('body')).toContainText('Creativity leaderboard')
      await expect(guest.page.locator('body')).toContainText('Creativity leaderboard')
    } finally {
      await host.context.close()
      await guest.context.close()
      await reconnect.context.close()
    }
  })
})
