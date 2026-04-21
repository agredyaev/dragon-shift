
You are a senior software architect and refactoring engineer.

Task:
Refactor the screen flow of my web game.

Process:
1. First, analyze the current implementation and its dependencies.
2. Identify all affected screens, components, frontend state boundaries, backend services, endpoints, entities, and transitions.
3. Run 3 independent validation passes on the analysis before implementation:
   - Validator 1: architecture and dependency coverage
   - Validator 2: screen flow and transition coverage
   - Validator 3: business rules, edge cases, and data consistency coverage
4. Do not start implementation until all 3 validators confirm that the analysis is complete.
5. If anything is unclear, list the exact gap and the minimum required assumption.

Architecture rules:
- All business logic must stay in the backend.
- Frontend must remain thin: views, rendering, input handling, and minimal view-state only.
- Do not place business rules in frontend components, hooks, stores, or client-side services.
- Preserve the existing visual style and product aesthetics.
- Follow DRY, KISS, DDD, and DI.
- Be highly intolerant of overengineering.
- Prefer the simplest solution that fully solves the task.
- Do not mix screen responsibilities.
- Each game step must be its own screen.

UX rules:
- The game must teach and guide the player through the flow by the UI itself.
- Guidance must come from the screen sequence, available actions, and constraints.
- Do not add extra tutorial copy, long explanatory text, or duplicated onboarding text.

Main product change:
Change the workshop flow.

Required refactor:
- Remove character creation from Workshop Step 0.
- Character creation must be a separate capability outside the workshop flow.
- A player must be able to create characters independently from workshops.
- Characters are a separate entity.

Required screen flow:

1. Start screen
- Keep the current hero section with the game title.
- Below it, show:
  - player name
  - password
- This screen creates an account.
- If the account name already exists, return a clear message that the player must choose a different name.

2. Player account screen
This screen must contain:
- Block 1: Create workshop
- Block 2: Create character
- Block 3: List of open workshops with Join buttons

Character rules:
- Maximum 5 characters per player.

Workshop join flow:
- Remove the current manual workshop ID input.
- Replace it with a list of open workshops with Join buttons.
- When the player clicks Join, show the list of characters created by that player.
- If the player has no characters, assign one random pre-generated character.

3. Lobby screen
- After character selection, the player enters the lobby.
- Lobby must remain a separate screen.
- Keep the current host controls in the lobby, including the button to start Phase 1.

Implementation rules:
- Do not merge multiple game steps into one screen.
- Do not hide core game steps inside modals.
- Keep responsibilities explicit and separated.
- Prefer simplification over expansion.
- Do not introduce abstractions unless they are clearly necessary.

Expected output:
A. Current-state analysis
- existing flow
- dependency map
- affected frontend areas
- affected backend areas
- risks
- assumptions

B. Validation results
- Validator 1 findings
- Validator 2 findings
- Validator 3 findings
- final conclusion on analysis completeness

C. Refactoring plan
- target screen map
- updated entities and responsibilities
- frontend changes
- backend changes
- API contract changes
- migration notes if needed

D. Implementation
- backend first
- frontend second
- thin views only

E. Final verification
- confirm business logic is backend-only
- confirm all game steps are separate screens
- confirm character creation is decoupled from workshop creation
- confirm workshop join flow uses the open workshop list, not manual ID input
- confirm player character selection works as required
- confirm fallback to a random pre-generated character when needed
- confirm no unnecessary abstractions were introduced

Quality bar:
- Be precise.
- Be minimal.
- Do not invent features.
- Do not add speculative improvements unless required for this refactor.
- If you propose a new abstraction, justify it.
