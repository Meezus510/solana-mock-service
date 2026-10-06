"""The healthy E2E provider fixture must satisfy Market's completed minute grid."""
import sys
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'scripts'))
from scenarios import token_fixture


def test_healthy_holder_fixture_covers_exact_one_and_five_minute_windows():
    cutoff=1791264360
    fixture=token_fixture(cutoff+21.388146)
    points=fixture['holder_chart']
    assert all(p['timestamp']<=cutoff for p in points)
    for minutes in (1,5):
        actual=[p['timestamp'] for p in points if cutoff-minutes*60<=p['timestamp']<=cutoff]
        assert actual==list(range(cutoff-minutes*60,cutoff+1,60))
    assert len(points)==31
