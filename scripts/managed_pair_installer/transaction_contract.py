"""Transaction schema constants for managed-pair installation."""

from __future__ import annotations

MAX_COMPONENT_BYTES = 256 * 1024 * 1024
MAX_ENVELOPE_BYTES = 2 * 1024 * 1024
MAX_STATE_BYTES = 64 * 1024
MAX_JOURNAL_BYTES = 64 * 1024
LOCK_NAME = ".managed-pair-transaction-v1.lock"
JOURNAL_NAME = ".managed-pair-bootstrap-transaction-v1.json"
JOURNAL_TEMP_NAME = ".managed-pair-bootstrap-transaction-v1.tmp"
LEGACY_JOURNAL_NAME = ".managed-pair-transaction.json"
LEGACY_JOURNAL_TEMP_NAME = ".managed-pair-transaction.tmp"
JOURNAL_CONTRACT = "ctx-managed-pair-transaction"
SLOTS = ("core", "companion", "envelope", "state")
SLOT_MAXIMUMS = {
    "core": MAX_COMPONENT_BYTES,
    "companion": MAX_COMPONENT_BYTES,
    "envelope": MAX_ENVELOPE_BYTES,
    "state": MAX_STATE_BYTES,
}
TRANSACTION_PHASES = (
    "stage_core",
    "stage_companion",
    "stage_envelope",
    "stage_state",
    "deactivate_state",
    "backup_core",
    "backup_companion",
    "backup_envelope",
    "activate_core",
    "activate_companion",
    "activate_envelope",
    "activate_state",
    "committed",
    "cleanup",
)
CRASH_CHECKPOINTS = tuple(
    checkpoint
    for phase in TRANSACTION_PHASES
    for checkpoint in (f"before_{phase}", f"after_{phase}")
    if checkpoint != "after_cleanup"
) + ("after_cleanup_files",)
