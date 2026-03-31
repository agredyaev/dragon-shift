import { expect, test, type WebSocketRoute } from '@playwright/test'

import {
  advanceWorkshopToVoting,
  createWorkshop,
  expectToStayOnHome,
  gotoApp,
  joinWorkshop,
  newPlayerContext,
  voteForVisibleDragon,
  waitForNotice,
} from './gameplay-helpers'

test.describe('dragon shift deployed gameplay', () => {
  test('host and guest can advance through the visible workshop flow', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await expect(host.page.getByTestId('session-panel')).toContainText('Workshop lobby')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Workshop lobby')

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
      await expect(host.page.getByTestId('session-panel')).toContainText('Creative pet awards')
      await expect(host.page.getByTestId('session-panel')).toContainText('Final player standings')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Workshop results')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Creative pet awards')

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
      await expect(host.page.getByTestId('session-panel')).toContainText('Workshop lobby')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Workshop lobby')
    } finally {
      await host.context.close()
      await guest.context.close()
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

      const reconnectTokenInput = original.page.getByTestId('reconnect-token-input')
      await expect(reconnectTokenInput).toHaveValue(/.+/)
      const reconnectToken = await reconnectTokenInput.inputValue()

      await original.context.close()
      originalClosed = true

      await gotoApp(reconnect.page)
      await reconnect.page.getByTestId('reconnect-session-code-input').fill(workshopCode)
      await reconnect.page.getByTestId('reconnect-token-input').fill(reconnectToken)
      await reconnect.page.getByTestId('reconnect-button').click()

      await expect(reconnect.page.getByTestId('session-panel')).toContainText('Workshop lobby')
      await expect(reconnect.page.getByTestId('connection-badge')).toContainText('Connected')
      await expect(reconnect.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)

      await joinWorkshop(lateJoiner.page, workshopCode, 'Carol')

      await expect(reconnect.page.getByTestId('session-panel')).toContainText('Players in view: 3')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Players in view: 3')
    } finally {
      if (!originalClosed) {
        await original.context.close()
      }
      await reconnect.context.close()
      await guest.context.close()
      await lateJoiner.context.close()
    }
  })

  test('same-browser reload restores session context and resyncs realtime updates', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)
    const lateJoiner = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await expect(host.page.getByTestId('session-panel')).toContainText('Workshop lobby')

      await host.page.reload()

      await expect(host.page.getByTestId('session-panel')).toContainText('Workshop lobby')
      await expect(host.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
      await expect(host.page.getByTestId('connection-badge')).toContainText('Connected')
      await expect(host.page.getByTestId('reconnect-session-code-input')).toHaveValue(workshopCode)
      await expect(host.page.getByTestId('reconnect-token-input')).toHaveValue(/.+/)

      await joinWorkshop(lateJoiner.page, workshopCode, 'Carol')

      await expect(host.page.getByTestId('session-panel')).toContainText('Players in view: 3')
      await expect(host.page.getByTestId('connection-badge')).toContainText('Connected')
    } finally {
      await host.context.close()
      await guest.context.close()
      await lateJoiner.context.close()
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
      await guest.context.close()
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
      await host.context.close()
      await reconnect.context.close()
    }
  })

  test('guest host-only rejection shows a visible error notice', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await guest.page.getByTestId('start-phase1-button').click()

      await waitForNotice(guest.page, 'Only the host can start the workshop.')
      await expect(host.page.getByTestId('session-panel')).toContainText('Workshop lobby')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Workshop lobby')
    } finally {
      await host.context.close()
      await guest.context.close()
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
      await guest.context.close()
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
      await host.context.close()
      await guest.context.close()
      await lateJoiner.context.close()
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
      await host.context.close()
      await guest.context.close()
    }
  })
})
