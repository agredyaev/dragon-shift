import { expect, test, type Browser, type BrowserContext, type Page, type Response } from '@playwright/test'
import * as fs from 'node:fs'
import * as path from 'node:path'
import { fileURLToPath } from 'node:url'

import { getProjectContextOptions } from '../project-profiles'
import { waitForNotice } from './gameplay-helpers'

const __filename = fileURLToPath(import.meta.url)
const __dirname = path.dirname(__filename)

const BASE_URL = process.env.E2E_BASE_URL ?? 'https://dragon-shift.34.49.131.192.nip.io'
const BASE_ORIGIN = new URL(BASE_URL).origin
const PASSWORD = process.env.E2E_CLIENT_PASSWORD ?? 'Qq12345678'
const CLIENT_COUNT = Number(process.env.E2E_CLIENT_COUNT ?? 30)
const CLIENT_NAME_OFFSET = Number(process.env.E2E_CLIENT_NAME_OFFSET ?? 1)
const CLIENT_NAME_PREFIX = process.env.E2E_CLIENT_NAME_PREFIX ?? 'test'
const ALLOW_ACCOUNT_CREATION = (process.env.E2E_ALLOW_ACCOUNT_CREATION ?? 'false').trim().toLowerCase() === 'true'
const ALLOW_STARTER_FALLBACK = (process.env.E2E_ALLOW_STARTER_FALLBACK ?? 'false').trim().toLowerCase() === 'true'
const EXTERNAL_WORKSHOP_CODE = process.env.E2E_EXTERNAL_WORKSHOP_CODE ?? ''
const HOST_ACCOUNT_NAME = process.env.E2E_HOST_ACCOUNT_NAME ?? 'test1'
const PHASE_DURATION_MS = 5 * 60_000
const PHASE1_ACTION_INTERVAL_MS = 22_000
const PHASE1_OBSERVATION_INTERVAL_MS = 70_000
const PHASE2_ACTION_INTERVAL_MS = 18_000
const SESSION_PROGRESS_TIMEOUT_MS = Number(process.env.E2E_SESSION_PROGRESS_TIMEOUT_MS ?? 20 * 60_000)
const SCORE_SYNC_TIMEOUT_MS = Number(process.env.E2E_SCORE_SYNC_TIMEOUT_MS ?? 15 * 60_000)
const MANUAL_ARCHIVE_WAIT_MS = Number(process.env.E2E_MANUAL_ARCHIVE_WAIT_MS ?? 30 * 60_000)
const RESPONSE_TIMEOUT_MS = 240_000
const COVERAGE_TARGET = (process.env.E2E_COVERAGE_TARGET ?? 'archive').trim().toLowerCase()
const RUN_ID = `load30-guests-${new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19)}`
const LOG_DIR = path.resolve(__dirname, '..', '.tmp', 'load-test-runs', RUN_ID)
const EVENTS_PATH = path.join(LOG_DIR, 'events.ndjson')
const SUMMARY_PATH = path.join(LOG_DIR, 'summary.md')
const SCREENSHOTS_DIR = path.join(LOG_DIR, 'screenshots')
const lobbyTitlePattern = /Workshop Lobby|Waiting lobby/

const phase1ActionIds = [
  'action-feed-meat',
  'action-play-music',
  'action-feed-fruit',
  'action-play-puzzle',
  'action-feed-fish',
  'action-play-fetch',
  'action-sleep',
] as const

const phase2ActionIds = [
  'action-play-fetch',
  'action-feed-meat',
  'action-play-music',
  'action-feed-fruit',
  'action-play-puzzle',
  'action-feed-fish',
  'action-sleep',
] as const

const observationMotifs = [
  'keeps following neon reflections',
  'reacts to synth beats before moving',
  'tilts its head when the lights flicker',
  'paces like it is guarding an arcade cabinet',
  'calms down when the room feels quieter',
  'perks up when food arrives after a short pause',
] as const

const handoverTagSets = [
  ['neon', 'music', 'patient'],
  ['arcade', 'night', 'gentle'],
  ['vhs', 'day', 'steady'],
  ['matrix', 'quiet', 'snacks'],
  ['synth', 'calm', 'rest'],
  ['comet', 'warm', 'puzzle'],
] as const

type Severity = 'info' | 'warn' | 'error' | 'blocker'
type Stage =
  | 'setup'
  | 'signin'
  | 'lobby'
  | 'phase1'
  | 'handover'
  | 'phase2'
  | 'score'
  | 'manual'
  | 'final'

type CharacterSource = 'owned' | 'starter' | 'unknown'

interface EventRecord {
  timestamp_utc: string
  severity: Severity
  stage: Stage
  player: string
  role: 'host' | 'guest'
  message: string
  detail: string
  url: string
}

interface IssueCount {
  key: string
  count: number
}

interface ClientCtx {
  index: number
  name: string
  role: 'host' | 'guest'
  context: BrowserContext
  page: Page
  currentStage: Stage
  signInStatus: 'logged-in' | 'failed' | 'pending'
  joinedWorkshop: boolean
  selectedCharacterSource: CharacterSource
  handoverSaved: boolean
  scoreReady: boolean
  phase1Actions: number
  phase1Observations: number
  phase2Actions: number
  xForwardedFor: string
}

function delay(ms: number) {
  return new Promise(resolve => setTimeout(resolve, ms))
}

function truncate(text: string, max = 400) {
  return text.length > max ? `${text.slice(0, max)}...` : text
}

function createObservation(client: ClientCtx, count: number) {
  const motif = observationMotifs[(client.index + count) % observationMotifs.length]
  return `Observation ${count + 1}: ${client.name}'s dragon ${motif}.`
}

function createHandoverTags(client: ClientCtx) {
  return handoverTagSets[(client.index - 1) % handoverTagSets.length]
}

