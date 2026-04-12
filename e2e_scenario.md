# Dragon Shift E2E Evolution Scenario

## Goal
Run a fresh public deployment through three manual iterations to improve gameplay flow, mechanics, pacing, and comfort.
Use 1 host and 4 player agents. Each agent runs in a separate browser context.
This is a manual evolution run and may go beyond the current automated suite.

Fresh public deployment means a newly deployed build or redeployed environment with no prior run-specific workshop state.

## What To Find
- gameplay bugs
- sync and reconnect bugs
- confusing UI or unclear next actions
- host burden or awkward control flow
- slow, repetitive, or uncomfortable interactions
- anything that causes friction, confusion, delay, or repeated steps for players

## Review Checklist
Critics must challenge any:
- vague phrase or subjective claim
- missing exact UI text
- missing exact count, phase, route, or timeout
- missing restart or iteration boundary
- missing log field or summary field
- step without one clear observable pass/fail result
- mismatch between a role and its responsibility
- assumption about what the current automated suite already covers
- missing exact step input, output, or expected rejection text
- any rule that can be interpreted two different ways
- any acceptance criterion that cannot be checked directly from the scenario

## Success Criteria
- Iteration 2 must not add a blocker, permission error, or sync mismatch that was absent in Iteration 1.
- Iteration 3 must not add a blocker, permission error, or sync mismatch that was absent in Iteration 2.
- Every host action must be followed by one visible next step for the players.
- No step may require the players to guess the next action.

## Wait Budget
- 30 seconds for join, late join, reload, reconnect, phase changes, and reset.
- 60 seconds for archive build.
- 10 seconds for short notices such as `Session synced.` or `Phase 1 started.`.
- 20 seconds for results reveal.

## Iteration Loop
- Iteration 1: baseline run on the first deployment.
- Iteration 2: restart the host and all four player agents from fresh browser contexts with cleared cookies and cleared local storage, then rerun from scratch using the issues and notes from Iteration 1.
- Iteration 3: restart the host and all four player agents again from fresh browser contexts with cleared cookies and cleared local storage, then rerun from scratch using the issues and notes from Iterations 1 and 2.
- Between iterations, keep the same public URL unless the app is redeployed. If the deployment changes, close the current iteration, record the new `BASE_URL`, build id, and image tag in the summary for the iteration that just ended, and start the next iteration from fresh browser contexts with cleared cookies and cleared local storage.
- The host writes a short summary at the end of each iteration before the next restart.

## Inputs
- `BASE_URL`: deployed public URL
- `RUN_ID`: unique run id
- `LOG_DIR`: `e2e/.tmp/agent-logs/${RUN_ID}`
- `SHOT_DIR`: `e2e/.tmp/agent-shots/${RUN_ID}`

## Roles

| Role | Main job | Watch for | Log focus |
|---|---|---|---|
| Host | Create the workshop and drive the full flow. | Phase timing, host-only actions, player counts, reset, and overall ease of control. | Flow changes, counts, blockers, final summary |
| Agent 1 | Baseline player. | Whether the main flow is easy to follow and comfortable to use. | Lobby text, phase labels, sync quality |
| Agent 2 | Late joiner. | Whether a player joining after Phase 1 can understand the current state immediately. | Join timing, state reconstruction, current phase |
| Agent 3 | Reload and reconnect. | Whether reload and reconnect preserve the session without confusion or manual cleanup. | Token handling, connection badge, recovery |
| Agent 4 | Edge cases and negative checks. | Invalid join, host-only action checks, and error text quality. | Error notices, permission boundaries |

## Logging
- Write one Markdown file per agent per iteration:
  - `e2e/.tmp/agent-logs/${RUN_ID}/iteration-1/host.md`
  - `e2e/.tmp/agent-logs/${RUN_ID}/iteration-1/agent-1.md`
  - `e2e/.tmp/agent-logs/${RUN_ID}/iteration-1/agent-2.md`
  - `e2e/.tmp/agent-logs/${RUN_ID}/iteration-1/agent-3.md`
  - `e2e/.tmp/agent-logs/${RUN_ID}/iteration-1/agent-4.md`
  - repeat for `iteration-2` and `iteration-3`
