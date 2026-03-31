import { expect, test, type Browser, type BrowserContext, type Page } from '@playwright/test'

import { getProjectContextOptions } from '../project-profiles'

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

export async function advanceWorkshopToVoting(hostPage: Page, guestPage: Page) {
  await hostPage.getByTestId('start-phase1-button').click()
  await waitForNotice(hostPage, 'Phase 1 started.')
  await expect(hostPage.getByTestId('session-panel')).toContainText('Discovery round')
  await expect(guestPage.getByTestId('session-panel')).toContainText('Discovery round')

  await hostPage.getByTestId('start-handover-button').click()
  await waitForNotice(hostPage, 'Handover started.')
  await expect(hostPage.getByTestId('session-panel')).toContainText('Handover')
  await expect(guestPage.getByTestId('session-panel')).toContainText('Handover')

  await hostPage.getByTestId('handover-tags-input').fill('calm,dusk,berries')
  await hostPage.getByTestId('save-handover-tags-button').click()
  await waitForNotice(hostPage, 'Handover tags saved.')

  await guestPage.getByTestId('handover-tags-input').fill('music,night,playful')
  await guestPage.getByTestId('save-handover-tags-button').click()
  await waitForNotice(guestPage, 'Handover tags saved.')

  await hostPage.getByTestId('start-phase2-button').click()
  await waitForNotice(hostPage, 'Phase 2 started.')
  await expect(hostPage.getByTestId('session-panel')).toContainText('Care round')
  await expect(guestPage.getByTestId('session-panel')).toContainText('Care round')

  await hostPage.getByTestId('end-game-button').click()
  await waitForNotice(hostPage, 'Voting started.')
  await expect(hostPage.getByTestId('session-panel')).toContainText('Voting')
  await expect(guestPage.getByTestId('session-panel')).toContainText('Voting')
  await expect(hostPage.getByTestId('session-panel')).toContainText('0 / 2 votes submitted')
  await expect(guestPage.getByTestId('session-panel')).toContainText('0 / 2 votes submitted')
  await expect(hostPage.locator('[data-testid^="vote-button-"]')).toHaveCount(1)
  await expect(guestPage.locator('[data-testid^="vote-button-"]')).toHaveCount(1)
}