async function readNoticeText(page: Page) {
  const notice = page.getByTestId('notice-bar')
  if (!(await notice.count())) {
    return ''
  }
  return ((await notice.textContent()) ?? '').trim()
}

async function safeResponseBody(response: Response) {
  try {
    return truncate(await response.text())
  } catch {
    return ''
  }
}

function safeRequestBody(page: Page, response: Response) {
  try {
    const body = response.request().postData() ?? ''
    return truncate(body)
  } catch {
    return ''
  }
}

function isPageClosedError(error: unknown) {
  return String(error).includes('Target page, context or browser has been closed')
}

function waitForApiResponse(page: Page, predicate: (response: Response) => boolean, timeout = RESPONSE_TIMEOUT_MS) {
  const pending = page.waitForResponse(predicate, { timeout })
  pending.catch(() => undefined)
  return pending
}

function formatDiagnosticDetail(parts: Array<string>) {
  return parts.filter(Boolean).join(' | ')
}

function topCounts(items: string[], limit: number): IssueCount[] {
  const counts = new Map<string, number>()
  for (const item of items) {
    counts.set(item, (counts.get(item) ?? 0) + 1)
  }
  return [...counts.entries()]
    .map(([key, count]) => ({ key, count }))
    .sort((left, right) => right.count - left.count || left.key.localeCompare(right.key))
    .slice(0, limit)
}

function extractStatusCode(event: Pick<EventRecord, 'message' | 'detail'>) {
  const responseMatch = event.message.match(/^http\.(\d{3})$/)
  if (responseMatch) {
    return responseMatch[1]
  }

  const detailMatch = event.detail.match(/\bstatus=(\d{3})\b/)
  return detailMatch?.[1] ?? null
}

function sanitizeArtifactSegment(value: string) {
  const sanitized = value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
  return sanitized.slice(0, 60) || 'item'
}

async function readConnectionText(page: Page) {
  return ((await page.getByTestId('connection-badge').textContent().catch(() => '')) ?? '').trim()
}

async function readWorkshopCodeText(page: Page) {
  return ((await page.getByTestId('workshop-code-badge').textContent().catch(() => '')) ?? '').trim()
}

async function readSessionPanelText(page: Page) {
  return truncate(((await page.getByTestId('session-panel').textContent().catch(() => '')) ?? '').replace(/\s+/g, ' ').trim(), 300)
}

async function buildClientSnapshot(client: ClientCtx) {
  const [notice, connection, workshopCode, sessionText] = await Promise.all([
    readNoticeText(client.page),
    readConnectionText(client.page),
    readWorkshopCodeText(client.page),
    readSessionPanelText(client.page),
  ])

  return formatDiagnosticDetail([
    connection ? `connection=${connection}` : '',
    workshopCode ? `workshop=${workshopCode}` : '',
    notice ? `notice=${notice}` : '',
    sessionText ? `session=${sessionText}` : '',
  ])
}

async function waitForOwnedCharacterChoice(client: ClientCtx, timeout = RESPONSE_TIMEOUT_MS) {
  const deadline = Date.now() + timeout

  while (Date.now() < deadline) {
    const selectButtons = client.page.getByTestId('select-character-button')
    const starterButton = client.page.getByTestId('use-starter-button')

    if (await selectButtons.count()) {
      await expect(selectButtons.first()).toBeVisible({ timeout: 5_000 })
      return 'owned' as const
    }

    if (await starterButton.count()) {
      await expect(starterButton).toBeVisible({ timeout: 5_000 })
      return 'starter' as const
    }

    await client.page.waitForTimeout(250)
  }

  throw new Error(`character choice did not become visible for ${client.name} | ${await buildClientSnapshot(client)}`)
}

function isSameOrigin(url: string) {
  try {
    return new URL(url).origin === BASE_ORIGIN
  } catch {
    return false
  }
}

function formatMs(ms: number) {
  const totalSeconds = Math.max(0, Math.round(ms / 1000))
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  return `${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`
}

test.use({
  actionTimeout: 30_000,
  navigationTimeout: 120_000,
})

