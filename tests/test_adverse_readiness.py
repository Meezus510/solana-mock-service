from pathlib import Path
import sys
from types import SimpleNamespace
import pytest

sys.path.insert(0, str(Path(__file__).parents[1] / 'scripts'))
import scenarios


@pytest.mark.parametrize('ready,deadlines', [(False,[160,160]), (True,[240,160,160])])
def test_adverse_preserves_cold_control_and_same_eligible_decision(monkeypatch,ready,deadlines):
    queries=[]
    def sql(owner,q,p=()):
        queries.append((q,p))
        if 'FROM strategy.rolling_decision' in q:return [('eligible',)]
        if 'SELECT p.paper_position_id' in q:
            from datetime import datetime,timezone
            return [('position',1,datetime.now(timezone.utc))]
        if 'SELECT state,gross_pnl' in q:return [('SL_CLOSED',-1,-2)]
        return [(1,)]
    monkeypatch.setattr(scenarios,'http',lambda *a:{'birdeye':{'tokens':{}}})
    suite=scenarios.Suite(SimpleNamespace(urls={'mock':'http://127.0.0.1:19080'},sql=sql))
    monkeypatch.setattr(suite,'scenario',lambda:None)
    monkeypatch.setattr(suite,'message',lambda *a,**k:None)
    seen=[]
    def wait(fn,timeout,label):
        seen.append(timeout)
        return fn()
    monkeypatch.setattr(scenarios,'wait',wait)
    assert suite.adverse(evidence_ready=ready)==('SL_CLOSED',-1,-2)
    assert seen==deadlines
    for q,p in queries:
        if 'JOIN paper.lgbm_inferences' in q:
            assert p[1:]==(('eligible','eligible') if ready else (None,None))


def test_primary_entry_readiness_requires_accepted_same_decision(monkeypatch):
    queries=[]
    def sql(owner,q,p=()):
        queries.append((q,p))
        if 'SELECT decision_id,decision,rejection_reason' in q:
            return [('early','REJECT','FEATURE_SCHEMA_UNAVAILABLE',0)]
        if 'FROM strategy.rolling_decision' in q:return [('eligible',)]
        return [(1,)]
    monkeypatch.setattr(scenarios,'http',lambda *a:{'birdeye':{'tokens':{}}})
    suite=scenarios.Suite(SimpleNamespace(args=SimpleNamespace(entry_evidence_ready=True),urls={'mock':'http://127.0.0.1:19080'},sql=sql))
    seen=[]
    def wait(fn,timeout,label):
        seen.append(timeout)
        return fn()
    monkeypatch.setattr(scenarios,'wait',wait)
    suite.minute_and_inference()
    assert seen==[100,100,240,70]
    readiness=next(q for q,p in queries if 'FROM strategy.rolling_decision' in q)
    assert "f.status='COMPLETE'" in readiness
    assert "i.decision='ACCEPTED_WAITING_FOR_ENTRY_REFERENCE'" in readiness
    assert any('JOIN paper.lgbm_inferences' in q and p[1:]==('eligible','eligible') for q,p in queries)


@pytest.mark.parametrize('ready,deadlines',[(False,[210]),(True,[240,210])])
def test_missing_price_preserves_deadline_and_qualifies_decision(monkeypatch,ready,deadlines):
    queries=[]
    def sql(owner,q,p=()):
        queries.append((q,p))
        if 'SELECT d.decision_id' in q:return [('eligible',)]
        if 'FROM strategy.execution_reference WHERE mint' in q or 'FROM paper.positions WHERE mint' in q:return [(0,)]
        return [(1,)]
    monkeypatch.setattr(scenarios,'http',lambda *a:{'birdeye':{'tokens':{}}})
    suite=scenarios.Suite(SimpleNamespace(args=SimpleNamespace(entry_evidence_ready=ready),urls={'mock':'http://127.0.0.1:19080'},sql=sql))
    monkeypatch.setattr(suite,'scenario',lambda:None)
    monkeypatch.setattr(suite,'message',lambda *a,**k:None)
    seen=[]
    def wait(fn,timeout,label):
        seen.append(timeout)
        return fn()
    monkeypatch.setattr(scenarios,'wait',wait)
    assert suite.point_deadline()['paper_entries']==0
    assert seen==deadlines
    terminal=next(p for q,p in queries if "execution_reference_status='UNAVAILABLE'" in q)
    assert terminal[1:]==(('eligible','eligible') if ready else (None,None))
