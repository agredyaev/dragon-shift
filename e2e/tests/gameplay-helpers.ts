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

  await page.getByTestId('save-dragon-button').click()
  await waitForNotice(page, 'Dragon profile saved.')
}

export async function generateDragonSprites(page: Page, timeout = 120_000) {
  await page.getByTestId('generate-sprites-button').click()
  await expect(page.getByTestId('notice-bar')).toContainText('Dragon sprites generated!', { timeout })
  await expect(page.getByTestId('sprite-preview-image')).toBeVisible({ timeout })
}

export async function newPlayerContext(
  browser: Browser,
): Promise<{ context: BrowserContext; page: Page }> {
  const context = await browser.newContext(getProjectContextOptions(test.info().project.name))
  const page = await context.newPage()
  return { context, page }
}

export async function gotoApp(page: Page) {
  await page.goto('/')
  await expect(page.getByTestId('hero-panel')).toBeVisible()
}

export async function waitForNotice(page: Page, text: string) {
  await expect(page.getByTestId('notice-bar')).toContainText(text)
}

export async function expectPhaseVisible(pages: Page[], text: string) {
  for (const page of pages) {
    await expect(page.getByTestId('session-panel')).toContainText(text)
  }
}

export async function createWorkshop(page: Page, hostName: string) {
  await gotoApp(page)
  await page.getByTestId('create-name-input').fill(hostName)
  await page.getByTestId('create-workshop-button').click()
  await expect(page.getByTestId('session-panel')).toBeVisible()
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
  await expect(page.getByTestId('session-panel')).toBeVisible()
  await expect(page.getByTestId('connection-badge')).toContainText('Connected')
  await waitForNotice(page, 'Session synced.')
  await expect(page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
}

export async function expectToStayOnHome(page: Page) {
  await expect(page.getByTestId('hero-panel')).toBeVisible()
  await expect(page.getByTestId('session-panel')).toHaveCount(0)
  await expect(page.getByTestId('connection-badge')).toContainText('Offline')
}

export async function voteForVisibleDragon(page: Page) {
  const voteButton = page.locator('[data-testid^="vote-button-"]').first()
  await expect(voteButton).toBeVisible()
  await voteButton.click()
}

export async function openCharacterCreation(hostPage: Page, ...otherPages: Page[]) {
  await hostPage.getByTestId('start-phase0-button').click()
  await waitForNotice(hostPage, 'Character creation opened.')
  await expectPhaseVisible([hostPage, ...otherPages], 'Character creation')
}

export async function enterPhase1(hostPage: Page, ...otherPages: Page[]) {
  await hostPage.getByTestId('start-phase1-button').click()
  await waitForNotice(hostPage, 'Phase 1 started.')
  await expectPhaseVisible([hostPage, ...otherPages], 'Discovery round')
}

export async function enterHandover(hostPage: Page, ...otherPages: Page[]) {
  await hostPage.getByTestId('start-handover-button').click()
  await waitForNotice(hostPage, 'Handover started.')
  await expectPhaseVisible([hostPage, ...otherPages], 'Handover')
}

export async function saveHandoverTags(page: Page, tags: string) {
  await page.getByTestId('handover-tags-input').fill(tags)
  await page.getByTestId('save-handover-tags-button').click()
  await waitForNotice(page, 'Handover tags saved.')
}

export async function enterPhase2(hostPage: Page, ...otherPages: Page[]) {
  await hostPage.getByTestId('start-phase2-button').click()
  await waitForNotice(hostPage, 'Phase 2 started.')
  await expectPhaseVisible([hostPage, ...otherPages], 'Care round')
}

export async function enterJudge(hostPage: Page, ...otherPages: Page[]) {
  await hostPage.getByTestId('end-game-button').click()
  await waitForNotice(hostPage, 'Judge review started.')
  await expectPhaseVisible([hostPage, ...otherPages], 'Judge review')
}

export async function enterVoting(hostPage: Page, ...otherPages: Page[]) {
  await hostPage.getByTestId('start-voting-button').click()
  await waitForNotice(hostPage, 'Design voting started.')
  await expectPhaseVisible([hostPage, ...otherPages], 'Design voting')
}

export async function advanceWorkshopToVoting(hostPage: Page, guestPage: Page) {
  await openCharacterCreation(hostPage, guestPage)

  await saveDragonProfile(hostPage, 'A confident coral dragon with striped wings and lantern eyes.')
  await saveDragonProfile(guestPage, 'A moss-green dragon with wide fins and a comet tail.')

  await enterPhase1(hostPage, guestPage)

  await enterHandover(hostPage, guestPage)

  await saveHandoverTags(hostPage, 'calm,dusk,berries')

  await saveHandoverTags(guestPage, 'music,night,playful')

  await enterPhase2(hostPage, guestPage)

  await enterJudge(hostPage, guestPage)

  await enterVoting(hostPage, guestPage)
  await expect(hostPage.getByTestId('session-panel')).toContainText('0 / 2 votes submitted')
  await expect(guestPage.getByTestId('session-panel')).toContainText('0 / 2 votes submitted')
  await expect(hostPage.locator('[data-testid^="vote-button-"]')).toHaveCount(1)
  await expect(guestPage.locator('[data-testid^="vote-button-"]')).toHaveCount(1)
}
