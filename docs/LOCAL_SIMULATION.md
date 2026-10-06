# Solana provider mocks and production simulation

The mock serves external providers. Signal, Market Evidence, Social Evidence and
Strategy run their actual parsers, PostgreSQL transactions, APIs, schedulers,
frozen model and paper ledger. Strategy does not dispatch trades to Transaction
in this production path. Transaction has a separate mock instance and execution
suite; no Strategy-to-Transaction adapter is added here.

## Native local stack

Requires Rust, OpenSSL, PostgreSQL 18 binaries, and the project's Python venv
with its normal service dependencies, Telethon, psycopg, LightGBM and protobuf
6.31.1 or newer compatible with Transaction's generated protobuf code. Repositories:

```
Documents/crypto_project/solana-mock-service
Documents/crypto_project/transaction-service
Documents/crypto_project/microservices/{signal_service,strategy_service,shared_features}
Documents/market-evidence-service
Documents/social-evidence-service
```

From `crypto_project`:

```sh
venv/bin/python solana-mock-service/scripts/simulate.py check --profile local
venv/bin/python solana-mock-service/scripts/simulate.py up
venv/bin/python solana-mock-service/scripts/simulate.py run --suite production --extended
venv/bin/python solana-mock-service/scripts/simulate.py status
venv/bin/python solana-mock-service/scripts/simulate.py down
```

The parent repository also provides `bash ops/local-services.sh` as a shared
entrypoint. The explicit `--profile local` selects the isolated mock environment.
`check` verifies prerequisites and port availability without starting services.

`up` builds the binaries and creates `/tmp/crypto-production-simulation` with a
private PostgreSQL cluster, ephemeral TLS certificates, six separate service
roles/databases, synthetic authority baselines, fixture channel/cohort, and a
copy of the pinned production model. Default ports: mock 18080, Transaction mock
18081, Market 18088, Strategy 18090, Transaction 18091, delivery proxy 18092,
PostgreSQL 55493. All bind to numeric loopback. No Telegram authentication,
production environment files or launchd jobs are used. Strategy remains paper
only. Transaction uses live execution semantics against the local chain mock.

Every suite requires a fresh stack. Use `--run-dir` consistently on all commands.
`down` preserves evidence; `destroy` stops owned processes and deletes only an
initialized simulation directory. Process identities are checked before stopping.
Use `--no-build` after builds. Repository, Python, model, PostgreSQL binary paths
and every port have CLI overrides (`--help`).

Suites:

- `core`: prospective callout, complete enrichment, frozen acceptance, on-time
  reference, paper entry and TP/PnL; rejected fixture; Signal restart/dedup;
  Social canonical no-match/mint matching/flood-wait; lost ACKs on three real
  service lanes, conflicting identities, PIT and worker fencing/restart.
- `production`: core plus actual token-creation and market-regime collectors.
- `collectors`: ingestion/enrichment plus both collectors.
- `transaction`: the existing Transaction execution scenarios against its own
  mock. These include chain/provider faults, confirmation and wallet reconciliation.
- `all`: production plus Transaction.
- `--extended`: additionally exercises bounded enrichment 429 retry and terminal 503 partial evidence,
  conservative SL-before-TP intrabar behavior, missing price window expiry,
  PostgreSQL outage recovery, and admission pressure above the 20-mint cap.

Scenarios use **real time**. Core takes several minutes; extended adds several
more. Transaction's complete suite takes substantially longer. The 120-minute
maximum-hold soak is opt-in with `--max-hold-soak` and preserves the full two-hour
wall-clock window. Long capacity soaks and MTProto transport/auth tests are not
part of the short suites. No model, threshold, policy deadline or evidence
availability timestamp is modified to accelerate a result.

Each run writes `reports/CONSOLIDATED_REPORT.md`, assertion/domain traces,
provider request records and exact mutation wires through the delivery proxy.
Logs and dummy Transaction credentials stay in the private run directory.

## Provider contracts and controls

