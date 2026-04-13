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
- dragon action mechanics that produce wrong stat changes
- score calculation errors
- shuffle/redistribution bugs in Phase 2
- achievement trigger failures
- day/night cycle inconsistencies
- tick/decay drift or incorrect stat degradation
- LLM judge or image generation failures
- voting edge cases (self-vote, duplicate, odd player count)

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
- Dragon stats must change by the exact expected deltas after every action.
- Score formula must match `happiness + hunger + energy + (achievements.len() * 50)`.
- Phase 2 shuffle must assign every player a different dragon than they had in Phase 1 (when player_count > 1).

## Wait Budget
- 30 seconds for join, late join, reload, reconnect, phase changes, and reset.
- 60 seconds for archive build.
- 10 seconds for short notices such as `Session synced.` or `Phase 1 started.`.
- 20 seconds for results reveal.
- 10 seconds for dragon action responses (Feed, Play, Sleep).
- 30 seconds for LLM judge response.
- 60 seconds for LLM image generation response.

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
| Agent 1 | Baseline player. Performs correct dragon actions. | Whether the main flow is easy to follow and comfortable to use. Stats change correctly. | Lobby text, phase labels, sync quality, stat deltas |
| Agent 2 | Late joiner. Tests incorrect actions to verify penalties. | Whether a player joining after Phase 1 can understand the current state immediately. Wrong-action penalties. | Join timing, state reconstruction, current phase, penalty values |
| Agent 3 | Reload and reconnect. Tests persistence of dragon state. | Whether reload and reconnect preserve the session and dragon stats without confusion or manual cleanup. | Token handling, connection badge, recovery, stat persistence |
| Agent 4 | Edge cases and negative checks. Tests blocked actions. | Invalid join, host-only action checks, error text quality, and action boundary conditions. | Error notices, permission boundaries, blocked actions |

## Domain Reference

### Dragon Traits (all dragons are identical)
- `active_time`: Day
- `day_food`: Meat
- `night_food`: Fruit
- `day_play`: Fetch
- `night_play`: Music
- `sleep_rate`: 1

### Condition Hint Format
The server generates a condition hint shown to players:
`"Active at day, prefers meat by day and fruit by night, enjoys fetch by day and music by night, and tires at rate 1."`

### Starting Stats
- `hunger`: 50
- `energy`: 50
- `happiness`: 50

### Action Mechanics

#### Feed
- Correct food (matching time of day): `hunger += 40`, `happiness += 15`
- Wrong food: `hunger += 5`, `happiness -= 20`
- Blocked when: `hunger >= 95`
- Blocked message: `"Dragon is not hungry right now."`
- Day food: Meat → input `"meat"`
- Night food: Fruit → input `"fruit"`

#### Play
- Correct play (matching time of day): `energy -= 20`, `happiness += 30`
- Wrong play: `energy -= 15`, `happiness -= 20`
- Blocked when: `hunger < 20` OR `energy < 20`
- Blocked message (hunger): `"Dragon is too hungry to play."`
- Blocked message (energy): `"Dragon is too tired to play."`
- Day play: Fetch → input `"fetch"`
- Night play: Music → input `"music"`

#### Sleep
- Correct time (night when active_time=Day): `energy += 50`, `happiness += 10`
- Wrong time (day when active_time=Day): `energy += 50`, `happiness += 10` (sleep always works but time affects decay shield)
- Blocked when: `energy >= 90`
- Blocked message: `"Dragon is not tired enough to sleep."`

### Achievements
- `master_chef`: First `food_try` for the current day/night period is the correct food type.
- `playful_spirit`: First `play_try` for the current day/night period is the correct play type.
- Achievement value: 50 points each in score formula.

### Tick/Decay Mechanics
- `hunger -= 1` per tick (×2 in Phase 2)
- `energy -= sleep_rate * time_penalty * decay_multiplier`
  - `time_penalty`: 1 if correct active_time, 2 if wrong active_time
  - `decay_multiplier`: 1 in Phase 1, 2 in Phase 2
- Happiness decay per tick: `base(1) + (1 if hunger<30) + (1 if energy<30) + (1 if wrong_time AND no sleep_shield)`
- Day/night boundary (hour 6, hour 18): `food_tries` and `play_tries` reset

### Score Formula
`score = happiness + hunger + energy + (achievements.len() * 50)`

