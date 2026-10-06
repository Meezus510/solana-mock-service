"""Domain assertions for real services; fixtures enter only at provider boundaries."""

import hashlib
import json
import random
import time
from datetime import datetime, timedelta, timezone
from urllib.error import HTTPError

from simulate import http, stamp, wait

SOL = "So11111111111111111111111111111111111111112"
MINT = "AUvBbyBxZMx9hqMCcZ4ATQDbjiq6H3phx6en1nNxpump"


def capacity_mint(index):
    """Encode deterministic synthetic keys without a runner-only dependency."""
    raw = hashlib.sha256(f"capacity-fixture-{index}".encode()).digest()
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    number = int.from_bytes(raw, "big")
    encoded = ""
    while number:
        number, remainder = divmod(number, 58)
        encoded = alphabet[remainder] + encoded
    return "1" * (len(raw) - len(raw.lstrip(b"\0"))) + encoded


def token_fixture(at, seed=6, future_minutes=31):
    rng = random.Random(seed)
    base = int(at) // 60 * 60
    price = 1.0
    bars = []
    for minute in range(-300, future_minutes):
        change = rng.gauss(0.004, 0.022)
        opening = price
        price = max(0.01, price * (1 + change))
        bars.append(
            {
                "unix_time": base + minute * 60,
                "o": opening,
                "h": max(opening, price) * 1.02,
                "l": min(opening, price) * 0.98,
                "c": price,
                "v": 1000 * (1 + abs(change) * 20),
                "visible_at": base + (minute + 1) * 60,
            }
        )
    current = next(b["c"] for b in reversed(bars) if b["unix_time"] + 60 <= at)
    return {
        "price": current,
        "candles": bars,
        "overview": {
            "price": current,
            "marketCap": 100000,
            "liquidity": 25000,
            "holder": 500,
            "uniqueWallet5m": 100,
            "vBuy1mUSD": 2000,
            "vSell1mUSD": 1000,
            "vBuy5mUSD": 10000,
            "vSell5mUSD": 5000,
        },
        "security": {"freezeAuthority": None, "mintAuthority": None},
        "holder_profile": {"holderCount": 500, "top10HolderPercent": 15},
        "holder_chart": [
            {"timestamp": base + i * 60, "holder": 500 + i} for i in range(-30, 1)
        ],
        "meme": {
            "price": current,
            "liquidity": 25000,
            "volume_1h_usd": 100000,
            "price_change_1h_percent": 5,
            "price_change_15m_percent": 2,
            "price_change_5m_percent": 1,
        },
        "creation": {
            "tokenAddress": MINT,
            "blockUnixTime": base - 86400,
            "owner": SOL,
            "txHash": "fixture-creation",
        },
    }


