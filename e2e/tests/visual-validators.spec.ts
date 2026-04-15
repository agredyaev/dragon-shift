import { mkdirSync, writeFileSync } from 'node:fs'
import * as path from 'node:path'
import { fileURLToPath } from 'node:url'

import { expect, test, type Browser, type BrowserContext, type Page } from '@playwright/test'

import {
  createWorkshop,
  enterHandover,
  enterJudge,
  enterPhase1,
  enterPhase2,
  enterVoting,
  gotoApp,
  joinWorkshop,
  newPlayerContext,
  openCharacterCreation,
  generateDragonSprites,
  readReconnectToken,
  saveDragonProfile,
  saveHandoverTags,
  voteForVisibleDragon,
  waitForNotice,
} from './gameplay-helpers'

type ValidatorRole =
  | 'Host'
  | 'Validator 1'
  | 'Validator 2'
  | 'Validator 3'
  | 'Validator 4'
  | 'Validator 5'

type PhaseWindow = 'home' | 'lobby' | 'phase0' | 'phase1' | 'handover' | 'phase2' | 'judge' | 'voting' | 'end'

type ValidatorCtx = {
  role: ValidatorRole
  context: BrowserContext
  page: Page
  reconnectToken: string
}

type ReportEntry = {
  phase: string
  window: PhaseWindow
  status: 'pass' | 'warn' | 'bug'
  summary: string
  details: string
  screenshot: string
}

type PhaseCheck = {
  label: string
  fn: (page: Page) => Promise<{ summary: string; details: string; status?: 'pass' | 'warn' | 'bug' }>
}

const BASE_URL = process.env.E2E_BASE_URL ?? 'https://dragon-shift.34.54.200.112.nip.io/'
const RUN_ID = `visual-${new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19)}`
const __filename = fileURLToPath(import.meta.url)
const __dirname = path.dirname(__filename)
const ARTIFACT_ROOT = path.resolve(__dirname, '..', '.tmp', 'visual-validator-runs', RUN_ID)
const SCREENSHOT_ROOT = path.join(ARTIFACT_ROOT, 'shots')
const REPORT_ROOT = path.join(ARTIFACT_ROOT, 'reports')
const lobbyTitlePattern = /Workshop lobby|Waiting lobby/

const phase0Descriptions = [
  'A brass dragon with clockwork ribs, plum wings, and ember freckles.',
  'A jade dragon with petal fins, moonlit eyes, and a ribbon tail.',
  'A charcoal dragon with cobalt horns and a lantern glow under the scales.',
  'A coral dragon with shell-like cheeks, teal wings, and a comet tail.',
  'A lilac dragon with fern horns, glassy eyes, and silver claws.',
  'A sand-colored dragon with moss frills, bright whiskers, and heavy paws.',
]

const phase1Observations = [
  'Daytime hints are visible right away and the dragon reads as approachable.',
  'Stat bars are readable, but the action cluster feels dense on a single column.',
  'The observation input is clear, though save feedback is easy to miss while focused on stats.',
  'Speech hint adds personality, but it competes visually with the condition hint copy.',
  'The countdown chip is useful, though the round name and focus card feel slightly repetitive.',
  'Action cooldown feedback is understandable, but the disabled state needs stronger contrast.',
]

const handoverTags = [
  'calm,dusk,berries',
  'music,night,playful',
  'river,day,meat',
  'lantern,fetch,warm',
  'cozy,fruit,quiet',
  'windy,watchful,sleep',
]

const roleNames: Array<{ role: ValidatorRole; joinName: string }> = [
  { role: 'Host', joinName: 'HostAlice' },
  { role: 'Validator 1', joinName: 'V1Basil' },
  { role: 'Validator 2', joinName: 'V2Coral' },
  { role: 'Validator 3', joinName: 'V3Dune' },
  { role: 'Validator 4', joinName: 'V4Ember' },
  { role: 'Validator 5', joinName: 'V5Fable' },
]

function ensureArtifactDirs() {
  mkdirSync(SCREENSHOT_ROOT, { recursive: true })
  mkdirSync(REPORT_ROOT, { recursive: true })
}

function slugify(value: string) {
  return value.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
}

function roleFileStem(role: ValidatorRole) {
  return slugify(role)
}