### Phase 2 Shuffle Algorithm
- Dragon IDs and Player IDs are sorted (BTreeMap keys).
- Last dragon goes to first player; rest shift by 1 position.
- Single player: dragon stays the same, speech = `"New shift, same dragon..."`
- Multi-player: every player receives a different dragon than Phase 1.

### Voting Rules
- Self-vote forbidden: cannot vote for the dragon currently assigned to you.
- Duplicate vote: overwrites the previous vote (BTreeMap insert).
- Reveal requires ALL eligible voters to have submitted.
- Single player: immediate finalize (0 eligible voters).
- Vote count display: `"{submitted} / {eligible} votes submitted"`.

### Phase UI Labels
| Phase | UI Label |
|---|---|
| Lobby | `Workshop lobby` |
| Phase1 | `Discovery round` |
| Handover | `Handover` |
| Phase2 | `Care round` |
| Voting | `Voting` |
| End | `Workshop results` |

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

### Phase 0: Health and Setup (Steps 1–8)

1. Host sends `GET /api/live` and expects HTTP 200 with JSON `ok=true` and `status=live`.
2. Host sends `GET /api/ready` and expects HTTP 200 with JSON `ok=true`, `service=app-server`, `status=ready`, and `checks.store=true`.
3. Host opens the app at `BASE_URL` and creates a workshop.
4. Host records the workshop code.
5. Agent 1 joins normally and logs `Session synced.`.
6. Agent 3 joins normally, logs `Session synced.`, and records the reconnect token.
7. Agent 4 first attempts join code `999999`, expects `Workshop not found.`, then clears the form and joins normally.
8. Agent 2 joins normally (server rejects post-lobby joins with `"This workshop has already started. New players can only join in the lobby."`). All 5 clients confirm `Workshop lobby` and the same workshop code.

### Phase 1: Discovery Round (Steps 9–16)

9. Host starts Phase 1 and logs the exact notice `Phase 1 started.`.
10. All 5 clients confirm `Discovery round` and `Players in view: 5`.

11. **Discovery: Condition Hint Verification.**
    Each player reads their dragon's condition hint from the session panel. Expected text pattern:
    `"Active at day, prefers meat by day and fruit by night, enjoys fetch by day and music by night, and tires at rate 1."`
    Log the exact text. If the hint differs, log as `kind: bug`.

12. **Discovery: Submit Observations.**
    Each player submits an observation via the SubmitObservation command through the UI. Example observations:
    - Agent 1: `"My dragon seems to like meat during the day."`
    - Agent 2: `"It looks tired when I make it play at night."`
    - Agent 3: `"The dragon perks up when I play fetch."`
    - Agent 4: `"It doesn't seem hungry after eating meat."`
    - Host: `"I noticed it sleeps better at night."`
    Verify each observation appears in the session view. If observations are not visible or the submission fails, log as `kind: bug`.

13. **Discovery: Dragon Actions in Phase 1.**
    Agent 1 performs a correct Feed action: input `"meat"` (daytime food).
    - Expected: `hunger` changes from 50 → 90, `happiness` changes from 50 → 65.
    - Verify the stat display updates within 10 seconds.
    - If this is the first `food_try` of the current period, Agent 1 earns `master_chef` achievement.
    Agent 2 performs a wrong Feed action: input `"fruit"` (daytime — wrong food).
    - Expected: `hunger` changes from 50 → 55, `happiness` changes from 50 → 30.
    - No `master_chef` achievement awarded.
    Agent 3 performs a correct Play action: input `"fetch"` (daytime play).
    - Expected: `energy` changes from 50 → 30, `happiness` changes from 50 → 80.
    - If this is the first `play_try` of the current period, Agent 3 earns `playful_spirit` achievement.
    Agent 4 tests a blocked action: attempts Sleep when `energy` is 50 (< 90, so sleep should work). Then feeds until `hunger >= 95` and attempts another Feed — expects blocked message `"Dragon is not hungry right now."`.

14. **Discovery: Stat Persistence After Actions.**
    Agent 3 reloads the page. After reload, verify dragon stats match the values from after the Play action (accounting for any tick decay that occurred). The stats must not reset to 50/50/50. Log exact pre-reload and post-reload stat values.

15. Agent 4 checks `Start Phase 2` before the host does. If the control is visible, clicking it must show `Only the host can begin Phase 2.`. If it is hidden, record that as a pass.

