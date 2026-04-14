import { expect, test, type WebSocketRoute } from '@playwright/test'

import {
  advanceWorkshopToVoting,
  createWorkshop,
  expectToStayOnHome,
  gotoApp,
  joinWorkshop,
  newPlayerContext,
  readReconnectToken,
  readSessionSnapshot,
  voteForVisibleDragon,
  waitForNotice,
} from './gameplay-helpers'

async function safeClose(...contexts: Array<{ close: () => Promise<void> }>) {
  await Promise.allSettled(contexts.map(context => context.close()))
}

const lobbyTitlePattern = /Workshop lobby|Waiting lobby/

test.describe('dragon shift deployed gameplay', () => {
  test('host and guest can advance through the visible workshop flow', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await expect(host.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)
      await expect(guest.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)

      await advanceWorkshopToVoting(host.page, guest.page)

      await voteForVisibleDragon(host.page)
      await expect(host.page.getByTestId('session-panel')).toContainText('1 / 2 votes submitted')
      await expect(guest.page.getByTestId('session-panel')).toContainText('1 / 2 votes submitted')

      await voteForVisibleDragon(guest.page)
      await expect(host.page.getByTestId('session-panel')).toContainText('2 / 2 votes submitted')
      await expect(guest.page.getByTestId('session-panel')).toContainText('2 / 2 votes submitted')

      await host.page.getByTestId('reveal-results-button').click()
      await waitForNotice(host.page, 'Voting results revealed.')
      await expect(host.page.getByTestId('session-panel')).toContainText('Workshop results')
      await expect(host.page.getByTestId('session-panel')).toContainText('Creativity Leaderboard')
      await expect(host.page.getByTestId('session-panel')).toContainText('Mechanics leaderboard')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Workshop results')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Creativity Leaderboard')

      await expect(host.page.getByTestId('archive-panel')).toContainText('Build the workshop archive')
      await host.page.getByTestId('build-archive-button').click()
      await waitForNotice(host.page, 'Workshop archive ready.')
      await expect(host.page.getByTestId('archive-panel')).toContainText('Artifacts:')
      await expect(host.page.getByTestId('archive-panel')).toContainText('Captured final standings')
      await expect(host.page.getByTestId('archive-panel')).toContainText('Captured dragons')
      await expect(guest.page.getByTestId('archive-panel')).toContainText('Build the workshop archive')
      await expect(guest.page.getByTestId('build-archive-button')).toHaveCount(0)

      await host.page.getByTestId('reset-workshop-button').click()
      await waitForNotice(host.page, 'Workshop reset.')
      await expect(host.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)
      await expect(guest.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)
    } finally {
      await safeClose(host.context, guest.context)
    }
  })

  test('reconnect flow re-attaches an existing browser session', async ({ browser }) => {
    const original = await newPlayerContext(browser)
    const reconnect = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)
    const lateJoiner = await newPlayerContext(browser)
    let originalClosed = false

    try {
      const workshopCode = await createWorkshop(original.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      const reconnectToken = await readReconnectToken(original.page)

      await original.context.close()
      originalClosed = true

      await gotoApp(reconnect.page)
      await reconnect.page.getByTestId('reconnect-session-code-input').fill(workshopCode)
      await reconnect.page.getByTestId('reconnect-token-input').fill(reconnectToken)
      await reconnect.page.getByTestId('reconnect-button').click()

      await expect(reconnect.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)
      await expect(reconnect.page.getByTestId('connection-badge')).toContainText('Connected')
      await expect(reconnect.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)

      await joinWorkshop(lateJoiner.page, workshopCode, 'Carol')

      await expect(reconnect.page.getByTestId('session-panel')).toContainText('Players in view: 3')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Players in view: 3')
    } finally {
      if (!originalClosed) {
        await safeClose(original.context)
      }
      await safeClose(reconnect.context, guest.context, lateJoiner.context)
    }
  })

  test('same-browser reload restores session context and resyncs realtime updates', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)
    const lateJoiner = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await expect(host.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)

      await host.page.reload()

      await expect(host.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)
      await expect(host.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
      await expect(host.page.getByTestId('connection-badge')).toContainText('Connected')
      const snapshot = await readSessionSnapshot(host.page)
      expect(snapshot.sessionCode).toBe(workshopCode)
      expect(snapshot.reconnectToken).toBeTruthy()

      await joinWorkshop(lateJoiner.page, workshopCode, 'Carol')

      await expect(host.page.getByTestId('session-panel')).toContainText('Players in view: 3')
      await expect(host.page.getByTestId('connection-badge')).toContainText('Connected')
    } finally {
      await safeClose(host.context, guest.context, lateJoiner.context)
    }
  })

  test('invalid join shows a visible error and stays on the home flow', async ({ browser }) => {
    const guest = await newPlayerContext(browser)

    try {
      await gotoApp(guest.page)
      await guest.page.getByTestId('join-session-code-input').fill('999999')
      await guest.page.getByTestId('join-name-input').fill('Bob')
      await guest.page.getByTestId('join-workshop-button').click()

      await waitForNotice(guest.page, 'Workshop not found.')
      await expectToStayOnHome(guest.page)
    } finally {
      await safeClose(guest.context)
    }
  })

  test('invalid reconnect shows a visible error and stays on the home flow', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const reconnect = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')

      await gotoApp(reconnect.page)
      await reconnect.page.getByTestId('reconnect-session-code-input').fill(workshopCode)
      await reconnect.page.getByTestId('reconnect-token-input').fill('invalid-token')
      await reconnect.page.getByTestId('reconnect-button').click()

      await waitForNotice(reconnect.page, 'Session identity is invalid or expired.')
      await expectToStayOnHome(reconnect.page)
    } finally {
      await safeClose(host.context, reconnect.context)
    }
  })

  test('guest does not see host-only controls in the lobby', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await expect(guest.page.getByTestId('controls-panel')).toHaveCount(0)
      await expect(guest.page.getByTestId('start-phase0-button')).toHaveCount(0)
      await expect(host.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)
      await expect(guest.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)
    } finally {
      await safeClose(host.context, guest.context)
    }
  })

  test('join network failure shows degraded-path feedback', async ({ browser }) => {
    const guest = await newPlayerContext(browser)

    try {
      await guest.page.route('**/api/workshops/join', async route => {
        await route.abort('failed')
      })

      await gotoApp(guest.page)
      await guest.page.getByTestId('join-session-code-input').fill('123456')
      await guest.page.getByTestId('join-name-input').fill('Bob')
      await guest.page.getByTestId('join-workshop-button').click()

      await waitForNotice(guest.page, 'failed to reach backend:')
      await expectToStayOnHome(guest.page)
    } finally {
      await safeClose(guest.context)
    }
  })

  test('disconnected session can resync after using the session sync control', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)
    const lateJoiner = await newPlayerContext(browser)
    let resolveHostRealtimeSocket: ((socket: WebSocketRoute) => void) | null = null
    const hostRealtimeSocket = new Promise<WebSocketRoute>(resolve => {
      resolveHostRealtimeSocket = resolve
    })

    try {
      await host.context.routeWebSocket(/\/api\/workshops\/ws$/, ws => {
        ws.connectToServer()
        resolveHostRealtimeSocket?.(ws)
        resolveHostRealtimeSocket = null
      })

      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      const realtimeSocket = await hostRealtimeSocket
      await realtimeSocket.close()
      await expect(host.page.getByTestId('connection-badge')).toContainText('Offline')

      await joinWorkshop(lateJoiner.page, workshopCode, 'Carol')
      await expect(host.page.getByTestId('session-panel')).toContainText('Players in view: 2')

      await host.page.getByTestId('sync-session-button').click()
      await waitForNotice(host.page, 'Session synced.')

      await expect(host.page.getByTestId('session-panel')).toContainText('Players in view: 3')
    } finally {
      await safeClose(host.context, guest.context, lateJoiner.context)
    }
  })

  test('archive build failure shows degraded-path feedback', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await advanceWorkshopToVoting(host.page, guest.page)
      await voteForVisibleDragon(host.page)
      await voteForVisibleDragon(guest.page)
      await host.page.getByTestId('reveal-results-button').click()
      await waitForNotice(host.page, 'Voting results revealed.')

      await host.page.route('**/api/workshops/judge-bundle', async route => {
        await route.abort('failed')
      })

      await host.page.getByTestId('build-archive-button').click()
      await waitForNotice(host.page, 'failed to reach backend:')
      await expect(host.page.getByTestId('archive-panel')).toContainText('Build the workshop archive')
    } finally {
      await safeClose(host.context, guest.context)
    }
  })
})
