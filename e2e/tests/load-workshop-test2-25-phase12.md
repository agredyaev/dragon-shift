# Manual Load Run

Use the existing `load-workshop-30clients.spec.ts` with these env vars for the
`load2..load25 -> workshop 988875 -> cover Phase 1 and Phase 2` scenario.

For a fresh manual run, create the workshop from the UI gear settings with short
phase lengths, for example Phase 1 = `2` minutes and Handover/Phase 2 = `2`
minutes. Blank fields use the app defaults.

```bash
cd e2e
E2E_BASE_URL=http://127.0.0.1:4100 \
E2E_EXTERNAL_WORKSHOP_CODE=988875 \
E2E_CLIENT_COUNT=24 \
E2E_CLIENT_NAME_PREFIX=load \
E2E_CLIENT_NAME_OFFSET=2 \
E2E_ALLOW_ACCOUNT_CREATION=false \
E2E_ALLOW_STARTER_FALLBACK=true \
E2E_HOST_ACCOUNT_NAME=test1 \
E2E_CLIENT_PASSWORD='<set-real-password>' \
E2E_COVERAGE_TARGET=phase2 \
npx playwright test tests/load-workshop-30clients.spec.ts --project=chromium
```

Behavior:

- guest clients are `load2` through `load25`
- host remains manual as `test1`
- missing guest accounts can be created on first sign-in with the supplied password
- guests without owned dragons can still join via random starter
- guests join the existing workshop from the UI
- guests stay active through Lobby, Phase 1, Handover, and Phase 2
- hotspots, HTTP errors, request failures, console errors, disconnects, and per-client activity are logged to the generated summary and NDJSON events
- the run stops after all guests have covered Phase 2 activity