class Suite:
    def __init__(self, stack):
        self.stack = stack
        self.results = []
        self.trace = {}
        self.mock = stack.urls["mock"]
        self.config = http(self.mock + "/__mock/scenario")

    def check(self, label, fn):
        started = stamp()
        try:
            detail = fn()
            self.results.append(
                {
                    "name": label,
                    "status": "PASS",
                    "started_at": started,
                    "detail": detail,
                }
            )
            print("PASS", label, flush=True)
        except Exception as exc:
            self.results.append(
                {
                    "name": label,
                    "status": "FAIL",
                    "started_at": started,
                    "error": str(exc),
                }
            )
            print("FAIL", label, str(exc), flush=True)

    def scalar(self, owner, query, params=()):
        return self.stack.sql(owner, query, params)[0][0]

    def scenario(self):
        http(self.mock + "/__mock/scenario", self.config)

    def fault(self, provider, key, body=None, status=200, delay_ms=0, malformed=False):
        logs = http(self.mock + "/__mock/requests")
        call = sum(r["provider_request"] == key for r in logs) + 1
        self.config[provider].setdefault("faults", {})[key] = [
            {
                "on_call": call,
                "status": status,
                "body": body,
                "delay_ms": delay_ms,
                "malformed": malformed,
            }
        ]
        self.scenario()
        return call

    def message(self, message_id, mint=MINT, text=None, age=0):
        head = http(self.mock + "/__mock/telegram/head", {"channel": "FixtureCalls"})[
            "head_message_id"
        ]
        message_id = max(message_id, head + 1)
        row = {
            "channel": "FixtureCalls",
            "id": message_id,
            "event_ts": datetime.fromtimestamp(
                time.time() - age, timezone.utc
            ).isoformat(),
            "text": text or f"$TEST Solana CA: {mint}",
            "author_id": "7",
            "author_kind": "USER",
            "visible_at": time.time(),
        }
        http(self.mock + "/__mock/telegram/messages", row)
        return row

    def setup(self):
        if "proxy" in self.stack.state["processes"]:
            for path in [
                "/market/v1/normalized-signals",
                "/strategy/v1/market-events",
                "/strategy/v1/production-market-events",
            ]:
                http(
                    self.stack.urls["proxy"] + "/__proxy/drop-ack",
                    {"path": path, "count": 1},
                )
        fixture = token_fixture(time.time())
        sol_fixture = json.loads(json.dumps(fixture))
        scale = 150 / sol_fixture["price"]
        for bar in sol_fixture["candles"]:
            for field in ["o", "h", "l", "c"]:
                bar[field] *= scale
        sol_fixture["price"] = 150
        sol_fixture["overview"]["price"] = 150
        sol_fixture["meme"]["price"] = 150
        sol_fixture["creation"]["tokenAddress"] = SOL
        self.config["birdeye"] = {
            "tokens": {
                MINT: fixture,
                SOL: sol_fixture,
            },
            "faults": {},
            "responses": {},
        }
        # Keep execution economics coherent with mock prices and token decimals.
        self.config["tokens"][MINT] = {
            "decimals": 6,
            "usd_price": fixture["price"],
            "tokens_per_lamport": 150 / (1000 * fixture["price"]),
        }
        self.scenario()
        self.stack.command(
            [
                self.stack.social / "target/debug/social-evidence-service",
                "track-mint",
                MINT,
            ]
        )
        wait(
            lambda: any(
                r["provider_request"].startswith("updates")
                for r in http(self.mock + "/__mock/requests")
            ),
            label="Signal polling",
        )

    def healthy(self):
        self.setup()
        id = int(time.time())
        row = self.message(id)
        self.trace["message"] = row
        wait(
            lambda: (
                self.scalar(
                    "signal",
                    "SELECT count(*) FROM signal.normalized_signals WHERE mint=%s",
                    (MINT,),
                )
                == 1
            ),
            60,
            "normalized signal",
        )
        wait(
            lambda: (
                self.scalar(
                    "market",
                    "SELECT count(*) FROM market_state.signal_decision_snapshots WHERE mint=%s",
                    (MINT,),
                )
                == 1
            ),
            60,
            "Market snapshot",
        )
        wait(
            lambda: (
                self.scalar(
                    "strategy",
                    "SELECT count(*) FROM strategy_integration.market_event_read_model WHERE subject='market.decision-snapshot.v1'",
                )
                >= 1
            ),
            60,
            "Strategy snapshot inbox",
        )
        wait(
            lambda: (
                self.scalar(
                    "market",
                    "SELECT count(*) FROM market_state.signal_enrichment_jobs WHERE mint=%s AND state='COMPLETE'",
                    (MINT,),
                )
                >= 1
            ),
            90,
            "enrichment completion",
        )
        wait(
            lambda: (
                self.scalar(
                    "social",
                    "SELECT count(*) FROM social_evidence.social_message_token_mentions WHERE mint=%s",
                    (MINT,),
                )
                >= 1
            ),
            30,
            "social mint match",
        )
        wait(
            lambda: (
                self.scalar(
                    "market",
                    "SELECT count(*) FROM market_state.lifecycle_admissions WHERE mint=%s",
                    (MINT,),
                )
                == 1
            ),
            60,
            "adaptive lifecycle admission",
        )
        wait(
            lambda: (
                self.scalar(
                    "market",
                    "SELECT count(*) FROM market_state.birdeye_cu_reservation WHERE dispatch_at IS NOT NULL AND completed_at IS NOT NULL",
                )
                > 0
            ),
            30,
            "provider CU accounting",
        )
        id = row["id"]
        self.trace["signal"] = self.stack.sql(
            "signal",
            "SELECT signal_id,payload_hash FROM signal.normalized_signals WHERE mint=%s",
            (MINT,),
        )
        self.trace["lifecycle"] = self.stack.sql(
            "market",
            "SELECT lifecycle_id,mint FROM market_state.lifecycle_admissions WHERE mint=%s",
            (MINT,),
        )
        return {
            "mint": MINT,
            "message_id": id,
            "snapshot": self.stack.sql(
                "market",
                "SELECT signal_id,status FROM market_state.signal_decision_snapshots WHERE mint=%s",
                (MINT,),
            ),
        }

    def replay(self):
        row = self.trace["message"]
        http(self.mock + "/__mock/telegram/messages", row)
        self.stack.stop("signal")
        self.stack.start(
            "signal", [self.stack.python, "-m", "signal_service", "telegram"]
        )
        wait(
            lambda: (
                self.scalar(
                    "signal",
                    "SELECT count(*) FROM signal.listener_health WHERE status='connected'",
                )
                > 0
            ),
            label="reconnected listener",
        )
        assert (
            self.scalar(
                "signal",
                "SELECT count(*) FROM signal.normalized_signals WHERE mint=%s",
                (MINT,),
            )
            == 1
        )
        assert (
            self.scalar(
                "market",
                "SELECT count(*) FROM market_state.signal_decision_snapshots WHERE mint=%s",
                (MINT,),
            )
            == 1
        )
        return {"normalized_signals": 1, "snapshots": 1}

    def social(self):
        row = self.message(
            int(time.time()) + 10, text="A channel update without an address"
        )
        wait(
            lambda: (
                self.scalar(
                    "social",
                    "SELECT count(*) FROM social_evidence.social_message_events WHERE external_message_id=%s",
                    (str(row["id"]),),
                )
                == 1
            ),
            30,
            "no-match canonical evidence",
        )
        assert (
            self.scalar(
                "social",
                "SELECT count(*) FROM social_evidence.social_message_token_mentions m JOIN social_evidence.social_message_events e USING(message_event_id) WHERE e.external_message_id=%s",
                (str(row["id"]),),
            )
            == 0
        )
        return {"canonical_message_id": row["id"], "matched_mints": 0}

    def provider_contracts(self):
        paths = [
            "/defi/price?address=" + MINT,
            "/defi/ohlcv?address=" + MINT,
            "/defi/v3/ohlcv?address=" + MINT,
            "/defi/token_overview?address=" + MINT,
            "/defi/token_security?address=" + MINT,
            "/token/v1/holder-profile?token_address=" + MINT,
            "/token/v1/holder/chart?token_address=" + MINT,
            "/defi/v3/token/meme/list",
            "/defi/token_creation_info?address=" + MINT,
        ]
        for p in paths:
            assert http(self.mock + p)["success"]
        return {"endpoints": len(paths)}

    def social_fault(self):
        call = self.fault(
            "telegram",
            "page:social",
            {
                "status": "FLOOD_WAIT",
                "messages": [],
                "channel_exhausted": False,
                "flood_wait_seconds": 1,
                "error_class": "FloodWaitError",
            },
        )
        wait(
            lambda: any(
                r["provider_request"] == "page:social"
                and r["call"] == call
                and r["fault"]
                for r in http(self.mock + "/__mock/requests")
            ),
            30,
            "Social flood wait injection",
        )
        wait(
            lambda: (
                self.scalar(
                    "social",
                    "SELECT count(*) FROM social_evidence.acquisition_attempts WHERE stop_reason='flood_wait'",
                )
                > 0
            ),
            30,
            "Social flood wait persistence",
        )
        self.config["telegram"]["faults"] = {}
        self.scenario()
        return {"fault_call": call}

    def minute_and_inference(self):
        ready = getattr(getattr(self.stack, 'args', None), 'entry_evidence_ready', False)
        wait(
            lambda: (
                self.scalar(
                    "market",
                    "SELECT count(*) FROM market_evidence.candle_versions WHERE mint=%s",
                    (MINT,),
                )
                > 0
            ),
            100,
            "completed minute",
        )
        wait(
            lambda: (
                self.scalar(
                    "strategy",
                    "SELECT count(*) FROM paper.lgbm_inferences WHERE mint=%s",
                    (MINT,),
                )
                > 0
            ),
            100,
            "frozen inference",
        )
        rows = self.stack.sql(
            "strategy",
            "SELECT decision_id,decision,rejection_reason,after_cost_ev FROM paper.lgbm_inferences WHERE mint=%s",
            (MINT,),
        )
        self.trace["inferences"] = rows
        assert all(
            r[1] in ["REJECT", "ACCEPTED_WAITING_FOR_ENTRY_REFERENCE"] for r in rows
        )
        eligible = None
        if ready:
            started = time.monotonic()
            eligible = wait(
                lambda: self.stack.sql(
                    "strategy",
                    "SELECT d.decision_id FROM strategy.rolling_decision d JOIN strategy.frozen_feature_snapshots f USING(decision_id) JOIN paper.lgbm_inferences i USING(decision_id) WHERE d.mint=%s AND f.status='COMPLETE' AND i.decision='ACCEPTED_WAITING_FOR_ENTRY_REFERENCE' ORDER BY d.decision_at LIMIT 1",
                    (MINT,),
                ),
                240,
                "accepted PIT-safe model decision readiness",
            )[0][0]
            self.trace['acceptance_readiness'] = {'decision_id': eligible, 'evidence_warmup_seconds': time.monotonic()-started, 'entry_deadline_seconds': 70}
        wait(
            lambda: (
                self.scalar(
                    "strategy",
                    "SELECT count(*) FROM paper.positions p JOIN paper.lgbm_inferences i USING(inference_id) WHERE p.mint=%s AND (%s::text IS NULL OR i.decision_id=%s)",
                    (MINT, eligible, eligible),
                )
                >= 1
            ),
            70,
            "accepted model paper entry",
        )
        self.trace["positions"] = self.stack.sql(
            "strategy",
            "SELECT paper_position_id,entry_price,entry_at FROM paper.positions WHERE mint=%s",
            (MINT,),
        )
        return rows

    def rejected_cold(self):
        """Retained diagnostic: original 100-second cold-start expectation."""
        return self.rejected(evidence_ready=False)

    def rejected(self, *, evidence_ready=True):
        mint = "BUvBbyBxZMx9hqMCcZ4ATQDbjiq6H3phx6en1nNxpump"
        fixture = token_fixture(time.time(), 4)
        fixture["creation"]["tokenAddress"] = mint
        self.config["birdeye"]["tokens"][mint] = fixture
        self.scenario()
        started = time.monotonic()
        self.message(int(time.time()) + 1, mint=mint)
        eligible = None
        if evidence_ready:
            # History admission is separately cadence-limited. Do not assert a
            # model result before its PIT-safe feature precondition exists.
            eligible = wait(
                lambda: self.stack.sql(
                    "strategy",
                    "SELECT d.decision_id,d.decision_at,d.observation_minute FROM strategy.rolling_decision d JOIN strategy.frozen_feature_snapshots f USING(decision_id) WHERE d.mint=%s AND f.status='COMPLETE' ORDER BY d.decision_at LIMIT 1",
                    (mint,),
                ),
                240,
                "PIT-safe rejected-fixture evidence readiness (warmup)",
            )[0]
        ready_elapsed = time.monotonic() - started
        check_started = time.monotonic()
        wait(
            lambda: (
                self.scalar(
                    "strategy",
                    "SELECT count(*) FROM paper.lgbm_inferences WHERE mint=%s AND decision='REJECT' AND rejection_reason='BELOW_FROZEN_EV_GATE' AND (%s::text IS NULL OR decision_id=%s)",
                    (mint, eligible[0] if eligible else None, eligible[0] if eligible else None),
                )
                >= 1
            ),
            100,
            "frozen model rejection",
        )
        assert (
            self.scalar(
                "strategy",
                "SELECT count(*) FROM paper.positions WHERE mint=%s",
                (mint,),
            )
            == 0
        )
        detail = {"mint": mint, "reason": "BELOW_FROZEN_EV_GATE",
                  "eligible_decision": eligible,
                  "evidence_warmup_seconds": ready_elapsed,
                  "model_check_seconds": time.monotonic() - check_started,
                  "model_check_deadline_seconds": 100,
                  "cold_start_100s_exceeded": ready_elapsed > 100}
        self.trace["rejection_readiness"] = detail
        return detail

    def paper_exit(self):
        position = self.stack.sql(
            "strategy",
            "SELECT paper_position_id,entry_price,entry_at FROM paper.positions WHERE mint=%s AND state='OPEN' ORDER BY entry_at LIMIT 1",
            (MINT,),
        )[0]
        _, price, entry_at = position
        price = float(price)
        start = int(entry_at.timestamp()) // 60 * 60 + 60
        for b in self.config["birdeye"]["tokens"][MINT]["candles"]:
            if b["unix_time"] >= start:
                b.update(o=price, h=price * 2.1, l=price * 0.95, c=price * 2.05)
        self.scenario()
        wait(
            lambda: (
                self.scalar(
                    "strategy",
                    "SELECT count(*) FROM paper.positions WHERE paper_position_id=%s AND state='TP_CLOSED'",
                    (position[0],),
                )
                == 1
            ),
            160,
            "paper TP exit",
        )
        row = self.stack.sql(
            "strategy",
            "SELECT state,gross_pnl,net_pnl FROM paper.positions WHERE paper_position_id=%s",
            (position[0],),
        )[0]
        assert float(row[1]) > 0 and float(row[2]) > 0
        return row

    def delivery_ambiguity(self):
        proxy = self.stack.urls["proxy"]
        paths = [
            "/market/v1/normalized-signals",
            "/strategy/v1/market-events",
            "/strategy/v1/production-market-events",
        ]

        def recovered():
            records = http(proxy + "/__proxy/requests")
            for path in paths:
                dropped = next(
                    (r for r in records if r["path"] == path and r["drop_ack"]), None
                )
                if dropped is None:
                    return False
                if not any(
                    r["path"] == path
                    and r["request"] == dropped["request"]
                    and not r["drop_ack"]
                    and 200 <= r["upstream_status"] < 300
                    for r in records
                ):
                    return False
            return records

        records = wait(recovered, 60, "commit then lost-ACK recovery")
        for path in paths:
            first = next(r for r in records if r["path"] == path and r["drop_ack"])
            body = json.loads(first["request"])
            if path.endswith("production-market-events"):
                payload = json.loads(body["payload_json"])
                payload["mint"] = "conflicting-mint"
                body["payload_json"] = json.dumps(payload)
                body["payload_hash"] = hashlib.sha256(
                    body["payload_json"].encode()
                ).hexdigest()
            elif path.endswith("market-events"):
                body["payload"]["mint"] = "conflicting-mint"
                raw = json.dumps(body["payload"], sort_keys=True, separators=(",", ":"))
                body["payload_hash"] = hashlib.sha256(raw.encode()).hexdigest()
            else:
                body["mint"] = "conflicting-mint"
            try:
                http(proxy + path, body)
            except HTTPError as exc:
                assert exc.code in (400, 409, 422)
            else:
                raise AssertionError("conflicting immutable identity was accepted")
        return {
            "lanes": len(paths),
            "real_commit_before_ack_loss": True,
            "conflicts_rejected": True,
        }

    def pit(self):
        from urllib.parse import urlencode

        before = self.stack.sql(
            "strategy",
            "SELECT decision_id,features,status FROM strategy.frozen_feature_snapshots ORDER BY decision_id",
        )
        natural = self.stack.sql(
            "market",
            "SELECT candle_start,response_received_at FROM market_evidence.candle_versions WHERE mint=%s ORDER BY candle_start LIMIT 1",
            (MINT,),
        )[0]
        start, received = natural
        # Receipt is later than candle completion; query the instant before acquisition.
        q = {
            "mint": MINT,
            "from": int(start.timestamp()),
            "to": int(start.timestamp()) + 60,
            "as_of": int(received.timestamp()) - 1,
            "limit": 100,
        }
        assert not http(self.stack.urls["market"] + "/v1/candles?" + urlencode(q))[
            "candles"
        ]
        for bar in self.config["birdeye"]["tokens"][MINT]["candles"]:
            if bar["unix_time"] <= int(start.timestamp()):
                bar.update(o=999, h=1000, l=998, c=999)
        self.scenario()
        # Provider revision does not alter an immutable completed acquisition or frozen features.
        self.provider_contracts()
        after = self.stack.sql(
            "strategy",
            "SELECT decision_id,features,status FROM strategy.frozen_feature_snapshots WHERE decision_id=ANY(%s) ORDER BY decision_id",
            ([r[0] for r in before],),
        )
        assert after == before
        future = http(self.mock + "/defi/v3/ohlcv?address=" + MINT)["data"]["items"]
        assert all(b["unix_time"] + 60 <= time.time() for b in future)
        return {
            "frozen_snapshots_preserved": len(before),
            "future_open_bars_excluded": True,
            "pre_receipt_query_empty": True,
        }

    def worker_recovery(self):
        # Ask the real worker to start concurrently; its PostgreSQL lease must fence it.
        result = self.stack.command(
            [self.stack.python, "-m", "strategy_service.rolling_decision_worker"],
            check=False,
        )
        assert result.returncode != 0
        for name in [
            "rolling",
            "references",
            "frozen",
            "adaptive8",
            "adaptive24",
            "tracking",
            "social",
        ]:
            command = self.stack.state["processes"][name]["command"]
            self.stack.stop(name)
            self.stack.start(name, command)
        wait(
            lambda: http(self.stack.urls["strategy"] + "/health").get("pipeline_ready"),
            60,
            "worker restart readiness",
        )
        assert (
            self.scalar(
                "strategy",
                "SELECT count(*) FROM (SELECT inference_id FROM paper.positions GROUP BY inference_id HAVING count(*)>1) d",
            )
            == 0
        )
        assert (
            self.scalar(
                "market",
                "SELECT count(*) FROM market_state.lifecycle_admissions WHERE mint=%s",
                (MINT,),
            )
            == 1
        )
        return {
            "restarted_workers": 7,
            "second_writer_excluded": True,
            "duplicate_economic_entries": 0,
        }

    def database_recovery(self):
        names = [
            "market",
            "signal",
            "rolling",
            "references",
            "tracking",
            "frozen",
            "adaptive8",
            "adaptive24",
            "social",
            "transaction",
        ]
        if "lifecycle_admission" in self.stack.state["processes"]:
            names.insert(0, "lifecycle_admission")
        commands = {
            name: self.stack.state["processes"][name]["command"] for name in names
        }
        immutable = self.stack.sql(
            "strategy",
            "SELECT inference_id,decision_id,decision,after_cost_ev FROM paper.lgbm_inferences ORDER BY inference_id",
        )
        # Interrupt actual sessions, including the PostgreSQL authority leases.
        self.stack.command(
            [
                self.stack.pg / "pg_ctl",
                "-D",
                self.stack.root / "pg",
                "-m",
                "fast",
                "-w",
                "stop",
            ]
        )
        try:
            for name in names:
                self.stack.stop(name)
        finally:
            self.stack.command(
                [
                    self.stack.pg / "pg_ctl",
                    "-D",
                    self.stack.root / "pg",
                    "-l",
                    self.stack.root / "postgres.log",
                    "-w",
                    "start",
                ]
            )
        transaction_env = dict(
            line.split("=", 1)
            for line in (self.stack.root / "service.env").read_text().splitlines()
        )
        for name in names:
            self.stack.start(
                name, commands[name], transaction_env if name == "transaction" else None
            )
        wait(
            lambda: http(self.stack.urls["market"] + "/health").get("ready"),
            120,
            "Market DB restart recovery",
        )
        wait(
            lambda: http(self.stack.urls["strategy"] + "/health").get("pipeline_ready"),
            60,
            "Strategy DB restart recovery",
        )
        wait(
            lambda: (
                http(self.stack.urls["transaction"] + "/readyz").get("status")
                == "ready"
            ),
            60,
            "Transaction DB restart recovery",
        )
        preserved = self.stack.sql(
            "strategy",
            "SELECT inference_id,decision_id,decision,after_cost_ev FROM paper.lgbm_inferences WHERE inference_id=ANY(%s) ORDER BY inference_id",
            ([r[0] for r in immutable],),
        )
        assert preserved == immutable
        assert (
            self.scalar(
                "signal",
                "SELECT count(*) FROM signal.normalized_signals WHERE mint=%s",
                (MINT,),
            )
            == 1
        )
        return {
            "processes_recovered": len(names),
            "immutable_inferences_preserved": len(immutable),
            "duplicate_normalized_signals": 0,
        }

    def adverse(self, *, evidence_ready=False):
        mint = "CUvBbyBxZMx9hqMCcZ4ATQDbjiq6H3phx6en1nNxpump"
        fixture = token_fixture(time.time(), 6)
        fixture["creation"]["tokenAddress"] = mint
        self.config["birdeye"]["tokens"][mint] = fixture
        self.scenario()
        self.message(int(time.time()) + 1, mint=mint)
        warmup_started = time.monotonic()
        eligible = None
        if evidence_ready:
            eligible = wait(
                lambda: self.stack.sql(
                    "strategy",
                    "SELECT d.decision_id FROM strategy.rolling_decision d JOIN strategy.frozen_feature_snapshots f USING(decision_id) WHERE d.mint=%s AND f.status='COMPLETE' ORDER BY d.decision_at LIMIT 1",
                    (mint,),
                ),
                240,
                "adverse PIT-safe evidence readiness",
            )[0][0]
        warmup_seconds = time.monotonic() - warmup_started
        if evidence_ready:
            self.trace['adverse_readiness'] = {'decision_id': eligible, 'evidence_warmup_seconds': warmup_seconds, 'cold_entry_160s_exceeded': warmup_seconds > 160, 'entry_deadline_seconds': 160}
        wait(
            lambda: (
                self.scalar(
                    "strategy",
                    "SELECT count(*) FROM paper.positions p JOIN paper.lgbm_inferences i USING(inference_id) WHERE p.mint=%s AND p.state='OPEN' AND (%s::text IS NULL OR i.decision_id=%s)",
                    (mint, eligible, eligible),
                )
                > 0
            ),
            160,
            "adverse path accepted entry",
        )
        position, price, entry = self.stack.sql(
            "strategy",
            "SELECT p.paper_position_id,p.entry_price,p.entry_at FROM paper.positions p JOIN paper.lgbm_inferences i USING(inference_id) WHERE p.mint=%s AND p.state='OPEN' AND (%s::text IS NULL OR i.decision_id=%s) LIMIT 1",
            (mint, eligible, eligible),
        )[0]
        for b in self.config["birdeye"]["tokens"][mint]["candles"]:
            if b["unix_time"] >= int(entry.timestamp()) // 60 * 60 + 60:
                b.update(
                    o=float(price),
                    h=float(price) * 2.1,
                    l=float(price) * 0.85,
                    c=float(price) * 1.1,
                )
        self.scenario()
        wait(
            lambda: (
                self.scalar(
                    "strategy",
                    "SELECT count(*) FROM paper.positions WHERE paper_position_id=%s AND state='SL_CLOSED'",
                    (position,),
                )
                == 1
            ),
            160,
            "conservative intrabar stop-loss",
        )
        row = self.stack.sql(
            "strategy",
            "SELECT state,gross_pnl,net_pnl FROM paper.positions WHERE paper_position_id=%s",
            (position,),
        )[0]
        assert float(row[1]) < 0 and float(row[2]) < float(row[1])
        return row

    def point_deadline(self):
        mint = "DUvBbyBxZMx9hqMCcZ4ATQDbjiq6H3phx6en1nNxpump"
        at = time.time()
        fixture = token_fixture(at, 6)
        # Initial quote is valid. Post-decision price is explicitly unavailable.
        fixture["prices"] = [
            {"visible_at": at - 1, "value": fixture["price"]},
            {"visible_at": at + 50, "value": None},
        ]
        fixture["creation"]["tokenAddress"] = mint
        self.config["birdeye"]["tokens"][mint] = fixture
        self.scenario()
        self.message(int(at) + 1, mint=mint)
        eligible = None
        if getattr(getattr(self.stack, 'args', None), 'entry_evidence_ready', False):
            eligible = wait(
                lambda: self.stack.sql(
                    'strategy',
                    "SELECT d.decision_id FROM strategy.rolling_decision d JOIN strategy.frozen_feature_snapshots f USING(decision_id) JOIN paper.lgbm_inferences i USING(decision_id) WHERE d.mint=%s AND f.status='COMPLETE' AND i.decision='ACCEPTED_WAITING_FOR_ENTRY_REFERENCE' ORDER BY d.decision_at LIMIT 1",
                    (mint,),
                ), 240, 'missing-price eligible decision readiness',
            )[0][0]
        return self.verify_point_deadline(mint, eligible)

    def verify_point_deadline(self, mint, eligible=None):
        """Retain the210s deadline; qualify terminal checks by eligible decision."""
        wait(
            lambda: (
                self.scalar(
                    "strategy",
                    "SELECT count(*) FROM strategy.rolling_decision d JOIN paper.lgbm_inferences i USING(decision_id) WHERE d.mint=%s AND d.execution_reference_status='UNAVAILABLE' AND i.decision='ACCEPTED_WAITING_FOR_ENTRY_REFERENCE' AND (%s::text IS NULL OR d.decision_id=%s)",
                    (mint, eligible, eligible),
                )
                > 0
            ),
            210,
            "point-price window expiry",
        )
        assert (
            self.scalar(
                "strategy",
                "SELECT count(*) FROM strategy.execution_reference WHERE mint=%s",
                (mint,),
            )
            == 0
        )
        assert (
            self.scalar(
                "strategy",
                "SELECT count(*) FROM paper.positions WHERE mint=%s",
                (mint,),
            )
            == 0
        )
        return {"mint": mint, "reference_status": "UNAVAILABLE", "paper_entries": 0}

    def provider_retry(self):
        mint = "EUvBbyBxZMx9hqMCcZ4ATQDbjiq6H3phx6en1nNxpump"
        fixture = token_fixture(time.time(), 4)
        fixture["creation"]["tokenAddress"] = mint
        self.config["birdeye"]["tokens"][mint] = fixture
        key = "/defi/token_security:" + mint
        self.config["birdeye"]["faults"][key] = [
            {"on_call": 1, "status": 429},
            {"on_call": 2, "status": 503},
        ]
        self.scenario()
        self.message(int(time.time()) + 1, mint=mint)
        wait(
            lambda: (
                self.scalar(
                    "market",
                    "SELECT count(*) FROM market_state.signal_enrichment_jobs WHERE mint=%s AND state='PARTIAL'",
                    (mint,),
                )
                == 1
            ),
            100,
            "explicit partial enrichment after terminal 503",
        )
        rows = [
            r
            for r in http(self.mock + "/__mock/requests")
            if r["provider_request"] == key
        ]
        assert len(rows) == 2 and sum(bool(r["fault"]) for r in rows) == 2
        assert (
            self.stack.sql(
                "market",
                "SELECT payload #>> '{provider_responses,token_security,error_code}' FROM market_state.signal_enrichment_snapshots WHERE mint=%s",
                (mint,),
            )[0][0]
            == "Server"
        )
        return {"endpoint": key, "requests": len(rows), "faults_exercised": 2}

    def capacity(self):
        started = stamp()
        mints = []
        fixture = token_fixture(time.time(), 4)
        for index in range(25):
            mint = capacity_mint(index)
            mints.append(mint)
            token = json.loads(json.dumps(fixture))
            token["creation"]["tokenAddress"] = mint
            self.config["birdeye"]["tokens"][mint] = token
        self.scenario()
        for index, mint in enumerate(mints):
            self.message(int(time.time()) + index, mint=mint)
        wait(
            lambda: (
                self.scalar(
                    "market",
                    "SELECT count(*) FROM market_state.lifecycle_admissions WHERE mint=ANY(%s)",
                    (mints,),
                )
                == len(mints)
            ),
            120,
            "capacity lifecycle admissions",
        )

        def observed():
            rows = self.stack.sql(
                "market",
                "SELECT boundary_id,boundary_at,configured_cap,eligible_candidate_count,selected_candidate_count,excluded_candidate_count FROM market_state.scheduler_boundary_observation WHERE created_at>%s AND eligible_candidate_count>configured_cap ORDER BY boundary_at DESC LIMIT 1",
                (started,),
            )
            return rows[0] if rows else None

        boundary = wait(
            observed, 90, "capacity exclusion at natural scheduler boundary"
        )
        identity, at, cap, eligible, selected, excluded = boundary
        assert cap == 20 and selected == cap and excluded == eligible - selected
        assert (
            self.scalar(
                "market",
                "SELECT count(*) FROM market_state.scheduler_boundary_candidate WHERE boundary_id=%s AND NOT selected AND exclusion_reason='CAP_EXCLUDED'",
                (identity,),
            )
            == excluded
        )
        wait(
            lambda: (
                self.scalar(
                    "market",
                    "SELECT count(*) FROM market_evidence.candle_versions WHERE candle_start=%s",
                    (at.replace(second=0, microsecond=0) - timedelta(seconds=60),),
                )
                > 0
            ),
            35,
            "capacity provider completion",
        )
        return {
            "fixture_mints": len(mints),
            "configured_cap": cap,
            "eligible": eligible,
            "selected": selected,
            "excluded": excluded,
            "boundary_id": identity,
        }

    def max_hold(self):

        mint = "FUvBbyBxZMx9hqMCcZ4ATQDbjiq6H3phx6en1nNxpump"
        token = token_fixture(time.time(), 6, future_minutes=125)
        token["creation"]["tokenAddress"] = mint
        self.config["birdeye"]["tokens"][mint] = token
        self.scenario()
        self.message(int(time.time()) + 1, mint=mint)
        wait(
            lambda: (
                self.scalar(
                    "strategy",
                    "SELECT count(*) FROM paper.positions WHERE mint=%s AND state='OPEN'",
                    (mint,),
                )
                > 0
            ),
            180,
            "maximum-hold accepted entry",
        )
        position, price, entry, deadline = self.stack.sql(
            "strategy",
            "SELECT paper_position_id,entry_price,entry_at,max_hold_deadline FROM paper.positions WHERE mint=%s AND state='OPEN' LIMIT 1",
            (mint,),
        )[0]
        assert deadline - entry == timedelta(minutes=120)
        for bar in token["candles"]:
            if bar["unix_time"] >= int(entry.timestamp()) // 60 * 60 + 60:
                bar.update(
                    o=float(price),
                    h=float(price) * 1.02,
                    l=float(price) * 0.98,
                    c=float(price),
                )
        self.scenario()
        print(
            "Maximum-hold soak preserves the production deadline: " + str(deadline),
            flush=True,
        )
        wait(
            lambda: (
                self.scalar(
                    "strategy",
                    "SELECT count(*) FROM paper.positions WHERE paper_position_id=%s AND state='MAX_HOLD_CLOSED'",
                    (position,),
                )
                == 1
            ),
            7350,
            "real maximum-hold exit",
            poll_seconds=15,
        )
        row = self.stack.sql(
            "strategy",
            "SELECT state,gross_pnl,net_pnl FROM paper.positions WHERE paper_position_id=%s",
            (position,),
        )[0]
        assert abs(float(row[1])) < 1e-9 and float(row[2]) < 0
        return {"entry_at": entry, "deadline": deadline, "outcome": row}

    def creation(self):
        self.stack.sql(
            "market",
            "INSERT INTO market_state.token_creation_runtime(policy,activated_at,enabled) VALUES('token-creation-v1',%s,true) ON CONFLICT(policy) DO UPDATE SET enabled=true",
            (self.stack.state["activation"],),
        )
        self.stack.start(
            "creation",
            [self.stack.market / "target/debug/token-creation-collector"],
            {"TOKEN_CREATION_ENABLED": "1"},
        )
        try:
            wait(
                lambda: (
                    self.scalar(
                        "market",
                        "SELECT count(*) FROM market_evidence.token_creation_metadata WHERE mint=%s",
                        (MINT,),
                    )
                    == 1
                ),
                40,
                "token creation metadata",
            )
            assert (
                self.scalar(
                    "market",
                    "SELECT count(*) FROM market_state.token_creation_attempt WHERE mint=%s AND status='SUCCEEDED'",
                    (MINT,),
                )
                == 1
            )
            return self.stack.sql(
                "market",
                "SELECT mint,created_at FROM market_evidence.token_creation_metadata WHERE mint=%s",
                (MINT,),
            )
        finally:
            self.stack.stop("creation")

    def regime(self):
        self.stack.command(
            [self.stack.market / "target/debug/market-regime-collector"],
            {"MARKET_REGIME_ONCE": "1"},
        )
        rows = self.stack.sql(
            "market",
            "SELECT status,quality FROM market_evidence.market_regime_snapshots ORDER BY available_at DESC LIMIT 1",
        )
        assert rows and rows[0] == ("COMPLETE", "COMPLETE"), rows
        return rows

    def transaction(self):
        env = {
            "MOCK_URL": self.stack.urls["mock_transaction"],
            "TRANSACTION_URL": self.stack.urls["transaction"],
            "LOCAL_STACK_DIR": str(self.stack.root),
            "PSQL": str(self.stack.pg / "psql"),
            "TRANSACTION_TEST_DSN": self.stack.dsn("transaction"),
            "PGPASSWORD": "local",
        }
        self.stack.command(
            [
                self.stack.python,
                self.stack.transaction / "scripts/mock_scenarios.py",
                "--report",
                self.stack.root / "transaction-scenarios.json",
            ],
            env,
        )
        result = json.loads(
            (self.stack.root / "transaction-scenarios.json").read_text()
        )
        return result

    def report(self):
        out = self.stack.root / "reports"
        out.mkdir(exist_ok=True)
        (out / "results.json").write_text(
            json.dumps(
                {
                    "results": self.results,
                    "trace": self.trace,
                    "model_sha256": self.stack.state["model_sha256"],
                    "observed_at": stamp(),
                },
                indent=2,
                default=str,
            )
            + "\n"
        )
        if "proxy" in self.stack.state["processes"]:
            (out / "delivery_requests.json").write_text(
                json.dumps(
                    http(self.stack.urls["proxy"] + "/__proxy/requests"), indent=2
                )
                + "\n"
            )
        (out / "provider_requests.json").write_text(
            json.dumps(http(self.mock + "/__mock/requests"), indent=2) + "\n"
        )
        lines = [
            "# Production simulation results",
            "",
            f"Run: `{self.stack.root}`",
            f"Model SHA-256: `{self.stack.state['model_sha256']}`",
            "",
            "| Scenario | Result |",
            "| --- | --- |",
        ]
        lines.extend(f"| {r['name']} | {r['status']} |" for r in self.results)
        lines.extend(
            [
                "",
                "[Assertions and domain trace](results.json) · [Provider requests](provider_requests.json)",
                "",
                "## Limitations",
                "The suite uses real wall time. --extended adds provider retries, an adverse intrabar path and the 60-second point-price deadline. The optional --max-hold-soak adds the real 120-minute deadline; see scenario results to determine whether it ran. Transaction tests are separate from Strategy.",
            ]
        )
        lines.extend([
            "",
            "[Delivery requests](delivery_requests.json)" if (out / "delivery_requests.json").exists() else "",
            "[Transaction scenario details](../transaction-scenarios.json)" if (self.stack.root / "transaction-scenarios.json").exists() else "",
            "",
            "Local profile only: mock providers and isolated databases. Real MTProto, production providers, JetStream delivery and remote deployment are not validated by this report.",
        ])
        (out / "CONSOLIDATED_REPORT.md").write_text("\n".join(lines) + "\n")
        return out / "CONSOLIDATED_REPORT.md"


