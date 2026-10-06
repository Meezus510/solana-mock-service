"""Fixture setup preserves synthetic identities and exact provider horizons."""
from pathlib import Path
import sys

SCRIPTS = Path(__file__).parents[1] / "scripts"
sys.path.insert(0, str(SCRIPTS))
import scenarios


def test_capacity_keys_match_original_base58_algorithm():
    import base58
    import hashlib
    keys = [scenarios.capacity_mint(i) for i in range(25)]
    assert len(set(keys)) == 25
    for i, key in enumerate(keys):
        assert key == base58.b58encode(hashlib.sha256(f"capacity-fixture-{i}".encode()).digest()).decode()


def test_fixture_has_explicit_fifteen_minute_return():
    fixture = scenarios.token_fixture(1791264360)
    assert fixture["meme"]["price_change_15m_percent"] == 2
    assert fixture["meme"]["price_change_5m_percent"] == 1
    assert fixture["meme"]["price_change_1h_percent"] == 5


def rejection_fixture(monkeypatch):
    from types import SimpleNamespace
    queries = []
    def sql(owner, query, params=()):
        queries.append((query, params))
        if "FROM strategy.rolling_decision" in query:
            return [("eligible-decision", "cutoff", 3)]
        if "FROM paper.positions" in query:
            return [(0,)]
        return [(1,)]
    stack = SimpleNamespace(urls={"mock": "http://127.0.0.1:19080"}, sql=sql)
    monkeypatch.setattr(scenarios, "http", lambda *a: {"birdeye": {"tokens": {}}})
    suite = scenarios.Suite(stack)
    monkeypatch.setattr(suite, "scenario", lambda: None)
    monkeypatch.setattr(suite, "message", lambda *a, **k: None)
    waits = []
    def wait(fn, timeout, label):
        waits.append((timeout, label))
        result = fn()
        assert result
        return result
    monkeypatch.setattr(scenarios, "wait", wait)
    return suite, queries, waits


def test_rejection_requires_complete_snapshot_and_same_decision(monkeypatch):
    suite, queries, waits = rejection_fixture(monkeypatch)
    result = suite.rejected()
    assert result["eligible_decision"][0] == "eligible-decision"
    assert [timeout for timeout, _ in waits] == [240, 100]
    model_query = next((q, p) for q, p in queries if "FROM paper.lgbm_inferences" in q)
    assert model_query[1][1:] == ("eligible-decision", "eligible-decision")
    assert "BELOW_FROZEN_EV_GATE" in model_query[0]
    assert any("FROM paper.positions" in q for q, _ in queries)


def test_original_cold_deadline_control_is_retained(monkeypatch):
    suite, queries, waits = rejection_fixture(monkeypatch)
    result = suite.rejected_cold()
    assert result["eligible_decision"] is None
    assert [timeout for timeout, _ in waits] == [100]
    assert not any("FROM strategy.rolling_decision" in q for q, _ in queries)
