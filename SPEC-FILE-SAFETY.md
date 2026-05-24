# Photo Pipeline — File Safety Spec

> **Status: Deferred.** This is a follow-up project to be implemented after the Export & Library Organisation features are complete.

---

## Problem

The app currently performs file operations with no crash safety. A power loss, OS kill, or unhandled exception mid-operation can leave files in an inconsistent state — partially moved, at neither source nor destination, or with a corrupted sidecar.

---

## Affected Operations

### Existing

| Operation | Current risk |
|-----------|-------------|
| **Sort** (RAW/JPG subfolders) | `shutil.move()` directly — crash mid-move leaves file missing from source and absent from destination |
| **Prune → Trash** | Uses `send2trash` (OS trash) — already safe and reversible, no changes needed |
| **Sidecar write** (prune marks `.photo_pipeline.json`) | `write_text()` directly — crash mid-write produces corrupted JSON, all marks lost |
| **Sidecar write** (pair marks `.photo-pipeline-pairs.json`) | Same as above |

### New (Export & Library Organisation)

| Operation | Risk |
|-----------|------|
| **Export — move** | Same as Sort |
| **Export — copy** | Crash mid-copy leaves a partial file at destination; original is intact but destination file is corrupt |
| **Auto-sort — move** | Same as Sort |
| **Auto-sort — copy** | Same as Export copy |

---

## Solution 1: Atomic Sidecar Writes

All sidecar writes (prune marks, pair marks) must go through an atomic write helper:

1. Write new content to `<filename>.photo-pipeline.tmp`
2. `os.replace()` (atomic on POSIX and Windows) renames `.tmp` → final filename

A crash between steps 1 and 2 leaves an orphaned `.tmp` — harmless, the original sidecar is untouched. A crash during step 2 is safe because `os.replace()` is atomic — either the old or the new version exists cleanly, never a corrupt intermediate.

The `.tmp` orphan can be silently cleaned up on next scan of the folder.

---

## Solution 2: Operation Journal for File Moves/Copies

Before executing any batch of file operations (Sort, Export, Auto-sort), write a **journal file** to the source folder describing every planned operation. Execute the operations one by one, marking each as complete in the journal. Delete the journal on successful completion.

### Journal file

Location: `<source-folder>/.photo-pipeline-journal.json`

```json
{
  "operation": "export",
  "started_at": "2025-01-15T14:32:00Z",
  "mode": "move",
  "ops": [
    {
      "src": "/Photos/Shoot/RAW/IMG_0001.cr3",
      "dest": "/Library/2025/Jan-2025/RAW/IMG_0001.cr3",
      "status": "done"
    },
    {
      "src": "/Photos/Shoot/RAW/IMG_0002.cr3",
      "dest": "/Library/2025/Jan-2025/RAW/IMG_0002.cr3",
      "status": "pending"
    }
  ]
}
```

Statuses: `pending` → `in_progress` → `done` / `failed`

### Safe move sequence

**Same filesystem** — `os.rename()` is atomic on POSIX; use it directly:
1. Mark op `in_progress` in journal
2. `os.rename(src, dest)`
3. Mark op `done` in journal

**Cross-filesystem** — copy + verify + delete:
1. Mark op `in_progress` in journal
2. Copy `src` → `dest.photo-pipeline.tmp`
3. Verify: compare file sizes (optionally MD5 for extra safety)
4. `os.replace()` renames `.tmp` → `dest` (atomic)
5. Mark op `done` in journal
6. Delete `src`

A crash at step 2–4 leaves a `.tmp` at the destination — the source is still intact. The journal records the op as `in_progress`, so resume logic knows to re-attempt it (cleaning up the `.tmp` first).

---

## Solution 3: Resume / Cleanup on Startup

On launch, the app scans the working folder (and optionally the library folder) for orphaned journal files.

If a journal is found:

```
A previous operation did not complete.

  Export started 15 Jan 2025 at 14:32
  12 of 36 files completed

  [Resume]   [Abandon]
```

- **Resume** — re-execute all `pending` and `in_progress` ops, skipping `done` ones
- **Abandon** — leave all files exactly where they are; delete the journal

---

## Implementation notes

- The atomic write helper should be a single shared function used by all sidecar writers — do not duplicate the pattern
- Journal writes themselves should use the atomic write helper (a corrupted journal is as bad as no journal)
- `.photo-pipeline.tmp` and `.photo-pipeline-journal.json` should be added to `.gitignore`
- The journal file location should follow the source folder, not the destination, so it is always accessible even if the destination is unmounted

---

## Out of scope for this spec

- Checksumming every file (size comparison is sufficient for the first pass)
- Full undo/redo history beyond resume
- Network filesystem edge cases (NFS, SMB — rename may not be atomic)
