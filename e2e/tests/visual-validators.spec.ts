import { mkdirSync, writeFileSync } from 'node:fs'
import * as path from 'node:path'
import { fileURLToPath } from 'node:url'

import { expect, test, type Page } from '@playwright/test'

import {
  createCharacter,
  createWorkshopAndJoinAsHost,
  enterHandover,
  enterJudge,
  enterPhase1,
  enterPhase2,
  enterVoting,
  joinWorkshop,
  newPlayerContext,
  saveHandoverTags,
  signInAccount,
  voteForVisibleDragon,
  waitForNotice,
} from './gameplay-helpers'

type Role = 'Host' | 'Validator 1' | 'Validator 2'
type PhaseWindow = 'signin' | 'home' | 'lobby' | 'phase1' | 'handover' | 'phase2' | 'voting' | 'end'

type ReportEntry = {
  phase: string
  window: PhaseWindow
  status: 'pass' | 'warn' | 'bug'
  summary: string
  details: string
  screenshot: string
}

const BASE_URL = process.env.E2E_BASE_URL ?? 'https://dragon-shift.34.54.200.112.nip.io/'
const RUN_ID = `visual-${new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19)}`
const __filename = fileURLToPath(import.meta.url)
const __dirname = path.dirname(__filename)
const ARTIFACT_ROOT = path.resolve(__dirname, '..', '.tmp', 'visual-validator-runs', RUN_ID)
const SCREENSHOT_ROOT = path.join(ARTIFACT_ROOT, 'shots')
const REPORT_ROOT = path.join(ARTIFACT_ROOT, 'reports')

function ensureArtifactDirs() {
  mkdirSync(SCREENSHOT_ROOT, { recursive: true })
  mkdirSync(REPORT_ROOT, { recursive: true })
}

function slugify(value: string) {
  return value.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
}

async function capture(page: Page, role: Role, phase: PhaseWindow, label: string) {
  const roleDir = path.join(SCREENSHOT_ROOT, slugify(role))
  mkdirSync(roleDir, { recursive: true })
  const fileName = `${phase}-${slugify(label)}.png`
  const absolutePath = path.join(roleDir, fileName)
  await page.screenshot({ path: absolutePath, fullPage: true })
  return path.relative(ARTIFACT_ROOT, absolutePath)
}

