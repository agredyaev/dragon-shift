# Dragon Shift — Game Methodology

Technical reference for all game mechanics, formulas, and scoring criteria.

---

## Game Flow

```
Lobby → Phase 1 (Observe) → Handover → Phase 2 (Care) → Voting → End
```

| Phase     | Purpose                                   | Duration (configurable) |
|-----------|-------------------------------------------|-------------------------|
| Lobby     | Players join, set descriptions, ready up   | `phase0_minutes`        |
| Phase 1   | Creator observes and discovers dragon      | `phase1_minutes`        |
| Handover  | Creator writes 3 handover tags             | `phase2_minutes`        |
| Phase 2   | Caretaker follows handover instructions    | `phase2_minutes`        |
| Voting    | Anonymous vote for favorite dragon         | untimed                 |
| End       | Results displayed, judge scores applied    | untimed                 |

---

## Dragon Preferences

Each dragon is assigned **random hidden preferences** at Phase 1 start. Initial Phase 1 stats begin at **80/80/80** (hunger/energy/happiness), then reset to **80/80/80** when Phase 2 starts.

| Attribute       | Values                    | Combinations |
|-----------------|---------------------------|--------------|
| `active_time`   | Day, Night                | 2            |
| `favorite_food` | Meat, Fruit, Fish         | 3            |
| `favorite_play` | Fetch, Puzzle, Music      | 3            |
| `sleep_rate`    | 1 or 2                    | 2            |

**Total unique preference profiles: 2 × 3 × 3 × 2 = 36**

Dragon names are randomly generated from a pool of fantasy prefix/suffix pairs (e.g., Emberclaw, Frostscale).

---

## Time System

- Time is an integer `0..47`, advancing by 1 every **1 second** (real time).
- **Day**: ticks 12–35 (24 ticks). **Night**: ticks 36–47 and 0–11 (24 ticks).
- Full day/night cycle = **48 seconds**.
- Food/play try counters (`food_tries`, `play_tries`) reset on day↔night transitions.

---

## Stat Decay (per tick)

### Base Decay

| Stat      | Formula                                                            |
|-----------|--------------------------------------------------------------------|
| Hunger    | `hunger -= decay_multiplier`                                       |
| Energy    | `energy -= sleep_rate × time_penalty × decay_multiplier`           |
| Happiness | `happiness -= (1 + H + E + T + P) × decay_multiplier`             |

Where:
- `decay_multiplier` = **1** (Phase 1) or **2** (Phase 2)
- `time_penalty` = **2** if dragon is active during wrong time, else **1**
- `H` = +1 if `hunger < 20`
- `E` = +1 if `energy < 20`
- `T` = +1 if wrong active time AND no sleep shield
- `P` = `min(penalty_stacks, 4)` — accumulated wrong-action penalties

### Phase 2 Decay Example

A player with 4 penalty stacks and no other modifiers:
```
happiness -= (1 + 0 + 0 + 0 + 4) × 2 = 10 per tick
```

With low hunger and low energy additionally:
```
happiness -= (1 + 1 + 1 + 0 + 4) × 2 = 14 per tick
```

### Penalty Stack Decay

- 1 penalty stack removed every **6 ticks** (natural decay).
- Correct actions immediately remove 1 stack.
- Wrong actions immediately add 1 stack.

---

## Actions

All actions share a **2-tick cooldown**. Attempting an action during cooldown returns `CooldownViolation` (no action applied, `cooldown_violations` counter incremented).

### Feed

| Condition           | Effect                                                              |
|---------------------|---------------------------------------------------------------------|
| `hunger >= 95`      | **Blocked** (AlreadyFull)                                           |
| Correct food        | hunger +40, happiness +20, penalty_stacks -1, correct_actions +1    |
| Wrong food          | hunger +5, happiness −(12 + escalation), penalty_stacks +1          |

**Escalation**: `penalty = 12 + min(wrong_food_count - 1, 3) × 3`. Range: 12–21.

### Play