- Host summaries:
  - `e2e/.tmp/agent-logs/${RUN_ID}/iteration-1/summary.md`
  - `e2e/.tmp/agent-logs/${RUN_ID}/iteration-2/summary.md`
  - `e2e/.tmp/agent-logs/${RUN_ID}/iteration-3/summary.md`
  - `e2e/.tmp/agent-logs/${RUN_ID}/final-summary.md`
- Each iteration summary must include: iteration goal, issues found, friction points, fixes to carry forward, and pass/fail status.
- The final summary must include: the overall result, the main recurring problems, the biggest improvements, and the next recommended changes.
- If the deployment changes during the run, the summary for the iteration that just ended must record the new `BASE_URL`, build id, and image tag.
- Write one entry for every planned step and one extra entry for every failure, warning, or recovered mismatch.
- Every log entry must include:
  - `timestamp_utc`
  - `run_id`
  - `iteration`
  - `role`
  - `step_id`
  - `action`
  - `expected`
  - `actual`
  - `status`
  - `kind`
  - `issue_id`
  - `build_id`
  - `image_tag`
  - `workshop_code`
  - `phase`
  - `player_count`
  - `url`
  - `route`
  - `browser_profile`
  - `browser_version`
  - `viewport`
  - `console`
  - `network`
  - `request_id`
  - `response_id`
  - `evidence`
  - `note`
  - `friction_score`
- Use `none` when a field does not apply.
- `kind` should be one of `ok`, `bug`, `friction`, `blocker`, or `question`.
- Issue ids use `DS-E2E-###`.
- Timestamps must be UTC ISO-8601.
- Log `console` and `network` as the exact error text.
- `friction_score` must be `0` for no friction, `1` for minor friction, `2` for moderate friction, or `3` for severe friction.

## Execution
Use the same flow for each iteration.

1. Host sends `GET /api/live` and expects HTTP 200 with JSON `ok=true` and `status=live`.
2. Host sends `GET /api/ready` and expects HTTP 200 with JSON `ok=true`, `service=app-server`, `status=ready`, and `checks.store=true`.
3. Host opens the app at `BASE_URL` and creates a workshop.
4. Host records the workshop code.
5. Agent 1 joins normally and logs `Session synced.`.
6. Agent 3 joins normally, logs `Session synced.`, and records the reconnect token.
7. Agent 4 first attempts join code `999999`, expects `Workshop not found.`, then clears the form and joins normally.
8. Host confirms all current clients show `Workshop lobby` and the same workshop code.
9. Host starts Phase 1 and logs the exact notice `Phase 1 started.`.
10. Agent 2 joins after the Phase 1 notice is visible and confirms the client lands on `Discovery round`.
11. All joined clients confirm `Discovery round` and `Players in view: 5`.
12. Agent 4 checks `Start Phase 2` before the host does. If the control is visible, clicking it must show `Only the host can begin Phase 2.`. If it is hidden, record that as a pass.
13. Host clicks `Start handover` and logs the exact notice `Handover started.`.
14. Every player enters a non-empty comma-separated handover tag list in `handover-tags-input`, clicks `Save handover tags`, and records the exact notice `Handover tags saved.` after each submit. Example accepted input: `calm,dusk,berries`.
15. Agent 3 reloads the current page once and confirms the current session still shows `Handover`, `Connected`, and the workshop code badge on the same page.
16. Agent 3 reconnects from a fresh context using the saved token in `reconnect-token-input`, clicks `Reconnect`, and confirms `Reconnected to workshop.`, `Connected`, `Handover`, and the same workshop code badge.
17. Host starts Phase 2 and logs the exact notice `Phase 2 started.`.
18. All clients confirm `Care round`.
19. Agent 4 checks `End game` before the host does. If the control is visible, clicking it must show `Only the host can end the workshop.`. If it is hidden, record that as a pass.
20. Host clicks `End game` and all clients confirm `Voting` and `0 / 5 votes submitted`.
21. Each player votes once using the visible `Vote` button.
22. The host votes once using the visible `Vote` button.
23. Agent 4 makes one duplicate click on the same `Vote` button and expects the vote count to stay unchanged after the second click.
24. Host confirms `5 / 5 votes submitted`.
25. Agent 4 checks `Reveal results` before the host does. If the control is visible, clicking it must show `Only the host can reveal voting results.`. If it is hidden, record that as a pass.
26. Agent 4 confirms `Build archive` is not visible to non-host clients.
27. Agent 4 checks `Reset workshop` before the host does. If the control is visible, clicking it must show `Only the host can reset the workshop.`. If it is hidden, record that as a pass.
28. Host clicks `Reveal results`.
29. Every client confirms `Workshop results`, `Creative pet awards`, and `Final player standings`.
30. Host clicks `Build Archive`.
31. Host confirms `Workshop archive ready.`, `Captured final standings`, and `Captured dragons`.
32. The host and each non-host client confirm the archive panel is visible and the build button is gone.
33. Host clicks `Reset workshop`.
34. Every client returns to `Workshop lobby` and `Workshop results` is no longer visible.
35. Host writes the iteration summary with iteration goal, issues found, friction points, fixes to carry forward, and pass/fail status.
36. After Iteration 3, the host writes `final-summary.md` with the overall result, recurring issues, the biggest improvements, and the next recommended changes.

