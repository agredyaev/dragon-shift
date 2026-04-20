import { expect, test, type Browser, type BrowserContext, type Page } from '@playwright/test'

import { getProjectContextOptions } from '../project-profiles'

const SESSION_SNAPSHOT_STORAGE_KEY = 'dragon-switch/platform/session-snapshot'

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

export async function saveDragonProfile(page: Page, description?: string) {
  if (description) {
    await page.getByTestId('dragon-description-input').fill(description)
  }

  const saveResponse = page.waitForResponse(response =>
    response.url().includes('/api/workshops/command')
    && response.request().method() === 'POST'
    && response.status() === 200,
  )

  await page.getByTestId('save-dragon-button').click()
  await saveResponse
  await expect(page.getByTestId('save-dragon-button')).toHaveText('Looks good!')
}

export async function generateDragonSprites(page: Page, timeout = 120_000) {
  await page.getByTestId('generate-sprites-button').click()
  const previewImages = page.locator('.sprite-grid__image')
  await expect(previewImages).toHaveCount(4, { timeout })
  await expect(previewImages.first()).toBeVisible({ timeout })
  await expect(page.getByTestId('save-dragon-button')).toBeVisible({ timeout })
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
  await expect(page.getByTestId('hero-panel')).toBeVisible()
}

export async function waitForNotice(page: Page, text: string) {
  const aliases: Record<string, string[]> = {
    'Character creation opened.': ['Opening character creation…'],
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

export async function createWorkshop(page: Page, hostName: string) {
  await gotoApp(page)
  await page.getByTestId('create-name-input').fill(hostName)
  await page.getByTestId('create-workshop-button').click()
  await expect(page.getByTestId('lobby-panel')).toBeVisible()
  await expect(page.getByTestId('connection-badge')).toContainText('Connected')
  await waitForNotice(page, 'Session synced.')
  const workshopBadge = page.getByTestId('workshop-code-badge')
  await expect(workshopBadge).toBeVisible()
  const workshopText = (await workshopBadge.textContent()) ?? ''
  const match = workshopText.match(/(\d{6})/)
  if (!match) {
    throw new Error(`failed to extract workshop code from badge: ${workshopText}`)
  }
  return match[1]
}

export async function joinWorkshop(page: Page, workshopCode: string, playerName: string) {
  await gotoApp(page)
  await page.getByTestId('join-session-code-input').fill(workshopCode)
  await page.getByTestId('join-name-input').fill(playerName)
  await page.getByTestId('join-workshop-button').click()
  await expect(page.getByTestId('lobby-panel')).toBeVisible()
  await expect(page.getByTestId('connection-badge')).toContainText('Connected')
  await waitForNotice(page, 'Session synced.')
  await expect(page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
}

export async function expectToStayOnHome(page: Page) {
  await expect(page.getByTestId('hero-panel')).toBeVisible()
  await expect(page.getByTestId('lobby-panel')).toHaveCount(0)
  await expect(page.getByTestId('connection-badge')).toContainText('Offline')
}

export async function voteForVisibleDragon(page: Page) {
  const voteButtons = page.locator('[data-testid^="vote-button-"]')
  await expect(voteButtons.first()).toBeVisible()
  const count = await voteButtons.count()

  for (let i = 0; i < count; i++) {
    const button = voteButtons.nth(i)
    await button.click()

    // Wait briefly then check if a self-vote rejection appeared
    await page.waitForTimeout(500)
    const notice = page.getByTestId('notice-bar')
    const noticeText = (await notice.count()) ? (await notice.textContent()) ?? '' : ''
    if (noticeText.toLowerCase().includes('cannot vote for your own')) {
      continue // self-vote rejected, try the next dragon
    }

    return // vote accepted
  }

  throw new Error('no valid vote button found (all rejected as self-vote)')
}

export async function dismissGameOverOverlay(...pages: Page[]) {
  for (const page of pages) {
    const overlay = page.getByTestId('game-over-overlay')
    // Judge scoring runs asynchronously via LLM (Vertex AI); the overlay
    // only appears once scores arrive, so allow extra wait time.
    await expect(overlay).toBeVisible({ timeout: 45_000 })
    await page.getByTestId('game-over-continue-button').click()
    await expect(overlay).toHaveCount(0)
  }
}

export async function openCharacterCreation(hostPage: Page, ...otherPages: Page[]) {
  await hostPage.getByTestId('start-phase0-button').click()
  for (const page of [hostPage, ...otherPages]) {
    await expect(page.getByTestId('dragon-description-input')).toBeVisible()
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
  // end-session-button is host-only; only check it on the host page
  await expect(hostPage.getByTestId('end-session-button')).toBeVisible({ timeout: 120_000 })
}

export async function enterVoting(hostPage: Page, ...otherPages: Page[]) {
  for (const page of [hostPage, ...otherPages]) {
    await expect(page.locator('.voting-grid')).toBeVisible()
    await expect(page.locator('body')).toContainText('Vote for the most creative dragon design')
  }
}

export async function advanceWorkshopToVoting(hostPage: Page, guestPage: Page) {
  await openCharacterCreation(hostPage, guestPage)

  await hostPage.getByTestId('dragon-description-input').fill('A confident coral dragon with striped wings and lantern eyes.')
  await guestPage.getByTestId('dragon-description-input').fill('A moss-green dragon with wide fins and a comet tail.')

  await generateDragonSprites(hostPage)
  await generateDragonSprites(guestPage)

  await saveDragonProfile(hostPage)
  await saveDragonProfile(guestPage)

  await enterPhase1(hostPage, guestPage)

  await enterHandover(hostPage, guestPage)

  await saveHandoverTags(hostPage, 'calm,dusk,berries')

  await saveHandoverTags(guestPage, 'music,night,playful')

  await enterPhase2(hostPage, guestPage)

  await enterJudge(hostPage, guestPage)

  await enterVoting(hostPage, guestPage)
  await expect(hostPage.locator('body')).toContainText('0 / 2 votes submitted')
  await expect(guestPage.locator('body')).toContainText('0 / 2 votes submitted')
  await expect(hostPage.locator('[data-testid^="vote-button-"]')).toHaveCount(2)
  await expect(guestPage.locator('[data-testid^="vote-button-"]')).toHaveCount(2)
}
