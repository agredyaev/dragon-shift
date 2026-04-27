import { expect, test, type WebSocketRoute } from '@playwright/test'

import {
  advanceWorkshopToVoting,
  cloneSignedInSession,
  createCharacter,
  createWorkshop,
  createWorkshopAndJoinAsHost,
  dismissGameOverOverlay,
  enterHandover,
  enterJudge,
  enterPhase1,
  enterPhase2,
  enterVoting,
  expectNoUnexpectedBootstrapPanels,
  expectNoUnexpectedPreSessionPanels,
  expectToStayOnHome,
  hostJoinOwnWorkshop,
  installUnexpectedPreSessionObserver,
  joinWorkshop,
  newPlayerContext,
  readSessionSnapshot,
  saveHandoverTags,
  signInAccount,
  voteForVisibleDragon,
  waitForNotice,
} from './gameplay-helpers'

async function safeClose(...contexts: Array<{ close: () => Promise<void> }>) {
  await Promise.allSettled(contexts.map(context => context.close()))
}

const lobbyTitlePattern = /Workshop Lobby|Waiting lobby/

test.describe('dragon shift deployed gameplay', () => {
  test('host and guest can advance through the visible workshop flow', async ({ browser }) => {
    test.setTimeout(180_000)
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshopAndJoinAsHost(host.page, 'Alice')
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
      await waitForNotice(host.page, 'Voting finished.')

      await host.page.getByTestId('end-session-button').click()
      await waitForNotice(host.page, 'Game over ready.')
      await dismissGameOverOverlay(host.page, guest.page)

      await expect(host.page.getByTestId('session-panel')).toContainText('Game over')
      await expect(host.page.getByTestId('session-panel')).toContainText('Creativity leaderboard')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Game over')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Creativity leaderboard')

      await expect(host.page.getByTestId('archive-panel')).toContainText('Build the workshop archive')
      await expect(guest.page.getByTestId('archive-panel')).toContainText('Build the workshop archive')

      await host.page.getByTestId('archive-workshop-button').click()
      await waitForNotice(host.page, 'Workshop archive ready.')
      await expect(host.page.getByTestId('archive-panel')).toContainText('Captured final standings')
      await expect(host.page.getByTestId('session-panel')).toContainText('Game over')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Game over')

      await host.page.getByTestId('leave-workshop-button').click()
      await expect(host.page.getByTestId('open-workshops-panel')).toBeVisible()
      const archivedRow = host.page.locator('.roster__item').filter({ hasText: workshopCode }).first()
      await expect(archivedRow).toContainText('Archived', { timeout: 15_000 })
      await expect(archivedRow.getByTestId('join-workshop-button')).toHaveCount(0)
      const reviewResponse = host.page.waitForResponse(response =>
        response.url().includes('/api/workshops/join')
        && response.request().method() === 'POST'
        && response.ok(),
      )
      await archivedRow.getByTestId('review-workshop-button').click()
      await reviewResponse
      await dismissGameOverOverlay(host.page)
      await expect(host.page.getByTestId('session-panel')).toContainText('Game over')
      await expect(host.page.getByTestId('archive-panel')).toContainText('Captured final standings')
    } finally {
      await safeClose(host.context, guest.context)
    }
  })

  test('standalone character creation feeds the later workshop flow', async ({ browser }) => {
    test.setTimeout(180_000)
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      await signInAccount(host.page, 'Alice')
      await createCharacter(host.page, 'A confident coral dragon with striped wings and lantern eyes.')
      await signInAccount(guest.page, 'Bob')
      await createCharacter(guest.page, 'A moss-green dragon with wide fins and a comet tail.')

      const workshopCode = await createWorkshopAndJoinAsHost(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await expect(host.page.locator('.dragon-stage__sprite')).toHaveCount(0)
      await expect(guest.page.locator('.dragon-stage__sprite')).toHaveCount(0)

      await enterPhase1(host.page, guest.page)

      await expect(host.page.locator('.dragon-stage__sprite')).toBeVisible()
      await expect(guest.page.locator('.dragon-stage__sprite')).toBeVisible()

      await enterHandover(host.page, guest.page)
      await saveHandoverTags(host.page, 'calm,dusk,berries')
      await saveHandoverTags(guest.page, 'music,night,playful')

      await enterPhase2(host.page, guest.page)

      await expect(host.page.locator('.dragon-stage__sprite')).toBeVisible()
      await expect(guest.page.locator('.dragon-stage__sprite')).toBeVisible()

      await enterJudge(host.page, guest.page)
      await enterVoting(host.page, guest.page)

      const hostVotingSprites = host.page.locator('.voting-card__sprite-img')
      const guestVotingSprites = guest.page.locator('.voting-card__sprite-img')
      await expect(hostVotingSprites).toHaveCount(8)
      await expect(guestVotingSprites).toHaveCount(8)
    } finally {
      await safeClose(host.context, guest.context)
    }
  })

  test('fresh context can restore the workshop from browser storage bootstrap', async ({ browser }) => {
    const original = await newPlayerContext(browser)
    const reconnect = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)
    const lateJoiner = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshopAndJoinAsHost(original.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      const snapshot = await readSessionSnapshot(original.page)
      await cloneSignedInSession(original.page, reconnect.context, reconnect.page, snapshot)

      await expect(reconnect.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)
      await expect(reconnect.page.getByTestId('connection-badge')).toContainText('Connected')
      await expect(reconnect.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
      await expect(reconnect.page.getByTestId('session-bootstrap-panel')).toHaveCount(0)
      await waitForNotice(reconnect.page, 'Session synced.')

      await joinWorkshop(lateJoiner.page, workshopCode, 'Carol')

      await expect(reconnect.page.getByTestId('session-panel')).toContainText('Carol')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Carol')
    } finally {
      await safeClose(original.context, reconnect.context, guest.context, lateJoiner.context)
    }
  })

  test('same-browser reload restores session context and resyncs realtime updates', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)
    const lateJoiner = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshopAndJoinAsHost(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await expect(host.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)

      await installUnexpectedPreSessionObserver(host.page)
      await host.page.reload()

      await expect(host.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)
      await expect(host.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
      await expect(host.page.getByTestId('connection-badge')).toContainText('Connected')
      await expect(host.page.getByTestId('session-bootstrap-panel')).toHaveCount(0)
      await expectNoUnexpectedPreSessionPanels(host.page)
      await expectNoUnexpectedBootstrapPanels(host.page)
      const snapshot = await readSessionSnapshot(host.page)
      expect(snapshot.sessionCode).toBe(workshopCode)
      expect(snapshot.reconnectToken).toBeTruthy()

      await joinWorkshop(lateJoiner.page, workshopCode, 'Carol')

      await expect(host.page.getByTestId('session-panel')).toContainText('Carol')
      await expect(host.page.getByTestId('connection-badge')).toContainText('Connected')
    } finally {
      await safeClose(host.context, guest.context, lateJoiner.context)
    }
  })

  test('stale saved api target is ignored in favor of the current app origin', async ({ browser }) => {
    const player = await newPlayerContext(browser)

    try {
      const storageKey = 'dragon-switch/platform/api-base-url'
      const staleApiBaseUrl = 'http://127.0.0.1:9'
      const expectedApiBaseUrl = new URL('/', String(test.info().project.use.baseURL)).origin

      await player.context.addInitScript(([key, apiBaseUrl]) => {
        window.localStorage.setItem(key, apiBaseUrl)
      }, [storageKey, staleApiBaseUrl] as const)

      const signInRequest = player.page.waitForRequest(request =>
        request.method() === 'POST' && request.url().includes('/api/auth/signin'),
      )

      await signInAccount(player.page, 'StaleApiGuard')

      expect((await signInRequest).url()).toBe(`${expectedApiBaseUrl}/api/auth/signin`)
    } finally {
      await safeClose(player.context)
    }
  })

  test('stale open-workshop entry shows a visible error and stays on the home flow', async ({ browser }) => {
    const guest = await newPlayerContext(browser)

    try {
      await guest.page.route('**/api/workshops/open', async route => {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({
            workshops: [{
              sessionCode: '999999',
              hostName: 'MissingHost',
              playerCount: 1,
              createdAt: '2026-04-24T00:00:00Z',
            }],
            nextCursor: null,
            prevCursor: null,
          }),
        })
      })
      await guest.page.route('**/api/workshops/999999/eligible-characters', async route => {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ characters: [] }),
        })
      })
      await guest.page.route('**/api/workshops/join', async route => {
        await route.fulfill({
          status: 404,
          contentType: 'application/json',
          body: JSON.stringify({ ok: false, error: 'Workshop not found.' }),
        })
      })

      await guest.page.goto('/')
      await guest.page.getByTestId('signin-name-input').fill('Bob')
      await guest.page.getByTestId('signin-password-input').fill('password-1234')
      await guest.page.getByTestId('signin-submit-button').click()
      await expect(guest.page.getByTestId('open-workshops-panel')).toBeVisible()

      const row = guest.page.locator('.roster__item').filter({ hasText: '999999' }).first()
      await row.getByTestId('join-workshop-button').click()
      await expect(guest.page.getByTestId('pick-character-panel')).toBeVisible()
      await guest.page.getByTestId('use-starter-button').click()

      await waitForNotice(guest.page, 'Workshop not found.')
      await expectToStayOnHome(guest.page)
    } finally {
      await safeClose(guest.context)
    }
  })

  test('guest does not see host-only controls in the lobby', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshopAndJoinAsHost(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await expect(guest.page.getByTestId('controls-panel')).toContainText('hidden')
      await expect(guest.page.getByTestId('start-phase1-button')).toHaveCount(0)
      await expect(host.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)
      await expect(guest.page.getByTestId('session-panel')).toContainText(lobbyTitlePattern)
    } finally {
      await safeClose(host.context, guest.context)
    }
  })

  test('guest-first reserved lobby keeps host controls with creator and remains listed after leave', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await expect(guest.page.getByTestId('controls-panel')).toContainText('hidden')
      await expect(guest.page.getByTestId('start-phase1-button')).toHaveCount(0)

      await hostJoinOwnWorkshop(host.page, workshopCode)
      await expect(host.page.getByTestId('controls-panel')).toContainText('visible')
      await expect(host.page.getByTestId('start-phase1-button')).toBeVisible()
      await expect(guest.page.getByTestId('controls-panel')).toContainText('hidden')
      await expect(guest.page.getByTestId('start-phase1-button')).toHaveCount(0)

      await guest.page.getByTestId('leave-workshop-button').click()
      await host.page.getByTestId('leave-workshop-button').click()
      await expect(host.page.getByTestId('open-workshops-panel')).toBeVisible()
      const row = host.page.locator('.roster__item').filter({ hasText: workshopCode }).first()
      await expect(row).toContainText('2 player(s)', { timeout: 15_000 })
      await expect(row.getByTestId('delete-workshop-button')).toHaveCount(0)
    } finally {
      await safeClose(host.context, guest.context)
    }
  })

  test('account home only shows delete for an owned empty workshop before join', async ({ browser }) => {
    const host = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      const row = host.page.locator('.roster__item').filter({ hasText: workshopCode }).first()

      await expect(row).toContainText('0 player(s)')
      await expect(row.getByTestId('delete-workshop-button')).toBeVisible()
      await expect(row.getByTestId('join-workshop-button')).toBeVisible()

      const refreshedOpenWorkshops = host.page.waitForResponse(response =>
        response.url().includes('/api/workshops/open')
        && response.request().method() === 'GET'
        && response.ok(),
      )

      await hostJoinOwnWorkshop(host.page, workshopCode)
      await host.page.getByTestId('leave-workshop-button').click()
      await refreshedOpenWorkshops
      await expect(host.page.getByTestId('open-workshops-panel')).toBeVisible()

      const joinedRow = host.page.locator('.roster__item').filter({ hasText: workshopCode }).first()
      await expect(joinedRow).toContainText('1 player(s)')
      await expect(joinedRow.getByTestId('delete-workshop-button')).toHaveCount(0)
    } finally {
      await safeClose(host.context)
    }
  })

  test('join network failure shows degraded-path feedback', async ({ browser }) => {
    const guest = await newPlayerContext(browser)

    try {
      await guest.page.route('**/api/workshops/open', async route => {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({
            workshops: [{
              sessionCode: '123456',
              hostName: 'Alice',
              playerCount: 1,
              createdAt: '2026-04-24T00:00:00Z',
            }],
            nextCursor: null,
            prevCursor: null,
          }),
        })
      })
      await guest.page.route('**/api/workshops/123456/eligible-characters', async route => {
        await route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ characters: [] }),
        })
      })
      await guest.page.route('**/api/workshops/join', async route => {
        await route.abort('failed')
      })

      await guest.page.goto('/')
      await guest.page.getByTestId('signin-name-input').fill('Bob')
      await guest.page.getByTestId('signin-password-input').fill('password-1234')
      await guest.page.getByTestId('signin-submit-button').click()
      await expect(guest.page.getByTestId('open-workshops-panel')).toBeVisible()

      const row = guest.page.locator('.roster__item').filter({ hasText: '123456' }).first()
      await row.getByTestId('join-workshop-button').click()
      await expect(guest.page.getByTestId('pick-character-panel')).toBeVisible()
      await guest.page.getByTestId('use-starter-button').click()

      await waitForNotice(guest.page, 'failed to reach backend:')
      await expect(guest.page.getByTestId('pick-character-panel')).toBeVisible()
      await expect(guest.page.getByTestId('open-workshops-panel')).toHaveCount(0)
    } finally {
      await safeClose(guest.context)
    }
  })

  test('disconnected session can resync after page reload', async ({ browser }) => {
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

      const workshopCode = await createWorkshopAndJoinAsHost(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      const realtimeSocket = await hostRealtimeSocket
      await realtimeSocket.close()
      await expect(host.page.getByTestId('connection-badge')).toContainText('Offline')

      await joinWorkshop(lateJoiner.page, workshopCode, 'Carol')
      await expect(host.page.getByTestId('session-panel')).not.toContainText('Carol')

      await installUnexpectedPreSessionObserver(host.page)
      await host.page.reload()
      await waitForNotice(host.page, 'Session synced.')
      await expect(host.page.getByTestId('session-panel')).toContainText('Carol')
      await expect(host.page.getByTestId('session-bootstrap-panel')).toHaveCount(0)
      await expectNoUnexpectedPreSessionPanels(host.page)
      await expectNoUnexpectedBootstrapPanels(host.page)
    } finally {
      await safeClose(host.context, guest.context, lateJoiner.context)
    }
  })
})
