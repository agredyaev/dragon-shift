import * as fs from 'node:fs'
import * as path from 'node:path'

export type Role = 'Host' | 'Agent 1' | 'Agent 2' | 'Agent 3' | 'Agent 4'
export type Kind = 'ok' | 'bug' | 'friction' | 'blocker' | 'question'
export type EntryStatus = 'pass' | 'fail' | 'warn' | 'skip'

export interface LogEntry {
  timestamp_utc: string
  run_id: string
  iteration: number
  role: Role
  step_id: string
  action: string
  expected: string
  actual: string
  status: EntryStatus
  kind: Kind
  issue_id: string
  build_id: string
  image_tag: string
  workshop_code: string
  phase: string
  player_count: string
  url: string
  route: string
  browser_profile: string
  browser_version: string
  viewport: string
  console: string
  network: string
  request_id: string
  response_id: string
  evidence: string
  note: string
  friction_score: number
}

export interface Issue {
  title: string
  issue_id: string
  kind: Kind
  role: Role
  iteration: number
  step_id: string
  run_id: string
  build_id: string
  image_tag: string
  timestamp_utc: string
  workshop_code: string
  url: string
  route: string
  phase: string
  player_count: string
  expected: string
  actual: string
  impact: string
  browser_profile: string
  console: string
  network: string
  evidence: string
  note: string
}

export class ScenarioLogger {
  private entries: Map<string, LogEntry[]> = new Map()
  private issues: Issue[] = []
  private issueCounter = 0

  constructor(
    public readonly runId: string,
    public readonly logDir: string,
    public readonly buildId: string,
    public readonly imageTag: string,
    public readonly baseUrl: string,
  ) {}

  log(
    entry: Partial<LogEntry> &
      Pick<LogEntry, 'role' | 'iteration' | 'step_id' | 'action' | 'expected' | 'actual' | 'status' | 'kind'>,
  ): LogEntry {
    const full: LogEntry = {
      timestamp_utc: new Date().toISOString(),
      run_id: this.runId,
      build_id: this.buildId,
      image_tag: this.imageTag,
      url: this.baseUrl,
      issue_id: 'none',
      workshop_code: 'none',
      phase: 'none',
      player_count: 'none',
      route: 'none',
      browser_profile: 'chromium',
      browser_version: 'none',
      viewport: 'none',
      console: 'none',
      network: 'none',
      request_id: 'none',
      response_id: 'none',
      evidence: 'none',
      note: 'none',
      friction_score: 0,
      ...entry,
    }

    const key = `${entry.role}-${entry.iteration}`
    if (!this.entries.has(key)) {
      this.entries.set(key, [])
    }
    this.entries.get(key)!.push(full)
    return full
  }

  addIssue(issue: Omit<Issue, 'issue_id'>): Issue {
    this.issueCounter++
    const id = `DS-E2E-${String(this.issueCounter).padStart(3, '0')}`
    const full = { ...issue, issue_id: id }
    this.issues.push(full)
    return full
  }

  getIssues(): Issue[] {
    return [...this.issues]
  }

  getIterationIssues(iteration: number): Issue[] {
    return this.issues.filter(i => i.iteration === iteration)
  }

  writeIterationLogs(iteration: number): void {
    const iterDir = path.join(this.logDir, `iteration-${iteration}`)
    fs.mkdirSync(iterDir, { recursive: true })

    const roleToFile: Record<Role, string> = {
      Host: 'host.md',
      'Agent 1': 'agent-1.md',
      'Agent 2': 'agent-2.md',
      'Agent 3': 'agent-3.md',
      'Agent 4': 'agent-4.md',
    }

    for (const [role, filename] of Object.entries(roleToFile)) {
      const key = `${role}-${iteration}`
      const entries = this.entries.get(key) ?? []
      const content = this.formatAgentLog(role as Role, iteration, entries)
      fs.writeFileSync(path.join(iterDir, filename), content, 'utf-8')
    }
  }

  writeIterationSummary(
    iteration: number,
    summary: {
      goal: string
      issuesFound: string[]
      frictionPoints: string[]
      fixesToCarryForward: string[]
      passFail: string
    },
  ): void {
    const iterDir = path.join(this.logDir, `iteration-${iteration}`)
    fs.mkdirSync(iterDir, { recursive: true })

    const content = [
      `# Iteration ${iteration} Summary`,
      '',
      `**Run ID**: ${this.runId}`,
      `**Build ID**: ${this.buildId}`,
      `**Image Tag**: ${this.imageTag}`,
      `**URL**: ${this.baseUrl}`,
      '',
      `## Goal`,
      summary.goal,
      '',
      `## Issues Found`,
      ...(summary.issuesFound.length > 0 ? summary.issuesFound.map(i => `- ${i}`) : ['- None']),
      '',
      `## Friction Points`,
      ...(summary.frictionPoints.length > 0 ? summary.frictionPoints.map(f => `- ${f}`) : ['- None']),
      '',
      `## Fixes to Carry Forward`,
      ...(summary.fixesToCarryForward.length > 0
        ? summary.fixesToCarryForward.map(f => `- ${f}`)
        : ['- None']),
      '',
      `## Pass/Fail`,
      summary.passFail,
      '',
    ].join('\n')

    fs.writeFileSync(path.join(iterDir, 'summary.md'), content, 'utf-8')
  }

  writeFinalSummary(summary: {
    overallResult: string
    recurringIssues: string[]
    biggestImprovements: string[]
    nextChanges: string[]
  }): void {
    fs.mkdirSync(this.logDir, { recursive: true })

    const content = [
      `# Final Summary`,
      '',
      `**Run ID**: ${this.runId}`,
      `**Build ID**: ${this.buildId}`,
      `**Image Tag**: ${this.imageTag}`,
      `**URL**: ${this.baseUrl}`,
      '',
      `## Overall Result`,
      summary.overallResult,
      '',
      `## Recurring Issues`,
      ...(summary.recurringIssues.length > 0 ? summary.recurringIssues.map(i => `- ${i}`) : ['- None']),
      '',
      `## Biggest Improvements`,
      ...(summary.biggestImprovements.length > 0
        ? summary.biggestImprovements.map(i => `- ${i}`)
        : ['- None']),
      '',
      `## Next Recommended Changes`,
      ...(summary.nextChanges.length > 0 ? summary.nextChanges.map(c => `- ${c}`) : ['- None']),
      '',
    ].join('\n')

    fs.writeFileSync(path.join(this.logDir, 'final-summary.md'), content, 'utf-8')
  }

  private formatAgentLog(role: Role, iteration: number, entries: LogEntry[]): string {
    const lines = [
      `# ${role} — Iteration ${iteration}`,
      '',
      `**Run ID**: ${this.runId}`,
      `**Build ID**: ${this.buildId}`,
      '',
    ]

    for (const entry of entries) {
      lines.push(`## Step ${entry.step_id}: ${entry.action}`)
      lines.push('')
      lines.push('```yaml')
      for (const [key, val] of Object.entries(entry)) {
        lines.push(`${key}: ${String(val)}`)
      }
      lines.push('```')
      lines.push('')
    }

    return lines.join('\n')
  }
}
