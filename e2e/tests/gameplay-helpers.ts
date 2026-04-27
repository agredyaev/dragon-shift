import { expect, test, type Browser, type BrowserContext, type Page, type Response } from '@playwright/test'

import { getProjectContextOptions } from '../project-profiles'

const SESSION_SNAPSHOT_STORAGE_KEY = 'dragon-switch/platform/session-snapshot'
const SESSION_GAME_STATE_STORAGE_KEY = 'dragon-switch/platform/session-game-state'

type SessionSnapshot = {
  sessionCode: string
  reconnectToken: string
  playerId: string
  coordinatorType: string
}

export async function readSessionSnapshot(page: Page): Promise<SessionSnapshot> {
  const snapshot = await page.evaluate(storageKey => {
    const raw = window.sessionStorage.getItem(storageKey)
    if (!raw) {
      return null
    }

    return JSON.parse(raw)
  }, SESSION_SNAPSHOT_STORAGE_KEY)

  if (!snapshot) {
    throw new Error('session snapshot is missing from browser sessionStorage')
  }

  return snapshot as SessionSnapshot
}

export async function readReconnectToken(page: Page) {
  const snapshot = await readSessionSnapshot(page)
  return snapshot.reconnectToken
}

export async function readSessionGameState(page: Page) {
  const state = await page.evaluate(storageKey => {
    const raw = window.sessionStorage.getItem(storageKey)
    if (!raw) {
      return null
    }

    return JSON.parse(raw)
  }, SESSION_GAME_STATE_STORAGE_KEY)

  if (!state) {
    throw new Error('session game state is missing from browser sessionStorage')
  }

  return state as {
    phase?: string
    session?: { code?: string }
  }
}

export async function newPlayerContext(
  browser: Browser,
): Promise<{ context: BrowserContext; page: Page }> {
  const context = await browser.newContext(
    getProjectContextOptions(test.info().project.name, test.info().project.use.baseURL),
  )
  const page = await context.newPage()
  return { context, page }
}

export async function gotoApp(page: Page) {
  await page.goto('/')
  await Promise.race([
    page.getByTestId('signin-panel').waitFor({ state: 'visible', timeout: 15_000 }),
    page.getByTestId('open-workshops-panel').waitFor({ state: 'visible', timeout: 15_000 }),
    page.getByTestId('lobby-panel').waitFor({ state: 'visible', timeout: 15_000 }),
  ])
}

export async function signInAccount(page: Page, name: string, password = 'password-1234') {
  await gotoApp(page)
  if (await page.getByTestId('signin-panel').count()) {
    await page.getByTestId('signin-name-input').fill(name)
    await page.getByTestId('signin-password-input').fill(password)
    await page.getByTestId('signin-submit-button').click()
  }
  await expect(page.getByTestId('open-workshops-panel')).toBeVisible()
}

export async function waitForNotice(page: Page, text: string) {
  const aliases: Record<string, string[]> = {
    'Scoring opened.': ['Opening design voting…'],
    'Voting finished.': ['Finishing voting…'],
  }

  const accepted = [text, ...(aliases[text] ?? [])]
  const notice = page.getByTestId('notice-bar')
  const deadline = Date.now() + 15_000

  while (Date.now() < deadline) {
    if (await notice.count()) {
      const message = (await notice.textContent()) ?? ''
      if (accepted.some(candidate => message.includes(candidate))) {
        return
      }
    }
    await page.waitForTimeout(200)
  }

  throw new Error(`notice did not match any expected text: ${accepted.join(' | ')}`)
}

export async function expectPhaseVisible(pages: Page[], text: string, timeout = 15_000) {
  for (const page of pages) {
    await expect(page.locator('body')).toContainText(text, { timeout })
  }
}

