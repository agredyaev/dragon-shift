import { expect, test, type Browser, type BrowserContext, type Page } from '@playwright/test'
import * as path from 'node:path'
import { fileURLToPath } from 'node:url'

import { getProjectContextOptions } from '../project-profiles'
import { ScenarioLogger, type Role, type Kind, type EntryStatus } from './e2e-scenario-logger'
import {
  cloneSignedInSession,
  createCharacter,
  createWorkshopAndJoinAsHost,
  dismissGameOverOverlay,
  enterHandover,
  enterJudge,
  enterPhase1,
  enterPhase2,
  enterVoting,
  joinWorkshop,
  readSessionSnapshot,
  saveHandoverTags,
  signInAccount,
  voteForVisibleDragon,
  waitForNotice,
} from './gameplay-helpers'

const __filename = fileURLToPath(import.meta.url)
const __dirname = path.dirname(__filename)

const BASE_URL = process.env.E2E_BASE_URL ?? 'https://dragon-shift.34.54.200.112.nip.io'
const BUILD_ID = process.env.E2E_BUILD_ID ?? 'ca0870d'
const IMAGE_TAG = process.env.E2E_IMAGE_TAG ?? BUILD_ID
const RUN_ID = `run-${new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19)}`
const LOG_DIR = path.resolve(__dirname, '..', '.tmp', 'agent-logs', RUN_ID)

interface PlayerCtx {
  context: BrowserContext
  page: Page
  role: Role
}

async function freshPlayerCtx(browser: Browser, role: Role): Promise<PlayerCtx> {
  const opts = getProjectContextOptions(test.info().project.name, test.info().project.use.baseURL)
  const context = await browser.newContext(opts)
  const page = await context.newPage()
  return { context, page, role }
}

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
    viewport: opts.page?.viewportSize() ? `${opts.page.viewportSize()!.width}x${opts.page.viewportSize()!.height}` : 'none',
    note: opts.note ?? 'none',
  })
}