16. Host clicks `Start handover` and logs the exact notice `Handover started.`.

### Handover Phase (Steps 17–20)

17. Every player enters a non-empty comma-separated handover tag list in `handover-tags-input`, clicks `Save handover tags`, and records the exact notice `Handover tags saved.` after each submit. Example accepted input: `calm,dusk,berries`.

18. Agent 3 reloads the current page once and confirms the current session still shows `Handover`, `Connected`, and the workshop code badge on the same page.

19. Agent 3 reconnects from a fresh context using the saved token in `reconnect-token-input`, clicks `Reconnect`, and confirms `Reconnected to workshop.`, `Connected`, `Handover`, and the same workshop code badge.

20. Host starts Phase 2 and logs the exact notice `Phase 2 started.`.

### Phase 2: Care Round (Steps 21–30)

21. All clients confirm `Care round`.

22. **Shuffle Verification.**
    Each player checks which dragon they are now caring for. With 5 players (sorted BTreeMap IDs), the shuffle rotates: last dragon → first player, rest shift by 1. Every player must have a DIFFERENT dragon than they had in Phase 1. Log each player's Phase 1 dragon ID and Phase 2 dragon ID. If any player has the same dragon, log as `kind: bug`.

23. **Phase 2: Correct Actions with Accelerated Decay.**
    Agent 1 performs a correct Feed action: input `"meat"` (daytime food).
    - Expected on a fresh Phase 2 dragon (stats still at baseline minus tick decay): `hunger += 40`, `happiness += 15`.
    - Note: Phase 2 decay_multiplier = 2, so stats degrade faster between actions.
    Agent 1 performs a correct Play action: input `"fetch"` (daytime play).
    - Expected: `energy -= 20`, `happiness += 30`.
    Log exact stat values before and after each action.

24. **Phase 2: Wrong Actions and Penalties.**
    Agent 2 performs a wrong Feed action: input `"fruit"` (daytime — wrong).
    - Expected: `hunger += 5`, `happiness -= 20`.
    Agent 2 performs a wrong Play action: input `"music"` (daytime — wrong).
    - Expected: `energy -= 15`, `happiness -= 20`.
    Log exact stat values. Verify happiness can go negative or is clamped to 0.

25. **Phase 2: Blocked Action Boundaries.**
    Agent 4 attempts to trigger each blocked condition:
    - Feed when `hunger >= 95`: expects `"Dragon is not hungry right now."`
    - Play when `hunger < 20`: expects `"Dragon is too hungry to play."`
    - Play when `energy < 20`: expects `"Dragon is too tired to play."`
    - Sleep when `energy >= 90`: expects `"Dragon is not tired enough to sleep."`
    If the action goes through instead of being blocked, log as `kind: bug`.

26. **Phase 2: Achievement Verification.**
    Check Agent 1's dragon for `master_chef` (first food_try was correct) and `playful_spirit` (first play_try was correct) achievements. These should each add 50 to the score. Agent 2's dragon should have NO achievements (first tries were wrong). Log achievement lists for both.

27. **Phase 2: Decay Observation.**
    Wait 3 ticks (observe stat changes over ~3 intervals). Record hunger, energy, and happiness at each tick boundary for one dragon. Verify:
    - `hunger` decreases by 2 per tick (Phase 2 multiplier).
    - `energy` decreases by `sleep_rate(1) * time_penalty * 2`.
    - `happiness` decreases by at least 1 per tick, more if hunger<30 or energy<30.
    Log exact values at each observation point.

28. **Phase 2: Score Calculation Before Voting.**
    For each player, record current dragon stats and achievements. Calculate expected score:
    `score = happiness + hunger + energy + (achievements.len() * 50)`
    Compare with displayed score in UI. Log both values. If they differ, log as `kind: bug`.

29. Agent 4 checks `End game` before the host does. If the control is visible, clicking it must show `Only the host can end the workshop.`. If it is hidden, record that as a pass.

30. Host clicks `End game` and all clients confirm `Voting` and `0 / 5 votes submitted`.

### Voting Phase (Steps 31–40)

31. **Self-Vote Check.**
    Each player verifies they do NOT see a vote button for the dragon currently assigned to them. The vote buttons should only show dragons assigned to OTHER players. If a self-vote button is visible, log as `kind: bug`.