| Condition           | Effect                                                              |
|---------------------|---------------------------------------------------------------------|
| `hunger < 20`       | **Blocked** (TooHungryToPlay)                                       |
| `energy < 20`       | **Blocked** (TooTiredToPlay)                                        |
| Correct play        | energy -20, happiness +30, penalty_stacks -1, correct_actions +1    |
| Wrong play          | energy -15, happiness −(12 + escalation), penalty_stacks +1         |

**Escalation**: `penalty = 12 + min(wrong_play_count - 1, 3) × 3`. Range: 12–21.

### Sleep

| Condition           | Effect                                                              |
|---------------------|---------------------------------------------------------------------|
| `energy >= 90`      | **Blocked** (TooAwakeToSleep)                                       |
| Correct time*       | energy +50, happiness +15, penalty_stacks -1, correct_actions +1, correct_sleep_count +1 |
| Wrong time*         | energy +50, happiness +0, wrong_sleep_count +1, penalty_stacks +1   |

*Correct sleep time: Day-active dragon sleeping at night, or Night-active dragon sleeping during day.*

Sleep grants a 1-tick `sleep_shield` that suppresses the wrong-time happiness decay component.

---

## Condition Hint

Players receive a `condition_hint` string — a mood-based description that does NOT reveal hidden preferences. It reports:

1. **Mood** — happiness level bracket (cheerful/relaxed/grumpy/unhappy)
2. **Hunger** — belly fullness bracket
3. **Energy** — alertness bracket
4. **Time reactivity** — "lively" if matching active time, "sluggish" if not

This requires the player to interpret behavioral signals, not read raw data.

---

## Phase Transitions

### Phase 1 → Handover
- Creator writes exactly **3 handover tags** (capped at 3).
- Discovery observations capped at last **6** entries.
- Disconnected players get auto-filled fallback tags.
- Connected players with < 3 tags **block** the transition.

### Handover → Phase 2
- Dragon assignment: **rotate-by-one** (BTreeMap key order). Last dragon → first player.
- All dragon stats **reset to 80/80/80**.
- All counters reset: `wrong_food_count`, `wrong_play_count`, `wrong_sleep_count`, `correct_sleep_count`, `cooldown_violations`, `total_actions`, `correct_actions`, `penalty_stacks`, `penalty_decay_timer`, `peak_penalty_stacks`, `found_correct_food`, `found_correct_play`, `food_tries`, `play_tries`, `phase2_ticks`, `phase2_lowest_happiness` (->100).

### Phase 2 → Voting
- `award_phase_end_achievements()` called before `enter_voting()`.
- If ≤1 eligible player, voting finalizes immediately.

### Voting → End
- `finalize_voting()` sets all player scores to **0**.
- LLM judge is called asynchronously.
- `apply_judge_scores()` distributes `observation_score` → creator, `care_score` → caretaker.

---

## Scoring Model

Two independent leaderboards:

| Leaderboard | Source                                   | Details                            |
|-------------|------------------------------------------|------------------------------------|
| Creativity  | Player votes                             | Anonymous ballot, "Dragon #N"      |
| Mechanics   | LLM judge (`observation_score + care_score`) | Per-player composite score     |

### Judge Scoring

The LLM judge (Gemini) evaluates each dragon and produces:

| Score              | Range | Awarded To           | Criteria                                                      |
|--------------------|-------|----------------------|---------------------------------------------------------------|
| `observation_score`| 0–100 | Original owner (P1)  | Accuracy of observations vs real preferences, quality of handover tags |
| `care_score`       | 0–100 | Current owner (P2)   | Adherence to handover instructions, action correctness, final stats |
| `creativity_score` | 0–100 | (displayed only)     | Creativity of observations and tags                           |

**Final player score** = `observation_score` (for dragon they created) + `care_score` (for dragon they cared for).

### Judge Input (JudgeDragonBundle)

The judge receives per dragon:
- Handover chain: discovery observations + handover tags
- Phase 2 action traces with `was_correct` (bool) and `block_reason` (string)
- Final stats: hunger, energy, happiness
- Actual preferences: 4 fields (`active_time`, `favorite_food`, `favorite_play`, `sleep_rate`)
- Summary stats: `total_actions`, `correct_actions`, `wrong_food_count`, `wrong_play_count`, `wrong_sleep_count`, `correct_sleep_count`, `cooldown_violations`, `penalty_stacks_at_end`, `phase2_lowest_happiness`