test.describe.serial('e2e evolution scenario', () => {
  for (let iteration = 1; iteration <= 3; iteration++) {
    test(`iteration ${iteration}`, async ({ browser }) => {
      const host = await freshPlayerCtx(browser, 'Host')
      const agent1 = await freshPlayerCtx(browser, 'Agent 1')
      const agent2 = await freshPlayerCtx(browser, 'Agent 2')
      let agent3 = await freshPlayerCtx(browser, 'Agent 3')
      const agent4 = await freshPlayerCtx(browser, 'Agent 4')

      let workshopCode = ''

      try {
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
          expect(ok).toBeTruthy()
        }

        {
          const res = await host.page.request.get(`${BASE_URL}/api/ready`)
          const body = await res.json()
          const ok = res.status() === 200 && body.ok === true && body.status === 'ready'
          logStep({
            role: 'Host',
            iteration,
            step: '2',
            action: 'GET /api/ready',
            expected: 'HTTP 200, ok=true, status=ready',
            actual: `HTTP ${res.status()}, ok=${body.ok}, status=${body.status}`,
            status: ok ? 'pass' : 'fail',
            kind: ok ? 'ok' : 'blocker',
            page: host.page,
          })
          expect(ok).toBeTruthy()
        }

        await signInAccount(host.page, 'HostAlice')
        await signInAccount(agent1.page, 'Agent1Bob')
        await signInAccount(agent2.page, 'Agent2Carol')
        await signInAccount(agent3.page, 'Agent3Dave')
        await signInAccount(agent4.page, 'Agent4Eve')

        await createCharacter(host.page, 'A brass dragon with ember freckles and clockwork ribs.')
        await createCharacter(agent1.page, 'A jade dragon with ribbon tail and moonlit eyes.')
        await createCharacter(agent2.page, 'A charcoal dragon with cobalt horns and lantern glow.')
        await createCharacter(agent3.page, 'A coral dragon with shell cheeks and comet tail.')
        await createCharacter(agent4.page, 'A lilac dragon with fern horns and silver claws.')

        workshopCode = await createWorkshopAndJoinAsHost(host.page, 'HostAlice')
        logStep({
          role: 'Host',
          iteration,
          step: '3',
          action: 'Create lobby and host joins explicitly',
          expected: 'Workshop lobby is created and host enters through explicit join',
          actual: `Workshop ${workshopCode} created and host joined`,
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Lobby',
          playerCount: '1',
          page: host.page,
        })

        await joinWorkshop(agent1.page, workshopCode, 'Agent1Bob')
        await joinWorkshop(agent2.page, workshopCode, 'Agent2Carol')
        await joinWorkshop(agent3.page, workshopCode, 'Agent3Dave')
        await joinWorkshop(agent4.page, workshopCode, 'Agent4Eve')

        for (const ctx of [host, agent1, agent2, agent3, agent4]) {
          await expect(ctx.page.getByTestId('session-panel')).toContainText('Workshop Lobby')
        }
        logStep({
          role: 'Host',
          iteration,
          step: '4',
          action: 'All players join from open workshop list',
          expected: 'All five players reach the lobby',
          actual: 'All players show Workshop Lobby',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Lobby',
          playerCount: '5',
          page: host.page,
        })

        await enterPhase1(host.page, agent1.page, agent2.page, agent3.page, agent4.page)
        logStep({
          role: 'Host',
          iteration,
          step: '5',
          action: 'Start Phase 1',
          expected: 'All players enter discovery',
          actual: 'All players show Phase 1 discovery',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Phase1',
          playerCount: '5',
          page: host.page,
        })

        await enterHandover(host.page, agent1.page, agent2.page, agent3.page, agent4.page)
        await saveHandoverTags(host.page, 'calm,dusk,berries')
        await saveHandoverTags(agent1.page, 'music,night,playful')
        await saveHandoverTags(agent2.page, 'river,day,meat')
        await saveHandoverTags(agent3.page, 'lantern,fetch,warm')
        await saveHandoverTags(agent4.page, 'cozy,fruit,quiet')
        logStep({
          role: 'Host',
          iteration,
          step: '6',
          action: 'Collect handover notes from all players',
          expected: 'All players save their handover tags',
          actual: 'Five handover submissions completed',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Handover',
          playerCount: '5',
          page: host.page,
        })

        const snapshot = await readSessionSnapshot(agent3.page)
        await agent3.context.close()
        agent3 = await freshPlayerCtx(browser, 'Agent 3')
        await cloneSignedInSession(host.page, agent3.context, agent3.page, snapshot)
        await expect(agent3.page.getByTestId('session-panel')).toContainText('Shift Change!')
        logStep({
          role: 'Agent 3',
          iteration,
          step: '7',
          action: 'Restore from fresh context via browser storage bootstrap',
          expected: 'Fresh context reconnects into current handover screen',
          actual: 'Fresh context restored Shift Change screen',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Handover',
          playerCount: '5',
          page: agent3.page,
        })

        await enterPhase2(host.page, agent1.page, agent2.page, agent3.page, agent4.page)
        await enterJudge(host.page, agent1.page, agent2.page, agent3.page, agent4.page)
        await enterVoting(host.page, agent1.page, agent2.page, agent3.page, agent4.page)
        logStep({
          role: 'Host',
          iteration,
          step: '8',
          action: 'Advance to design voting',
          expected: 'All players reach voting view',
          actual: 'Voting grid is visible on all clients',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Voting',
          playerCount: '5',
          page: host.page,
        })

        await voteForVisibleDragon(agent1.page)
        await voteForVisibleDragon(agent2.page)
        await voteForVisibleDragon(agent3.page)
        await voteForVisibleDragon(agent4.page)
        await voteForVisibleDragon(host.page)
        await expect(host.page.getByTestId('session-panel')).toContainText('5 / 5 votes submitted')
        logStep({
          role: 'Host',
          iteration,
          step: '9',
          action: 'Collect all votes',
          expected: 'Voting reaches 5 / 5 submitted',
          actual: 'Voting progress reached 5 / 5',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Voting',
          playerCount: '5',
          page: host.page,
        })

        await host.page.getByTestId('reveal-results-button').click()
        await waitForNotice(host.page, 'Voting finished.')
        await host.page.getByTestId('end-session-button').click()
        await waitForNotice(host.page, 'Game over ready.')
        await dismissGameOverOverlay(host.page, agent1.page, agent2.page, agent3.page, agent4.page)
        logStep({
          role: 'Host',
          iteration,
          step: '10',
          action: 'Reveal results and open final end screen',
          expected: 'All players see final end screen after overlay dismissal',
          actual: 'Game over and leaderboard visible on all clients',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'End',
          playerCount: '5',
          page: host.page,
        })

        await host.page.getByTestId('reset-game-button').click()
        await waitForNotice(host.page, 'Workshop reset.')
        await expect(host.page.getByTestId('session-panel')).toContainText('Workshop Lobby')
        logStep({
          role: 'Host',
          iteration,
          step: '11',
          action: 'Reset workshop back to lobby',
          expected: 'Workshop returns to lobby',
          actual: 'Workshop Lobby visible again',
          status: 'pass',
          kind: 'ok',
          workshopCode,
          phase: 'Lobby',
          playerCount: '5',
          page: host.page,
        })

        logger.writeIterationLogs(iteration)
        logger.writeIterationSummary(iteration, {
          goal: 'Validate the current account-home, explicit join, storage-bootstrap reconnect flow.',
          issuesFound: [],
          frictionPoints: [],
          fixesToCarryForward: ['None identified in this iteration'],
          passFail: 'PASS',
        })

        if (iteration === 3) {
          logger.writeFinalSummary({
            overallResult: 'PASS — current supported flow completed across all iterations.',
            recurringIssues: ['None'],
            biggestImprovements: [
              'Explicit host join is exercised directly',
              'Fresh-context reconnect uses the supported browser-storage bootstrap path',
              'Legacy manual reconnect and manual join assumptions are removed from the scenario',
            ],
            nextChanges: [
              'Run the scenario against rebuilt local kind and capture real artifacts',
              'Decide whether workshop archive build should regain an explicit host trigger in the UI',
            ],
          })
        }
      } finally {
        await host.context.close().catch(() => undefined)
        await agent1.context.close().catch(() => undefined)
        await agent2.context.close().catch(() => undefined)
        await agent3.context.close().catch(() => undefined)
        await agent4.context.close().catch(() => undefined)
      }
    })
  }
})