Existing Solana RPC/WebSocket, Jupiter and Jito routes remain available. Birdeye
paths are mounted at the root, matching the real client's path construction:

| GET endpoint | Fixture data |
| --- | --- |
| `/defi/price` | `price` or visible `prices[{visible_at,value}]`; optional `update_unix_time` |
| `/defi/ohlcv`, `/defi/v3/ohlcv` | `candles[{unix_time,o,h,l,c,v,visible_at}]` |
| `/defi/token_overview` | `overview` object |
| `/defi/token_security` | `security` object |
| `/token/v1/holder-profile` | `holder_profile` object |
| `/token/v1/holder/chart` | `holder_chart` array with `timestamp` and `holder` |
| `/defi/v3/token/meme/list` | each token's `meme` object |
| `/defi/token_creation_info` | `creation` provenance object |

OHLCV respects requested ranges and exposes only completed, visible minutes.
Holder history respects its range/count; meme samples respect liquidity/volume
filters, sorting and limits. Unconfigured mints fail closed.
Fixtures enter through `birdeye.tokens[mint]` in the scenario. Raw response
replacement is available in `birdeye.responses[endpoint]` for schema/parity cases.

`POST /__mock/scenario` **replaces the entire scenario**; read the current scenario
first and preserve other sections. `POST /__mock/reset` clears runtime state and
requests/counters, retaining the current scenario unless a scenario body is supplied.
Send `{}` to reset to defaults and clear configured provider fixtures/faults. `GET /__mock/requests`
returns endpoint/query/attempt/receipt-time/fault records.

Both `birdeye.faults` and `telegram.faults` accept a map to arrays of:

```json
{"on_call":1,"status":429,"delay_ms":0,"body":{"success":false},"malformed":false}
```

`on_call:0` applies repeatedly. Status 0 defaults to 200. `delay_ms` produces real
transport delay; `malformed:true` returns invalid JSON. Use HTTP status, raw
response replacement and delays for auth errors, throttling, outage, timeout,
empty/partial/wrong-mint/wrong-interval responses or candle revisions.
Birdeye fault keys can be endpoint-wide or `endpoint:mint` for targeted failures.
Telegram keys include `head`, `page`, `updates`, or scoped `page:social` and
`updates:signal`; the consumers have independent cursors.

Publish raw events with `POST /__mock/telegram/messages`:

```json
{"channel":"FixtureCalls","id":100,"event_ts":"2026-10-03T05:00:00Z","text":"Solana CA: AUvBbyBxZMx9hqMCcZ4ATQDbjiq6H3phx6en1nNxpump","author_id":"7","author_kind":"USER"}
```

Optional `visible_at` is an epoch timestamp; otherwise receipt time is used.
Exact retries are idempotent; conflicting channel/message identities return 409.
`GET /__mock/telegram/channels` and `POST /__mock/telegram/{head,page,updates}`
provide bounded history/live acquisition. Signal uses the Telethon-shaped
adapter; Social uses its independent existing NDJSON subprocess protocol.
Script `FLOOD_WAIT` or `DISCONNECTED` payloads for acquisition failures.
This is application-boundary simulation, not MTProto emulation.

The delivery proxy forwards to the **real handlers**, then optionally discards
their successful ACK. `POST /__proxy/drop-ack` accepts `path` and `count`;
`GET /__proxy/requests` records exact mutation wires and upstream outcomes.
It does not replace service-domain responses.

Market test-provider selection requires `MARKET_EVIDENCE_TEST_PROVIDER=1` and a
numeric HTTP loopback origin. It retains production CU reservations and durable
claims. Default production constructors still pin Birdeye's production origin.
Social's synthetic initializer/scan interval require its explicit test mode and
`social_simulation` database. Market initialization similarly requires
`market_simulation`. Python dotenv loading is explicitly disabled in the launcher, and Signal fixture
startup skips the credential loader. Transaction's token-account cleanup cadence
matches its existing local profile (20 seconds). Production migrations and artifacts are owned by each
service; no shared-schema access is added to service code.
