"""
Trash operation — move files to the system Trash (reversible).

trash_files(records)
    Calls send2trash on each path individually so a failure on one file
    doesn't abort the rest.  Returns (succeeded, failed) where failed is
    a list of (PhotoRecord, error_message) tuples.
"""
from __future__ import annotations

from typing import List, Tuple

from send2trash import send2trash, TrashPermissionError

from app.models.photo_record import PhotoRecord


def trash_files(
    records: List[PhotoRecord],
) -> Tuple[List[PhotoRecord], List[Tuple[PhotoRecord, str]]]:
    """
    Move each record's file to the system Trash.

    Returns
    -------
    succeeded : List[PhotoRecord]
        Records whose files were successfully trashed.
    failed : List[Tuple[PhotoRecord, str]]
        (record, error_message) pairs for files that could not be trashed.
    """
    succeeded: List[PhotoRecord] = []
    failed: List[Tuple[PhotoRecord, str]] = []

    for record in records:
        if not record.path.exists():
            # Already gone — treat as success so it gets cleaned from the model
            succeeded.append(record)
            continue
        try:
            send2trash(str(record.path))
            succeeded.append(record)
        except TrashPermissionError as exc:
            failed.append((record, f"Permission denied: {exc}"))
        except Exception as exc:  # noqa: BLE001
            failed.append((record, str(exc)))

    return succeeded, failed