export async function installUnexpectedPreSessionObserver(page: Page) {
  await page.addInitScript(() => {
    const windowWithFlag = window as Window & {
      __unexpectedPreSessionPanels?: string[]
      __unexpectedPreSessionObserverInstalled?: boolean
      __unexpectedBootstrapPanels?: string[]
    }

    if (windowWithFlag.__unexpectedPreSessionObserverInstalled) {
      return
    }

    windowWithFlag.__unexpectedPreSessionObserverInstalled = true
    windowWithFlag.__unexpectedPreSessionPanels = []
    windowWithFlag.__unexpectedBootstrapPanels = []

    const collect = () => {
      const matches = [
        ['signin-panel', '[data-testid="signin-panel"]'],
        ['open-workshops-panel', '[data-testid="open-workshops-panel"]'],
        ['pick-character-panel', '[data-testid="pick-character-panel"]'],
      ] as const

      for (const [name, selector] of matches) {
        if (document.querySelector(selector)) {
          windowWithFlag.__unexpectedPreSessionPanels?.push(name)
        }
      }

      if (document.querySelector('[data-testid="session-bootstrap-panel"]')) {
        windowWithFlag.__unexpectedBootstrapPanels?.push('session-bootstrap-panel')
      }
    }

    collect()
    new MutationObserver(collect).observe(document.documentElement, {
      childList: true,
      subtree: true,
      attributes: true,
    })
  })
}

export async function expectNoUnexpectedPreSessionPanels(page: Page) {
  const panels = await page.evaluate(() => {
    const windowWithFlag = window as Window & {
      __unexpectedPreSessionPanels?: string[]
    }
    return windowWithFlag.__unexpectedPreSessionPanels ?? []
  })

  expect(panels, `unexpected pre-session panels appeared: ${panels.join(', ')}`).toEqual([])
}

export async function expectNoUnexpectedBootstrapPanels(page: Page) {
  const panels = await page.evaluate(() => {
    const windowWithFlag = window as Window & {
      __unexpectedBootstrapPanels?: string[]
    }
    return windowWithFlag.__unexpectedBootstrapPanels ?? []
  })

  expect(panels, `unexpected bootstrap panels appeared: ${panels.join(', ')}`).toEqual([])
}

export async function extractWorkshopCode(page: Page) {
  const row = page.locator('.roster__item').first()
  await expect(row).toBeVisible()
  const text = (await row.textContent()) ?? ''
  const match = text.match(/Code:\s*(\d{6})/)
  if (!match) {
    throw new Error(`failed to extract workshop code from open workshops row: ${text}`)
  }
  return match[1]
}

export async function createWorkshop(page: Page, hostName: string) {
  await signInAccount(page, hostName)
  await page.getByTestId('create-workshop-button').click()
  const notice = page.getByTestId('notice-bar')
  await expect(notice).toContainText('Workshop ')
  const noticeText = (await notice.textContent()) ?? ''
  const match = noticeText.match(/Workshop\s+(\d{6})\s+created\./)
  if (!match) {
    throw new Error(`failed to extract workshop code from create notice: ${noticeText}`)
  }
  await expect(page.getByTestId('open-workshops-panel')).toBeVisible()
  return match[1]
}

export async function createWorkshopAndJoinAsHost(page: Page, hostName: string) {
  const workshopCode = await createWorkshop(page, hostName)
  await hostJoinOwnWorkshop(page, workshopCode)
  return workshopCode
}

function waitForEligibleCharactersResponse(page: Page, workshopCode: string): Promise<Response> {
  return page.waitForResponse(response =>
    response.url().includes(`/api/workshops/${workshopCode}/eligible-characters`)
    && response.request().method() === 'GET'
    && response.ok(),
  )
}