test.describe.serial('live ui load test - guest workshop accounts', () => {
  test('guest accounts authenticate, join workshops, stay active across phases, and wait for manual host progress', async ({ browser }) => {
    test.skip(test.info().project.name !== 'chromium', 'load test runs in the single desktop project only')
    test.setTimeout(120 * 60_000)

    fs.mkdirSync(SCREENSHOTS_DIR, { recursive: true })
    fs.writeFileSync(EVENTS_PATH, '', 'utf-8')

    const events: EventRecord[] = []
    const blockers: string[] = []
    const warnings: string[] = []
    const disconnectSamples = new Set<string>()
    const diagnosticScreenshotKeys = new Set<string>()
    let workshopCode = 'not-created'
    let phase1CountdownAtStart = 'missing'
    let phase2CountdownAtStart = 'missing'
    let manualCompletionSignal = 'not-observed'
    let phase2CoverageSignal = 'not-observed'

    function recordEvent(event: EventRecord) {
      events.push(event)
      fs.appendFileSync(EVENTS_PATH, `${JSON.stringify(event)}\n`, 'utf-8')
      if (event.severity === 'blocker') {
        blockers.push(`${event.stage} | ${event.player} | ${event.message} | ${event.detail}`)
      } else if (event.severity === 'warn' || event.severity === 'error') {
        warnings.push(`${event.stage} | ${event.player} | ${event.message} | ${event.detail}`)
      }
    }

    function log(client: ClientCtx | null, severity: Severity, stage: Stage, message: string, detail = '') {
      recordEvent({
        timestamp_utc: new Date().toISOString(),
        severity,
        stage,
        player: client?.name ?? 'system',
        role: client?.role ?? 'guest',
        message,
        detail: truncate(detail),
        url: client?.page.url() ?? BASE_URL,
      })
    }

    async function captureDiagnosticScreenshotOnce(client: ClientCtx, label: string) {
      const key = `${client.name}|${client.currentStage}|${label}`
      if (diagnosticScreenshotKeys.has(key)) {
        return
      }
      diagnosticScreenshotKeys.add(key)

      const fileName = `diagnostic-${sanitizeArtifactSegment(client.name)}-${sanitizeArtifactSegment(client.currentStage)}-${sanitizeArtifactSegment(label)}.png`
      const target = path.join(SCREENSHOTS_DIR, fileName)
      await client.page.screenshot({ path: target, fullPage: true }).catch(() => undefined)
    }

    function attachMonitors(client: ClientCtx) {
      client.page.on('console', msg => {
        if (msg.type() !== 'error' && msg.type() !== 'warning') {
          return
        }
        const location = msg.location()
        log(
          client,
          msg.type() === 'error' ? 'error' : 'warn',
          client.currentStage,
          `console.${msg.type()}`,
          formatDiagnosticDetail([
            location.url ? `location=${location.url}:${location.lineNumber}:${location.columnNumber}` : '',
            msg.text(),
          ]),
        )
        if (msg.type() === 'error') {
          void captureDiagnosticScreenshotOnce(client, 'console-error')
        }
      })

      client.page.on('pageerror', error => {
        void (async () => {
          const snapshot = await buildClientSnapshot(client)
          log(client, 'blocker', client.currentStage, 'pageerror', formatDiagnosticDetail([error.stack ?? error.message, snapshot]))
          await captureDiagnosticScreenshotOnce(client, 'pageerror')
        })()
      })

      client.page.on('requestfailed', request => {
        void (async () => {
          if (!isSameOrigin(request.url()) && !request.url().includes('/api/')) {
            return
          }
          const snapshot = await buildClientSnapshot(client)
          log(
            client,
            'error',
            client.currentStage,
            'requestfailed',
            formatDiagnosticDetail([
              `${request.method()} ${request.url()}`,
              `failure=${request.failure()?.errorText ?? 'unknown failure'}`,
              `postData=${truncate(request.postData() ?? '')}`,
              snapshot,
            ]),
          )
          await captureDiagnosticScreenshotOnce(client, 'requestfailed')
        })()
      })

      client.page.on('response', async response => {
        if (!isSameOrigin(response.url()) || response.status() < 400) {
          return
        }
        const severity: Severity = response.status() >= 500 ? 'error' : 'warn'
        const responseBody = await safeResponseBody(response)
        const snapshot = await buildClientSnapshot(client)
        log(
          client,
          severity,
          client.currentStage,
          `http.${response.status()}`,
          formatDiagnosticDetail([
            `${response.request().method()} ${response.url()}`,
            `postData=${safeRequestBody(client.page, response)}`,
            responseBody ? `response=${responseBody}` : '',
            snapshot,
          ]),
        )
        if (response.status() >= 500 || response.status() === 429) {
          await captureDiagnosticScreenshotOnce(client, `http-${response.status()}`)
        }
      })
    }

    async function createClient(browserInstance: Browser, index: number): Promise<ClientCtx> {
      const accountNumber = CLIENT_NAME_OFFSET + index - 1
      const context = await browserInstance.newContext(
        getProjectContextOptions(test.info().project.name, BASE_URL),
      )
      const xForwardedFor = `10.250.${Math.floor(accountNumber / 250)}.${(accountNumber % 250) + 1}`
      await context.setExtraHTTPHeaders({ 'X-Forwarded-For': xForwardedFor })
      const page = await context.newPage()
      const client: ClientCtx = {
        index,
        name: `${CLIENT_NAME_PREFIX}${accountNumber}`,
        role: 'guest',
        context,
        page,
        currentStage: 'setup',
        signInStatus: 'pending',
        joinedWorkshop: false,
        selectedCharacterSource: 'unknown',
        handoverSaved: false,
        scoreReady: false,
        phase1Actions: 0,
        phase1Observations: 0,
        phase2Actions: 0,
        xForwardedFor,
      }
      attachMonitors(client)
      return client
    }

    async function captureScreenshots(clients: ClientCtx[], label: string) {
      await Promise.allSettled(clients.map(async client => {
        const target = path.join(SCREENSHOTS_DIR, `${label}-${client.name}.png`)
        await client.page.screenshot({ path: target, fullPage: true })
      }))
    }

    async function signIn(client: ClientCtx): Promise<void> {
      client.currentStage = 'signin'
      await client.page.goto('/')
      await expect(client.page.getByTestId('signin-panel')).toBeVisible({ timeout: RESPONSE_TIMEOUT_MS })

      await client.page.getByTestId('signin-name-input').fill(client.name)
      await client.page.getByTestId('signin-password-input').fill(PASSWORD)

      const responsePromise = waitForApiResponse(client.page, response =>
        response.url().includes('/api/auth/signin')
        && response.request().method() === 'POST',
      )

      await client.page.getByTestId('signin-submit-button').click()
      const response = await responsePromise
      const body = await safeResponseBody(response)

      if (response.status() === 200) {
        client.signInStatus = 'logged-in'
        await expect(client.page.getByTestId('open-workshops-panel')).toBeVisible({ timeout: RESPONSE_TIMEOUT_MS })
        log(client, 'info', 'signin', 'signed in existing account', body || `status=${response.status()}`)
        return
      }

      if (response.status() === 201) {
        if (!ALLOW_ACCOUNT_CREATION) {
          client.signInStatus = 'failed'
          throw new Error(`unexpected account creation for ${client.name}; expected existing account login only`)
        }

        client.signInStatus = 'logged-in'
        await expect(client.page.getByTestId('open-workshops-panel')).toBeVisible({ timeout: RESPONSE_TIMEOUT_MS })
        log(client, 'info', 'signin', 'created and signed in account', body || `status=${response.status()}`)
        return
      }

      const notice = await readNoticeText(client.page)
      client.signInStatus = 'failed'
      throw new Error(`signin failed for ${client.name}: status=${response.status()} body=${body} notice=${notice}`)
    }

    async function createWorkshop(host: ClientCtx) {
      host.currentStage = 'lobby'
      await expect(host.page.getByTestId('open-workshops-panel')).toBeVisible({ timeout: 30_000 })
      await host.page.getByTestId('create-workshop-button').click()
      await waitForNotice(host.page, 'Workshop ')
      const noticeText = await readNoticeText(host.page)
      const match = noticeText.match(/Workshop\s+(\d{6})\s+created\./)
      if (!match) {
        throw new Error(`failed to extract workshop code from notice: ${noticeText}`)
      }
      workshopCode = match[1]
      await expect(host.page.getByTestId('open-workshops-panel')).toBeVisible({ timeout: RESPONSE_TIMEOUT_MS })
      await expect(host.page.getByTestId('session-panel')).toHaveCount(0)
      log(host, 'info', 'lobby', 'workshop created', workshopCode)
      return workshopCode
    }

    async function joinWorkshop(client: ClientCtx, code: string, retry = 0): Promise<void> {
      client.currentStage = 'lobby'
      await expect(client.page.getByTestId('open-workshops-panel')).toBeVisible({ timeout: 60_000 })
      const row = client.page.locator('.roster__item').filter({ hasText: code }).first()
      await expect(row).toBeVisible({ timeout: 60_000 })
      await row.getByTestId('join-workshop-button').click()
      await expect(client.page.getByTestId('pick-character-panel')).toBeVisible({ timeout: RESPONSE_TIMEOUT_MS })

      const joinPromise = waitForApiResponse(client.page, response =>
        response.url().includes('/api/workshops/join')
        && response.request().method() === 'POST',
      )

      const joinChoice = await waitForOwnedCharacterChoice(client)
      client.selectedCharacterSource = joinChoice
      if (joinChoice === 'owned') {
        await client.page.getByTestId('select-character-button').first().click()
      } else {
        if (!ALLOW_STARTER_FALLBACK) {
          throw new Error(`starter character required for ${client.name}, but starter fallback is disabled`)
        }
        await client.page.getByTestId('use-starter-button').click()
      }

      const joinResponse = await joinPromise
      const joinBody = await safeResponseBody(joinResponse)
      if (!joinResponse.ok()) {
        if (retry < 1) {
          log(client, 'warn', 'lobby', 'join failed, retrying once', `status=${joinResponse.status()} body=${joinBody}`)
          await client.page.goto('/')
          await expect(client.page.getByTestId('open-workshops-panel')).toBeVisible({ timeout: RESPONSE_TIMEOUT_MS })
          await delay(5_000)
          await joinWorkshop(client, code, retry + 1)
          return
        }
        throw new Error(`join failed for ${client.name}: status=${joinResponse.status()} body=${joinBody}`)
      }

      await expect(client.page.getByTestId('session-panel')).toBeVisible({ timeout: RESPONSE_TIMEOUT_MS })
      await expect(client.page.getByTestId('lobby-panel')).toBeVisible({ timeout: RESPONSE_TIMEOUT_MS })
      await expect(client.page.getByTestId('workshop-code-badge')).toContainText(code)
      await waitForNotice(client.page, 'Session synced.').catch(async () => {
        log(client, 'warn', 'lobby', 'join completed without synced notice', await readNoticeText(client.page))
      })

      client.joinedWorkshop = true
      log(client, 'info', 'lobby', 'joined workshop', `${code} | character=${client.selectedCharacterSource}`)
    }

    async function waitForAllClientsText(clients: ClientCtx[], expected: string | RegExp, timeout = RESPONSE_TIMEOUT_MS) {
      await Promise.all(clients.map(async client => {
        await expect(client.page.getByTestId('session-panel')).toContainText(expected, { timeout })
      }))
    }

    async function waitForAllClientsConnected(clients: ClientCtx[], timeout = RESPONSE_TIMEOUT_MS) {
      await Promise.all(clients.map(async client => {
        await expect(client.page.getByTestId('connection-badge')).toContainText('Connected', { timeout })
      }))
    }

    async function sampleConnected(client: ClientCtx, stage: Stage) {
      const connectionText = ((await client.page.getByTestId('connection-badge').textContent().catch(() => '')) ?? '').trim()
      if (connectionText === 'Connected') {
        return true
      }
      const key = `${client.name}|${stage}|${connectionText || 'missing'}`
      if (!disconnectSamples.has(key)) {
        disconnectSamples.add(key)
        log(client, 'blocker', stage, 'client not connected', formatDiagnosticDetail([connectionText || 'missing', await buildClientSnapshot(client)]))
        await captureDiagnosticScreenshotOnce(client, `connection-${stage}`)
      }
      return false
    }

    async function waitForPhase1(client: ClientCtx, timeout = SESSION_PROGRESS_TIMEOUT_MS) {
      client.currentStage = 'phase1'
      const deadline = Date.now() + timeout
      while (Date.now() < deadline) {
        await sampleConnected(client, 'phase1')
        const bodyText = ((await client.page.locator('body').textContent().catch(() => '')) ?? '')
        const observationCount = await client.page.getByTestId('observation-input').count().catch(() => 0)
        if (bodyText.includes('Phase 1: Discovery') && observationCount > 0) {
          if (phase1CountdownAtStart === 'missing') {
            const countdown = ((await client.page.getByTestId('phase-countdown').textContent().catch(() => '')) ?? '').trim()
            if (countdown) {
              phase1CountdownAtStart = countdown
            }
          }
          return
        }
        await client.page.waitForTimeout(1_000)
      }
      throw new Error(`phase 1 did not start for ${client.name} | ${await buildClientSnapshot(client)}`)
    }

    async function submitObservation(client: ClientCtx, text: string) {
      const responsePromise = waitForApiResponse(client.page, response =>
        response.url().includes('/api/workshops/command')
        && response.request().method() === 'POST',
      )

      await client.page.getByTestId('observation-input').fill(text)
      await client.page.getByTestId('submit-observation-button').click()
      const response = await responsePromise
      if (!response.ok()) {
        const body = await safeResponseBody(response)
        log(client, 'warn', 'phase1', 'submit observation returned non-ok response', `status=${response.status()} body=${body}`)
      }
      client.phase1Observations++
    }

    async function clickAction(client: ClientCtx, stage: Stage, actionId: string) {
      if (client.page.isClosed()) {
        return false
      }

      const button = client.page.getByTestId(actionId)
      if ((await button.count()) === 0) {
        log(client, 'warn', stage, 'action button missing', actionId)
        return false
      }
      if (!(await button.isEnabled())) {
        log(client, 'warn', stage, 'action button disabled', actionId)
        return false
      }

      const responsePromise = waitForApiResponse(client.page, response =>
        response.url().includes('/api/workshops/command')
        && response.request().method() === 'POST',
      )

      try {
        await button.click()
      } catch (error) {
        if (isPageClosedError(error)) {
          return false
        }
        throw error
      }

      const response = await responsePromise
      if (!response.ok()) {
        const body = await safeResponseBody(response)
        log(client, 'warn', stage, 'action returned non-ok response', `action=${actionId} status=${response.status()} body=${body}`)
        return false
      }

      return true
    }

    async function runPhase1Activity(client: ClientCtx) {
      await waitForPhase1(client)
      let actionCursor = client.index % phase1ActionIds.length
      let nextActionAt = Date.now() + (client.index % 10) * 900
      let nextObservationAt = Date.now() + (client.index % 7) * 1_700
      const deadline = Date.now() + PHASE_DURATION_MS + SESSION_PROGRESS_TIMEOUT_MS

      while (Date.now() < deadline) {
        await sampleConnected(client, 'phase1')

        if (await client.page.getByTestId('handover-rule-1').count().catch(() => 0)) {
          return
        }

        const bodyText = ((await client.page.locator('body').textContent().catch(() => '')) ?? '')
        if (bodyText.includes('Shift Change!')) {
          return
        }

        const now = Date.now()
        if (now >= nextObservationAt && await client.page.getByTestId('observation-input').count().catch(() => 0)) {
          await submitObservation(client, createObservation(client, client.phase1Observations)).catch(error => {
            log(client, 'warn', 'phase1', 'submit observation failed', String(error))
          })
          nextObservationAt += PHASE1_OBSERVATION_INTERVAL_MS
          continue
        }

        if (now >= nextActionAt) {
          const actionId = phase1ActionIds[actionCursor % phase1ActionIds.length]
          const clicked = await clickAction(client, 'phase1', actionId).catch(error => {
            log(client, 'warn', 'phase1', 'phase1 action failed', `${actionId} :: ${String(error)}`)
            return false
          })
          if (clicked) {
            client.phase1Actions++
          }
          actionCursor++
          nextActionAt += PHASE1_ACTION_INTERVAL_MS
          continue
        }

        await client.page.waitForTimeout(500)
      }

      throw new Error(`phase 1 did not transition to handover for ${client.name} | ${await buildClientSnapshot(client)}`)
    }

    async function waitForHandover(client: ClientCtx, timeout = SESSION_PROGRESS_TIMEOUT_MS) {
      client.currentStage = 'handover'
      const deadline = Date.now() + timeout
      while (Date.now() < deadline) {
        await sampleConnected(client, 'handover')
        const inputCount = await client.page.getByTestId('handover-rule-1').count().catch(() => 0)
        const bodyText = ((await client.page.locator('body').textContent().catch(() => '')) ?? '')
        if (inputCount > 0 || bodyText.includes('Shift Change!')) {
          return
        }
        await client.page.waitForTimeout(1_000)
      }
      throw new Error(`handover did not become available for ${client.name} | ${await buildClientSnapshot(client)}`)
    }

    async function saveHandover(client: ClientCtx) {
      await waitForHandover(client)
      const [tag1, tag2, tag3] = createHandoverTags(client)
      const responsePromise = waitForApiResponse(client.page, response =>
        response.url().includes('/api/workshops/command')
        && response.request().method() === 'POST',
      )

      await client.page.getByTestId('handover-rule-1').fill(tag1)
      await client.page.getByTestId('handover-rule-2').fill(tag2)
      await client.page.getByTestId('handover-rule-3').fill(tag3)
      await client.page.getByTestId('save-handover-tags-button').click()
      const response = await responsePromise
      if (!response.ok()) {
        const body = await safeResponseBody(response)
        throw new Error(`handover save failed for ${client.name}: status=${response.status()} body=${body}`)
      }

      client.handoverSaved = true
      await waitForNotice(client.page, 'Handover tags saved.').catch(async () => {
        log(client, 'warn', 'handover', 'handover save completed without success notice', await readNoticeText(client.page))
      })
    }

    async function waitForPhase2(client: ClientCtx, timeout = SESSION_PROGRESS_TIMEOUT_MS) {
      client.currentStage = 'phase2'
      const deadline = Date.now() + timeout
      while (Date.now() < deadline) {
        await sampleConnected(client, 'phase2')
        const creatorLabelCount = await client.page.locator('.phase2-creator-label').count().catch(() => 0)
        const bodyText = ((await client.page.locator('body').textContent().catch(() => '')) ?? '')
        if (creatorLabelCount > 0 || bodyText.includes('Phase 2: New Shift')) {
          if (phase2CountdownAtStart === 'missing') {
            const countdown = ((await client.page.getByTestId('phase-countdown').textContent().catch(() => '')) ?? '').trim()
            if (countdown) {
              phase2CountdownAtStart = countdown
            }
          }
          return
        }
        await client.page.waitForTimeout(1_000)
      }
      throw new Error(`phase 2 did not start for ${client.name} | ${await buildClientSnapshot(client)}`)
    }

    async function runPhase2Activity(client: ClientCtx) {
      await waitForPhase2(client)
      let actionCursor = (client.index * 2) % phase2ActionIds.length
      let nextActionAt = Date.now() + (client.index % 9) * 1_200
      const deadline = Date.now() + PHASE_DURATION_MS + SESSION_PROGRESS_TIMEOUT_MS
      const targetPhase2Actions = COVERAGE_TARGET === 'phase2' ? 1 : Number.POSITIVE_INFINITY

      while (Date.now() < deadline) {
        if (client.page.isClosed()) {
          return
        }

        await sampleConnected(client, 'phase2')

        const bodyText = ((await client.page.locator('body').textContent().catch(() => '')) ?? '')
        const scoreButtonCount = await client.page.getByRole('button', { name: 'View score' }).count().catch(() => 0)
        if (bodyText.includes('Scoring') || scoreButtonCount > 0) {
          return
        }

        const now = Date.now()
        if (now >= nextActionAt) {
          const actionId = phase2ActionIds[actionCursor % phase2ActionIds.length]
          const clicked = await clickAction(client, 'phase2', actionId).catch(error => {
            log(client, 'warn', 'phase2', 'phase2 action failed', `${actionId} :: ${String(error)}`)
            return false
          })
          if (clicked) {
            client.phase2Actions++
            if (client.phase2Actions >= targetPhase2Actions) {
              return
            }
          }
          actionCursor++
          nextActionAt += PHASE2_ACTION_INTERVAL_MS
          continue
        }

        await client.page.waitForTimeout(500)
      }

      throw new Error(`phase 2 did not transition to scoring for ${client.name} | ${await buildClientSnapshot(client)}`)
    }

    async function waitForScoreReadyForClient(client: ClientCtx, timeout = SCORE_SYNC_TIMEOUT_MS) {
      client.currentStage = 'score'
      const deadline = Date.now() + timeout
      while (Date.now() < deadline) {
        await sampleConnected(client, 'score')
        await client.page.getByRole('button', { name: 'View score' }).click().catch(() => undefined)
        const scoreLeaderboardVisible = await client.page.getByText('Score leaderboard').isVisible().catch(() => false)
        if (scoreLeaderboardVisible) {
          client.scoreReady = true
          return
        }
        await delay(5_000)
      }
      throw new Error(`judge score leaderboard did not become ready for ${client.name} | ${await buildClientSnapshot(client)}`)
    }

    async function runGuestWorkshopFlow(client: ClientCtx) {
      await runPhase1Activity(client)
      await saveHandover(client)
      await runPhase2Activity(client)
      phase2CoverageSignal = 'guest-phase2-complete'
      if (COVERAGE_TARGET === 'phase2') {
        return
      }
      await waitForScoreReadyForClient(client)
    }

    async function waitForManualArchive(clients: ClientCtx[], timeout = MANUAL_ARCHIVE_WAIT_MS) {
      const deadline = Date.now() + timeout

      log(
        null,
        'info',
        'manual',
        'waiting for manual host actions',
        `Manual host should continue workshop ${workshopCode}. This wait exits when guests can see the built archive or end-state archive panel.`,
      )

      while (Date.now() < deadline) {
        for (const client of clients) {
          await sampleConnected(client, 'manual')
          const openWorkshops = await client.page.evaluate(async () => {
            const response = await fetch('/api/workshops/open', { credentials: 'include' })
            if (!response.ok()) {
              return { ok: false, status: response.status, workshops: [] as Array<{ sessionCode: string, archived: boolean }> }
            }
            const json = await response.json() as { workshops?: Array<{ sessionCode?: string, archived?: boolean }> }
            return {
              ok: true,
              status: response.status,
              workshops: (json.workshops ?? []).map(workshop => ({
                sessionCode: workshop.sessionCode ?? '',
                archived: Boolean(workshop.archived),
              })),
            }
          }).catch(() => ({ ok: false, status: 0, workshops: [] as Array<{ sessionCode: string, archived: boolean }> }))
          if (openWorkshops.ok && openWorkshops.workshops.some(workshop => workshop.sessionCode === workshopCode && workshop.archived)) {
            manualCompletionSignal = 'open-workshops-archived-row-visible'
            log(client, 'info', 'manual', 'manual archive observed', manualCompletionSignal)
            return
          }
          const archiveReadyButton = await client.page.getByTestId('archive-workshop-button').count().catch(() => 0)
          const archiveReadyLabel = archiveReadyButton > 0
            ? await client.page.getByTestId('archive-workshop-button').textContent().catch(() => '')
            : ''
          const archivePanelText = ((await client.page.getByTestId('archive-panel').textContent().catch(() => '')) ?? '').trim()
          if (archivePanelText.includes('Captured final standings') || (archiveReadyLabel ?? '').includes('Archive ready')) {
            manualCompletionSignal = 'guest-visible-archive-state'
            log(client, 'info', 'manual', 'manual archive observed', manualCompletionSignal)
            return
          }
        }

        await delay(5_000)
      }

      throw new Error(`manual archive wait timed out after ${formatMs(timeout)} | ${await buildClientSnapshot(clients[0])}`)
    }

    async function waitForPhase2Coverage(clients: ClientCtx[], timeout = SCORE_SYNC_TIMEOUT_MS) {
      const deadline = Date.now() + timeout

      log(
        null,
        'info',
        'manual',
        'waiting for manual host progress',
        `Manual host ${HOST_ACCOUNT_NAME} should continue workshop ${workshopCode} until guests complete Phase 2 activity.`,
      )

      while (Date.now() < deadline) {
        for (const client of clients) {
          await sampleConnected(client, 'manual')
          if (client.phase2Actions > 0) {
            phase2CoverageSignal = 'guest-phase2-action-visible'
          }
        }

        if (clients.every(client => client.phase1Actions > 0 && client.phase1Observations > 0 && client.handoverSaved && client.phase2Actions > 0)) {
          phase2CoverageSignal = 'all-guests-covered-phase2'
          return
        }

        await delay(5_000)
      }

      throw new Error(`phase2 coverage wait timed out after ${formatMs(timeout)} | ${await buildClientSnapshot(clients[0])}`)
    }

    function buildSummary(clients: ClientCtx[]) {
      const loggedInCount = clients.filter(client => client.signInStatus === 'logged-in').length
      const joinedCount = clients.filter(client => client.joinedWorkshop).length
      const handoverCount = clients.filter(client => client.handoverSaved).length
      const scoreReadyCount = clients.filter(client => client.scoreReady).length
      const ownedSelections = clients.filter(client => client.selectedCharacterSource === 'owned').length
      const starterSelections = clients.filter(client => client.selectedCharacterSource === 'starter').length
      const severityCounts = new Map<Severity, number>()
      const stageCounts = new Map<Stage, number>()

      for (const event of events) {
        severityCounts.set(event.severity, (severityCounts.get(event.severity) ?? 0) + 1)
        stageCounts.set(event.stage, (stageCounts.get(event.stage) ?? 0) + 1)
      }

      const uniqueIssues = [...new Set(
        events
          .filter(event => event.severity !== 'info')
          .map(event => `${event.severity} | ${event.stage} | ${event.player} | ${event.message} | ${event.detail}`),
      )]
      const httpStatusCounts = topCounts(
        events
          .map(event => extractStatusCode(event))
          .filter((status): status is string => status !== null)
          .map(status => `HTTP ${status}`),
        20,
      )
      const rateLimitSignals = topCounts(
        events
          .filter(event => extractStatusCode(event) === '429' || /rate limit/i.test(event.message) || /rate limit/i.test(event.detail))
          .map(event => `${event.stage} | ${event.message}`),
        20,
      )
      const issueSignatures = topCounts(
        events
          .filter(event => event.severity !== 'info')
          .map(event => `${event.severity} | ${event.stage} | ${event.message}`),
        20,
      )
      const noisyClients = topCounts(
        events
          .filter(event => event.severity !== 'info')
          .map(event => `${event.player} (${event.role})`),
        20,
      )

      const clientBreakdown = clients.map(client => {
        return `- ${client.name}: signIn=${client.signInStatus}, joined=${client.joinedWorkshop}, character=${client.selectedCharacterSource}, handover=${client.handoverSaved}, scoreReady=${client.scoreReady}, phase1Actions=${client.phase1Actions}, phase1Observations=${client.phase1Observations}, phase2Actions=${client.phase2Actions}`
      })

      return [
        '# Load Test Summary',
        '',
        `- Run ID: ${RUN_ID}`,
        `- Base URL: ${BASE_URL}`,
        `- Workshop code: ${workshopCode}`,
        `- Client name prefix: ${CLIENT_NAME_PREFIX}`,
        `- Client name offset: ${CLIENT_NAME_OFFSET}`,
        `- Allow account creation: ${ALLOW_ACCOUNT_CREATION}`,
        `- Allow starter fallback: ${ALLOW_STARTER_FALLBACK}`,
        `- External workshop code: ${EXTERNAL_WORKSHOP_CODE || 'none'}`,
        `- Requested clients: ${CLIENT_COUNT}`,
        `- Authenticated accounts: ${loggedInCount}/${CLIENT_COUNT}`,
        `- Joined workshop: ${joinedCount}/${CLIENT_COUNT}`,
        `- Character selection: owned=${ownedSelections}, starter=${starterSelections}`,
        `- Handover saved: ${handoverCount}/${Math.max(joinedCount, 1)}`,
        `- Score ready: ${scoreReadyCount}/${Math.max(joinedCount, 1)}`,
        `- Phase 1 countdown at start: ${phase1CountdownAtStart}`,
        `- Phase 2 countdown at start: ${phase2CountdownAtStart}`,
        `- Manual completion signal: ${manualCompletionSignal}`,
        `- Phase 2 coverage signal: ${phase2CoverageSignal}`,
        `- Manual archive wait budget: ${formatMs(MANUAL_ARCHIVE_WAIT_MS)}`,
        `- Coverage target: ${COVERAGE_TARGET}`,
        `- Host account: ${HOST_ACCOUNT_NAME}`,
        `- Event counts: info=${severityCounts.get('info') ?? 0}, warn=${severityCounts.get('warn') ?? 0}, error=${severityCounts.get('error') ?? 0}, blocker=${severityCounts.get('blocker') ?? 0}`,
        '',
        '## Top Blockers',
        ...(blockers.length > 0 ? blockers.slice(0, 20).map(item => `- ${item}`) : ['- None']),
        '',
        '## Top Warnings',
        ...(warnings.length > 0 ? warnings.slice(0, 20).map(item => `- ${item}`) : ['- None']),
        '',
        '## HTTP Status Counts',
        ...(httpStatusCounts.length > 0 ? httpStatusCounts.map(item => `- ${item.count}x ${item.key}`) : ['- None']),
        '',
        '## Rate Limit Signals',
        ...(rateLimitSignals.length > 0 ? rateLimitSignals.map(item => `- ${item.count}x ${item.key}`) : ['- None']),
        '',
        '## Frequent Issue Signatures',
        ...(issueSignatures.length > 0 ? issueSignatures.map(item => `- ${item.count}x ${item.key}`) : ['- None']),
        '',
        '## Noisiest Clients',
        ...(noisyClients.length > 0 ? noisyClients.map(item => `- ${item.count}x ${item.key}`) : ['- None']),
        '',
        '## Client Breakdown',
        ...clientBreakdown,
        '',
        '## Unique Issues',
        ...(uniqueIssues.length > 0 ? uniqueIssues.slice(0, 60).map(issue => `- ${issue}`) : ['- None']),
        '',
        '## Stage Event Counts',
        ...(['setup', 'signin', 'lobby', 'phase1', 'handover', 'phase2', 'score', 'manual', 'final'] as Stage[])
          .map(stage => `- ${stage}: ${stageCounts.get(stage) ?? 0}`),
        '',
        '## Artifacts',
        `- Events: ${EVENTS_PATH}`,
        `- Screenshots: ${SCREENSHOTS_DIR}`,
      ].join('\n')
    }

    const clients = await Promise.all(Array.from({ length: CLIENT_COUNT }, (_, index) => createClient(browser, index + 1)))

    try {
      workshopCode = EXTERNAL_WORKSHOP_CODE || workshopCode
      log(null, 'info', 'setup', 'starting 30-client guest load test', `baseUrl=${BASE_URL} | accountPrefix=${CLIENT_NAME_PREFIX} | accountOffset=${CLIENT_NAME_OFFSET} | externalWorkshop=${EXTERNAL_WORKSHOP_CODE || 'none'}`)

      const signInResults = await Promise.allSettled(clients.map(client => signIn(client)))
      signInResults.forEach((result, index) => {
        if (result.status === 'rejected') {
          log(clients[index], 'blocker', 'signin', 'signin failed', String(result.reason))
        }
      })

        const signedInClients = clients.filter(client => client.signInStatus === 'logged-in')
        if (signedInClients.length !== CLIENT_COUNT) {
          throw new Error(`only ${signedInClients.length}/${CLIENT_COUNT} clients logged in successfully`)
        }

        const code = EXTERNAL_WORKSHOP_CODE || await createWorkshop(clients[0])

        const joinResults = await Promise.allSettled(clients.map(client => joinWorkshop(client, code)))
        joinResults.forEach((result, index) => {
          if (result.status === 'rejected') {
            log(clients[index], 'blocker', 'lobby', 'join workshop failed', String(result.reason))
          }
        })

      const joinedClients = clients.filter(client => client.joinedWorkshop)
      if (joinedClients.length !== CLIENT_COUNT) {
        throw new Error(`only ${joinedClients.length}/${CLIENT_COUNT} guest clients joined the workshop`)
      }

      if (!ALLOW_STARTER_FALLBACK && joinedClients.some(client => client.selectedCharacterSource !== 'owned')) {
        throw new Error('one or more guest clients did not select an owned character')
      }

      await waitForAllClientsText(joinedClients, lobbyTitlePattern, RESPONSE_TIMEOUT_MS)
      await waitForAllClientsConnected(joinedClients, RESPONSE_TIMEOUT_MS)
      log(null, 'info', 'lobby', 'all guest clients joined and connected', `${joinedClients.length}/${CLIENT_COUNT}`)
      log(null, 'info', 'manual', 'waiting for manual host progress', `Workshop ${code} is ready. Continue manually as ${HOST_ACCOUNT_NAME}, then drive Phase 1, Handover, and Phase 2.${COVERAGE_TARGET === 'archive' ? ' Continue through scoring and archive as well.' : ''}`)

      const guestFlowResults = await Promise.allSettled([
        ...joinedClients.map(client => runGuestWorkshopFlow(client)),
        ...(COVERAGE_TARGET === 'archive'
          ? [waitForManualArchive(joinedClients, MANUAL_ARCHIVE_WAIT_MS)]
          : [waitForPhase2Coverage(joinedClients, SCORE_SYNC_TIMEOUT_MS)]),
      ])
      guestFlowResults.forEach((result, index) => {
        if (result.status === 'rejected') {
          const client = joinedClients[index]
          if (client) {
            log(client, 'blocker', client.currentStage, 'guest flow failed', String(result.reason))
          } else {
            log(null, 'blocker', 'manual', 'manual archive wait failed', String(result.reason))
          }
        }
      })

      for (const client of joinedClients) {
        if (client.phase1Actions === 0 || client.phase1Observations === 0) {
          log(client, 'blocker', 'phase1', 'phase 1 activity below minimum', `actions=${client.phase1Actions} observations=${client.phase1Observations}`)
        }
        if (!client.handoverSaved) {
          log(client, 'blocker', 'handover', 'handover not saved')
        }
        if (client.phase2Actions === 0) {
          log(client, 'blocker', 'phase2', 'phase 2 activity below minimum', `actions=${client.phase2Actions}`)
        }
        if (COVERAGE_TARGET === 'archive' && !client.scoreReady) {
          log(client, 'blocker', 'score', 'score leaderboard not ready for client')
        }
      }

      await captureScreenshots(joinedClients.slice(0, 2), 'manual-complete')

      if (blockers.length > 0) {
        throw new Error(`load test reached manual completion with blockers recorded: ${blockers.length}`)
      }
    } catch (error) {
      log(null, 'blocker', 'final', 'load test aborted', String(error))
      await captureScreenshots(clients, 'failure')
      throw error
    } finally {
      const summary = buildSummary(clients)
      fs.writeFileSync(SUMMARY_PATH, summary, 'utf-8')

      await test.info().attach('load-test-summary', {
        path: SUMMARY_PATH,
        contentType: 'text/markdown',
      })
      await test.info().attach('load-test-events', {
        path: EVENTS_PATH,
        contentType: 'application/x-ndjson',
      })

      await Promise.allSettled(clients.map(client => client.context.close()))
    }
  })
})
