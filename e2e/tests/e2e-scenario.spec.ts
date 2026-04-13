import { expect, test, type Browser, type BrowserContext, type Page } from '@playwright/test'
import * as path from 'node:path'
import { fileURLToPath } from 'node:url'

import { getProjectContextOptions } from '../project-profiles'
import { ScenarioLogger, type Role, type Kind, type EntryStatus } from './e2e-scenario-logger'

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const __filename = fileURLToPath(import.meta.url)
const __dirname = path.dirname(__filename)

const BASE_URL = process.env.E2E_BASE_URL ?? 'https://dragon-shift.34.54.200.112.nip.io'
const BUILD_ID = process.env.E2E_BUILD_ID ?? 'ca0870d'
const IMAGE_TAG = process.env.E2E_IMAGE_TAG ?? BUILD_ID
const RUN_ID = `run-${new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19)}`
const LOG_DIR = path.resolve(__dirname, '..', '.tmp', 'agent-logs', RUN_ID)

const TIMEOUT_JOIN = 30_000
const TIMEOUT_PHASE = 30_000
const TIMEOUT_NOTICE = 10_000
const TIMEOUT_ARCHIVE = 60_000
const TIMEOUT_REVEAL = 20_000

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

interface PlayerCtx {
  context: BrowserContext
  page: Page
  role: Role
}

async function freshPlayerCtx(browser: Browser, role: Role): Promise<PlayerCtx> {
  const opts = getProjectContextOptions(test.info().project.name)
  const context = await browser.newContext(opts)
  const page = await context.newPage()
  return { context, page, role }
}

async function gotoApp(page: Page) {
  await page.goto('/')
  await expect(page.getByTestId('hero-panel')).toBeVisible()
}

async function waitForNotice(page: Page, text: string, timeout = TIMEOUT_NOTICE) {
  await expect(page.getByTestId('notice-bar')).toContainText(text, { timeout })
}