32. Each player votes once using the visible `Vote` button. After each vote, verify the count increments: `"1 / 5 votes submitted"` → `"2 / 5"` → etc.

33. The host votes once using the visible `Vote` button.

34. Agent 4 makes one duplicate click on the same `Vote` button and expects the vote count to stay unchanged after the second click (server overwrites with same value, so count stays at `5 / 5`).

35. Host confirms `5 / 5 votes submitted`.

36. Agent 4 checks `Reveal results` before the host does. If the control is visible, clicking it must show `Only the host can reveal voting results.`. If it is hidden, record that as a pass.

37. Agent 4 confirms `Build archive` is not visible to non-host clients.

38. Agent 4 checks `Reset workshop` before the host does. If the control is visible, clicking it must show `Only the host can reset the workshop.`. If it is hidden, record that as a pass.

39. Host clicks `Reveal results`.

40. **Results Content Verification.**
    Every client confirms `Workshop results`, `Creative pet awards`, and `Final player standings`.
    - Verify `Creative pet awards` section shows the winning dragon(s) with vote counts.
    - Verify `Final player standings` shows each player name and their score.
    - Verify scores match the formula: `happiness + hunger + energy + (achievements.len() * 50)`.
    - Log exact award text and standing rows for each client.

### Archive and Reset (Steps 41–48)

41. Host clicks `Build Archive`.

42. Host confirms `Workshop archive ready.`, `Captured final standings`, and `Captured dragons`.

43. The host and each non-host client confirm the archive panel is visible and the build button is gone.

44. **LLM Judge Verification (optional — requires LLM backend).**
    Host triggers `POST /api/workshops/judge-bundle` to generate the judge bundle. Verify response contains workshop data including dragon stats, player names, achievements, and handover tags.
    Then trigger `POST /api/llm/judge` with the bundle. Verify response includes:
    - `care_score`: numeric value (0–100 range expected)
    - `creativity_score`: numeric value (0–100 range expected)
    - `narrative`: non-empty string describing the workshop
    If the LLM backend is unavailable, log as `kind: question` with note `"LLM backend not configured"`.

45. **LLM Image Generation (optional — requires LLM backend).**
    Trigger `POST /api/llm/images` with a dragon description. Verify response includes a generated image URL or base64 data. If the LLM backend is unavailable, log as `kind: question`.

46. Host clicks `Reset workshop`.

47. Every client returns to `Workshop lobby` and `Workshop results` is no longer visible.

48. Host writes the iteration summary with iteration goal, issues found, friction points, fixes to carry forward, and pass/fail status.

### Final Summary (Step 49)

49. After Iteration 3, the host writes `final-summary.md` with the overall result, recurring issues, the biggest improvements, and the next recommended changes.

## Edge Case Test Matrix

### EC-1: Odd Player Count (3 players)
Run a separate workshop with 3 players (Host + 2 agents). Verify:
- Phase 2 shuffle: with 3 sorted dragon IDs [A, B, C], shuffle produces [C→Player1, A→Player2, B→Player3].
- No player keeps their Phase 1 dragon.
- Vote count shows `0 / 3 votes submitted`.
- Each player sees exactly 2 vote buttons (cannot vote for own dragon).

### EC-2: Single Player Workshop
Run a workshop with only the Host. Verify:
- Phase 2 shuffle: same dragon, speech includes `"New shift, same dragon..."`.
- Voting: immediate finalize with 0 eligible voters (cannot self-vote).
- Results: Host's score is displayed.
- No vote buttons are shown.

### EC-3: LeaveWorkshop
During Phase 1 with 5 players:
- Agent 4 sends a LeaveWorkshop command.
- Verify Agent 4 returns to the home screen.
- Verify remaining players see `Players in view: 4`.
- Verify the game can continue through all phases with 4 players.
- Verify voting shows `0 / 4 votes submitted`.

### EC-4: Join After Phase 1 Started
After Phase 1 has started:
- A new agent attempts to join with a valid workshop code.
- Expected rejection: `"This workshop has already started. New players can only join in the lobby."`
- The agent stays on the home screen.

### EC-5: Day/Night Boundary Reset
If the test can control or observe the game clock:
- Perform a Feed action just before a day/night boundary (e.g., hour 17).
- After the boundary crosses (hour 18), verify `food_tries` and `play_tries` have reset.
- Perform another Feed with the night food (`"fruit"`) and verify it earns `master_chef` for the new period.