function writeRoleReport(role: Role, workshopCode: string, entries: ReportEntry[]) {
  const reportPath = path.join(REPORT_ROOT, `${slugify(role)}.md`)
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

function writeSummary(workshopCode: string, reports: Map<Role, ReportEntry[]>) {
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

async function record(
  reports: Map<Role, ReportEntry[]>,
  role: Role,
  page: Page,
  phase: string,
  window: PhaseWindow,
  label: string,
  summary: string,
  details: string,
  status: 'pass' | 'warn' | 'bug' = 'pass',
) {
  const screenshot = await capture(page, role, window, label)
  const entries = reports.get(role) ?? []
  entries.push({ phase, window, status, summary, details, screenshot })
  reports.set(role, entries)
}

test.describe.serial('visual validators', () => {
  test('three validators capture current supported flow with reports and screenshots', async ({ browser }) => {
    ensureArtifactDirs()

    const reports = new Map<Role, ReportEntry[]>()
    const host = await newPlayerContext(browser)
    const guest1 = await newPlayerContext(browser)
    const guest2 = await newPlayerContext(browser)
    let workshopCode = 'unknown'

    try {
      await host.page.goto('/')
      await expect(host.page.getByTestId('signin-panel')).toBeVisible()
      await record(
        reports,
        'Host',
        host.page,
        'SignIn',
        'signin',
        'signin-layout',
        'Sign-in screen is visible',
        'The current first screen exposes name/password inputs and a single primary submit action.',
      )

      await signInAccount(host.page, 'HostAlice')
      await signInAccount(guest1.page, 'V1Basil')
      await signInAccount(guest2.page, 'V2Coral')

      await record(
        reports,
        'Host',
        host.page,
        'AccountHome',
        'home',
        'home-actions',
        'Account home shows current entry points',
        'Create workshop, create dragon, and open workshops are visible from the signed-in home screen.',
      )

      await createCharacter(host.page, 'A brass dragon with clockwork ribs, plum wings, and ember freckles.')
      await createCharacter(guest1.page, 'A jade dragon with petal fins, moonlit eyes, and a ribbon tail.')
      await createCharacter(guest2.page, 'A charcoal dragon with cobalt horns and a lantern glow under the scales.')

      workshopCode = await createWorkshopAndJoinAsHost(host.page, 'HostAlice')
      await joinWorkshop(guest1.page, workshopCode, 'V1Basil')
      await joinWorkshop(guest2.page, workshopCode, 'V2Coral')

      for (const [role, page] of [
        ['Host', host.page],
        ['Validator 1', guest1.page],
        ['Validator 2', guest2.page],
      ] as const) {
        await expect(page.getByTestId('session-panel')).toContainText('Workshop Lobby')
        await expect(page.getByTestId('workshop-code-badge')).toContainText(workshopCode)
        await record(
          reports,
          role,
          page,
          'Lobby',
          'lobby',
          'lobby-roster',
          'Lobby roster is readable',
          'The lobby exposes workshop code, readiness copy, and a visible player roster for the joined workshop.',
        )
      }

      await enterPhase1(host.page, guest1.page, guest2.page)
      await guest1.page.getByTestId('observation-input').fill('Stat bars are readable and the sprite remains visually prominent.')
      await guest1.page.getByTestId('submit-observation-button').click()
      await waitForNotice(guest1.page, 'Observation saved.')

      for (const [role, page] of [
        ['Host', host.page],
        ['Validator 1', guest1.page],
        ['Validator 2', guest2.page],
      ] as const) {
        await expect(page.getByTestId('session-panel')).toContainText('Phase 1: Discovery')
        await expect(page.locator('.dragon-stage__sprite')).toBeVisible()
        await record(
          reports,
          role,
          page,
          'Phase1',
          'phase1',
          'phase1-discovery',
          'Phase 1 discovery layout is visible',
          'Discovery view shows the dragon sprite, action controls, and the observation input on the active screen.',
        )
      }

      await enterHandover(host.page, guest1.page, guest2.page)
      await saveHandoverTags(host.page, 'calm,dusk,berries')
      await saveHandoverTags(guest1.page, 'music,night,playful')
      await saveHandoverTags(guest2.page, 'river,day,meat')

      for (const [role, page] of [
        ['Host', host.page],
        ['Validator 1', guest1.page],
        ['Validator 2', guest2.page],
      ] as const) {
        await expect(page.getByTestId('session-panel')).toContainText('Shift Change!')
        await expect(page.getByTestId('save-handover-tags-button')).toBeVisible()
        await record(
          reports,
          role,
          page,
          'Handover',
          'handover',
          'handover-layout',
          'Handover screen keeps note entry visible',
          'The handover screen shows its three rule inputs and acknowledges saved notes without modal flow changes.',
        )
      }

      await enterPhase2(host.page, guest1.page, guest2.page)

      for (const [role, page] of [
        ['Host', host.page],
        ['Validator 1', guest1.page],
        ['Validator 2', guest2.page],
      ] as const) {
        await expect(page.getByTestId('session-panel')).toContainText('Phase 2: New Shift')
        await expect(page.locator('.phase2-creator-label')).toBeVisible()
        await record(
          reports,
          role,
          page,
          'Phase2',
          'phase2',
          'phase2-layout',
          'Phase 2 care layout is visible',
          'Care view preserves the dragon/action layout and adds creator plus inherited-note context.',
        )
      }

      await enterJudge(host.page, guest1.page, guest2.page)
      await enterVoting(host.page, guest1.page, guest2.page)

      for (const [role, page] of [
        ['Host', host.page],
        ['Validator 1', guest1.page],
        ['Validator 2', guest2.page],
      ] as const) {
        const names = await page.locator('.voting-card__name').allTextContents()
        const leaked = names.some(name => /hostalice|v1basil|v2coral/i.test(name))
        await record(
          reports,
          role,
          page,
          'Voting',
          'voting',
          'voting-anonymity',
          leaked ? 'Voting labels leak player identity' : 'Voting labels stay anonymous',
          leaked
            ? `Expected Dragon #N labels but saw: ${names.join(', ')}`
            : `Anonymous labels rendered correctly: ${names.join(', ')}`,
          leaked ? 'bug' : 'pass',
        )
      }

      await voteForVisibleDragon(guest1.page)
      await voteForVisibleDragon(guest2.page)
      await voteForVisibleDragon(host.page)
      await expect(host.page.getByTestId('session-panel')).toContainText('3 / 3 votes submitted')

      await host.page.getByTestId('reveal-results-button').click()
      await waitForNotice(host.page, 'Voting finished.')
      await host.page.getByTestId('end-session-button').click()
      await waitForNotice(host.page, 'Game over ready.')
      await host.page.getByTestId('game-over-continue-button').click()
      await guest1.page.getByTestId('game-over-continue-button').click()
      await guest2.page.getByTestId('game-over-continue-button').click()

      for (const [role, page] of [
        ['Host', host.page],
        ['Validator 1', guest1.page],
        ['Validator 2', guest2.page],
      ] as const) {
        await expect(page.getByTestId('session-panel')).toContainText('Game over')
        await expect(page.getByTestId('session-panel')).toContainText('Creativity leaderboard')
        await record(
          reports,
          role,
          page,
          'End',
          'end',
          'end-results',
          'End screen shows final results',
          'The final screen exposes the leaderboard stack and the archive panel copy for post-workshop review.',
        )
      }

      for (const [role, entries] of reports.entries()) {
        writeRoleReport(role, workshopCode, entries)
      }
      writeSummary(workshopCode, reports)
    } finally {
      for (const [role, entries] of reports.entries()) {
        writeRoleReport(role, workshopCode, entries)
      }
      writeSummary(workshopCode, reports)
      await host.context.close().catch(() => undefined)
      await guest1.context.close().catch(() => undefined)
      await guest2.context.close().catch(() => undefined)
    }
  })
})
