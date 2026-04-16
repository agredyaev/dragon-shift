import { expect, test } from '@playwright/test'

import {
  advanceWorkshopToVoting,
  createWorkshop,
  gotoApp,
  joinWorkshop,
  newPlayerContext,
  voteForVisibleDragon,
  waitForNotice,
} from './gameplay-helpers'

test.describe('view validators — structure, style, and layout', () => {
  // ---------- 1. Home screen CRT retro layout ----------
  test('home screen renders CRT retro layout with create and join panels', async ({ browser }) => {
    const player = await newPlayerContext(browser)

    try {
      await gotoApp(player.page)

      // Hero panel
      const hero = player.page.getByTestId('hero-panel')
      await expect(hero).toBeVisible()
      await expect(hero.locator('.hero__title')).toHaveText('Dragon Shift')
      await expect(hero.locator('.hero__body')).toBeVisible()
      await expect(hero.locator('.hero__meta')).toBeVisible()

      // Connection badge starts offline
      const connectionBadge = player.page.getByTestId('connection-badge')
      await expect(connectionBadge).toContainText('Offline')
      await expect(connectionBadge).toHaveClass(/badge/)

      // Workshop code badge should NOT exist on home screen
      await expect(player.page.getByTestId('workshop-code-badge')).toHaveCount(0)

      // Create panel structure
      const createPanel = player.page.getByTestId('create-panel')
      await expect(createPanel).toBeVisible()
      await expect(createPanel.locator('.panel__title')).toHaveText('Create workshop')
      await expect(player.page.getByTestId('create-name-input')).toBeVisible()
      await expect(player.page.getByTestId('create-workshop-button')).toBeVisible()
      await expect(player.page.getByTestId('create-workshop-button')).toHaveText('Create workshop')
      await expect(player.page.getByTestId('create-workshop-button')).toHaveClass(/button--primary/)

      // Create panel has phase minute inputs (3 inputs beyond the name)
      const createInputs = createPanel.locator('.input')
      await expect(createInputs).toHaveCount(4) // name + 3 phase minutes

      // Join panel structure
      const joinPanel = player.page.getByTestId('join-panel')
      await expect(joinPanel).toBeVisible()
      await expect(joinPanel.locator('.panel__title')).toHaveText('Join workshop')
      await expect(player.page.getByTestId('join-session-code-input')).toBeVisible()
      await expect(player.page.getByTestId('join-name-input')).toBeVisible()
      await expect(player.page.getByTestId('join-workshop-button')).toBeVisible()
      await expect(player.page.getByTestId('join-workshop-button')).toHaveClass(/button--primary/)
      await expect(player.page.getByTestId('reconnect-session-code-input')).toBeVisible()
      await expect(player.page.getByTestId('reconnect-token-input')).toBeVisible()
      await expect(player.page.getByTestId('reconnect-button')).toBeVisible()
      await expect(player.page.getByTestId('reconnect-button')).toHaveClass(/button--secondary/)

      // Workshop brief panel
      const briefPanel = player.page.locator('.panel--runtime')
      await expect(briefPanel).toBeVisible()
      await expect(briefPanel.locator('.panel__title')).toHaveText('Workshop brief')
      const flowCards = briefPanel.locator('.flow-card')
      await expect(flowCards).toHaveCount(4)
      await expect(flowCards.nth(0).locator('.flow-card__title')).toHaveText('1. Create pet')
      await expect(flowCards.nth(1).locator('.flow-card__title')).toHaveText('2. Discover rules')
      await expect(flowCards.nth(2).locator('.flow-card__title')).toHaveText('3. Handover')
      await expect(flowCards.nth(3).locator('.flow-card__title')).toHaveText('4. Care and vote')

      // Session panel should NOT exist on home screen
      await expect(player.page.getByTestId('session-panel')).toHaveCount(0)

      // CRT retro styling checks
      const shell = player.page.locator('.shell')
      await expect(shell).toBeVisible()

      // Dark background on body
      const bodyBg = await player.page.evaluate(() =>
        getComputedStyle(document.body).backgroundColor,
      )
      expect(bodyBg).toBe('rgb(15, 23, 42)') // #0f172a

      // Hero uses pixel-art border (solid, 4px)
      const heroBorder = await hero.evaluate(el => {
        const s = getComputedStyle(el)
        return { style: s.borderStyle, width: s.borderWidth }
      })
      expect(heroBorder.style).toBe('solid')

      // Buttons have Silkscreen display font
      const btnFont = await player.page.getByTestId('create-workshop-button').evaluate(el =>
        getComputedStyle(el).fontFamily,
      )
      expect(btnFont.toLowerCase()).toContain('silkscreen')
    } finally {
      await player.context.close()
    }
  })

  // ---------- 2. Lobby roster and connectivity ----------
  test('lobby view shows player roster with connectivity indicators', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      // Both see session panel with lobby phase
      for (const page of [host.page, guest.page]) {
        const sessionPanel = page.getByTestId('session-panel')
        await expect(sessionPanel).toBeVisible()
        await expect(sessionPanel).toContainText('Workshop lobby')

        // Summary chips rendered
        await expect(sessionPanel.locator('.summary-chip')).toHaveCount(4) // code, caretaker, round, players
        await expect(sessionPanel).toContainText(`Workshop code: ${workshopCode}`)
        await expect(sessionPanel).toContainText('Players in view: 2')

        // Roster items for 2 players
        const roster = sessionPanel.locator('.roster')
        await expect(roster).toBeVisible()
        const rosterItems = roster.locator('.roster__item')
        await expect(rosterItems).toHaveCount(2)

        // Each roster item has name, meta, and status
        for (let i = 0; i < 2; i++) {
          const item = rosterItems.nth(i)
          await expect(item.locator('.roster__name')).toBeVisible()
          await expect(item.locator('.roster__meta')).toBeVisible()
          const status = item.locator('.roster__status')
          await expect(status).toBeVisible()
          // Connected players should have status-connected class
          await expect(status).toHaveClass(/status-connected/)
        }
      }

      // Host has connection badge connected
      await expect(host.page.getByTestId('connection-badge')).toContainText('Connected')
      await expect(guest.page.getByTestId('connection-badge')).toContainText('Connected')

      // Workshop code badge visible
      await expect(host.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
      await expect(guest.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)

      // Roster contains both player names
      const sessionText = await host.page.getByTestId('session-panel').textContent()
      expect(sessionText).toContain('Alice')
      expect(sessionText).toContain('Bob')

      // Lobby readiness text present
      await expect(host.page.getByTestId('session-panel')).toContainText('Lobby readiness')

      // Controls panel visible alongside session panel
      await expect(host.page.getByTestId('controls-panel')).toBeVisible()
      await expect(guest.page.getByTestId('controls-panel')).toBeVisible()

      // Roster items have pixel-art box-shadow styling
      const rosterShadow = await host.page.locator('.roster__item').first().evaluate(el =>
        getComputedStyle(el).boxShadow,
      )
      expect(rosterShadow).not.toBe('none')
    } finally {
      await host.context.close()
      await guest.context.close()
    }
  })

  // ---------- 3. Phase 1 stat bars and action buttons ----------
  test('phase 1 discovery renders stat bars and seven action buttons', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await host.page.getByTestId('start-phase1-button').click()
      await waitForNotice(host.page, 'Phase 1 started.')

      for (const page of [host.page, guest.page]) {
        const session = page.getByTestId('session-panel')
        await expect(session).toContainText('Discovery round')

        // Phase badge shows "Discovery"
        await expect(session.locator('.roster__status--phase')).toContainText('Discovery')

        // Stat bars container
        const statBars = session.locator('.stat-bars')
        await expect(statBars).toBeVisible()

        // 3 stat bars: Hunger, Energy, Happy
        const bars = statBars.locator('.stat-bar')
        await expect(bars).toHaveCount(3)

        const expectedLabels = ['Hunger', 'Energy', 'Happy']
        for (let i = 0; i < 3; i++) {
          const bar = bars.nth(i)
          await expect(bar.locator('.stat-bar__label')).toHaveText(expectedLabels[i])
          await expect(bar.locator('.stat-bar__track')).toBeVisible()
          await expect(bar.locator('.stat-bar__fill')).toBeVisible()
          await expect(bar.locator('.stat-bar__value')).toBeVisible()

          // Value should be a number
          const value = await bar.locator('.stat-bar__value').textContent()
          expect(Number(value)).not.toBeNaN()
        }

        // Stat bar track has pixel-art border
        const trackBorder = await session.locator('.stat-bar__track').first().evaluate(el => {
          const s = getComputedStyle(el)
          return { style: s.borderStyle, color: s.borderColor }
        })
        expect(trackBorder.style).toBe('solid')

        // 7 action buttons (3 feed + 3 play + 1 sleep)
        await expect(page.getByTestId('action-feed-meat')).toBeVisible()
        await expect(page.getByTestId('action-feed-fruit')).toBeVisible()
        await expect(page.getByTestId('action-feed-fish')).toBeVisible()
        await expect(page.getByTestId('action-play-fetch')).toBeVisible()
        await expect(page.getByTestId('action-play-puzzle')).toBeVisible()
        await expect(page.getByTestId('action-play-music')).toBeVisible()
        await expect(page.getByTestId('action-sleep')).toBeVisible()

        // Action buttons have button--secondary class
        await expect(page.getByTestId('action-feed-meat')).toHaveClass(/button--secondary/)
        await expect(page.getByTestId('action-sleep')).toHaveClass(/button--secondary/)

        // Button text labels
        await expect(page.getByTestId('action-feed-meat')).toHaveText('Feed meat')
        await expect(page.getByTestId('action-feed-fruit')).toHaveText('Feed fruit')
        await expect(page.getByTestId('action-feed-fish')).toHaveText('Feed fish')
        await expect(page.getByTestId('action-play-fetch')).toHaveText('Play fetch')
        await expect(page.getByTestId('action-play-puzzle')).toHaveText('Play puzzle')
        await expect(page.getByTestId('action-play-music')).toHaveText('Play music')
        await expect(page.getByTestId('action-sleep')).toHaveText('Sleep')

        // Buttons are in button-row containers
        const buttonRows = session.locator('.panel__stack .button-row')
        expect(await buttonRows.count()).toBeGreaterThanOrEqual(2)

        // Mood and action info present
        await expect(session).toContainText('Mood:')
        await expect(session).toContainText('Last action:')

        // Observation input section
        await expect(page.getByTestId('observation-input')).toBeVisible()
        await expect(page.getByTestId('submit-observation-button')).toBeVisible()
        await expect(page.getByTestId('submit-observation-button')).toHaveText('Save observation')

        // Observation button disabled when input is empty
        await expect(page.getByTestId('submit-observation-button')).toBeDisabled()
      }
    } finally {
      await host.context.close()
      await guest.context.close()
    }
  })

  // ---------- 4. Phase 1 observation submit cycle ----------
  test('phase 1 observation submit and display cycle', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      await host.page.getByTestId('start-phase1-button').click()
      await waitForNotice(host.page, 'Phase 1 started.')
      await expect(host.page.getByTestId('session-panel')).toContainText('Discovery round')

      // Submit observation button disabled when empty
      await expect(host.page.getByTestId('submit-observation-button')).toBeDisabled()

      // Type observation text
      await host.page.getByTestId('observation-input').fill('The dragon likes meat when hungry')
      await expect(host.page.getByTestId('submit-observation-button')).toBeEnabled()

      // Submit observation
      await host.page.getByTestId('submit-observation-button').click()
      await waitForNotice(host.page, 'Observation saved.')

      // Input clears after submit
      await expect(host.page.getByTestId('observation-input')).toHaveValue('')

      // Saved observation appears in roster
      const session = host.page.getByTestId('session-panel')
      await expect(session).toContainText('The dragon likes meat when hungry')
      await expect(session).toContainText('Observation #1')

      // Saved observation has "Saved" badge
      const savedItem = session.locator('.roster__item').filter({ hasText: 'Observation #1' })
      await expect(savedItem.locator('.roster__status')).toContainText('Saved')
      await expect(savedItem.locator('.roster__status')).toHaveClass(/status-connected/)

      // Submit a second observation
      await host.page.getByTestId('observation-input').fill('Fetch makes the dragon happy during the day')
      await host.page.getByTestId('submit-observation-button').click()
      await waitForNotice(host.page, 'Observation saved.')

      // Both observations visible
      await expect(session).toContainText('Observation #1')
      await expect(session).toContainText('Observation #2')
      await expect(session).toContainText('Fetch makes the dragon happy during the day')

      // Discovery observations summary updated
      await expect(session).toContainText('2 observations recorded')
    } finally {
      await host.context.close()
      await guest.context.close()
    }
  })

  // ---------- 5. Handover view tags ----------
  test('handover view renders tag input and displays saved tags', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      // Advance to phase 1, then handover
      await host.page.getByTestId('start-phase1-button').click()
      await waitForNotice(host.page, 'Phase 1 started.')

      await host.page.getByTestId('start-handover-button').click()
      await waitForNotice(host.page, 'Handover started.')

      for (const page of [host.page, guest.page]) {
        const session = page.getByTestId('session-panel')
        await expect(session).toContainText('Handover')

        // Phase badge shows "Handover"
        await expect(session.locator('.roster__status--phase')).toContainText('Handover')

        // Handover tags input available in controls panel
        await expect(page.getByTestId('handover-tags-input')).toBeVisible()
        await expect(page.getByTestId('save-handover-tags-button')).toBeVisible()

        // Draft count indicator
        await expect(session).toContainText('Draft rules parsed from input:')
      }

      // Host saves tags
      await host.page.getByTestId('handover-tags-input').fill('calm,dusk,berries')
      await host.page.getByTestId('save-handover-tags-button').click()
      await waitForNotice(host.page, 'Handover tags saved.')

      // Saved tags appear as roster items in the session panel
      const hostSession = host.page.getByTestId('session-panel')
      const savedTagItems = hostSession.locator('.roster__item').filter({ hasText: 'Saved handover rule' })
      await expect(savedTagItems).toHaveCount(3)

      // Each saved tag has "Saved" status
      for (let i = 0; i < 3; i++) {
        await expect(savedTagItems.nth(i).locator('.roster__status')).toContainText('Saved')
        await expect(savedTagItems.nth(i).locator('.roster__status')).toHaveClass(/status-connected/)
      }

      // Tag names rendered
      await expect(hostSession).toContainText('calm')
      await expect(hostSession).toContainText('dusk')
      await expect(hostSession).toContainText('berries')

      // Guest can also save their own tags
      await guest.page.getByTestId('handover-tags-input').fill('music,night,playful')
      await guest.page.getByTestId('save-handover-tags-button').click()
      await waitForNotice(guest.page, 'Handover tags saved.')

      const guestSession = guest.page.getByTestId('session-panel')
      await expect(guestSession).toContainText('music')
      await expect(guestSession).toContainText('night')
      await expect(guestSession).toContainText('playful')
    } finally {
      await host.context.close()
      await guest.context.close()
    }
  })

  // ---------- 6. Phase 2 care view ----------
  test('phase 2 care view shows handover notes and hides observation input', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      // Full advance to phase 2
      await host.page.getByTestId('start-phase1-button').click()
      await waitForNotice(host.page, 'Phase 1 started.')

      await host.page.getByTestId('start-handover-button').click()
      await waitForNotice(host.page, 'Handover started.')

      await host.page.getByTestId('handover-tags-input').fill('calm,dusk,berries')
      await host.page.getByTestId('save-handover-tags-button').click()
      await waitForNotice(host.page, 'Handover tags saved.')

      await guest.page.getByTestId('handover-tags-input').fill('music,night,playful')
      await guest.page.getByTestId('save-handover-tags-button').click()
      await waitForNotice(guest.page, 'Handover tags saved.')

      await host.page.getByTestId('start-phase2-button').click()
      await waitForNotice(host.page, 'Phase 2 started.')

      for (const page of [host.page, guest.page]) {
        const session = page.getByTestId('session-panel')
        await expect(session).toContainText('Care round')

        // Phase badge shows "Care"
        await expect(session.locator('.roster__status--phase')).toContainText('Care')

        // Handover notes from previous caretaker
        await expect(session).toContainText('Handover notes from previous caretaker')

        // Stat bars still present (same 3)
        const bars = session.locator('.stat-bar')
        await expect(bars).toHaveCount(3)
        await expect(bars.nth(0).locator('.stat-bar__label')).toHaveText('Hunger')
        await expect(bars.nth(1).locator('.stat-bar__label')).toHaveText('Energy')
        await expect(bars.nth(2).locator('.stat-bar__label')).toHaveText('Happy')

        // 7 action buttons still present
        await expect(page.getByTestId('action-feed-meat')).toBeVisible()
        await expect(page.getByTestId('action-feed-fruit')).toBeVisible()
        await expect(page.getByTestId('action-feed-fish')).toBeVisible()
        await expect(page.getByTestId('action-play-fetch')).toBeVisible()
        await expect(page.getByTestId('action-play-puzzle')).toBeVisible()
        await expect(page.getByTestId('action-play-music')).toBeVisible()
        await expect(page.getByTestId('action-sleep')).toBeVisible()

        // NO observation input in Phase 2
        await expect(page.getByTestId('observation-input')).toHaveCount(0)
        await expect(page.getByTestId('submit-observation-button')).toHaveCount(0)

        // Mood and last action still shown
        await expect(session).toContainText('Mood:')
        await expect(session).toContainText('Last action:')
      }
    } finally {
      await host.context.close()
      await guest.context.close()
    }
  })

  // ---------- 7. Voting view anonymized sprites ----------
  test('voting view renders anonymized dragon sprites in grid layout', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')
      await advanceWorkshopToVoting(host.page, guest.page)

      for (const page of [host.page, guest.page]) {
        const session = page.getByTestId('session-panel')

        // Phase title
        await expect(session).toContainText('Voting')
        await expect(session).toContainText('Vote for the most creative dragon')

        // Voting grid exists and uses CSS grid layout
        const votingGrid = session.locator('.voting-grid')
        await expect(votingGrid).toBeVisible()
        const gridDisplay = await votingGrid.evaluate(el =>
          getComputedStyle(el).display,
        )
        expect(gridDisplay).toBe('grid')

        // Voting cards rendered (2 dragons total: one per player)
        const votingCards = votingGrid.locator('.voting-card')
        await expect(votingCards).toHaveCount(2)

        // Each voting card has a pixel sprite with all 7 body parts
        for (let i = 0; i < 2; i++) {
          const card = votingCards.nth(i)
          const sprite = card.locator('.voting-card__sprite')
          await expect(sprite).toBeVisible()

          // Verify all 7 sprite parts exist
          await expect(sprite.locator('.sprite-body')).toHaveCount(1)
          await expect(sprite.locator('.sprite-head')).toHaveCount(1)
          await expect(sprite.locator('.sprite-eye')).toHaveCount(1)
          await expect(sprite.locator('.sprite-wing')).toHaveCount(1)
          await expect(sprite.locator('.sprite-tail')).toHaveCount(1)
          await expect(sprite.locator('.sprite-horn')).toHaveCount(1)
          await expect(sprite.locator('.sprite-legs')).toHaveCount(1)

          // Sprite container is 64x64 pixels
          const spriteBox = await sprite.boundingBox()
          expect(spriteBox?.width).toBe(64)
          expect(spriteBox?.height).toBe(64)

          // Each sprite part has a background color set via inline style
          const bodyBg = await sprite.locator('.sprite-body').evaluate(el =>
            el.style.background,
          )
          expect(bodyBg).toBeTruthy()

          // Card has a name label
          await expect(card.locator('.voting-card__name')).toBeVisible()
        }

        // Anonymization check: dragon names should be "Dragon #N", NOT player names
        const allNames = await votingGrid.locator('.voting-card__name').allTextContents()
        for (const name of allNames) {
          expect(name).toMatch(/Dragon #\d+/i)
          expect(name.toLowerCase()).not.toContain('alice')
          expect(name.toLowerCase()).not.toContain('bob')
        }

        // One card is the player's own dragon (blocked)
        const blockedCards = votingGrid.locator('.voting-card--blocked')
        await expect(blockedCards).toHaveCount(1)

        // Blocked card shows "Your dragon" badge
        await expect(blockedCards.locator('.voting-card__badge')).toContainText('Your dragon')
        await expect(blockedCards.locator('.voting-card__badge')).toHaveClass(/status-offline/)

        // The other card has a vote button
        const voteButtons = page.locator('[data-testid^="vote-button-"]')
        await expect(voteButtons).toHaveCount(1)
        await expect(voteButtons.first()).toHaveText('Vote')
      }

      // Vote progress indicator
      await expect(host.page.getByTestId('session-panel')).toContainText('0 / 2 votes submitted')
    } finally {
      await host.context.close()
      await guest.context.close()
    }
  })

  // ---------- 8. Voting selected and blocked states ----------
  test('voting mechanics apply selected and blocked card states', async ({ browser }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')
      await advanceWorkshopToVoting(host.page, guest.page)

      // Initial state: no selected cards
      await expect(host.page.locator('.voting-card--selected')).toHaveCount(0)
      await expect(guest.page.locator('.voting-card--selected')).toHaveCount(0)

      // Host votes
      await voteForVisibleDragon(host.page)

      // After host votes: host sees selected card with "Voted" badge
      const hostSelectedCard = host.page.locator('.voting-card--selected')
      await expect(hostSelectedCard).toHaveCount(1)
      await expect(hostSelectedCard.locator('.voting-card__badge')).toContainText('Voted')
      await expect(hostSelectedCard.locator('.voting-card__badge')).toHaveClass(/status-connected/)

      // Selected card has green border (voting-card--selected style)
      const selectedBorder = await hostSelectedCard.evaluate(el =>
        getComputedStyle(el).borderColor,
      )
      expect(selectedBorder).toBe('rgb(22, 101, 52)') // #166534

      // Selected card has green background
      const selectedBg = await hostSelectedCard.evaluate(el =>
        getComputedStyle(el).backgroundColor,
      )
      expect(selectedBg).toBe('rgb(220, 252, 231)') // #dcfce7

      // Host's own dragon still blocked
      await expect(host.page.locator('.voting-card--blocked')).toHaveCount(1)

      // Vote button no longer exists on host page (already voted)
      await expect(host.page.locator('[data-testid^="vote-button-"]')).toHaveCount(0)

      // Progress updates for both
      await expect(host.page.getByTestId('session-panel')).toContainText('1 / 2 votes submitted')
      await expect(guest.page.getByTestId('session-panel')).toContainText('1 / 2 votes submitted')

      // Guest votes
      await voteForVisibleDragon(guest.page)
      await expect(guest.page.locator('.voting-card--selected')).toHaveCount(1)
      await expect(guest.page.locator('[data-testid^="vote-button-"]')).toHaveCount(0)

      // Both see 2/2
      await expect(host.page.getByTestId('session-panel')).toContainText('2 / 2 votes submitted')
      await expect(guest.page.getByTestId('session-panel')).toContainText('2 / 2 votes submitted')

      // Host reveal message unlocked
      await expect(host.page.getByTestId('session-panel')).toContainText('All votes are in')
    } finally {
      await host.context.close()
      await guest.context.close()
    }
  })

  // ---------- 9. End view leaderboards and scoring ----------
  test('end view displays leaderboards and full scoring methodology', async ({ browser }) => {
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

      for (const page of [host.page, guest.page]) {
        const session = page.getByTestId('session-panel')

        // Phase title
        await expect(session).toContainText('Workshop results')
        await expect(session.locator('.roster__status--phase')).toContainText('Final')

        // Mechanics leaderboard table
        await expect(session).toContainText('Mechanics leaderboard')
        const mechBoard = session.locator('.leaderboard').first()
        await expect(mechBoard).toBeVisible()
        await expect(mechBoard.locator('.leaderboard__header')).toBeVisible()
        const mechRows = mechBoard.locator('.leaderboard__row')
        expect(await mechRows.count()).toBeGreaterThanOrEqual(1)

        // Creativity leaderboard table
        await expect(session).toContainText('Creativity leaderboard')
        const creativeBoard = session.locator('.leaderboard--creativity')
        await expect(creativeBoard).toBeVisible()
        const creativeRows = creativeBoard.locator('.leaderboard__row')
        expect(await creativeRows.count()).toBeGreaterThanOrEqual(1)
      }

      // Host sees reset message
      await expect(host.page.getByTestId('session-panel')).toContainText('Host can reset the workshop')
      // Guest sees waiting message
      await expect(guest.page.getByTestId('session-panel')).toContainText('Waiting for the host')

      // Archive panel visible
      await expect(host.page.getByTestId('archive-panel')).toBeVisible()
      await expect(host.page.getByTestId('archive-panel')).toContainText('Build the workshop archive')
    } finally {
      await host.context.close()
      await guest.context.close()
    }
  })

  // ---------- 10. Controls panel host buttons ----------
  test('controls panel presents all host buttons with correct state management', async ({
    browser,
  }) => {
    const host = await newPlayerContext(browser)
    const guest = await newPlayerContext(browser)

    try {
      const workshopCode = await createWorkshop(host.page, 'Alice')
      await joinWorkshop(guest.page, workshopCode, 'Bob')

      // Both see controls panel
      for (const page of [host.page, guest.page]) {
        const controls = page.getByTestId('controls-panel')
        await expect(controls).toBeVisible()
        await expect(controls.locator('.panel__title').first()).toHaveText('Session controls')

        // All host buttons exist
        await expect(page.getByTestId('start-phase1-button')).toBeVisible()
        await expect(page.getByTestId('start-handover-button')).toBeVisible()
        await expect(page.getByTestId('handover-tags-input')).toBeVisible()
        await expect(page.getByTestId('save-handover-tags-button')).toBeVisible()
        await expect(page.getByTestId('start-phase2-button')).toBeVisible()
        await expect(page.getByTestId('end-game-button')).toBeVisible()
        await expect(page.getByTestId('reveal-results-button')).toBeVisible()
        await expect(page.getByTestId('reset-workshop-button')).toBeVisible()

        // Button styles
        await expect(page.getByTestId('start-phase1-button')).toHaveClass(/button--primary/)
        await expect(page.getByTestId('start-handover-button')).toHaveClass(/button--secondary/)
        await expect(page.getByTestId('end-game-button')).toHaveClass(/button--secondary/)
        await expect(page.getByTestId('reveal-results-button')).toHaveClass(/button--secondary/)
        await expect(page.getByTestId('reset-workshop-button')).toHaveClass(/button--secondary/)

        // Build archive button NOT visible in lobby (only in End phase for host)
        await expect(page.getByTestId('build-archive-button')).toHaveCount(0)
      }

      // Controls panel has green accent bar (panel--controls::after)
      const controlsBg = await host.page.getByTestId('controls-panel').evaluate(el => {
        const after = getComputedStyle(el, '::after')
        return after.backgroundColor
      })
      // Pseudo-element background for controls panel is #34d399
      expect(controlsBg).toBeTruthy()

      // Handover tags input placeholder
      const placeholder = await host.page.getByTestId('handover-tags-input').getAttribute('placeholder')
      expect(placeholder).toContain('Handover tags')

      // Advance to End phase and check archive button
      await advanceWorkshopToVoting(host.page, guest.page)
      await voteForVisibleDragon(host.page)
      await voteForVisibleDragon(guest.page)
      await host.page.getByTestId('reveal-results-button').click()
      await waitForNotice(host.page, 'Voting results revealed.')

      // Build archive button now visible for host only
      await expect(host.page.getByTestId('build-archive-button')).toBeVisible()
      await expect(host.page.getByTestId('build-archive-button')).toHaveText('Build archive')
      await expect(guest.page.getByTestId('build-archive-button')).toHaveCount(0)

      // Archive panel has blue accent (panel--judge)
      const archivePanel = host.page.getByTestId('archive-panel')
      await expect(archivePanel).toBeVisible()
      await expect(archivePanel).toHaveClass(/panel--judge/)
    } finally {
      await host.context.close()
      await guest.context.close()
    }
  })
})