### EC-6: Reconnect Preserves Dragon Stats
- Agent 3 performs Feed + Play actions, recording exact stats.
- Agent 3 closes browser context entirely.
- Agent 3 reconnects using saved token.
- Verify dragon stats match pre-disconnect values (minus any tick decay during disconnect).
- Verify achievements are preserved.

### EC-7: Host-Only Controls Visibility
All host-only controls are visible to all players in the current UI. The server rejects non-host commands. Verify for each:

| Control | data-testid | Expected rejection text |
|---|---|---|
| Start Phase 1 | `start-phase1-button` | `Only the host can start the workshop.` |
| Start handover | `start-handover-button` | `Only the host can begin handover.` |
| Start Phase 2 | `start-phase2-button` | `Only the host can begin Phase 2.` |
| End game | `end-game-button` | `Only the host can end the workshop.` |
| Reveal results | `reveal-results-button` | `Only the host can reveal voting results.` |
| Reset workshop | `reset-workshop-button` | `Only the host can reset the workshop.` |
| Build archive | `build-archive-button` | Not visible to non-hosts (count = 0) |

### EC-8: Double Phase Transition
Host clicks `Start Phase 1` twice rapidly. Verify:
- First click succeeds with `Phase 1 started.`.
- Second click produces an error or is ignored (server should reject since already in Phase 1).

## API Verification Matrix

### API-1: Health Endpoints

| Endpoint | Method | Expected Status | Expected Body |
|---|---|---|---|
| `/api/live` | GET | 200 | `{"ok": true, "status": "live"}` |
| `/api/ready` | GET | 200 | `{"ok": true, "service": "app-server", "status": "ready", "checks": {"store": true}}` |

### API-2: Workshop Lifecycle

| Endpoint | Method | Purpose | Key Fields |
|---|---|---|---|
| `/api/workshops/create` | POST | Create workshop | `name` → returns `workshop_code`, `token` |
| `/api/workshops/join` | POST | Join/reconnect | `code`, `name`, `token`(optional) |
| `/api/workshops/command` | POST | Send session command | `token`, `command` (SessionCommand enum) |

### API-3: SessionCommand Enum
All gameplay commands go through `POST /api/workshops/command`:

| Command | Fields | Phase | Expected Effect |
|---|---|---|---|
| `StartPhase1` | none | Lobby→Phase1 | Generates dragons, assigns to players |
| `StartHandover` | none | Phase1→Handover | Begins handover tag collection |
| `SubmitHandoverTags` | `tags: Vec<String>` | Handover | Saves tags for the player |
| `StartPhase2` | none | Handover→Phase2 | Shuffles dragons, starts accelerated decay |
| `SubmitObservation` | `text: String` | Phase1 | Records player observation |
| `FeedDragon` | `food: String` | Phase1, Phase2 | Feeds dragon, updates hunger/happiness |
| `PlayWithDragon` | `play: String` | Phase1, Phase2 | Plays with dragon, updates energy/happiness |
| `SleepDragon` | none | Phase1, Phase2 | Rests dragon, updates energy/happiness |
| `EndGame` | none | Phase2→Voting | Starts voting |
| `CastVote` | `dragon_id: String` | Voting | Casts or overwrites vote |
| `RevealResults` | none | Voting→End | Reveals final results |
| `BuildArchive` | none | End | Generates archive artifacts |
| `ResetWorkshop` | none | End→Lobby | Resets to lobby |
| `LeaveWorkshop` | none | Any | Removes player from session |

### API-4: LLM Endpoints

| Endpoint | Method | Purpose | Expected Response |
|---|---|---|---|
| `/api/workshops/judge-bundle` | POST | Generate data bundle for LLM judge | JSON with dragons, stats, players, achievements, tags |
| `/api/llm/judge` | POST | AI evaluation of workshop | `care_score`, `creativity_score`, `narrative` |
| `/api/llm/images` | POST | Generate dragon portrait | Image URL or base64 data |

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
- dragon stats not matching expected deltas after actions
- achievements not triggering when conditions are met
- achievements triggering when conditions are NOT met
- score mismatch between formula and displayed value
- Phase 2 shuffle giving a player their own dragon
- decay rates not matching Phase 1 vs Phase 2 multipliers
- day/night food/play tries not resetting at boundary
- blocked action messages not appearing when conditions are met
- LLM endpoints returning errors or empty responses
- condition hint text not matching actual dragon traits
- vote button appearing for self-vote

