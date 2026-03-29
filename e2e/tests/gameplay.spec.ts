import { expect, test, type Browser, type BrowserContext, type Page } from '@playwright/test'

async function newPlayerContext(browser: Browser) {
  const context = await browser.newContext()
  const page = await context.newPage()
  return { context, page }
}

async function gotoApp(page: Page) {
  await page.goto('/')
  await expect(page.getByTestId('hero-panel')).toBeVisible()
}

async function createWorkshop(page: Page, hostName: string) {
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

async function joinWorkshop(page: Page, workshopCode: string, playerName: string) {
  await gotoApp(page)
  await page.getByTestId('join-session-code-input').fill(workshopCode)
  await page.getByTestId('join-name-input').fill(playerName)
  await page.getByTestId('join-workshop-button').click()
  await expect(page.getByTestId('session-panel')).toBeVisible()
  await expect(page.getByTestId('connection-badge')).toContainText('Connected')
  await waitForNotice(page, 'Session synced.')
  await expect(page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
}

async function waitForNotice(page: Page, text: string) {
  await expect(page.getByTestId('notice-bar')).toContainText(text)
}

async function voteForVisibleDragon(page: Page) {
  const voteButton = page.locator('[data-testid^="vote-button-"]').first()
  await expect(voteButton).toBeVisible()
  await voteButton.click()
}

test.describe('dragon shift deployed gameplay', () => {
  test('host and guest can advance through the visible workshop flow', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await expect(host.page.getByTestId('session-panel')).toContainText('Workshop lobby')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Workshop lobby')

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

      await guest.page.getByTestId('handover-tags-input').fill('music,night,playful')
      await guest.page.getByTestId('save-handover-tags-button').click()
      await waitForNotice(guest.page, 'Handover tags saved.')

      await host.page.getByTestId('start-phase2-button').click()
      await waitForNotice(host.page, 'Phase 2 started.')
      await expect(host.page.getByTestId('session-panel')).toContainText('Care round')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Care round')

      await host.page.getByTestId('end-game-button').click()
      await waitForNotice(host.page, 'Voting started.')
      await expect(host.page.getByTestId('session-panel')).toContainText('Voting')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Voting')
      await expect(host.page.getByTestId('session-panel')).toContainText('0 / 2 votes submitted')
      await expect(guest.page.getByTestId('session-panel')).toContainText('0 / 2 votes submitted')
      await expect(host.page.locator('[data-testid^="vote-button-"]')).toHaveCount(1)
      await expect(guest.page.locator('[data-testid^="vote-button-"]')).toHaveCount(1)

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
    let originalClosed = false

    try {
      const workshopCode = await createWorkshop(original.page, 'Alice')

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
    } finally {
      if (!originalClosed) {
        await original.context.close()
      }
      await reconnect.context.close()
    }
  })
})