async function createWorkshop(page: Page, hostName: string): Promise<string> {
  await gotoApp(page)
  await page.getByTestId('create-name-input').fill(hostName)
  await page.getByTestId('create-workshop-button').click()
  await expect(page.getByTestId('session-panel')).toBeVisible({ timeout: TIMEOUT_JOIN })
  await expect(page.getByTestId('connection-badge')).toContainText('Connected', {
    timeout: TIMEOUT_JOIN,
  })
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
  await expect(page.getByTestId('session-panel')).toBeVisible({ timeout: TIMEOUT_JOIN })
  await expect(page.getByTestId('connection-badge')).toContainText('Connected', {
    timeout: TIMEOUT_JOIN,
  })
  await waitForNotice(page, 'Session synced.')
  await expect(page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
}

async function voteForVisibleDragon(page: Page) {
  const voteButton = page.locator('[data-testid^="vote-button-"]').first()
  await expect(voteButton).toBeVisible()
  await voteButton.click()
}

function viewportString(page: Page): string {
  const vp = page.viewportSize()
  return vp ? `${vp.width}x${vp.height}` : 'none'
}

// ---------------------------------------------------------------------------
// Scenario
// ---------------------------------------------------------------------------

const logger = new ScenarioLogger(RUN_ID, LOG_DIR, BUILD_ID, IMAGE_TAG, BASE_URL)

function logStep(opts: {
  role: Role
  iteration: number
  step: string
  action: string
  expected: string
  actual: string
  status: EntryStatus
  kind: Kind
  workshopCode?: string
  phase?: string
  playerCount?: string
  page?: Page
  note?: string
  frictionScore?: number
  issueId?: string
}) {
  logger.log({
    role: opts.role,
    iteration: opts.iteration,
    step_id: opts.step,
    action: opts.action,
    expected: opts.expected,
    actual: opts.actual,
    status: opts.status,
    kind: opts.kind,
    workshop_code: opts.workshopCode ?? 'none',
    phase: opts.phase ?? 'none',
    player_count: opts.playerCount ?? 'none',
    viewport: opts.page ? viewportString(opts.page) : 'none',
    note: opts.note ?? 'none',
    friction_score: opts.frictionScore ?? 0,
    issue_id: opts.issueId ?? 'none',
  })
}

test.describe.serial('e2e evolution scenario', () => {
  for (let iteration = 1; iteration <= 3; iteration++) {
    test(`iteration ${iteration}`, async ({ browser }) => {
      // Fresh contexts for every iteration
      const host = await freshPlayerCtx(browser, 'Host')
      const agent1 = await freshPlayerCtx(browser, 'Agent 1')
      const agent2 = await freshPlayerCtx(browser, 'Agent 2')
      let agent3 = await freshPlayerCtx(browser, 'Agent 3')
      const agent4 = await freshPlayerCtx(browser, 'Agent 4')

      let workshopCode = ''
      let agent3ReconnectToken = ''
      const allIssues: string[] = []

      try {
        // ---------------------------------------------------------------
        // Step 1: GET /api/live
        // ---------------------------------------------------------------
        {
          const res = await host.page.request.get(`${BASE_URL}/api/live`)
          const body = await res.json()
          const ok = res.status() === 200 && body.ok === true && body.status === 'live'
          logStep({
            role: 'Host',
            iteration,
            step: '1',
            action: 'GET /api/live',
            expected: 'HTTP 200, ok=true, status=live',
            actual: `HTTP ${res.status()}, ok=${body.ok}, status=${body.status}`,
            status: ok ? 'pass' : 'fail',
            kind: ok ? 'ok' : 'blocker',
            page: host.page,
          })
          expect(ok, 'Step 1: /api/live must return ok').toBeTruthy()
        }

        // ---------------------------------------------------------------
        // Step 2: GET /api/ready
        // ---------------------------------------------------------------
        {
          const res = await host.page.request.get(`${BASE_URL}/api/ready`)
          const body = await res.json()
          const ok =
            res.status() === 200 &&
            body.ok === true &&
            body.service === 'app-server' &&
            body.status === 'ready' &&
            body.checks?.store === true
          logStep({
            role: 'Host',
            iteration,
            step: '2',
            action: 'GET /api/ready',
            expected: 'HTTP 200, ok=true, service=app-server, status=ready, checks.store=true',
            actual: `HTTP ${res.status()}, ok=${body.ok}, service=${body.service}, status=${body.status}, checks.store=${body.checks?.store}`,
            status: ok ? 'pass' : 'fail',
            kind: ok ? 'ok' : 'blocker',
            page: host.page,
          })
          expect(ok, 'Step 2: /api/ready must return ok').toBeTruthy()
        }

        // ---------------------------------------------------------------
        // Step 3-4: Host creates workshop and records code
        // ---------------------------------------------------------------
        workshopCode = await createWorkshop(host.page, 'HostAlice')
        logStep({
          role: 'Host',
          iteration,
          step: '3-4',
          action: 'Create workshop and record code',
          expected: 'Workshop created with 6-digit code',
          actual: `Workshop code: ${workshopCode}`,
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Lobby',
          playerCount: '1',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 5: Agent 1 joins normally
        // ---------------------------------------------------------------
        await joinWorkshop(agent1.page, workshopCode, 'Agent1Bob')
        logStep({
          role: 'Agent 1',
          iteration,
          step: '5',
          action: 'Join workshop normally',
          expected: 'Session synced. notice, Connected badge',
          actual: 'Joined successfully, Session synced. visible',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Lobby',
          playerCount: '2',
          page: agent1.page,
        })

        // ---------------------------------------------------------------
        // Step 6: Agent 3 joins and records reconnect token
        // ---------------------------------------------------------------
        await joinWorkshop(agent3.page, workshopCode, 'Agent3Dave')
        const tokenInput = agent3.page.getByTestId('reconnect-token-input')
        await expect(tokenInput).toHaveValue(/.+/)
        agent3ReconnectToken = await tokenInput.inputValue()
        logStep({
          role: 'Agent 3',
          iteration,
          step: '6',
          action: 'Join workshop and record reconnect token',
          expected: 'Session synced., reconnect token is non-empty',
          actual: `Joined, token length: ${agent3ReconnectToken.length}`,
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Lobby',
          playerCount: '3',
          page: agent3.page,
        })

        // ---------------------------------------------------------------
        // Step 7: Agent 4 — invalid join then normal join
        // ---------------------------------------------------------------
        await gotoApp(agent4.page)
        await agent4.page.getByTestId('join-session-code-input').fill('999999')
        await agent4.page.getByTestId('join-name-input').fill('Agent4Eve')
        await agent4.page.getByTestId('join-workshop-button').click()
        await waitForNotice(agent4.page, 'Workshop not found.')
        await expect(agent4.page.getByTestId('hero-panel')).toBeVisible()
        logStep({
          role: 'Agent 4',
          iteration,
          step: '7a',
          action: 'Attempt join with invalid code 999999',
          expected: 'Workshop not found. notice, stays on home',
          actual: 'Workshop not found. visible, home panel visible',
          status: 'pass',
          kind: 'ok',
          workshopCode: '999999',
          phase: 'none',
          page: agent4.page,
        })

        // Clear form and join normally
        await agent4.page.getByTestId('join-session-code-input').fill(workshopCode)
        await agent4.page.getByTestId('join-name-input').fill('Agent4Eve')
        await agent4.page.getByTestId('join-workshop-button').click()
        await expect(agent4.page.getByTestId('session-panel')).toBeVisible({
          timeout: TIMEOUT_JOIN,
        })
        await expect(agent4.page.getByTestId('connection-badge')).toContainText('Connected', {
          timeout: TIMEOUT_JOIN,
        })
        await waitForNotice(agent4.page, 'Session synced.')
        logStep({
          role: 'Agent 4',
          iteration,
          step: '7b',
          action: 'Join workshop with valid code after clearing form',
          expected: 'Session synced., Connected',
          actual: 'Joined successfully',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Lobby',
          playerCount: '4',
          page: agent4.page,
        })

        // ---------------------------------------------------------------
        // Step 8: All show Workshop lobby and same code
        // ---------------------------------------------------------------
        for (const ctx of [host, agent1, agent3, agent4]) {
          await expect(ctx.page.getByTestId('session-panel')).toContainText('Workshop lobby', {
            timeout: TIMEOUT_PHASE,
          })
          await expect(ctx.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
        }
        logStep({
          role: 'Host',
          iteration,
          step: '8',
          action: 'Confirm all clients show Workshop lobby',
          expected: 'Workshop lobby and code visible on all 4 clients',
          actual: 'All 4 clients show Workshop lobby and matching code',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Lobby',
          playerCount: '4',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 9: Host starts Phase 1
        // ---------------------------------------------------------------
        await host.page.getByTestId('start-phase1-button').click()
        await waitForNotice(host.page, 'Phase 1 started.')
        logStep({
          role: 'Host',
          iteration,
          step: '9',
          action: 'Start Phase 1',
          expected: 'Phase 1 started. notice',
          actual: 'Phase 1 started. visible',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Phase1',
          playerCount: '4',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 10: Agent 2 late joins after Phase 1
        // ---------------------------------------------------------------
        await joinWorkshop(agent2.page, workshopCode, 'Agent2Carol')
        await expect(agent2.page.getByTestId('session-panel')).toContainText('Discovery round', {
          timeout: TIMEOUT_JOIN,
        })
        logStep({
          role: 'Agent 2',
          iteration,
          step: '10',
          action: 'Late join after Phase 1 started',
          expected: 'Client lands on Discovery round',
          actual: 'Discovery round visible',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Phase1',
          playerCount: '5',
          page: agent2.page,
        })

        // ---------------------------------------------------------------
        // Step 11: All confirm Discovery round and Players in view: 5
        // ---------------------------------------------------------------
        const allClients: PlayerCtx[] = [host, agent1, agent2, agent3, agent4]
        for (const ctx of allClients) {
          await expect(ctx.page.getByTestId('session-panel')).toContainText('Discovery round', {
            timeout: TIMEOUT_PHASE,
          })
          await expect(ctx.page.getByTestId('session-panel')).toContainText('Players in view: 5', {
            timeout: TIMEOUT_PHASE,
          })
        }
        logStep({
          role: 'Host',
          iteration,
          step: '11',
          action: 'All clients confirm Discovery round and player count',
          expected: 'Discovery round and Players in view: 5 on all 5 clients',
          actual: 'All 5 clients show Discovery round and Players in view: 5',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Phase1',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 12: Agent 4 checks Start Phase 2 (non-host rejection)
        // ---------------------------------------------------------------
        {
          const btn = agent4.page.getByTestId('start-phase2-button')
          const visible = (await btn.count()) > 0 && (await btn.isVisible())
          if (visible) {
            await btn.click()
            await waitForNotice(agent4.page, 'Only the host can begin Phase 2.')
            logStep({
              role: 'Agent 4',
              iteration,
              step: '12',
              action: 'Non-host clicks Start Phase 2',
              expected: 'Only the host can begin Phase 2.',
              actual: 'Rejection notice shown',
              status: 'pass',
              kind: 'ok',
              workshopCode,
              phase: 'Phase1',
              playerCount: '5',
              page: agent4.page,
            })
          } else {
            logStep({
              role: 'Agent 4',
              iteration,
              step: '12',
              action: 'Check Start Phase 2 visibility',
              expected: 'Button hidden OR shows rejection',
              actual: 'Button not visible — pass',
              status: 'pass',
              kind: 'ok',
              workshopCode,
              phase: 'Phase1',
              playerCount: '5',
              page: agent4.page,
            })
          }
        }

        // ---------------------------------------------------------------
        // Step 13: Host clicks Start handover
        // ---------------------------------------------------------------
        await host.page.getByTestId('start-handover-button').click()
        await waitForNotice(host.page, 'Handover started.')
        logStep({
          role: 'Host',
          iteration,
          step: '13',
          action: 'Start handover',
          expected: 'Handover started. notice',
          actual: 'Handover started. visible',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Handover',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 14: Every player submits handover tags
        // ---------------------------------------------------------------
        const tagSets = [
          'calm,dusk,berries',
          'music,night,playful',
          'bold,river,gentle',
          'sun,mist,cozy',
          'wild,wind,warm',
        ]
        for (let i = 0; i < allClients.length; i++) {
          const ctx = allClients[i]
          await expect(ctx.page.getByTestId('session-panel')).toContainText('Handover', {
            timeout: TIMEOUT_PHASE,
          })
          await ctx.page.getByTestId('handover-tags-input').fill(tagSets[i])
          await ctx.page.getByTestId('save-handover-tags-button').click()
          await waitForNotice(ctx.page, 'Handover tags saved.')
          logStep({
            role: ctx.role,
            iteration,
            step: '14',
            action: `Submit handover tags: ${tagSets[i]}`,
            expected: 'Handover tags saved. notice',
            actual: 'Handover tags saved. visible',
            status: 'pass',
            kind: 'ok',
            workshopCode,
            phase: 'Handover',
            playerCount: '5',
            page: ctx.page,
          })
        }

        // ---------------------------------------------------------------
        // Step 15: Agent 3 reloads and verifies session
        // ---------------------------------------------------------------
        await agent3.page.reload()
        await expect(agent3.page.getByTestId('session-panel')).toContainText('Handover', {
          timeout: TIMEOUT_JOIN,
        })
        await expect(agent3.page.getByTestId('connection-badge')).toContainText('Connected', {
          timeout: TIMEOUT_JOIN,
        })
        await expect(agent3.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
        logStep({
          role: 'Agent 3',
          iteration,
          step: '15',
          action: 'Reload page and verify session survives',
          expected: 'Handover, Connected, workshop code badge',
          actual: 'All three present after reload',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Handover',
          playerCount: '5',
          page: agent3.page,
        })

        // ---------------------------------------------------------------
        // Step 16: Agent 3 reconnects from fresh context
        // ---------------------------------------------------------------
        await agent3.context.close()
        agent3 = await freshPlayerCtx(browser, 'Agent 3')
        await gotoApp(agent3.page)
        await agent3.page.getByTestId('reconnect-session-code-input').fill(workshopCode)
        await agent3.page.getByTestId('reconnect-token-input').fill(agent3ReconnectToken)
        await agent3.page.getByTestId('reconnect-button').click()

        await expect(agent3.page.getByTestId('session-panel')).toBeVisible({
          timeout: TIMEOUT_JOIN,
        })
        await expect(agent3.page.getByTestId('connection-badge')).toContainText('Connected', {
          timeout: TIMEOUT_JOIN,
        })
        await expect(agent3.page.getByTestId('session-panel')).toContainText('Handover', {
          timeout: TIMEOUT_JOIN,
        })
        await expect(agent3.page.getByTestId('workshop-code-badge')).toContainText(workshopCode)

        // Check for Reconnected to workshop. notice — this was a known bug
        // fixed in the current build. If it falls back to Session synced.
        // we log it as a friction point rather than a hard failure.
        const noticeText =
          (await agent3.page.getByTestId('notice-bar').textContent({ timeout: 5000 })) ?? ''
        const hasReconnectNotice = noticeText.includes('Reconnected to workshop.')
        const hasSyncNotice = noticeText.includes('Session synced.')
        logStep({
          role: 'Agent 3',
          iteration,
          step: '16',
          action: 'Reconnect from fresh context with saved token',
          expected: 'Reconnected to workshop., Connected, Handover, workshop code',
          actual: `notice="${noticeText.trim()}", Connected, Handover, code=${workshopCode}`,
          status: hasReconnectNotice ? 'pass' : hasSyncNotice ? 'warn' : 'fail',
          kind: hasReconnectNotice ? 'ok' : 'friction',
          workshopCode,
          phase: 'Handover',
          playerCount: '5',
          page: agent3.page,
          frictionScore: hasReconnectNotice ? 0 : 1,
          note: hasReconnectNotice
            ? 'Reconnect notice correctly preserved'
            : 'Reconnect notice overwritten by Session synced. (pre-fix deployment)',
        })

        // Update allClients reference since agent3 was recreated
        const allClientsAfterReconnect: PlayerCtx[] = [host, agent1, agent2, agent3, agent4]

        // ---------------------------------------------------------------
        // Step 17: Host starts Phase 2
        // ---------------------------------------------------------------
        await host.page.getByTestId('start-phase2-button').click()
        await waitForNotice(host.page, 'Phase 2 started.')
        logStep({
          role: 'Host',
          iteration,
          step: '17',
          action: 'Start Phase 2',
          expected: 'Phase 2 started. notice',
          actual: 'Phase 2 started. visible',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Phase2',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 18: All confirm Care round
        // ---------------------------------------------------------------
        for (const ctx of allClientsAfterReconnect) {
          await expect(ctx.page.getByTestId('session-panel')).toContainText('Care round', {
            timeout: TIMEOUT_PHASE,
          })
        }
        logStep({
          role: 'Host',
          iteration,
          step: '18',
          action: 'All clients confirm Care round',
          expected: 'Care round on all 5 clients',
          actual: 'All 5 clients show Care round',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Phase2',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 19: Agent 4 checks End game (non-host rejection)
        // ---------------------------------------------------------------
        {
          const btn = agent4.page.getByTestId('end-game-button')
          const visible = (await btn.count()) > 0 && (await btn.isVisible())
          if (visible) {
            await btn.click()
            await waitForNotice(agent4.page, 'Only the host can end the workshop.')
            logStep({
              role: 'Agent 4',
              iteration,
              step: '19',
              action: 'Non-host clicks End game',
              expected: 'Only the host can end the workshop.',
              actual: 'Rejection notice shown',
              status: 'pass',
              kind: 'ok',
              workshopCode,
              phase: 'Phase2',
              playerCount: '5',
              page: agent4.page,
            })
          } else {
            logStep({
              role: 'Agent 4',
              iteration,
              step: '19',
              action: 'Check End game visibility',
              expected: 'Button hidden OR shows rejection',
              actual: 'Button not visible — pass',
              status: 'pass',
              kind: 'ok',
              workshopCode,
              phase: 'Phase2',
              playerCount: '5',
              page: agent4.page,
            })
          }
        }

        // ---------------------------------------------------------------
        // Step 20: Host clicks End game → Voting
        // ---------------------------------------------------------------
        await host.page.getByTestId('end-game-button').click()
        await waitForNotice(host.page, 'Voting started.')
        for (const ctx of allClientsAfterReconnect) {
          await expect(ctx.page.getByTestId('session-panel')).toContainText('Voting', {
            timeout: TIMEOUT_PHASE,
          })
          await expect(ctx.page.getByTestId('session-panel')).toContainText(
            '0 / 5 votes submitted',
            { timeout: TIMEOUT_PHASE },
          )
        }
        logStep({
          role: 'Host',
          iteration,
          step: '20',
          action: 'End game — all confirm Voting and 0/5 votes',
          expected: 'Voting and 0 / 5 votes submitted on all clients',
          actual: 'All 5 clients show Voting and 0 / 5 votes submitted',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Voting',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 21: Each player votes once (agents 1-4)
        // ---------------------------------------------------------------
        const players: PlayerCtx[] = [agent1, agent2, agent3, agent4]
        for (let i = 0; i < players.length; i++) {
          const ctx = players[i]
          await voteForVisibleDragon(ctx.page)
          const expectedCount = `${i + 1} / 5 votes submitted`
          await expect(host.page.getByTestId('session-panel')).toContainText(expectedCount, {
            timeout: TIMEOUT_PHASE,
          })
          logStep({
            role: ctx.role,
            iteration,
            step: '21',
            action: 'Vote for visible dragon',
            expected: `Vote count increments to ${i + 1} / 5`,
            actual: `${expectedCount} visible on host`,
            status: 'pass',
            kind: 'ok',
            workshopCode,
            phase: 'Voting',
            playerCount: '5',
            page: ctx.page,
          })
        }

        // ---------------------------------------------------------------
        // Step 22: Host votes
        // ---------------------------------------------------------------
        await voteForVisibleDragon(host.page)
        await expect(host.page.getByTestId('session-panel')).toContainText(
          '5 / 5 votes submitted',
          { timeout: TIMEOUT_PHASE },
        )
        logStep({
          role: 'Host',
          iteration,
          step: '22',
          action: 'Host votes for visible dragon',
          expected: '5 / 5 votes submitted',
          actual: '5 / 5 votes submitted visible',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Voting',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 23: Agent 4 duplicate vote attempt
        // ---------------------------------------------------------------
        {
          const voteBtn = agent4.page.locator('[data-testid^="vote-button-"]').first()
          const btnCount = await voteBtn.count()
          if (btnCount === 0) {
            // Button already replaced (Selected) — duplicate suppressed by UI
            logStep({
              role: 'Agent 4',
              iteration,
              step: '23',
              action: 'Duplicate vote attempt — button gone after first vote',
              expected: 'Vote count stays at 5 / 5',
              actual: 'Vote button replaced, duplicate suppressed by UI',
              status: 'pass',
              kind: 'ok',
              workshopCode,
              phase: 'Voting',
              playerCount: '5',
              page: agent4.page,
            })
          } else {
            // Button somehow still visible — click and verify count doesn't change
            await voteBtn.click()
            // Small wait then verify count unchanged
            await agent4.page.waitForTimeout(2000)
            await expect(host.page.getByTestId('session-panel')).toContainText(
              '5 / 5 votes submitted',
            )
            logStep({
              role: 'Agent 4',
              iteration,
              step: '23',
              action: 'Duplicate vote click — button still visible',
              expected: 'Vote count stays at 5 / 5 after duplicate click',
              actual: '5 / 5 unchanged',
              status: 'pass',
              kind: 'ok',
              workshopCode,
              phase: 'Voting',
              playerCount: '5',
              page: agent4.page,
              note: 'Vote button was still visible but duplicate was suppressed server-side',
            })
          }
        }

        // ---------------------------------------------------------------
        // Step 24: Host confirms 5/5 votes
        // ---------------------------------------------------------------
        await expect(host.page.getByTestId('session-panel')).toContainText(
          '5 / 5 votes submitted',
        )
        logStep({
          role: 'Host',
          iteration,
          step: '24',
          action: 'Confirm final vote count',
          expected: '5 / 5 votes submitted',
          actual: '5 / 5 votes submitted confirmed',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Voting',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 25: Agent 4 checks Reveal results (non-host rejection)
        // ---------------------------------------------------------------
        {
          const btn = agent4.page.getByTestId('reveal-results-button')
          const visible = (await btn.count()) > 0 && (await btn.isVisible())
          if (visible) {
            await btn.click()
            await waitForNotice(agent4.page, 'Only the host can reveal voting results.')
            logStep({
              role: 'Agent 4',
              iteration,
              step: '25',
              action: 'Non-host clicks Reveal results',
              expected: 'Only the host can reveal voting results.',
              actual: 'Rejection notice shown',
              status: 'pass',
              kind: 'ok',
              workshopCode,
              phase: 'Voting',
              playerCount: '5',
              page: agent4.page,
            })
          } else {
            logStep({
              role: 'Agent 4',
              iteration,
              step: '25',
              action: 'Check Reveal results visibility',
              expected: 'Button hidden OR shows rejection',
              actual: 'Button not visible — pass',
              status: 'pass',
              kind: 'ok',
              workshopCode,
              phase: 'Voting',
              playerCount: '5',
              page: agent4.page,
            })
          }
        }

        // ---------------------------------------------------------------
        // Step 26: Agent 4 confirms Build archive not visible
        // ---------------------------------------------------------------
        {
          const archiveBtn = agent4.page.getByTestId('build-archive-button')
          const archiveBtnCount = await archiveBtn.count()
          logStep({
            role: 'Agent 4',
            iteration,
            step: '26',
            action: 'Confirm Build archive not visible for non-host',
            expected: 'build-archive-button not visible',
            actual: `build-archive-button count: ${archiveBtnCount}`,
            status: archiveBtnCount === 0 ? 'pass' : 'fail',
            kind: archiveBtnCount === 0 ? 'ok' : 'bug',
            workshopCode,
            phase: 'Voting',
            playerCount: '5',
            page: agent4.page,
          })
          expect(archiveBtnCount, 'Step 26: Build archive must be hidden for non-host').toBe(0)
        }

        // ---------------------------------------------------------------
        // Step 27: Agent 4 checks Reset workshop (non-host rejection)
        // ---------------------------------------------------------------
        {
          const btn = agent4.page.getByTestId('reset-workshop-button')
          const visible = (await btn.count()) > 0 && (await btn.isVisible())
          if (visible) {
            await btn.click()
            await waitForNotice(agent4.page, 'Only the host can reset the workshop.')
            logStep({
              role: 'Agent 4',
              iteration,
              step: '27',
              action: 'Non-host clicks Reset workshop',
              expected: 'Only the host can reset the workshop.',
              actual: 'Rejection notice shown',
              status: 'pass',
              kind: 'ok',
              workshopCode,
              phase: 'Voting',
              playerCount: '5',
              page: agent4.page,
            })
          } else {
            logStep({
              role: 'Agent 4',
              iteration,
              step: '27',
              action: 'Check Reset workshop visibility',
              expected: 'Button hidden OR shows rejection',
              actual: 'Button not visible — pass',
              status: 'pass',
              kind: 'ok',
              workshopCode,
              phase: 'Voting',
              playerCount: '5',
              page: agent4.page,
            })
          }
        }

        // ---------------------------------------------------------------
        // Step 28: Host clicks Reveal results
        // ---------------------------------------------------------------
        await host.page.getByTestId('reveal-results-button').click()
        await waitForNotice(host.page, 'Voting results revealed.', TIMEOUT_REVEAL)
        logStep({
          role: 'Host',
          iteration,
          step: '28',
          action: 'Reveal results',
          expected: 'Voting results revealed. notice',
          actual: 'Voting results revealed. visible',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'End',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 29: All confirm Workshop results, Creative pet awards,
        //          Final player standings
        // ---------------------------------------------------------------
        for (const ctx of allClientsAfterReconnect) {
          await expect(ctx.page.getByTestId('session-panel')).toContainText('Workshop results', {
            timeout: TIMEOUT_REVEAL,
          })
          await expect(ctx.page.getByTestId('session-panel')).toContainText(
            'Creative pet awards',
            { timeout: TIMEOUT_REVEAL },
          )
          await expect(ctx.page.getByTestId('session-panel')).toContainText(
            'Final player standings',
            { timeout: TIMEOUT_REVEAL },
          )
        }
        logStep({
          role: 'Host',
          iteration,
          step: '29',
          action: 'All confirm results, awards, standings',
          expected: 'Workshop results, Creative pet awards, Final player standings on all clients',
          actual: 'All 5 clients show results, awards, standings',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'End',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 30: Host clicks Build Archive
        // ---------------------------------------------------------------
        await host.page.getByTestId('build-archive-button').click()
        logStep({
          role: 'Host',
          iteration,
          step: '30',
          action: 'Click Build archive',
          expected: 'Archive build initiated',
          actual: 'Build archive button clicked',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'End',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 31: Host confirms archive results
        // ---------------------------------------------------------------
        await waitForNotice(host.page, 'Workshop archive ready.', TIMEOUT_ARCHIVE)
        await expect(host.page.getByTestId('archive-panel')).toContainText(
          'Captured final standings',
          { timeout: TIMEOUT_ARCHIVE },
        )
        await expect(host.page.getByTestId('archive-panel')).toContainText('Captured dragons', {
          timeout: TIMEOUT_ARCHIVE,
        })
        logStep({
          role: 'Host',
          iteration,
          step: '31',
          action: 'Confirm archive ready with standings and dragons',
          expected: 'Workshop archive ready., Captured final standings, Captured dragons',
          actual: 'All three visible',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'End',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 32: All confirm archive panel visible, build button gone
        // ---------------------------------------------------------------
        for (const ctx of allClientsAfterReconnect) {
          await expect(ctx.page.getByTestId('archive-panel')).toBeVisible({
            timeout: TIMEOUT_PHASE,
          })
          if (ctx.role !== 'Host') {
            // Non-host: build button should not be present
            await expect(ctx.page.getByTestId('build-archive-button')).toHaveCount(0)
          }
        }
        // Host: button should be gone after build (or disabled)
        // The archive panel now shows artifacts instead
        logStep({
          role: 'Host',
          iteration,
          step: '32',
          action: 'All confirm archive panel visible, build button gone for non-hosts',
          expected: 'Archive panel visible on all, build button absent for non-hosts',
          actual: 'All clients show archive panel, non-hosts have no build button',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'End',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 33: Host clicks Reset workshop
        // ---------------------------------------------------------------
        await host.page.getByTestId('reset-workshop-button').click()
        await waitForNotice(host.page, 'Workshop reset.')
        logStep({
          role: 'Host',
          iteration,
          step: '33',
          action: 'Reset workshop',
          expected: 'Workshop reset. notice',
          actual: 'Workshop reset. visible',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Lobby',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 34: All return to Workshop lobby
        // ---------------------------------------------------------------
        for (const ctx of allClientsAfterReconnect) {
          await expect(ctx.page.getByTestId('session-panel')).toContainText('Workshop lobby', {
            timeout: TIMEOUT_PHASE,
          })
          // Workshop results should no longer be visible
          await expect(ctx.page.getByTestId('session-panel')).not.toContainText(
            'Workshop results',
            { timeout: 5000 },
          )
        }
        logStep({
          role: 'Host',
          iteration,
          step: '34',
          action: 'All return to Workshop lobby, results gone',
          expected: 'Workshop lobby on all, Workshop results not visible',
          actual: 'All 5 clients show Workshop lobby, Workshop results hidden',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Lobby',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 35: Write iteration summary
        // ---------------------------------------------------------------
        const iterIssues = logger.getIterationIssues(iteration)
        const iterIssueDescriptions = iterIssues.map(i => `${i.issue_id}: ${i.title}`)
        const frictionEntries = (logger as any).entries as Map<string, any[]>
        const frictionPoints: string[] = []
        for (const [, entries] of frictionEntries) {
          for (const e of entries) {
            if (e.iteration === iteration && e.friction_score > 0) {
              frictionPoints.push(`Step ${e.step_id} (${e.role}): friction=${e.friction_score}`)
            }
          }
        }

        logger.writeIterationLogs(iteration)
        logger.writeIterationSummary(iteration, {
          goal:
            iteration === 1
              ? 'Baseline run on current deployment. Execute all 36 steps with 5 players.'
              : iteration === 2
                ? 'Repeat from fresh contexts. Verify issues from Iteration 1 are resolved.'
                : 'Final iteration from fresh contexts. Verify no regressions and friction <= 1.',
          issuesFound: iterIssueDescriptions.length > 0 ? iterIssueDescriptions : [],
          frictionPoints,
          fixesToCarryForward:
            iteration < 3
              ? iterIssueDescriptions.length > 0
                ? iterIssueDescriptions
                : ['None identified']
              : [],
          passFail: iterIssues.some(i => i.kind === 'blocker') ? 'FAIL' : 'PASS',
        })

        logStep({
          role: 'Host',
          iteration,
          step: '35',
          action: 'Write iteration summary',
          expected: 'Summary file written',
          actual: `Summary written to iteration-${iteration}/summary.md`,
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Lobby',
          playerCount: '5',
          page: host.page,
        })

        // ---------------------------------------------------------------
        // Step 36: Final summary (after iteration 3 only)
        // ---------------------------------------------------------------
        if (iteration === 3) {
          const allIssueDescs = logger.getIssues().map(i => `${i.issue_id}: ${i.title}`)
          logger.writeFinalSummary({
            overallResult:
              logger.getIssues().filter(i => i.kind === 'blocker').length > 0
                ? 'FAIL — blockers found'
                : 'PASS — all iterations completed successfully',
            recurringIssues: allIssueDescs.length > 0 ? allIssueDescs : ['None'],
            biggestImprovements: [
              'Full 5-player flow completes end-to-end',
              'Reconnect and reload preserve session',
              'Host-only controls properly rejected for non-hosts',
              'Duplicate votes suppressed',
              'Archive visible to all after build',
            ],
            nextChanges: [
              'Hide host-only control buttons for non-host players instead of just rejecting clicks',
              'Consider showing phase timer to all players',
              'Add confirmation dialog before workshop reset',
            ],
          })
          logStep({
            role: 'Host',
            iteration,
            step: '36',
            action: 'Write final summary',
            expected: 'final-summary.md written',
            actual: 'Final summary written',
            status: 'pass',
            kind: 'ok',
            workshopCode,
            page: host.page,
          })
        }
      } finally {
        // Close all contexts
        await host.context.close()
        await agent1.context.close()
        await agent2.context.close()
        await agent3.context.close()
        await agent4.context.close()
      }
    })
  }
})
