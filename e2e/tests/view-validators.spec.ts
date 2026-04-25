import { expect, test } from '@playwright/test'

import {
  advanceWorkshopToVoting,
  createCharacter,
  createWorkshopAndJoinAsHost,
  enterPhase1,
  joinWorkshop,
  newPlayerContext,
  signInAccount,
  voteForVisibleDragon,
  waitForNotice,
} from './gameplay-helpers'

test.describe('view validators — current screen contracts', () => {
  test('signin screen renders the account-entry contract', async ({ browser }) => {
    const player = await newPlayerContext(browser)

    try {
      await player.page.goto('/')

      await expect(player.page.getByTestId('signin-panel')).toBeVisible()
      await expect(player.page.getByTestId('signin-name-input')).toBeVisible()
      await expect(player.page.getByTestId('signin-password-input')).toBeVisible()
      await expect(player.page.getByTestId('signin-submit-button')).toHaveText('Sign In')
      await expect(player.page.getByTestId('open-workshops-panel')).toHaveCount(0)
      await expect(player.page.getByTestId('session-panel')).toHaveCount(0)
    } finally {
      await player.context.close()
    }
  })

  test('account home renders create and open-workshop actions', async ({ browser }) => {
    const player = await newPlayerContext(browser)

    try {
      await signInAccount(player.page, 'Alice')

      await expect(player.page.getByTestId('open-workshops-panel')).toBeVisible()
      await expect(player.page.getByTestId('create-workshop-button')).toBeVisible()
      await expect(player.page.getByTestId('create-character-button')).toBeVisible()
      await expect(player.page.getByTestId('app-bar-menu-trigger')).toContainText('Alice')
    } finally {
      await player.context.close()
    }
  })

  test('lobby view shows roster and host-only control visibility', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshopAndJoinAsHost(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await expect(host.page.getByTestId('session-panel')).toContainText('Workshop Lobby')
      await expect(guest.page.getByTestId('session-panel')).toContainText('Workshop Lobby')
      await expect(host.page.getByTestId('session-panel')).toContainText('2 / 2 ready')
      await expect(host.page.getByTestId('session-panel')).toContainText('Alice')
      await expect(host.page.getByTestId('session-panel')).toContainText('Bob')
      await expect(host.page.getByTestId('connection-badge')).toContainText('Connected')
      await expect(guest.page.getByTestId('connection-badge')).toContainText('Connected')
      await expect(host.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
      await expect(guest.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
      await expect(host.page.getByTestId('start-phase1-button')).toBeVisible()
      await expect(guest.page.getByTestId('start-phase1-button')).toHaveCount(0)
      await expect(host.page.getByTestId('controls-panel')).toContainText('visible')
      await expect(guest.page.getByTestId('controls-panel')).toContainText('hidden')
    } finally {
      await host.context.close()
      await guest.context.close()
    }
  })

  test('phase 1 renders discovery controls and custom dragon sprite', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      await signInAccount(host.page, 'Alice')
      await createCharacter(host.page, 'A coral dragon with lantern eyes and striped wings.')
      await signInAccount(guest.page, 'Bob')
      await createCharacter(guest.page, 'A moss dragon with a comet tail and silver fins.')

      const workshopCode = await createWorkshopAndJoinAsHost(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')
      await enterPhase1(host.page, guest.page)

      for (const page of [host.page, guest.page]) {
        await expect(page.getByTestId('session-panel')).toContainText('Phase 1: Discovery')
        await expect(page.getByTestId('observation-input')).toBeVisible()
        await expect(page.getByTestId('submit-observation-button')).toBeDisabled()
        await expect(page.getByTestId('action-feed-meat')).toBeVisible()
        await expect(page.getByTestId('action-play-fetch')).toBeVisible()
        await expect(page.getByTestId('action-sleep')).toBeVisible()
        await expect(page.locator('.dragon-stage__sprite')).toBeVisible()
      }
    } finally {
      await host.context.close()
      await guest.context.close()
    }
  })

  test('voting and end views render current result panels', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshopAndJoinAsHost(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')
      await advanceWorkshopToVoting(host.page, guest.page)

      for (const page of [host.page, guest.page]) {
        await expect(page.getByTestId('session-panel')).toContainText('Vote for the most creative dragon design')
        await expect(page.locator('.voting-grid')).toBeVisible()
        await expect(page.locator('.voting-card')).toHaveCount(2)
        await expect(page.locator('.voting-card--blocked')).toHaveCount(1)
      }

      await voteForVisibleDragon(host.page)
      await voteForVisibleDragon(guest.page)
      await host.page.getByTestId('reveal-results-button').click()
      await waitForNotice(host.page, 'Voting finished.')
      await host.page.getByTestId('end-session-button').click()
      await waitForNotice(host.page, 'Game over ready.')
      await host.page.getByTestId('game-over-continue-button').click()
      await guest.page.getByTestId('game-over-continue-button').click()

      for (const page of [host.page, guest.page]) {
        await expect(page.getByTestId('session-panel')).toContainText('Game over')
        await expect(page.getByTestId('session-panel')).toContainText('Creativity leaderboard')
        await expect(page.getByTestId('archive-panel')).toContainText('Build the workshop archive')
      }
    } finally {
      await host.context.close()
      await guest.context.close()
    }
  })
})
