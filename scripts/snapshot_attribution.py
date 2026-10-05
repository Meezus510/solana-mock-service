"""Conservative evidence classifier for local verification and explicit audit ledgers.

No I/O and no fallback to production. A success status alone proves no attribution.
"""
from dataclasses import dataclass

@dataclass(frozen=True)
class Evidence:
    requested: bool | None = None
    scope_correct: bool | None = None
    provider_success: bool | None = None
    coverage_complete: bool | None = None
    applicable_records: int | None = None
    persisted_records: int | None = None
    assembled_required_records: int | None = None
    expected_required_records: int | None = None
    published: bool | None = None
    strategy_available: bool | None = None
    within_deadline: bool | None = None
    settled: bool = True
    internal_error: str | None = None
    provider_error: str | None = None
    excluded_contract: str | None = None
    collection_finished: bool | None = None


def classify(e: Evidence) -> str:
    if e.internal_error:
        return 'internal_system_failure'
    if e.applicable_records is not None and e.persisted_records is not None and e.persisted_records < e.applicable_records:
        return 'internal_system_failure'
    if e.expected_required_records is not None and e.assembled_required_records is not None and e.persisted_records is not None and e.persisted_records >= e.expected_required_records and e.assembled_required_records < e.expected_required_records:
        return 'internal_system_failure'
    if e.excluded_contract:
        return 'intentionally_excluded'
    if not e.settled and e.within_deadline is True:
        return 'legitimately_pending'
    if e.requested is False or e.scope_correct is False:
        return 'internal_system_failure'
    if e.published is False or e.strategy_available is False or e.within_deadline is False:
        return 'internal_system_failure'
    if e.provider_error:
        return 'provider_failure_or_limitation'
    if e.collection_finished is False or e.coverage_complete is False:
        return 'internal_system_failure' if e.requested is True else 'unknown'
    if e.provider_success is True and e.applicable_records == 0 and all(v is True for v in (e.requested,e.scope_correct,e.coverage_complete,e.collection_finished)):
        return 'confirmed_provider_no_data'
    if e.applicable_records and e.persisted_records == e.applicable_records:
        if e.published is False or e.strategy_available is False or e.within_deadline is False:
            return 'internal_system_failure'
        if e.assembled_required_records == e.expected_required_records and all(v is True for v in (e.requested,e.scope_correct,e.provider_success,e.coverage_complete,e.collection_finished,e.published,e.strategy_available,e.within_deadline)):
            return 'complete'
    return 'unknown'