def run_suite(stack, name, extended=False, max_hold_soak=False):
    suite = Suite(stack)
    try:
        if name in ["core", "all", "collectors", "production"]:
            suite.check("healthy Signal → Market → Strategy + Social", suite.healthy)
            suite.check("all Birdeye response contracts", suite.provider_contracts)
        if name in ["core", "all", "production"]:
            suite.check("Signal restart and duplicate message", suite.replay)
            suite.check("Social no-match canonical persistence", suite.social)
            suite.check("Social flood-wait persistence", suite.social_fault)
            suite.check(
                "natural candles, frozen acceptance and paper entry",
                suite.minute_and_inference,
            )
            suite.check("paper TP exit and PnL", suite.paper_exit)
            suite.check("frozen model rejection", suite.rejected)
        if (
            name in ["core", "all", "production"]
            and "proxy" in stack.state["processes"]
        ):
            suite.check(
                "lost acknowledgements and identity conflicts", suite.delivery_ambiguity
            )
            suite.check(
                "point-in-time availability and frozen revision isolation", suite.pit
            )
            suite.check(
                "worker restart and single-writer fencing", suite.worker_recovery
            )
        if extended and name in ["core", "all", "production"]:
            suite.check(
                "bounded 429 retry and terminal 503 partial evidence",
                suite.provider_retry,
            )
            suite.check("conservative intrabar SL exit and PnL", lambda: suite.adverse(evidence_ready=getattr(stack.args, 'entry_evidence_ready', False)))
            suite.check("missing point-price deadline", suite.point_deadline)
            suite.check(
                "PostgreSQL outage and supervised recovery", suite.database_recovery
            )
            suite.check("scheduler capacity exclusion", suite.capacity)
        if max_hold_soak and name in ["core", "all", "production"]:
            suite.check("real 120-minute maximum-hold exit", suite.max_hold)
        if name in ["collectors", "all", "production"]:
            suite.check("token-creation collector", suite.creation)
            suite.check("market-regime collector", suite.regime)
        if name in ["transaction", "all"]:
            suite.check("existing Transaction execution scenarios", suite.transaction)
    finally:
        print("Report:", suite.report(), flush=True)
    if any(r["status"] == "FAIL" for r in suite.results):
        raise RuntimeError("simulation assertions failed; see report")