async function capture(page: Page, role: ValidatorRole, phase: PhaseWindow, label: string) {
  const roleDir = path.join(SCREENSHOT_ROOT, roleFileStem(role))
  mkdirSync(roleDir, { recursive: true })
  const fileName = `${phase}-${slugify(label)}.png`
  const absolutePath = path.join(roleDir, fileName)
  await page.screenshot({ path: absolutePath, fullPage: true })
  return path.relative(ARTIFACT_ROOT, absolutePath)
}

async function openReconnectWindow(
  browser: Browser,
  workshopCode: string,
  reconnectToken: string,
  expectedText: string,
) {
  const player = await newPlayerContext(browser)
  await player.page.goto('/')

  const sessionPanel = player.page.getByTestId('session-panel')
  const reconnectButton = player.page.getByTestId('reconnect-button')

  await Promise.race([
    sessionPanel.waitFor({ state: 'visible', timeout: 30_000 }).then(() => 'session'),
    reconnectButton.waitFor({ state: 'visible', timeout: 30_000 }).then(() => 'home'),
  ])

  if (await reconnectButton.isVisible().catch(() => false)) {
    await player.page.getByTestId('reconnect-session-code-input').fill(workshopCode)
    await player.page.getByTestId('reconnect-token-input').fill(reconnectToken)
    await reconnectButton.click()
  }

  await expect(sessionPanel).toContainText(expectedText)
  await expect(player.page.getByTestId('connection-badge')).toContainText('Connected')
  return player
}

async function rotateToWindow(
  browser: Browser,
  validator: ValidatorCtx,
  workshopCode: string,
  expectedText: string,
) {
  const reconnectToken = validator.reconnectToken
  await validator.context.close()
  const replacement = await openReconnectWindow(browser, workshopCode, reconnectToken, expectedText)
  validator.context = replacement.context
  validator.page = replacement.page
  validator.reconnectToken = await readReconnectToken(replacement.page)
}

async function collectChecks(
  page: Page,
  role: ValidatorRole,
  phase: string,
  window: PhaseWindow,
  checks: PhaseCheck[],
) {
  const entries: ReportEntry[] = []

  for (const check of checks) {
    const result = await check.fn(page)
    const screenshot = await capture(page, role, window, check.label)
    entries.push({
      phase,
      window,
      status: result.status ?? 'pass',
      summary: result.summary,
      details: result.details,
      screenshot,
    })
  }

  return entries
}

function writeRoleReport(role: ValidatorRole, workshopCode: string, entries: ReportEntry[]) {
  const reportPath = path.join(REPORT_ROOT, `${roleFileStem(role)}.md`)
  const lines = [
    `# ${role}`,
    '',
    `- Run ID: ${RUN_ID}`,
    `- Base URL: ${BASE_URL}`,
    `- Workshop code: ${workshopCode}`,
    '',
    '## Entries',
    '',
  ]

  for (const entry of entries) {
    lines.push(`### ${entry.phase} :: ${entry.summary}`)
    lines.push(`- Window: ${entry.window}`)
    lines.push(`- Status: ${entry.status}`)
    lines.push(`- Details: ${entry.details}`)
    lines.push(`- Screenshot: ${entry.screenshot}`)
    lines.push('')
  }

  writeFileSync(reportPath, lines.join('\n'), 'utf8')
}

function writeSummary(workshopCode: string, reports: Map<ValidatorRole, ReportEntry[]>) {
  const allEntries = Array.from(reports.values()).flat()
  const bugs = allEntries.filter(entry => entry.status === 'bug')
  const warnings = allEntries.filter(entry => entry.status === 'warn')
  const lines = [
    '# Visual Validators Summary',
    '',
    `- Run ID: ${RUN_ID}`,
    `- Base URL: ${BASE_URL}`,
    `- Workshop code: ${workshopCode}`,
    `- Reports: ${path.relative(ARTIFACT_ROOT, REPORT_ROOT)}`,
    `- Screenshots: ${path.relative(ARTIFACT_ROOT, SCREENSHOT_ROOT)}`,
    '',
    '## Status',
    '',
    `- Total checks: ${allEntries.length}`,
    `- Bugs: ${bugs.length}`,
    `- Warnings: ${warnings.length}`,
    '',
    '## Bugs',
    '',
    ...(bugs.length > 0
      ? bugs.map(entry => `- ${entry.phase} / ${entry.window}: ${entry.summary} -> ${entry.details}`)
      : ['- None recorded']),
    '',
    '## Warnings',
    '',
    ...(warnings.length > 0
      ? warnings.map(entry => `- ${entry.phase} / ${entry.window}: ${entry.summary} -> ${entry.details}`)
      : ['- None recorded']),
    '',
  ]

  writeFileSync(path.join(ARTIFACT_ROOT, 'summary.md'), lines.join('\n'), 'utf8')
}