### Judge Penalties

The prompt instructs the judge to:
- Penalize heavily for high `cooldown_violations` (spam)
- Penalize for high wrong action counts
- Reward high correct-action ratios
- Consider `phase2_lowest_happiness` as a quality indicator

---

## Achievements

13 achievements, awarded inline during actions, during ticks, or at phase end:

| Achievement          | When Awarded   | Criteria                                                      |
|----------------------|----------------|---------------------------------------------------------------|
| `master_chef`        | Inline (Feed)  | Correct food on a day/night cycle's first try (`food_tries == 1`) |
| `playful_spirit`     | Inline (Play)  | Correct play on a day/night cycle's first try (`play_tries == 1`) |
| `speed_learner`      | Inline         | Found both correct food AND play within first 3 total actions; stored even when the same action also returns `master_chef` or `playful_spirit` as the primary award |
| `steady_hand`        | Tick (Phase 2) | happiness >= 60 for 20+ Phase 2 ticks, lowest never < 60      |
| `no_mistakes`        | Phase End      | 0 wrong food + 0 wrong play + 0 wrong sleep, >= 5 total actions, and at least one correct food or play |
| `zen_master`         | Phase End      | 0 penalty stacks at end, >= 8 total actions                  |
| `button_masher`      | Phase End      | 5+ cooldown violations — spamming won't make it love you     |
| `restful_rhythm`     | Phase End      | 2+ correct-time sleeps, 0 successful wrong-time sleeps, and >= 5 total actions |
| `rock_bottom`        | Tick           | Happiness reached 0 — nowhere to go but up                   |
| `helicopter_parent`  | Phase End      | 20+ total actions — give the dragon some space               |
| `comeback_kid`       | Phase End      | Lowest happiness <= 15 but ended >= 70 — epic recovery       |
| `chaos_gremlin`      | Phase End      | Peak penalty stacks reached 4+ — maximum chaos achieved      |
| `perfectionist`      | Phase End      | >= 80% correct action ratio with >= 10 total actions         |

Achievements are awarded once per player, deduplicated, and persisted to the database. Phase-end achievements are finalized before every Phase 2 exit into voting, including `EndGame`, `StartVoting`, and auto-advance. Tick-awarded achievements are persisted immediately when awarded, even between normal throttled tick saves. `steady_hand` is awarded as soon as its 20-tick threshold is reached and is not revoked by later stat drops.

---

## Voting

- Anonymous: dragons displayed as "Dragon #1", "Dragon #2", etc.
- Players cannot vote for the dragon they currently care for.
- Real names revealed only in results.
- Creativity leaderboard = vote counts.

---

## API Protocol

| Operation         | Endpoint                     | Notes                                     |
|-------------------|------------------------------|--------------------------------------------|
| Create workshop   | `POST /api/workshops`        | Returns `{ "sessionCode": "..." }`         |
| Join session      | WebSocket upgrade            | Origin header required                     |
| Player commands    | WebSocket messages           | camelCase action names                     |
| Judge evaluation  | `POST /api/workshops/:code/judge` | Triggers LLM, applies scores         |
| Generate image    | `POST /api/workshops/:code/dragons/:id/image` | Imagen 4.0                   |

---

## Infrastructure

| Component      | Technology                    |
|----------------|-------------------------------|
| Backend        | Rust (Axum)                   |
| Frontend       | Rust (Leptos, WASM)           |
| Database       | PostgreSQL                    |
| LLM Judge      | Google Gemini 2.5 Flash       |
| LLM Images     | Google Imagen 4.0             |
| Deploy         | Terraform → GCE + nip.io SSL |
| Realtime       | WebSocket                     |

---

## Test Coverage

226 tests across 8 modules:

| Module       | Tests |
|--------------|-------|
| Domain       | 22    |
| App-server   | 111   |
| App-web      | 41    |
| Persistence  | 30    |
| Protocol     | 10    |
| Realtime     | 6     |
| Security     | 13    |
| Xtask        | 13    |