async function completeWorkshopJoin(
  page: Page,
  workshopCode: string,
  eligibleCharactersResponse: Promise<Response>,
) {
  await expect(page.getByTestId('pick-character-panel')).toBeVisible()
  const eligibleResponse = await eligibleCharactersResponse
  const eligiblePayload = await eligibleResponse.json() as { characters?: unknown[] }
  const hasOwnedCharacters = (eligiblePayload.characters?.length ?? 0) > 0

  const joinResponse = page.waitForResponse(response =>
    response.url().includes('/api/workshops/join')
    && response.request().method() === 'POST',
  )

  if (hasOwnedCharacters) {
    const selectCharacterButton = page.getByTestId('select-character-button').first()
    await expect(selectCharacterButton).toBeVisible()
    await selectCharacterButton.click()
  } else {
    await page.getByTestId('use-starter-button').click()
  }

  const join = await joinResponse
  expect(join.status(), `join workshop failed for ${workshopCode}`).toBe(200)

  await expect(page.getByTestId('session-panel')).toBeVisible({ timeout: 15_000 })
  await expect(page.getByTestId('lobby-panel')).toBeVisible({ timeout: 15_000 })
  await expect(page.getByTestId('connection-badge')).toContainText('Connected')
  await waitForNotice(page, 'Session synced.')
  await expect(page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
}

export async function joinWorkshop(page: Page, workshopCode: string, playerName: string) {
  await signInAccount(page, playerName)
  const row = page.locator('.roster__item').filter({ hasText: workshopCode }).first()
  await expect(row).toBeVisible({ timeout: 15_000 })
  const eligibleCharactersResponse = waitForEligibleCharactersResponse(page, workshopCode)
  await row.getByTestId('join-workshop-button').click()
  await completeWorkshopJoin(page, workshopCode, eligibleCharactersResponse)
}

export async function hostJoinOwnWorkshop(page: Page, workshopCode: string) {
  const row = page.locator('.roster__item').filter({ hasText: workshopCode }).first()
  await expect(row).toBeVisible({ timeout: 15_000 })
  const eligibleCharactersResponse = waitForEligibleCharactersResponse(page, workshopCode)
  await row.getByTestId('join-workshop-button').click()
  await completeWorkshopJoin(page, workshopCode, eligibleCharactersResponse)
}

export async function createCharacter(page: Page, description: string) {
  const ownedCharacterCount = await page.evaluate(async () => {
    const response = await fetch('/api/characters/mine', {
      credentials: 'same-origin',
    })

    if (!response.ok) {
      throw new Error(`failed to load owned characters: ${response.status}`)
    }

    const payload = await response.json() as { characters?: unknown[] }
    return payload.characters?.length ?? 0
  })

  if (ownedCharacterCount > 0) {
    return
  }

  await page.getByTestId('create-character-button').click()
  await expect(page.getByTestId('create-character-panel')).toBeVisible()
  await page.getByTestId('character-description-input').fill(description)
  await generateDragonSprites(page)
  await saveCharacter(page)
  await expect(page.getByTestId('open-workshops-panel')).toBeVisible()
}

export async function openCharacterCreation(...pages: Page[]) {
  await Promise.all(pages.map(async page => {
    await page.getByTestId('create-character-button').click()
    await expect(page.getByTestId('create-character-panel')).toBeVisible()
  }))
}

export async function saveDragonProfile(page: Page, description: string) {
  await expect(page.getByTestId('create-character-panel')).toBeVisible()
  await page.getByTestId('character-description-input').fill(description)
  await generateDragonSprites(page)
  await saveCharacter(page)
}

export async function bootstrapSessionFromSnapshot(
  page: Page,
  snapshot: SessionSnapshot,
  sessionGameState: { phase?: string; session?: { code?: string } },
  accountName: string,
  password = 'password-1234',
) {
  await page.context().addInitScript(
    ([sessionStorageKey, sessionGameStateStorageKey, accountStorageKey, sessionSnapshot, gameState, name, hero, pwd]) => {
      window.sessionStorage.setItem(sessionStorageKey, JSON.stringify(sessionSnapshot))
      window.sessionStorage.setItem(sessionGameStateStorageKey, JSON.stringify(gameState))
      window.localStorage.setItem(accountStorageKey, JSON.stringify({
        id: `e2e-${name}`,
        hero,
        name,
      }))
      window.localStorage.setItem('dragon-switch/e2e-bootstrap-password', pwd)
    },
    [
      SESSION_SNAPSHOT_STORAGE_KEY,
      SESSION_GAME_STATE_STORAGE_KEY,
      'dragon-switch/platform/account-snapshot',
      snapshot,
      sessionGameState,
      accountName,
      accountName,
      password,
    ] as const,
  )
  await installUnexpectedPreSessionObserver(page)
  await page.goto('/')
  await expect(page.getByTestId('connection-badge')).toContainText('Connected', { timeout: 15_000 })
  await expect(page.getByTestId('session-bootstrap-panel')).toHaveCount(0)
  await expect(page.getByTestId('session-panel')).toBeVisible({ timeout: 15_000 })
  await expectNoUnexpectedPreSessionPanels(page)
  await expectNoUnexpectedBootstrapPanels(page)
}

export async function cloneSignedInSession(
  sourcePage: Page,
  targetContext: BrowserContext,
  targetPage: Page,
  snapshot: SessionSnapshot,
) {
  const storage = await sourcePage.evaluate(() => ({
    account: window.localStorage.getItem('dragon-switch/platform/account-snapshot'),
    gameState: window.sessionStorage.getItem('dragon-switch/platform/session-game-state'),
  }))

  if (!storage.account) {
    throw new Error('account snapshot is missing from localStorage')
  }

  const sourceCookies = await sourcePage.context().cookies()
  if (sourceCookies.length > 0) {
    await targetContext.addCookies(sourceCookies)
  }

  await targetContext.addInitScript(
    ([accountSnapshot, sessionSnapshot, gameState]) => {
      window.localStorage.setItem('dragon-switch/platform/account-snapshot', accountSnapshot)
      window.sessionStorage.setItem(
        'dragon-switch/platform/session-snapshot',
        JSON.stringify(sessionSnapshot),
      )
      if (gameState) {
        window.sessionStorage.setItem('dragon-switch/platform/session-game-state', gameState)
      }
    },
    [storage.account, snapshot, storage.gameState] as const,
  )
  await installUnexpectedPreSessionObserver(targetPage)
  await targetPage.goto('/')
  await expect(targetPage.getByTestId('connection-badge')).toContainText('Connected', { timeout: 15_000 })
  await expectNoUnexpectedPreSessionPanels(targetPage)
  await expectNoUnexpectedBootstrapPanels(targetPage)
}

export async function saveCharacter(page: Page) {
  const saveResponse = page.waitForResponse(response =>
    response.url().includes('/api/characters')
    && response.request().method() === 'POST'
    && response.status() === 201,
  )

  await page.getByTestId('save-character-button').click()
  await saveResponse
  await waitForNotice(page, 'Character created.')
}

export async function generateDragonSprites(page: Page, timeout = 120_000) {
  await page.getByTestId('generate-sprites-button').click()
  const previewImages = page.locator('.sprite-grid__image')
  await expect(previewImages).toHaveCount(4, { timeout })
  await expect(previewImages.first()).toBeVisible({ timeout })
  await expect(page.getByTestId('save-character-button')).toBeVisible({ timeout })
}

export async function expectToStayOnHome(page: Page) {
  await expect(page.getByTestId('open-workshops-panel')).toBeVisible()
  await expect(page.getByTestId('lobby-panel')).toHaveCount(0)
  await expect(page.getByTestId('connection-badge')).toHaveCount(0)
}

export async function voteForVisibleDragon(page: Page) {
  const voteButtons = page.locator('[data-testid^="vote-button-"]')
  await expect(voteButtons.first()).toBeVisible()
  const count = await voteButtons.count()

  for (let i = 0; i < count; i++) {
    const button = voteButtons.nth(i)
    await button.click()

    await page.waitForTimeout(500)
    const notice = page.getByTestId('notice-bar')
    const noticeText = (await notice.count()) ? (await notice.textContent()) ?? '' : ''
    if (noticeText.toLowerCase().includes('cannot vote for your own')) {
      continue
    }

    return
  }

  throw new Error('no valid vote button found (all rejected as self-vote)')
}

export async function dismissGameOverOverlay(...pages: Page[]) {
  for (const page of pages) {
    const overlay = page.getByTestId('game-over-overlay')
    await expect(overlay).toBeVisible({ timeout: 45_000 })
    await page.getByTestId('game-over-continue-button').click()
    await expect(overlay).toHaveCount(0)
  }
}

export async function enterPhase1(hostPage: Page, ...otherPages: Page[]) {
  await hostPage.getByTestId('start-phase1-button').click()
  await waitForNotice(hostPage, 'Phase 1 started.')
  for (const page of [hostPage, ...otherPages]) {
    await expect(page.getByTestId('observation-input')).toBeVisible()
    await expect(page.locator('.dragon-stage')).toBeVisible()
  }
}

export async function enterHandover(hostPage: Page, ...otherPages: Page[]) {
  await hostPage.getByTestId('start-handover-button').click()
  await waitForNotice(hostPage, 'Handover started.')
  for (const page of [hostPage, ...otherPages]) {
    await expect(page.getByTestId('handover-rule-1')).toBeVisible()
  }
}

export async function saveHandoverTags(page: Page, tags: string) {
  const [rule1 = '', rule2 = '', rule3 = ''] = tags
    .split(',')
    .map(part => part.trim())
    .filter(part => part.length > 0)

  await page.getByTestId('handover-rule-1').fill(rule1)
  await page.getByTestId('handover-rule-2').fill(rule2)
  await page.getByTestId('handover-rule-3').fill(rule3)
  await page.getByTestId('save-handover-tags-button').click()
  await waitForNotice(page, 'Handover tags saved.')
}

export async function enterPhase2(hostPage: Page, ...otherPages: Page[]) {
  await hostPage.getByTestId('start-phase2-button').click()
  await waitForNotice(hostPage, 'Phase 2 started.')
  for (const page of [hostPage, ...otherPages]) {
    await expect(page.locator('.phase2-creator-label')).toBeVisible()
    await expect(page.getByTestId('action-feed-meat')).toBeVisible()
  }
}

export async function enterJudge(hostPage: Page, ...otherPages: Page[]) {
  await hostPage.getByTestId('end-game-button').click()
  await waitForNotice(hostPage, 'Scoring opened.')
  for (const page of [hostPage, ...otherPages]) {
    await expect(page.locator('body')).toContainText('Scoring', { timeout: 120_000 })
  }
  await expect(hostPage.getByTestId('end-session-button')).toBeVisible({ timeout: 120_000 })
}

export async function enterVoting(hostPage: Page, ...otherPages: Page[]) {
  for (const page of [hostPage, ...otherPages]) {
    await expect(page.locator('.voting-grid')).toBeVisible()
    await expect(page.locator('body')).toContainText('Vote for the most creative dragon design')
  }
}

export async function advanceWorkshopToVoting(hostPage: Page, guestPage: Page) {
  await enterPhase1(hostPage, guestPage)
  await enterHandover(hostPage, guestPage)
  await saveHandoverTags(hostPage, 'calm,dusk,berries')
  await saveHandoverTags(guestPage, 'music,night,playful')
  await enterPhase2(hostPage, guestPage)
  await enterJudge(hostPage, guestPage)
  await enterVoting(hostPage, guestPage)
  await expect(hostPage.locator('body')).toContainText('0 / 2 votes submitted')
  await expect(guestPage.locator('body')).toContainText('0 / 2 votes submitted')
  await expect(hostPage.locator('[data-testid^="vote-button-"]')).toHaveCount(1)
  await expect(guestPage.locator('[data-testid^="vote-button-"]')).toHaveCount(1)
}
