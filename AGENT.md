# AGENT.md

## Local Build

Use the local `kind` workflow for a full rebuild and deploy:

```bash
./scripts/refresh-local-kube-auth.sh
```

What it does:
- builds the web bundle with `xtask build-web`
- builds `app-server` for Linux arm64
- builds `dragon-shift-rust:kind-local`
- loads the image into `kind-dragon-shift-local`
- upgrades the Helm release in namespace `dragon-shift`
- restarts the deployment and refreshes the `http://127.0.0.1:4100` port-forward

## Local Checks

Run the standard Rust checks from `platform/`:

```bash
cargo check --workspace --all-targets
cargo test --workspace
```

Run focused package tests when needed:

```bash
cargo test -p app-web
cargo test -p app-server
```

## Local E2E

From `e2e/`, install dependencies once:

```bash
npm ci
```

Run the restart proof locally:

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/dragon_shift_test npm run test:restart-local
```

Run deployed-browser tests against the local `kind` deployment:

```bash
E2E_BASE_URL=http://127.0.0.1:4100 npm run test:deployed
```

## Notes

- Local `kind` cluster name: `dragon-shift-local`
- Local kube context: `kind-dragon-shift-local`
- The local app should be reachable at `http://127.0.0.1:4100`
- If `helm upgrade` targets the wrong cluster, check `scripts/refresh-local-kube-auth.sh` for the `--kube-context` flag