## What To Watch For
- wrong player counts
- stale lobby or phase text
- host-only controls visible to the wrong role
- reconnect token missing, invalid, or ignored
- reload losing the session
- duplicate votes or duplicate phase actions
- archive build missing or incomplete
- reset not syncing to every client
- unclear or missing error notices
- any moment where a player has to guess the next action
- any step that feels slower or harder than necessary

## Stop Conditions
- Stop the run if `/api/live` or `/api/ready` fail.
- Stop the run if workshop creation fails.
- Stop the run if any join, late join, reload, reconnect, phase change, vote completion, archive build, or reset does not reach the expected UI state within the wait budget.
- Stop the run if results reveal does not show `Workshop results`, `Creative pet awards`, and `Final player standings` within the wait budget.
- Stop the run if any console error or failed network request appears that blocks the current step.
- Stop the run if a recovered mismatch repeats after retry.

## Optional Fault Probes
If the harness can intercept requests without code changes:
- abort one join request and record the exact degraded-path notice `failed to reach backend:` plus the underlying error text
- abort one archive build request and record the exact degraded-path notice `failed to reach backend:` plus the underlying error text
- abort one websocket or reconnect request and record the exact degraded-path notice `failed to reach backend:` plus the underlying error text
- abort one invalid reconnect request and record the degraded-path message `Session identity is invalid or expired.`
- if a restart-capable harness is used in Iteration 3, restart the server and verify `Offline` -> `Session synced.` after `sync-session-button`

## Issue Format
```md
## Issue: <short title>
- issue_id: DS-E2E-001
- kind: bug | friction | blocker | question
- role: Host | Agent 1 | Agent 2 | Agent 3 | Agent 4
- iteration: 1 | 2 | 3
- step_id: <step number and name>
- run_id: <run id>
- build_id: <commit SHA or image tag>
- image_tag: <image tag>
- timestamp_utc: <UTC timestamp>
- workshop_code: <code>
- url: <BASE_URL>
- route: <exact page or route>
- phase: <current phase>
- player_count: <count>
- expected: <exact expected behavior>
- actual: <exact actual behavior>
- impact: low | medium | high
- browser_profile: <profile>
- console: <error text or none>
- network: <error text or none>
- evidence: <shot or video path or none>
- note: <short reproduction or suspicion>
```

## Acceptance
- Host, Agent 1, Agent 3, and Agent 4 reach the lobby in iteration 1.
- Agent 2 joins late during iteration 1 and lands on the active session state.
- Each later iteration starts from fresh host and player contexts.
- Agent 2 can join late and see the `Discovery round` phase label within the 30-second wait budget.
- Agent 3 can reload and reconnect without losing the session.
- Host-only actions are denied for non-hosts with exact app text: `Start Phase 2` -> `Only the host can begin Phase 2.`, `End game` -> `Only the host can end the workshop.`, `Reveal results` -> `Only the host can reveal voting results.`, `Reset workshop` -> `Only the host can reset the workshop.`
- `Build archive` stays hidden for non-host clients during the end phase.
- Voting reaches `5 / 5`.
- Duplicate voting is suppressed: the second click on the same vote button does not change the vote count.
- Results are visible on every client after reveal.
- Archive is visible on the host and all non-host clients after build.
- After reset, all clients return to `Workshop lobby` and `Workshop results` is hidden.
- The final iteration records a friction score of 0 or 1 for every planned step.