async function roleChecksForHome(page: Page) {
  return collectChecks(page, 'Host', 'Home', 'home', [
    {
      label: 'home-overview',
      fn: async currentPage => {
        await expect(currentPage.getByTestId('hero-panel')).toBeVisible()
        await expect(currentPage.getByTestId('create-panel')).toBeVisible()
        await expect(currentPage.getByTestId('join-panel')).toBeVisible()
        return {
          summary: 'Home screen is structurally complete',
          details: 'Hero, create panel, and join panel are all visible on first load.',
        }
      },
    },
  ])
}

test.describe.serial('visual validators', () => {
  test('six validators capture separate phase windows with reports and screenshots', async ({ browser }) => {
    ensureArtifactDirs()

    const validators: ValidatorCtx[] = []
    const reports = new Map<ValidatorRole, ReportEntry[]>()
    let workshopCode = ''

    try {
      for (const { role } of roleNames) {
        const player = await newPlayerContext(browser)
        await gotoApp(player.page)
        validators.push({
          role,
          context: player.context,
          page: player.page,
          reconnectToken: '',
        })
      }

      const host = validators[0]
      const guests = validators.slice(1)

      reports.set(host.role, await roleChecksForHome(host.page))

      workshopCode = await createWorkshop(host.page, roleNames[0].joinName)
      host.reconnectToken = await readReconnectToken(host.page)

      for (let index = 0; index < guests.length; index++) {
        const guest = guests[index]
        await joinWorkshop(guest.page, workshopCode, roleNames[index + 1].joinName)
        guest.reconnectToken = await readReconnectToken(guest.page)
      }

      for (const validator of validators) {
        const entries = reports.get(validator.role) ?? []
        entries.push(
          ...(await collectChecks(validator.page, validator.role, 'Lobby', 'lobby', [
            {
              label: 'lobby-roster',
              fn: async currentPage => {
                await expect(currentPage.getByTestId('session-panel')).toContainText(lobbyTitlePattern)
                await expect(currentPage.getByTestId('session-panel')).toContainText('Players in view: 6')
                return {
                  summary: 'Lobby sync is visible for all 6 players',
                  details: 'Roster and summary chips agree on a 6-player workshop in the waiting lobby.',
                }
              },
            },
          ])),
        )
        reports.set(validator.role, entries)
      }

      await openCharacterCreation(host.page, ...guests.map(validator => validator.page))

      for (const validator of validators) {
        await rotateToWindow(browser, validator, workshopCode, 'Character creation')
      }

      for (let index = 0; index < validators.length; index++) {
        const validator = validators[index]
        await generateDragonSprites(validator.page)
        await saveDragonProfile(validator.page, phase0Descriptions[index])
        validator.reconnectToken = await readReconnectToken(validator.page)

        const entries = reports.get(validator.role) ?? []
        entries.push(
          ...(await collectChecks(validator.page, validator.role, 'Phase 0', 'phase0', [
            {
              label: 'phase0-layout',
              fn: async currentPage => {
                await expect(currentPage.getByTestId('session-panel')).toContainText('Character creation')
                await expect(currentPage.getByTestId('dragon-description-input')).toBeVisible()
                await expect(currentPage.getByTestId('sprite-preview-image')).toBeVisible()
                await expect(currentPage.getByTestId('save-dragon-button')).toBeVisible()
                return {
                  summary: 'Phase 0 character creation is usable',
                  details: 'Description field, generated sprite preview, save button, and phase copy are present in the character creation window.',
                }
              },
            },
            {
              label: 'phase0-feedback',
              fn: async currentPage => {
                await expect(currentPage.getByTestId('notice-bar')).toContainText('Dragon profile saved.')
                return {
                  summary: 'Phase 0 save feedback is visible',
                  details: 'Saving the dragon now produces an explicit success notice instead of a generic command response.',
                }
              },
            },
          ])),
        )
        reports.set(validator.role, entries)
      }

      await enterPhase1(host.page, ...guests.map(validator => validator.page))

      for (const validator of validators) {
        await rotateToWindow(browser, validator, workshopCode, 'Discovery round')
      }

      for (let index = 0; index < validators.length; index++) {
        const validator = validators[index]
        await validator.page.getByTestId('observation-input').fill(phase1Observations[index])
        await validator.page.getByTestId('submit-observation-button').click()
        await waitForNotice(validator.page, 'Observation saved.')
        validator.reconnectToken = await readReconnectToken(validator.page)

        const entries = reports.get(validator.role) ?? []
        entries.push(
          ...(await collectChecks(validator.page, validator.role, 'Phase 1', 'phase1', [
            {
              label: 'phase1-visibility',
              fn: async currentPage => {
                await expect(currentPage.getByTestId('session-panel')).toContainText('Discovery round')
                await expect(currentPage.getByTestId('observation-input')).toBeVisible()
                await expect(currentPage.getByTestId('action-feed-meat')).toBeVisible()
                return {
                  summary: 'Phase 1 discovery tools are visible',
                  details: 'Observation input and action controls appear in the dedicated discovery window.',
                }
              },
            },
          ])),
        )
        reports.set(validator.role, entries)
      }

      await enterHandover(host.page, ...guests.map(validator => validator.page))

      for (const validator of validators) {
        await rotateToWindow(browser, validator, workshopCode, 'Handover')
      }

      for (let index = 0; index < validators.length; index++) {
        const validator = validators[index]
        await saveHandoverTags(validator.page, handoverTags[index])
        validator.reconnectToken = await readReconnectToken(validator.page)

        const entries = reports.get(validator.role) ?? []
        entries.push(
          ...(await collectChecks(validator.page, validator.role, 'Handover', 'handover', [
            {
              label: 'handover-entry',
              fn: async currentPage => {
                await expect(currentPage.getByTestId('session-panel')).toContainText('Handover')
                await expect(currentPage.getByTestId('handover-tags-input')).toBeVisible()
                await expect(currentPage.getByTestId('notice-bar')).toContainText('Handover tags saved.')
                return {
                  summary: 'Handover window accepts rules and confirms save',
                  details: 'The dedicated handover window keeps tag entry visible and acknowledges each save clearly.',
                }
              },
            },
          ])),
        )
        reports.set(validator.role, entries)
      }

      await enterPhase2(host.page, ...guests.map(validator => validator.page))

      for (const validator of validators) {
        await rotateToWindow(browser, validator, workshopCode, 'Care round')
      }

      for (const validator of validators) {
        const entries = reports.get(validator.role) ?? []
        entries.push(
          ...(await collectChecks(validator.page, validator.role, 'Phase 2', 'phase2', [
            {
              label: 'phase2-handover-context',
              fn: async currentPage => {
                await expect(currentPage.getByTestId('session-panel')).toContainText('Care round')
                await expect(currentPage.getByTestId('session-panel')).toContainText('Handover notes from previous caretaker')
                return {
                  summary: 'Phase 2 care window shows inherited context',
                  details: 'Care round keeps the action controls while surfacing handover guidance above the stats.',
                }
              },
            },
            {
              label: 'phase2-observation-hidden',
              fn: async currentPage => {
                const count = await currentPage.getByTestId('observation-input').count()
                return count === 0
                  ? {
                      summary: 'Phase 2 correctly removes discovery input',
                      details: 'Observation authoring is hidden in care mode, which keeps the phase focused.',
                    }
                  : {
                      status: 'bug',
                      summary: 'Phase 2 still exposes discovery input',
                      details: 'Observation controls should be absent during the care window.',
                    }
              },
            },
          ])),
        )
        reports.set(validator.role, entries)
      }

      await enterJudge(host.page, ...guests.map(validator => validator.page))

      for (const validator of validators) {
        await rotateToWindow(browser, validator, workshopCode, 'Judge review')
      }

      for (const validator of validators) {
        const entries = reports.get(validator.role) ?? []
        entries.push(
          ...(await collectChecks(validator.page, validator.role, 'Judge', 'judge', [
            {
              label: 'judge-review-layout',
              fn: async currentPage => {
                await expect(currentPage.getByTestId('session-panel')).toContainText('Judge review')
                await expect(currentPage.getByTestId('session-panel')).toContainText('Judge feedback by dragon')
                return {
                  summary: 'Judge review separates mechanics feedback from voting',
                  details: 'The dedicated judge window shows mechanics scores and qualitative feedback before anonymous voting starts.',
                }
              },
            },
          ])),
        )
        reports.set(validator.role, entries)
      }

      await enterVoting(host.page, ...guests.map(validator => validator.page))

      for (const validator of validators) {
        await rotateToWindow(browser, validator, workshopCode, 'Design voting')
      }

      for (const validator of validators) {
        const entries = reports.get(validator.role) ?? []
        entries.push(
          ...(await collectChecks(validator.page, validator.role, 'Voting', 'voting', [
            {
              label: 'voting-anonymity',
              fn: async currentPage => {
                await expect(currentPage.getByTestId('session-panel')).toContainText('Design voting')
                const cardNames = await currentPage.locator('.voting-card__name').allTextContents()
                const leaksPlayerName = cardNames.some(name => /hostalice|v1basil|v2coral|v3dune|v4ember|v5fable/i.test(name))
                return leaksPlayerName
                  ? {
                      status: 'bug',
                      summary: 'Voting leaks player identity in card labels',
                      details: `Expected anonymous dragon labels, but saw: ${cardNames.join(', ')}`,
                    }
                  : {
                      summary: 'Voting cards stay anonymous',
                      details: `Card labels stayed anonymized: ${cardNames.join(', ')}`,
                    }
              },
            },
          ])),
        )
        reports.set(validator.role, entries)
      }

      for (const validator of guests) {
        await voteForVisibleDragon(validator.page)
      }
      await voteForVisibleDragon(host.page)
      await expect(host.page.getByTestId('session-panel')).toContainText('6 / 6 votes submitted')

      await host.page.getByTestId('reveal-results-button').click()
      await waitForNotice(host.page, 'Voting results revealed.')

      for (const validator of validators) {
        await expect(validator.page.getByTestId('session-panel')).toContainText('Workshop results')
      }

      for (const validator of validators) {
        await rotateToWindow(browser, validator, workshopCode, 'Workshop results')
      }

      for (const validator of validators) {
        const entries = reports.get(validator.role) ?? []
        entries.push(
          ...(await collectChecks(validator.page, validator.role, 'End', 'end', [
            {
              label: 'end-leaderboards',
              fn: async currentPage => {
                await expect(currentPage.getByTestId('session-panel')).toContainText('Workshop results')
                await expect(currentPage.getByTestId('session-panel')).toContainText('Creativity Leaderboard')
                await expect(currentPage.getByTestId('session-panel')).toContainText('Mechanics leaderboard')
                return {
                  summary: 'End screen shows split leaderboards',
                  details: 'Creative and mechanics rankings are both visible after reveal in the final window.',
                }
              },
            },
            {
              label: 'end-archive-affordance',
              fn: async currentPage => {
                const archiveVisible = await currentPage.getByTestId('archive-panel').isVisible()
                return archiveVisible
                  ? {
                      summary: 'Archive panel stays visible in final results',
                      details: 'Final results keep the archive affordance nearby, which helps post-workshop review.',
                    }
                  : {
                      status: 'warn',
                      summary: 'Archive panel is not visible in final results',
                      details: 'Expected archive affordance in the final window, but it was not visible.',
                    }
              },
            },
          ])),
        )
        reports.set(validator.role, entries)
      }

      for (const validator of validators) {
        writeRoleReport(validator.role, workshopCode, reports.get(validator.role) ?? [])
      }
      writeSummary(workshopCode, reports)
    } finally {
      for (const validator of validators) {
        writeRoleReport(validator.role, workshopCode || 'unknown', reports.get(validator.role) ?? [])
      }
      writeSummary(workshopCode || 'unknown', reports)
      for (const validator of validators) {
        await validator.context.close().catch(() => undefined)
      }
    }
  })
})
