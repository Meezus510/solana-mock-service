"""Stream-first completed-minute cutover against the mock Birdeye REST + WebSocket.

Real Signal → Market (LIVE, claims, CU) → Strategy run unchanged; only the
completed-minute source differs. The oracle is the fixture itself: every first
candle version Market records must equal the fixture bar, whichever source
served it. A completed-minute REST request (`time_to - time_from == 59`) for a
(mint, minute) means REST served or confirmed it; no request means the stream did.

Phases (real time): healthy stream, forced disconnect, a bar the stream never
sends while connected plus a malformed frame, then a stream that lags past the
seal grace. The last phase documents a hazard rather than asserting success.
"""

import json
import time

from scenarios import MINT, Suite, capacity_mint, token_fixture
from simulate import http, stamp, wait

EXTRA = [capacity_mint(900 + i) for i in range(4)]
QUIET_MINT = EXTRA[-1]


def minute(t):
    return int(t) // 60 * 60


class StreamFirstSuite(Suite):
    def __init__(self, stack, policy):
        super().__init__(stack)
        self.policy = policy
        self.phases = []

    def ws(self, **changes):
        self.config["birdeye"].setdefault("ws", {}).update(changes)
        self.scenario()

    def phase(self, name, minutes):
        start = minute(time.time()) + 60
        self.phases.append({"phase": name, "from": start, "to": start + minutes * 60})
        wait(lambda: time.time() >= start + minutes * 60 + 40, minutes * 60 + 140, name, 1.0)

    def run(self):
        self.setup()
        now = time.time()
        self.quiet = set()
        for index, mint in enumerate(EXTRA):
            fixture = token_fixture(now, seed=20 + index, future_minutes=60)
            fixture["creation"]["tokenAddress"] = mint
            if mint == QUIET_MINT:
                # Two future minutes without trades: neither REST nor the stream has them.
                for offset in (4, 5):
                    gone = minute(now) + offset * 60
                    fixture["candles"] = [b for b in fixture["candles"] if b["unix_time"] != gone]
                    self.quiet.add((mint, gone))
                # No trades means no price change: like Birdeye REST, the next bar
                # opens at the last traded close and its range includes it.
                bars = fixture["candles"]
                for i in range(1, len(bars)):
                    if bars[i]["unix_time"] - bars[i - 1]["unix_time"] > 60:
                        bars[i]["o"] = bars[i - 1]["c"]
                        bars[i]["h"] = max(bars[i]["h"], bars[i]["o"])
                        bars[i]["l"] = min(bars[i]["l"], bars[i]["o"])
            self.config["birdeye"]["tokens"][mint] = fixture
        self.config["birdeye"]["tokens"][MINT] = token_fixture(now, future_minutes=60)
        self.config["birdeye"]["ws"] = {}
        self.scenario()
        base = int(now)
        for index, mint in enumerate([MINT, *EXTRA]):
            self.message(base + index, mint=mint)
        wait(
            lambda: self.scalar(
                "market",
                "SELECT count(DISTINCT mint) FROM market_evidence.candle_versions WHERE mint = ANY(%s)",
                ([MINT, *EXTRA],),
            )
            == 1 + len(EXTRA),
            240,
            "completed minutes for every fixture mint",
            1.0,
        )
        self.tracking_from = minute(time.time()) + 60
        self.phase("healthy", 5)
        self.ws(disconnect_at=[time.time() + 25])
        self.phase("disconnect", 3)
        skip_at = minute(time.time()) + 60
        self.ws(skip=[{"mint": MINT, "unix_time": skip_at}], malformed_at=[time.time() + 20])
        self.skipped = (MINT, skip_at)
        self.phase("stream_misses_bar_and_malformed", 2)
        self.ws(frame_delay_ms=20_000)
        self.phase("stream_lags_past_grace", 3)
        self.ws(frame_delay_ms=0)
        return self.evaluate()

    def evaluate(self):
        fixtures = self.config["birdeye"]["tokens"]
        mints = [MINT, *EXTRA]
        rows = self.stack.sql(
            "market",
            """SELECT DISTINCT ON (mint, candle_start) mint, extract(epoch FROM candle_start)::bigint,
                      open, high, low, close, volume
               FROM market_evidence.candle_versions
               WHERE mint = ANY(%s) AND interval_seconds = 60 AND provider = 'birdeye-v1'
               ORDER BY mint, candle_start, response_received_at, observed_seq""",
            (mints,),
        )
        recorded = {(m, int(t)): tuple(map(float, v)) for m, t, *v in rows}
        rest = {}
        for r in http(self.mock + "/__mock/requests"):
            q = r.get("query") or {}
            if not str(r.get("provider_request", "")).startswith("/defi/v3/ohlcv"):
                continue
            try:
                start, end = int(q["time_from"]), int(q["time_to"])
            except (KeyError, ValueError):
                continue
            if end - start == 59:
                rest.setdefault((q.get("address"), start), 0)
                rest[(q.get("address"), start)] += 1
        summary = []
        problems = []
        for phase in self.phases:
            out = {"phase": phase["phase"], "minutes": 0, "stream": 0, "rest": 0,
                   "exact": 0, "mismatch": [], "missing": [], "quiet_ok": 0}
            for mint in mints:
                bars = {b["unix_time"]: b for b in fixtures[mint]["candles"]}
                for t in range(phase["from"], phase["to"], 60):
                    out["minutes"] += 1
                    out["rest" if (mint, t) in rest else "stream"] += 1
                    want = bars.get(t)
                    got = recorded.get((mint, t))
                    if want is None:
                        out["quiet_ok"] += got is None
                        if got is not None:
                            out["mismatch"].append([mint[:6], t, "candle for a quiet minute"])
                        continue
                    if got is None:
                        out["missing"].append([mint[:6], t])
                        continue
                    exp = (want["o"], want["h"], want["l"], want["c"], want["v"])
                    if all(abs(a - b) <= 1e-9 * max(1.0, abs(b)) for a, b in zip(got, exp)):
                        out["exact"] += 1
                    else:
                        out["mismatch"].append([mint[:6], t, [round(x, 6) for x in got], [round(x, 6) for x in exp]])
            summary.append(out)
            if phase["phase"] == "stream_lags_past_grace":
                # The lag guard notices only after a sealed minute receives a late
                # update: at most the onset minute per mint may be truncated.
                onset = {}
                for row in out["mismatch"]:
                    onset[row[0]] = onset.get(row[0], 0) + 1
                out["truncated_per_mint"] = onset
                if out["missing"] or any(n > 1 for n in onset.values()):
                    problems.append(phase["phase"])
            elif out["mismatch"] or out["missing"]:
                problems.append(phase["phase"])
        ws = http(self.mock + "/__mock/birdeye-ws")
        detail = {"policy": self.policy, "phases": summary, "ws": ws,
                  "skipped": list(self.skipped), "quiet": sorted(self.quiet),
                  "rest_completed_minute_requests": sum(rest.values())}
        out_dir = self.stack.root / "reports"
        out_dir.mkdir(exist_ok=True)
        (out_dir / f"stream_first_{self.policy}.json").write_text(json.dumps(detail, indent=2, default=str) + "\n")
        print(json.dumps({p["phase"]: {k: (len(v) if isinstance(v, list) else v) for k, v in p.items() if k != "phase"} for p in summary}), flush=True)
        if problems:
            raise AssertionError(f"candles differ from the fixture in phases {problems}")
        return detail


def run_stream_first(stack, policy):
    suite = StreamFirstSuite(stack, policy)
    try:
        suite.check(f"stream-first cutover ({policy})", suite.run)
    finally:
        print("Report:", suite.report(), flush=True)
    if any(r["status"] == "FAIL" for r in suite.results):
        raise RuntimeError("stream-first assertions failed; see report")
