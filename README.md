# Solana provider mocks

The existing Solana RPC/Jupiter/Jito mocks support transaction fixtures. Snapshot
fixtures add the nine Birdeye endpoints used by Market and a Telegram protocol
bridge for Social. All responses are deterministic local fixtures. The mock never
contacts a chain, paid provider, Telegram, production database or real session.

## Snapshot completeness verification

From this checkout, after pulling current `main`, run:

```bash
/opt/strategy-service/venv/bin/python scripts/run_snapshot_completeness.py \
  --python /opt/strategy-service/venv/bin/python
```

The default sibling checkouts are `market-evidence-service`,
`social-evidence-service`, and `strategy_service`. The supplied Python must have
pytest and psycopg installed. Rust dependency caches and repository-pinned
compilers must already be present; builds use `--locked --offline -j 1`.
The host needs root, systemd, `unshare`, PostgreSQL 16 and `ip`. The runner fails
closed if it cannot establish a separate network namespace. It clears inherited
configuration, creates disposable databases inside that namespace, binds only
loopback, and uses a transient cgroup with CPUQuota=40%, MemoryHigh=1200M,
MemoryMax=1800M, TasksMax=128 and low I/O weight. Production services keep running.

Only snapshot-related fixtures/regressions run. `MOCK_PROVIDERS=snapshots` disables
Solana/Jupiter/Jito routes. No production configuration, credentials, models,
Telegram sessions or production database is used. The runner stops its local
processes and leaves logs, request evidence, matrix results and `summary.json` in
a uniquely named `/root/snapshot-completeness-mocks-<UTC>` directory.

Coverage includes fast candles/overview/price, enrichment, holder profile/window
mapping, regime horizons/sample coverage, actual Strategy event receipt and replay,
point-in-time feature materialization, Social pagination/cursors/classification,
request failure classes, cancellation, leases, atomic publication and orphan
recovery. Every expected matrix input is accounted for, including missing results.
Internal write failures are explicit negative controls, not provider no-data.

`summary.json` distinguishes test execution failures from architecture findings.
An unfinished provider page or incomplete aggregate member coverage can therefore
make the overall command exit nonzero even when the audit code executes correctly.
Do not change expected outcomes to make that gate green. The runner prints an
explicit verdict and never claims all production gaps have provider attribution.
Passing a finite fixture matrix is not proof of 100% production reliability.

## Deterministic scenario controls

`POST /__mock/scenario` accepts `birdeye.faults` keyed by endpoint and
`telegram.messages`, `telegram.head_message_id`, and `telegram.faults` keyed by
`/telegram/head` or `/telegram/page`. Faults have `on_call` (0 = every call),
`status`, `delay_ms`, `malformed`, and an optional full `body`. Setting a scenario
resets per-endpoint call ordinals while retaining request evidence. Reset clears
both scenario state and logs. GET `/__mock/requests` returns scoped requests,
selected faults, response status/body/digest and row count. It stores no headers
or credentials. Logs are capped at 10,000 requests per disposable run.

The default legacy OHLCV fixture uses the observed `unixTime` shape; v3 uses
`unix_time`. Candles are scoped to the requested mint, interval and range. The
meme endpoint deliberately has 5m/1h returns and no invented 15m return. Body
controls allow empty data, unsupported fields, duplicate rows, wrong identities,
unfinished pages and field-level contradictions without replacing service logic.

`MOCK_BIND_ADDR` is a local-only loopback override. For manual fixture work:
`APP_ENV=local MOCK_PROVIDERS=snapshots MOCK_BIND_ADDR=127.0.0.1:18080 cargo run`.
The isolated runner is preferred on a production host.

## Limits of this verification

Fixtures exercise current checked-out service code, not installed production binaries.
The receipt-to-outbox-to-Strategy inbox check uses actual serialized Market events
and Strategy ingestion; it does not run the full live Signal admission, event
transport, Strategy scheduling/decision loop, or Transaction pipeline. Social
fixtures test durable pages, classification and restart cursors, but do not prove
production snapshot-to-Social association or live source discovery coverage.
Historical provider responses, real retention/access entitlements, production
queue contention and every crash boundary remain outside this local matrix.
Establishing provider-only attribution for production still requires a complete
expected-work ledger and correlated provider/persistence/publication/consumption
evidence for each gap. Missing telemetry remains unknown.

For a focused Strategy rerun using a retained local Market fixture export:

```bash
/opt/strategy-service/venv/bin/python scripts/run_snapshot_completeness.py \
  --python /opt/strategy-service/venv/bin/python --only strategy \
  --market-events /root/snapshot-completeness-mocks-<UTC>/market-events.json
```

This creates a new isolated database and reuses fixture envelopes; it does not
reacquire provider data or rerun unaffected Rust builds.

Use `--only market` for a focused Market/regime rerun. It runs the acquisition
matrices, provider fault matrix and OHLCV/horizon contract regressions in fresh
isolated databases, without running Social or Strategy.

For regime acquisition repairs, run only the relevant isolated fixtures:

```sh
/opt/strategy-service/venv/bin/python scripts/run_snapshot_completeness.py --only regime --python /opt/strategy-service/venv/bin/python
```

This mode starts a disposable local PostgreSQL database and snapshot mock in a
network namespace with loopback only, clears production environment variables,
and caps CPU/memory. It exercises regime fallback, exact candle coverage, rejected
response retention and atomic persistence; it skips Social, Strategy and unrelated
provider endpoint matrices. Rust tests run sequentially because they share fixture
reset endpoints. Successful fixtures do not prove production provider-only attribution.

## Existing local simulation

The retained local simulation adapters and launcher are documented in [LOCAL_SIMULATION.md](docs/LOCAL_SIMULATION.md). Their full-provider routes use the default `MOCK_PROVIDERS=all`; the new completeness fixtures use `MOCK_PROVIDERS=snapshots`. The automation performance lab keeps Transaction in SHADOW and does not run the legacy live-semantics launcher.
