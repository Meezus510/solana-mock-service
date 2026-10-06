#!/usr/bin/env python3
"""Isolated native production simulation. Never sources production environment files."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import secrets
import shutil
import signal
import socket
import subprocess
import time
import urllib.request
import uuid
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parents[1]
OWNER = "solana-mock-production-simulation-v1"


def stamp():
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def http(url, payload=None):
    request = urllib.request.Request(
        url,
        data=None if payload is None else json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        return json.load(response)


def wait(fn, timeout=60, label="condition", poll_seconds=0.2):
    end = time.monotonic() + timeout
    last = None
    while time.monotonic() < end:
        try:
            result = fn()
            if result:
                return result
        except Exception as exc:
            last = exc
        time.sleep(poll_seconds)
    raise RuntimeError(f"timed out waiting for {label}: {last}")


class Stack:
    def __init__(self, args):
        self.args = args
        self.root = Path(args.run_dir).resolve()
        self.project = Path(args.project).resolve()
        self.market = Path(args.market).resolve()
        self.social = Path(args.social).resolve()
        self.transaction = Path(args.transaction).resolve()
        self.python = os.path.abspath(args.python or self.project / "venv/bin/python")
        self.pg = Path(args.pg_bin).resolve()
        self.path = self.root / "state.json"
        self.state = (
            json.loads(self.path.read_text())
            if self.path.exists()
            else {"owner": OWNER, "processes": {}, "created_at": stamp()}
        )
        if self.state.get("owner") != OWNER:
            raise RuntimeError("refusing unrecognized simulation state")
        if self.state.get("paths"):
            paths = self.state["paths"]
            for name in ["project", "market", "social", "transaction"]:
                setattr(self, name, Path(paths[name]))
            self.python = paths["python"]
            self.pg = Path(paths["pg"])
        self.ports = {
            "mock": args.mock_port,
            "market": args.market_port,
            "strategy": args.strategy_port,
            "transaction": args.transaction_port,
            "postgres": args.pg_port,
            "mock_transaction": args.transaction_mock_port,
            "proxy": args.proxy_port,
        }
        if self.path.exists():
            self.ports = self.state["ports"]
        self.ports.setdefault("proxy", args.proxy_port)
        self.urls = {
            k: f"http://127.0.0.1:{v}" for k, v in self.ports.items() if k != "postgres"
        }
        self.env = {
            k: os.environ[k]
            for k in ["PATH", "HOME", "TMPDIR", "LANG"]
            if k in os.environ
        }
        self.env.update(
            PYTHONUNBUFFERED="1",
            PYTHON_DOTENV_DISABLED="1",
            PAPER_ONLY="true",
            REAL_EXECUTION_ENABLED="false",
            APP_ENV="local",
            PYTHONPATH=os.pathsep.join(
                map(
                    str,
                    [
                        HERE / "adapters",
                        self.project / "microservices/signal_service/src",
                        self.project / "microservices/strategy_service/src",
                        self.project / "microservices/research_service/src",
                        self.project / "microservices/shared_features/src",
                        self.project,
                    ],
                )
            ),
            MARKET_EVIDENCE_TEST_PROVIDER="1",
            MARKET_EVIDENCE_TEST_PROVIDER_URL=self.urls["mock"],
            TELEGRAM_FIXTURE_URL=self.urls["mock"],
            SIGNAL_TELEGRAM_PROVIDER="fixture",
            SOCIAL_EVIDENCE_TEST_PROVIDER="1",
            MOCK_BIND=f"127.0.0.1:{self.ports['mock']}",
            MOCK_CONFIG_DIR=str(HERE / "config"),
            MARKET_EVIDENCE_API_URL=self.urls["proxy"] + "/market",
            STRATEGY_SERVICE_API_URL=self.urls["proxy"] + "/strategy",
            MARKET_EVIDENCE_HEALTH_ADDR=f"127.0.0.1:{self.ports['market']}",
            MARKET_EVIDENCE_NORMALIZED_SIGNAL_URL=self.urls["proxy"]
            + "/market/v1/normalized-signals",
            MARKET_EVIDENCE_OUTBOUND_CONSUMERS="strategy_service="
            + self.urls["proxy"]
            + "/strategy/v1/market-events",
            BIRDEYE_API_KEY="fixture",
            TG_API_ID="1",
            TG_API_HASH="fixture",
            TG_SESSION_STRING="",
            TG_SESSION_FILE=str(self.root / "fixture_session"),
            TG_PHONE="",
            TG_BASE_LISTENER_ENABLED="false",
            TG_CATCHUP_LIMIT="100",
            TG_CATCHUP_RESEARCH_ONLY="false",
            SIGNAL_STORAGE_BACKEND="postgres",
            STRATEGY_CONFIG_PATH=str(self.root / "strategy_config.json"),
            TG_CHANNEL_ENTITY_CACHE_PATH=str(self.root / "channel_cache.json"),
            SOCIAL_EVIDENCE_COHORT_ID="fixture",
            SOCIAL_EVIDENCE_TEST_SCAN_SECONDS="2",
            SOCIAL_EVIDENCE_INTERVAL_SECONDS="2",
            SOCIAL_EVIDENCE_PYTHON=self.python,
            SOCIAL_EVIDENCE_PROVIDER_SCRIPT=str(HERE / "adapters/social_fixture.py"),
            ADAPTIVE_STORAGE_BACKEND="postgres",
            ADAPTIVE_CHART_BACKEND="postgres",
            MARKET_EVIDENCE_PROVIDER_MODE="BIRDEYE_LIVE",
            MARKET_EVIDENCE_PROVIDER_AUTHORITY_GENERATION="1",
            MARKET_EVIDENCE_LIFECYCLE_AUTHORITY_GENERATION="1",
            MARKET_EVIDENCE_HUMAN_LIVE_ACTIVATION="true",
            MARKET_EVIDENCE_TRACKED_MINT_OPERATING_LIMIT="20",
            MARKET_EVIDENCE_COMPLETED_MINUTE_CONCURRENCY="4",
            MARKET_EVIDENCE_ACQUISITION_DEADLINE_SECONDS="30",
            MARKET_EVIDENCE_ACCOUNT_RPS="100",
            MARKET_EVIDENCE_ACCOUNT_CU_PER_MINUTE="50000",
            MARKET_EVIDENCE_SAFETY_RESERVE_BPS="1000",
            MARKET_EVIDENCE_SERVICE_TIME_MS="100",
            MARKET_EVIDENCE_LIVE_MAX_CONCURRENT_REQUESTS="5",
            MARKET_EVIDENCE_LIVE_MAX_REQUESTS_PER_SECOND="50",
            MARKET_EVIDENCE_LIVE_MAX_CU_PER_SECOND="1000",
            MARKET_EVIDENCE_SIGNAL_PIPELINE_ENABLED="true",
            MARKET_EVIDENCE_SIGNAL_PROVIDER_AUTHORITY_ENABLED="true",
            MARKET_EVIDENCE_ENRICHMENT_PROVIDER_AUTHORITY_ENABLED="true",
            MARKET_EVIDENCE_FAST_MARKET_PROVIDER_AUTHORITY_ENABLED="true",
            RUST_FAST_MARK_ADMISSION_ENABLED="true",
            RUST_ENRICHMENT_ADMISSION_ENABLED="true",
            MARKET_EVIDENCE_ADMISSION_MODE="LEXICAL_V1",
            MARKET_EVIDENCE_GLOBAL_ADMISSION_MODE="OFF",
            MARKET_EVIDENCE_ACTIVATION_CONTEXT="true",
            POSTGRES_HOST="127.0.0.1",
            POSTGRES_PORT=str(self.ports["postgres"]),
            POSTGRES_USER="market",
            POSTGRES_DB="market_simulation",
            POSTGRES_PASSWORD="local",
            POSTGRES_SSLMODE="verify-full",
            POSTGRES_SSLROOTCERT=str(self.root / "tls/ca.crt"),
            PGSSLMODE="verify-full",
            PGSSLROOTCERT=str(self.root / "tls/ca.crt"),
        )
        if "activation" in self.state:
            self.env["MARKET_EVIDENCE_PROVIDER_ACTIVATION_BOUNDARY"] = self.state[
                "activation"
            ]
        for name in ["signal", "market", "strategy", "social", "research"]:
            self.env[
                name.upper() + "_SERVICE_DSN"
                if name in ["signal", "strategy", "research"]
                else name.upper() + "_EVIDENCE_DSN"
            ] = self.dsn(name)

    def dsn(self, name):
        return (
            f"postgresql://{name}@127.0.0.1:{self.ports['postgres']}/{name}_simulation"
        )

    def save(self):
        self.state["ports"] = self.ports
        self.path.write_text(json.dumps(self.state, indent=2) + "\n")
        self.path.chmod(0o600)

    def command(self, cmd, env=None, check=True):
        with (self.root / "setup.log").open("a") as out:
            result = subprocess.run(
                list(map(str, cmd)),
                cwd=self.root,
                env={**self.env, **(env or {})},
                stdout=out,
                stderr=subprocess.STDOUT,
            )
        if check and result.returncode:
            raise RuntimeError(
                f"command failed ({result.returncode}): {cmd}; see {self.root}/setup.log"
            )
        return result

    def start(self, name, cmd, env=None):
        if name in self.state["processes"]:
            raise RuntimeError(f"{name} already registered; stop before restarting")
        with (self.root / f"{name}.log").open("a") as out:
            p = subprocess.Popen(
                list(map(str, cmd)),
                cwd=self.root,
                env={**self.env, **(env or {})},
                stdout=out,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
        self.state.setdefault("commands", {})[name] = list(map(str, cmd))
        self.state["processes"][name] = {
            "pid": p.pid,
            "command": list(map(str, cmd)),
            "started": subprocess.run(
                ["ps", "-p", str(p.pid), "-o", "lstart="],
                capture_output=True,
                text=True,
            ).stdout.strip(),
        }
        self.save()
        return p

    def build(self):
        for repo, extra in [
            (HERE, []),
            (self.market, ["-p", "market-evidence-service", "--bins"]),
            (self.social, []),
            (self.transaction, ["--features", "runtime"]),
        ]:
            self.command(
                ["cargo", "build", "--manifest-path", repo / "Cargo.toml", *extra]
            )

    def check(self):
        """Check prerequisites without starting processes or reading credentials."""
        failures = []
        paths = {
            "Python": Path(self.python),
            "PostgreSQL initdb": self.pg / "initdb",
            "PostgreSQL pg_ctl": self.pg / "pg_ctl",
            "PostgreSQL psql": self.pg / "psql",
            "Market manifest": self.market / "Cargo.toml",
            "Social manifest": self.social / "Cargo.toml",
            "Transaction manifest": self.transaction / "Cargo.toml",
            "Mock manifest": HERE / "Cargo.toml",
            "frozen model": Path(self.args.model or self.project / "reports/rolling_lgbm_adaptive_v1/run_full/lifecycle_h120m/model.pkl"),
        }
        if self.args.no_build:
            paths.update({
                "mock binary": HERE / "target/debug/provider-mock-service",
                "Market binary": self.market / "target/debug/market-evidence-service",
                "Market initializer": self.market / "target/debug/simulation-init",
                "Social binary": self.social / "target/debug/social-evidence-service",
                "Transaction binary": self.transaction / "target/debug/transaction-service",
            })
        for label, path in paths.items():
            ok = path.is_file()
            print(f"{'OK' if ok else 'FAIL'} {label}: {path}")
            if not ok:
                failures.append(label)
        for tool in ["cargo", "openssl"]:
            ok = shutil.which(tool, path=self.env.get("PATH")) is not None
            print(f"{'OK' if ok else 'FAIL'} {tool}")
            if not ok:
                failures.append(tool)
        if Path(self.python).is_file():
            result = subprocess.run(
                [self.python, "-c", "import psycopg, telethon, lightgbm; from google.protobuf import runtime_version; runtime_version.ValidateProtobufRuntimeVersion(runtime_version.Domain.PUBLIC, 6, 31, 1, '', 'local-preflight'); import signal_service, strategy_service, research_service"],
                cwd="/tmp", env=self.env, capture_output=True, text=True,
            )
            print(f"{'OK' if result.returncode == 0 else 'FAIL'} Python service dependencies")
            if result.returncode:
                print(result.stderr)
                failures.append("Python dependencies")
        if len(set(self.ports.values())) != len(self.ports):
            failures.append("duplicate local ports")
        if not self.path.exists():
            for name, port in self.ports.items():
                try:
                    with socket.socket() as sock:
                        sock.bind(("127.0.0.1", port))
                    print(f"OK {name} port {port}")
                except (OSError, OverflowError) as exc:
                    print(f"FAIL {name} port {port}: {exc}")
                    failures.append(name + " port")
        if failures:
            raise RuntimeError("local prerequisites failed: " + ", ".join(failures))
        print("Local mock prerequisites ready; builds and runtime scenarios are separate checks.")

    def sql(self, name, query, params=()):
        import psycopg

        with psycopg.connect(
            self.dsn(name),
            sslmode="verify-full",
            sslrootcert=str(self.root / "tls/ca.crt"),
        ) as conn:
            cur = conn.execute(query, params)
            return cur.fetchall() if cur.description else []

    def pg_up(self):
        tls = self.root / "tls"
        tls.mkdir()
        data = self.root / "pg"
        self.command(
            [
                "openssl",
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                "2",
                "-subj",
                "/CN=simulation-ca",
                "-keyout",
                tls / "ca.key",
                "-out",
                tls / "ca.crt",
            ]
        )
        self.command(
            [
                "openssl",
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-subj",
                "/CN=127.0.0.1",
                "-keyout",
                tls / "server.key",
                "-out",
                tls / "server.csr",
            ]
        )
        (tls / "san.ext").write_text("subjectAltName=IP:127.0.0.1,DNS:localhost\n")
        self.command(
            [
                "openssl",
                "x509",
                "-req",
                "-in",
                tls / "server.csr",
                "-CA",
                tls / "ca.crt",
                "-CAkey",
                tls / "ca.key",
                "-CAcreateserial",
                "-days",
                "2",
                "-extfile",
                tls / "san.ext",
                "-out",
                tls / "server.crt",
            ]
        )
        (tls / "server.key").chmod(0o600)
        self.command([self.pg / "initdb", "-D", data, "-A", "trust", "-U", "postgres"])
        with (data / "postgresql.conf").open("a") as f:
            f.write(
                f"\nport={self.ports['postgres']}\nlisten_addresses='127.0.0.1'\nunix_socket_directories='{self.root}'\nssl=on\nssl_cert_file='{tls}/server.crt'\nssl_key_file='{tls}/server.key'\nmax_connections=150\n"
            )
        self.command(
            [
                self.pg / "pg_ctl",
                "-D",
                data,
                "-l",
                self.root / "postgres.log",
                "-w",
                "start",
            ]
        )
        self.state["postgres_started"] = True
        self.save()
        import psycopg

        with psycopg.connect(
            f"host=127.0.0.1 port={self.ports['postgres']} dbname=postgres user=postgres",
            autocommit=True,
        ) as conn:
            for name in ["signal", "market", "strategy", "social", "transaction", "research"]:
                conn.execute(f"CREATE ROLE {name} LOGIN")
                conn.execute(f"CREATE DATABASE {name}_simulation OWNER {name}")

    def up(self):
        if self.path.exists():
            raise RuntimeError(
                "run directory already initialized; use a fresh directory"
            )
        if self.root.exists() and any(self.root.iterdir()):
            raise RuntimeError("run directory must be empty")
        self.check()
        self.root.mkdir(parents=True, exist_ok=True)
        self.root.chmod(0o700)
        for port in self.ports.values():
            with socket.socket() as sock:
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                sock.bind(("127.0.0.1", port))
        self.state["paths"] = {
            name: str(getattr(self, name))
            for name in ["project", "market", "social", "transaction", "python", "pg"]
        }
        self.state["activation"] = stamp()
        self.env["MARKET_EVIDENCE_PROVIDER_ACTIVATION_BOUNDARY"] = self.state[
            "activation"
        ]
        self.save()
        try:
            if not self.args.no_build:
                self.build()
            self.pg_up()
            self.command([self.python, "-m", "signal_service.schema", "migrate"])
            self.command([self.python, "-m", "strategy_service.schema", "migrate"])
            self.command([self.python, "-m", "research_service.schema", "migrate"])
            self.command([self.python, "-m", "research_service.schema", "check"])
            self.command([self.market / "target/debug/simulation-init"])
            self.sql(
                "signal",
                "INSERT INTO signal.signal_storage_authority(authority_key,generation,backend,activated_at) VALUES('signal-storage',1,'POSTGRES_PRIMARY',%s) ON CONFLICT(authority_key) DO UPDATE SET backend='POSTGRES_PRIMARY',activated_at=excluded.activated_at",
                (self.state["activation"],),
            )
            self.sql(
                "strategy",
                "INSERT INTO strategy.adaptive_storage_authority(authority_key,backend,authority_generation,activated_at) VALUES('adaptive-storage','POSTGRES_PRIMARY',1,%s) ON CONFLICT DO NOTHING",
                (self.state["activation"],),
            )
            self.sql(
                "strategy",
                "INSERT INTO strategy.adaptive_input_authority(authority_key,backend,authority_generation,activated_at,snapshot_cutoff,candle_cutoff) VALUES('adaptive-input','POSTGRES_PRIMARY',1,%s,0,0) ON CONFLICT DO NOTHING",
                (self.state["activation"],),
            )
            for hours in [8, 24]:
                self.sql(
                    "strategy",
                    "INSERT INTO strategy.adaptive_decision_checkpoints(worker_id,legacy_snapshot_seq,handoff_available_at) VALUES(%s,0,%s)",
                    (
                        f"adaptive_2x_{hours}h_release_gate_v1_scheduler_v2_20260819_005",
                        self.state["activation"],
                    ),
                )
            config = {
                "direct_callouts": {
                    "enabled": True,
                    "channels": [
                        {
                            "name": "FixtureCalls",
                            "source_channel": "kingdom_direct_callout",
                            "dedupe_mints": True,
                        }
                    ],
                }
            }
            (self.root / "strategy_config.json").write_text(json.dumps(config))
            model = Path(
                self.args.model
                or self.project
                / "reports/rolling_lgbm_adaptive_v1/run_full/lifecycle_h120m/model.pkl"
            )
            shutil.copy2(model, self.root / "model.pkl")
            self.state["model_sha256"] = hashlib.sha256(
                (self.root / "model.pkl").read_bytes()
            ).hexdigest()
            self.save()
            self.start("mock", [HERE / "target/debug/provider-mock-service"])
            wait(lambda: http(self.urls["mock"] + "/health"))
            http(
                self.urls["mock"] + "/__mock/scenario",
                {
                    "telegram": {
                        "channels": [
                            {"id": 1001, "username": "FixtureCalls", "access_hash": 1}
                        ]
                    }
                },
            )
            self.start(
                "inbox",
                [
                    self.python,
                    "-m",
                    "strategy_service.market_event_inbox",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    str(self.ports["strategy"]),
                ],
            )
            wait(lambda: http(self.urls["strategy"] + "/health").get("ready"))
            self.start(
                "proxy",
                [
                    self.python,
                    HERE / "adapters/delivery_proxy.py",
                    "--port",
                    str(self.ports["proxy"]),
                    "--market-port",
                    str(self.ports["market"]),
                    "--strategy-port",
                    str(self.ports["strategy"]),
                ],
            )
            wait(lambda: http(self.urls["proxy"] + "/health").get("ready"))
            self.start("market", [self.market / "target/debug/market-evidence-service"])
            wait(
                lambda: http(self.urls["market"] + "/health").get("ready"),
                120,
                "Market readiness",
            )
            for name, module in [
                ("rolling", "rolling_decision_worker"),
                ("references", "execution_reference_worker"),
                ("tracking", "tracking_intent_dispatch"),
            ]:
                self.start(name, [self.python, "-m", "strategy_service." + module])
            self.start(
                "frozen",
                [
                    self.python,
                    "-m",
                    "strategy_service.frozen_lgbm_paper_runtime",
                    "--dsn",
                    self.dsn("strategy"),
                    "--model-artifact",
                    self.root / "model.pkl",
                    "--activation",
                    self.state["activation"],
                ],
            )
            for hours in [8, 24]:
                self.start(
                    f"adaptive{hours}",
                    [
                        self.python,
                        "-m",
                        "strategy_service.adaptive_runtime",
                        "--hours",
                        str(hours),
                        "--model",
                        self.root / "model.pkl",
                    ],
                )
            wait(
                lambda: http(self.urls["strategy"] + "/health").get("pipeline_ready"),
                60,
                "Strategy worker readiness",
            )
            self.start("signal", [self.python, "-m", "signal_service", "telegram"])
            wait(
                lambda: self.sql(
                    "signal",
                    "SELECT 1 FROM signal.listener_health WHERE status='connected'",
                ),
                60,
                "Signal connection",
            )
            self.command(
                [self.social / "target/debug/social-evidence-service", "migrate"]
            )
            channels = [
                {
                    "external_source_id": "1001",
                    "telegram_id": 1001,
                    "access_hash": 1,
                    "username": "FixtureCalls",
                    "channel_ref": "FixtureCalls",
                    "display_name": "FixtureCalls",
                    "status": "known",
                    "access_status": "accessible",
                    "service_state": "ACTIVE",
                }
            ]
            (self.root / "social_channels.json").write_text(json.dumps(channels))
            self.command(
                [
                    self.social / "target/debug/social-evidence-service",
                    "fixture-init",
                    self.root / "social_channels.json",
                ]
            )
            self.command(
                [self.social / "target/debug/social-evidence-service", "run-known", "1"]
            )
            self.start(
                "social",
                [self.social / "target/debug/social-evidence-service", "run", "1"],
            )
            self.transaction_up()
            print("Simulation ready: " + str(self.root), flush=True)
        except BaseException:
            self.down()
            raise

    def transaction_up(self):
        self.start(
            "mock_transaction",
            [HERE / "target/debug/provider-mock-service"],
            {"MOCK_BIND": f"127.0.0.1:{self.ports['mock_transaction']}"},
        )
        wait(lambda: http(self.urls["mock_transaction"] + "/health"))
        wallet = http(self.urls["mock_transaction"] + "/__mock/keypair", {})
        env = {
            "APP_CONFIG_DIR": str(self.transaction / "config"),
            "TRANSACTION_MODE": "live",
            "TRANSACTION_HTTP_BIND": f"127.0.0.1:{self.ports['transaction']}",
            "TRANSACTION_NATS_ENABLED": "false",
            # Match Transaction's existing local-stack.sh cleanup cadence.
            "TRANSACTION_TOKEN_ACCOUNT_SWEEP_SECONDS": "20",
            "POSTGRES_USER": "transaction",
            "POSTGRES_DB": "transaction_simulation",
            "TRANSACTION_SOLANA_RPC_URLS": self.urls["mock_transaction"]
            + "/solana-rpc",
            "TRANSACTION_JUPITER_BASE_URL": self.urls["mock_transaction"] + "/jupiter",
            "TRANSACTION_JITO_URL": self.urls["mock_transaction"] + "/jito",
            "TRANSACTION_WALLET_ID": str(uuid.uuid4()),
            "TRANSACTION_WALLET_PUBKEY_BASE58": wallet["pubkey"],
            "TRANSACTION_SIGNER_KEYPAIR_BASE58": wallet["secret_base58"],
            "TRANSACTION_SIGNED_PAYLOAD_KEY_BASE64": base64.b64encode(
                secrets.token_bytes(32)
            ).decode(),
            "JUPITER_API_KEY": "fixture",
            "TRANSACTION_SOLANA_NETWORK": "localnet",
            "POSTGRES_PASSWORD": "local",
        }
        (self.root / "service.env").write_text(
            "\n".join(f"{k}={v}" for k, v in env.items()) + "\n"
        )
        (self.root / "service.env").chmod(0o600)
        self.start(
            "transaction", [self.transaction / "target/debug/transaction-service"], env
        )
        wait(
            lambda: http(self.urls["transaction"] + "/readyz").get("status") == "ready",
            60,
            "Transaction readiness",
        )

    def stop(self, name):
        item = self.state["processes"].get(name)
        if not item:
            return
        pid = item["pid"]
        # Verify command identity before signalling a persisted PID.
        cmd = subprocess.run(
            ["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True
        ).stdout.strip()
        started = subprocess.run(
            ["ps", "-p", str(pid), "-o", "lstart="], capture_output=True, text=True
        ).stdout.strip()
        identity_args = (
            item["command"][1:] if len(item["command"]) > 1 else item["command"]
        )
        stat = subprocess.run(
            ["ps", "-p", str(pid), "-o", "stat="], capture_output=True, text=True
        ).stdout.strip()
        if stat.startswith("Z"):
            cmd = ""
        if (
            cmd
            and all(part in cmd for part in identity_args)
            and (not item.get("started") or started == item["started"])
        ):
            try:
                os.killpg(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            for _ in range(100):
                if (
                    not subprocess.run(
                        ["ps", "-p", str(pid), "-o", "stat="],
                        capture_output=True,
                        text=True,
                    )
                    .stdout.strip()
                    .strip("Z")
                ):
                    break
                time.sleep(0.05)
        elif cmd:
            # The owner may have exited between ps reads, or the PID was reused.
            # Forget the stale registration; never signal the replacement.
            print(f"Skipped stale process registration: {name} pid={pid}", flush=True)
        self.state["processes"].pop(name, None)
        self.save()

    def down(self):
        for name in reversed(list(self.state["processes"])):
            self.stop(name)
        if self.state.get("postgres_started"):
            self.command(
                [
                    self.pg / "pg_ctl",
                    "-D",
                    self.root / "pg",
                    "-m",
                    "fast",
                    "-w",
                    "stop",
                ],
                check=False,
            )
            self.state["postgres_started"] = False
            self.save()

    def status(self):
        failures = []
        for name, url in self.urls.items():
            try:
                result = http(url + ("/readyz" if name == "transaction" else "/health"))
                print(name, result)
                ready = (result.get("status") == "ready" if name == "transaction"
                         else result.get("status") == "ok" if name.startswith("mock")
                         else result.get("pipeline_ready") if name == "strategy"
                         else result.get("ready"))
                if not ready:
                    failures.append(name)
            except Exception as exc:
                print(name, type(exc).__name__)
                failures.append(name)
        for name, item in self.state["processes"].items():
            actual = subprocess.run(["ps", "-p", str(item["pid"]), "-o", "command="],
                                    capture_output=True, text=True).stdout.strip()
            started = subprocess.run(["ps", "-p", str(item["pid"]), "-o", "lstart="],
                                     capture_output=True, text=True).stdout.strip()
            if (not actual or not all(part in actual for part in item["command"][1:])
                    or (item.get("started") and item["started"] != started)):
                print("FAIL process identity:", name)
                failures.append(name)
        print("logs:", self.root)
        if not self.path.exists() or failures:
            raise RuntimeError("local stack not ready: " + ", ".join(failures or ["uninitialized"]))

    def run(self):
        if self.state.get("suite_started"):
            raise RuntimeError(
                "scenario suites require a fresh stack; use a new run directory"
            )
        self.state["suite_started"] = stamp()
        self.save()
        from scenarios import run_suite

        run_suite(self, self.args.suite, self.args.extended, self.args.max_hold_soak)


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("command", choices=["check", "up", "status", "run", "down", "destroy"])
    p.add_argument("--profile", choices=["local"], default="local",
                   help="isolated PostgreSQL and mock providers; no production environment")
    project = HERE.parent
    p.add_argument("--run-dir", default="/tmp/crypto-production-simulation")
    p.add_argument("--project", default=str(project))
    p.add_argument("--market", default=str(project.parent / "market-evidence-service"))
    p.add_argument("--social", default=str(project.parent / "social-evidence-service"))
    p.add_argument("--transaction", default=str(project / "transaction-service"))
    p.add_argument("--python")
    p.add_argument("--model")
    p.add_argument("--pg-bin", default="/opt/homebrew/opt/postgresql@18/bin")
    for name, port in [
        ("mock", 18080),
        ("market", 18088),
        ("strategy", 18090),
        ("transaction", 18091),
        ("pg", 55493),
        ("proxy", 18092),
    ]:
        p.add_argument("--" + name + "-port", type=int, default=port)
    p.add_argument("--transaction-mock-port", type=int, default=18081)
    p.add_argument("--no-build", action="store_true")
    p.add_argument(
        "--suite",
        choices=["core", "collectors", "production", "transaction", "all"],
        default="core",
    )
    p.add_argument("--extended", action="store_true")
    p.add_argument(
        "--max-hold-soak",
        action="store_true",
        help="run the real 120-minute maximum-hold window",
    )
    args = p.parse_args()
    stack = Stack(args)
    if args.command == "destroy":
        if not stack.path.exists():
            raise RuntimeError("refusing to destroy unrecognized directory")
        stack.down()
        shutil.rmtree(stack.root)
    else:
        getattr(stack, args.command)()


if __name__ == "__main__":
    main()
