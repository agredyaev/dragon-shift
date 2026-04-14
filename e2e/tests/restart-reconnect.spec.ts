import { existsSync, readFileSync } from 'node:fs'

import { expect, test, type APIRequestContext } from '@playwright/test'

import {
  createWorkshop,
  joinWorkshop,
  newPlayerContext,
  readReconnectToken,
  saveDragonProfile,
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

  test('restart -> reconnect -> continued realtime correctness end-to-end', async ({ browser, request }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)
    const reconnect = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await host.page.getByTestId('start-phase0-button').click()
      await waitForNotice(host.page, 'Character creation opened.')
      await expect(host.page.getByTestId('session-panel')).toContainText('Character creation')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Character creation')

      await saveDragonProfile(host.page, 'A lantern-scaled dragon with ember whiskers.')
      await saveDragonProfile(guest.page, 'A mint dragon with ribbon fins and calm eyes.')

      await host.page.getByTestId('start-phase1-button').click()
      await waitForNotice(host.page, 'Phase 1 started.')
      await expect(host.page.getByTestId('session-panel')).toContainText('Discovery round')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Discovery round')

      await host.page.getByTestId('start-handover-button').click()
      await waitForNotice(host.page, 'Handover started.')
      await expect(host.page.getByTestId('session-panel')).toContainText('Handover')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Handover')

      await host.page.getByTestId('handover-tags-input').fill('calm,dusk,berries')
      await host.page.getByTestId('save-handover-tags-button').click()
      await waitForNotice(host.page, 'Handover tags saved.')
      await expect(host.page.getByTestId('session-panel')).toContainText('3 / 3 handover rules saved')

      const reconnectToken = await readReconnectToken(host.page)
      const beforeRestart = readManagedState()

      process.kill(beforeRestart.managerPid, 'SIGHUP')

      await waitForManagedState(
        state => state.childPid !== null && state.childPid !== beforeRestart.childPid,
        30_000,
      )
      await waitForReady(request, beforeRestart.baseUrl)

      await expect(host.page.getByTestId('connection-badge')).toContainText('Offline')

      await reconnect.page.goto('/')
      await reconnect.page.getByTestId('reconnect-session-code-input').fill(workshopCode)
      await reconnect.page.getByTestId('reconnect-token-input').fill(reconnectToken)
      await reconnect.page.getByTestId('reconnect-button').click()

      await expect(reconnect.page.getByTestId('connection-badge')).toContainText('Connected')
      await waitForNotice(reconnect.page, 'Reconnected to workshop.')
      await expect(reconnect.page.getByTestId('session-panel')).toContainText('Handover')
      await expect(reconnect.page.getByTestId('session-panel')).toContainText('3 / 3 handover rules saved')
      await expect(reconnect.page.getByTestId('session-panel')).toContainText('berries')

      await expect(guest.page.getByTestId('connection-badge')).toContainText('Offline')
      await guest.page.getByTestId('sync-session-button').click()
      await waitForNotice(guest.page, 'Session synced.')
      await expect(guest.page.getByTestId('connection-badge')).toContainText('Connected')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Handover')

      await guest.page.getByTestId('handover-tags-input').fill('music,night,playful')
      await guest.page.getByTestId('save-handover-tags-button').click()
      await waitForNotice(guest.page, 'Handover tags saved.')
      await expect(reconnect.page.getByTestId('session-panel')).toContainText('Handover')

      await reconnect.page.getByTestId('start-phase2-button').click()
      await waitForNotice(reconnect.page, 'Phase 2 started.')
      await expect(reconnect.page.getByTestId('session-panel')).toContainText('Care round')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Care round')

      await reconnect.page.getByTestId('end-game-button').click()
      await waitForNotice(reconnect.page, 'Judge review started.')
      await expect(reconnect.page.getByTestId('session-panel')).toContainText('Judge review')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Judge review')

      await reconnect.page.getByTestId('start-voting-button').click()
      await waitForNotice(reconnect.page, 'Design voting started.')
      await expect(reconnect.page.getByTestId('session-panel')).toContainText('Design voting')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Design voting')
      await expect(reconnect.page.getByTestId('session-panel')).toContainText('0 / 2 votes submitted')

      await voteForVisibleDragon(guest.page)
      await expect(reconnect.page.getByTestId('session-panel')).toContainText('1 / 2 votes submitted')
      await expect(guest.page.getByTestId('session-panel')).toContainText('1 / 2 votes submitted')

      await voteForVisibleDragon(reconnect.page)
      await expect(reconnect.page.getByTestId('session-panel')).toContainText('2 / 2 votes submitted')
      await expect(guest.page.getByTestId('session-panel')).toContainText('2 / 2 votes submitted')
    } finally {
      await host.context.close()
      await guest.context.close()
      await reconnect.context.close()
    }
  })
})
