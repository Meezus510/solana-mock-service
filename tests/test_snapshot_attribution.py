from dataclasses import replace
import sys
from pathlib import Path
import pytest
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'scripts'))
from snapshot_attribution import Evidence, classify

EMPTY=Evidence(requested=True,scope_correct=True,provider_success=True,coverage_complete=True,applicable_records=0,persisted_records=0,collection_finished=True,within_deadline=True)
FULL=replace(EMPTY,applicable_records=13,persisted_records=13,expected_required_records=5,assembled_required_records=5,published=True,strategy_available=True)

@pytest.mark.parametrize('e,expected',[
    (EMPTY,'confirmed_provider_no_data'),(FULL,'complete'),
    (replace(EMPTY,requested=False),'internal_system_failure'),
    (replace(EMPTY,scope_correct=False),'internal_system_failure'),
    (replace(EMPTY,coverage_complete=False),'internal_system_failure'),
    (replace(EMPTY,collection_finished=False),'internal_system_failure'),
    (replace(EMPTY,internal_error='swallowed_exception'),'internal_system_failure'),
    (replace(FULL,persisted_records=12),'internal_system_failure'),
    (replace(FULL,assembled_required_records=4),'internal_system_failure'),
    (replace(FULL,published=False),'internal_system_failure'),
    (replace(FULL,strategy_available=False),'internal_system_failure'),
    (replace(FULL,within_deadline=False),'internal_system_failure'),
    (replace(EMPTY,within_deadline=False),'internal_system_failure'),
    (replace(EMPTY,published=False),'internal_system_failure'),
    (replace(EMPTY,strategy_available=False),'internal_system_failure'),
    (replace(EMPTY,requested=False,settled=False,within_deadline=True),'legitimately_pending'),
    (replace(EMPTY,requested=False,excluded_contract='provider-authority-disabled-v1'),'intentionally_excluded'),
    (Evidence(),'unknown'),
    *[(replace(EMPTY,**{field:None}),'unknown') for field in ['requested','scope_correct','provider_success','coverage_complete','collection_finished','applicable_records']],
    *[(replace(EMPTY,provider_success=False,provider_error=error),'provider_failure_or_limitation') for error in ['timeout','rate_limit','forbidden','retention','malformed_provider_payload','unsupported_required_horizon']],
])
def test_attribution_requires_evidence_and_detects_internal_controls(e,expected):
    assert classify(e)==expected

@pytest.mark.parametrize('error',['timeout','rate_limit','unsupported_required_horizon'])
def test_provider_error_cannot_hide_provider_returned_records_lost_by_system(error):
    assert classify(replace(FULL,persisted_records=12,provider_error=error))=='internal_system_failure'