## Stop Conditions
- Stop the run if `/api/live` or `/api/ready` fail.
- Stop the run if workshop creation fails.
- Stop the run if any join, late join, reload, reconnect, phase change, vote completion, archive build, or reset does not reach the expected UI state within the wait budget.
- Stop the run if results reveal does not show `Workshop results`, `Creative pet awards`, and `Final player standings` within the wait budget.
- Stop the run if any console error or failed network request appears that blocks the current step.
- Stop the run if a recovered mismatch repeats after retry.
- Stop the run if dragon stats change by unexpected deltas after an action (indicates a domain logic bug).

## Optional Fault Probes
If the harness can intercept requests without code changes:
- abort one join request and record the exact degraded-path notice `failed to reach backend:` plus the underlying error text
- abort one archive build request and record the exact degraded-path notice `failed to reach backend:` plus the underlying error text
- abort one websocket or reconnect request and record the exact degraded-path notice `failed to reach backend:` plus the underlying error text
- abort one invalid reconnect request and record the degraded-path message `Session identity is invalid or expired.`
- abort one FeedDragon command request and verify the notice shows the backend error
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
- Host, Agent 1, Agent 2, Agent 3, and Agent 4 reach the lobby in iteration 1.
- Each later iteration starts from fresh host and player contexts.
- Agent 3 can reload and reconnect without losing the session or dragon stats.
- Host-only actions are denied for non-hosts with exact app text: `Start Phase 1` → `Only the host can start the workshop.`, `Start handover` → `Only the host can begin handover.`, `Start Phase 2` → `Only the host can begin Phase 2.`, `End game` → `Only the host can end the workshop.`, `Reveal results` → `Only the host can reveal voting results.`, `Reset workshop` → `Only the host can reset the workshop.`
- `Build archive` stays hidden for non-host clients during the end phase.
- Voting reaches `5 / 5`.
- Duplicate voting is suppressed: the second click on the same vote button does not change the vote count.
- Results are visible on every client after reveal.
- Archive is visible on the host and all non-host clients after build.
- After reset, all clients return to `Workshop lobby` and `Workshop results` is hidden.
- The final iteration records a friction score of 0 or 1 for every planned step.
- **Dragon action deltas match the exact values in the Domain Reference section.**
- **Achievements trigger correctly: `master_chef` on first correct food, `playful_spirit` on first correct play.**
- **Score formula matches `happiness + hunger + energy + (achievements.len() * 50)` for every player.**
- **Phase 2 shuffle assigns every player a different dragon (when player_count > 1).**
- **Blocked actions produce the exact blocked message text.**
- **Condition hint text matches the dragon's actual traits.**

## Validator Axes (for independent parallel validation)

When running 5 validators in parallel, each validator focuses on one axis:

### Validator 1: Flow & Sync
- Full phase lifecycle: Lobby → Phase1 → Handover → Phase2 → Voting → End → Reset
- Phase labels match expected UI text at each transition
- Player counts update correctly after join/leave/reconnect
- Reconnect and reload preserve session state
- Reset returns all clients to lobby

### Validator 2: Dragon Mechanics
- Feed/Play/Sleep actions produce correct stat deltas
- Blocked actions produce correct rejection messages
- Achievements trigger on correct first-try conditions
- Stats persist through reload/reconnect
- Phase 2 decay multiplier is 2x vs Phase 1

### Validator 3: Shuffle & Voting
- Phase 2 shuffle gives every player a different dragon
- Self-vote buttons are not shown
- Vote count increments correctly
- Duplicate votes don't change count
- Reveal shows correct awards and standings
- Score formula is correct

### Validator 4: Permissions & Errors
- All host-only controls rejected for non-hosts with exact text
- Invalid join code → `Workshop not found.`
- Invalid reconnect token → `Session identity is invalid or expired.`
- Post-lobby join → `This workshop has already started...`
- Build archive hidden for non-hosts
- Network failure → `failed to reach backend:` notice

### Validator 5: API & LLM
- Health endpoints return expected JSON
- Workshop create/join/command API contracts
- Judge bundle contains complete workshop data
- LLM judge returns care_score, creativity_score, narrative
- LLM images returns generated content
- Error responses for invalid commands
