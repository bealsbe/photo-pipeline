# File Integrity Audit

## Overview

All file-writing operations in the app, ranked by risk of data loss.  
Each issue has a **severity**, a **location**, a **description of the failure mode**, and a **proposed fix**.

---

## Critical — Data loss is possible

### 1. Non-atomic sidecar writes

**Files:** `app/main_window.py:809`, `app/models/sidecar.py:74`  
**Operations:** prune marks (`.photo_pipeline.json`), pair keys (`.photo-pipeline-pairs.json`)

Both sidecars are written with `Path.write_text(json.dumps(...))`.  
`write_text` truncates the file then writes. A crash, power loss, or OS kill between truncate and write completion leaves a zero-byte or partially-written JSON file. On the next open, `json.loads` fails silently and all prune marks / pair data is gone — permanently, since there is no backup.

**Fix:** atomic write via temp file + `os.replace()`:
```python
import tempfile, os
tmp = sidecar.with_suffix(".tmp")
tmp.write_text(json.dumps(data, indent=2))
os.replace(tmp, sidecar)   # atomic on POSIX; near-atomic on Windows
```
Both sidecar writers and `_save_prune_marks` need this pattern.

---

### 2. Empty-folder cleanup walks past source root

**File:** `app/ops/library.py` — the cleanup block added at the end of `execute()`

The cleanup loop stops when `p == self._root`, but `self._root` is the **library destination** root.  
Source files come from a completely different tree (the shoot folder the user selected).  
The path `while p != self._root` never becomes true for source ancestors, so the loop
climbs all the way to the filesystem root (`p == p.parent`), attempting `rmdir()` on every
ancestor of every moved file — including `/home/user`, `/home`, etc.

Because `rmdir()` only removes truly empty directories this is unlikely to delete anything
it shouldn't in practice, but it is actively dangerous on any shoot folder that happened
to be fully moved, and it is wrong by design.

**Fix:** accept a `source_root: Optional[Path] = None` parameter in `execute()`.
Walk up only to `source_root` (not `self._root`). AutoSortDialog should pass the source
folder; ExportDialog can omit it (working folder cleanup is less critical / not needed).

```python
def execute(self, mode, progress=None, source_root=None):
    ...
    if mode == "move" and succeeded:
        bound = source_root or Path(op.src.root)   # absolute fallback
        for d in dirs_to_check:
            p = d
            while p != bound and p != p.parent:
                expanded.add(p)
                p = p.parent
```

---

### 3. `shutil.move()` in Sort has no post-copy verification

**File:** `app/ops/separate.py:144`

`shutil.move(src, dest)` on a cross-device move copies then deletes the source.
Unlike `library.py`, `separate.py` never checks that the copy completed correctly
(no size comparison, no error path that cleans up a partial dest file).
If the disk fills mid-copy: partial file at dest, source deleted, data gone.

**Fix:** replicate the `library.py` cross-device pattern — use `os.rename()` first,
fall back to copy + size-verify + delete:
```python
st = src.stat()
try:
    os.rename(str(src), str(op.final_dest))
except OSError:
    shutil.copy2(str(src), str(op.final_dest))
    if op.final_dest.stat().st_size != st.st_size:
        op.final_dest.unlink(missing_ok=True)
        raise RuntimeError(f"size mismatch after copy: {src.name}")
    src.unlink()
try:
    os.utime(str(op.final_dest), (st.st_atime, st.st_mtime))
except OSError:
    pass
```

---

### 4 + 5. Partial write and OVERWRITE — must be fixed together

**File:** `app/ops/library.py:336`

These two issues share a root cause and must be implemented as a unit. Fixing #4 without
#5 makes overwrite scenarios *worse*, not better.

**The problem:**
`shutil.copy2(src, dest)` writes directly into `dest`. Two failure modes:

- **Copy fails mid-write (disk full, I/O error):** `dest` is now a truncated partial file.
  If we then call `dest.unlink()` to clean up (the obvious fix for #4), we've destroyed
  whatever was already at `dest` before the export started. Net result: both the new file
  and the original destination file are gone.

- **No failure, but OVERWRITE resolution:** `shutil.copy2` silently stomps the existing
  `dest` with no backup. If `src` turns out to be corrupt, the good copy at `dest` is gone.

**The fix — atomic copy via temp + `os.replace()` (closes both issues):**

Write to a `.~tmp` file in the same directory first, verify it, then rename it atomically
over `dest`. The original `dest` is only replaced after the new file is confirmed good.
If anything fails before the rename, the original is completely untouched.

```python
tmp_dest = dest.with_suffix(dest.suffix + ".~tmp")
try:
    shutil.copy2(str(src), str(tmp_dest))
    if tmp_dest.stat().st_size != stat.st_size:
        raise RuntimeError(f"size mismatch after copy: {src.name}")
    os.replace(str(tmp_dest), str(dest))   # atomic — dest is old or new, never partial
except Exception:
    tmp_dest.unlink(missing_ok=True)       # clean up temp, original dest untouched
    raise
```

This pattern applies to **both** `library.py` (Export/Auto-sort) and `separate.py` (Sort)
on any path where `shutil.copy2` is used.

**Guarantee when implemented:** no RAW or JPG file — source or destination — is destroyed
under any copy/write failure mode. The only remaining risk is a cross-device move where
the source delete fails after a successful copy, leaving two copies (harmless).

---

### 6. Stale conflict detection between plan creation and execute

**File:** `app/ops/library.py` — `LibraryPlan.__init__` vs `execute()`

`LibraryPlan` detects conflicts at construction time. The user may spend time reviewing
the conflict bar before clicking Export. Files that arrive at the destination during that
window are silently overwritten (if the op's resolution was OVERWRITE from the "no
conflict" default path).

**Fix:** re-check `dest.exists()` at the start of each `execute()` iteration. If a
previously clean dest now exists, either skip or raise:
```python
if op.resolution != Resolution.OVERWRITE and dest.exists():
    failed.append((op, f"destination appeared after plan: {dest.name}"))
    continue
```

---

### 7. `send2trash` may permanently delete on some filesystems

**File:** `app/ops/trash.py:40`

On network shares (NFS, SMB/CIFS) and FAT/exFAT volumes, `send2trash` cannot use
the OS trash mechanism and may fall back to a permanent `os.remove()`. The
`TrashPermissionError` is caught, but the silent-unlink fallback is not.  
Files on a NAS or SD card reader go to `/dev/null`, not Trash.

**Fix:** detect and warn before committing. `send2trash` itself doesn't expose the
fallback behavior as a distinct exception, but we can check at dialog-open time:
```python
import shutil
if not shutil.disk_usage(path).free:   # proxy: check if it's a real FS
    warn("Files on network/removable drives may be permanently deleted.")
```
Or at minimum: document this in the PruneReviewDialog warning label.

---

### 8. Silent sidecar save failures give no user feedback

**File:** `app/main_window.py:812`, `app/models/sidecar.py:76`

Both sidecar writers have `except Exception: pass`. If the save fails (permissions,
read-only volume, disk full), the user continues editing prune marks or pairs with no
indication their changes are not being persisted. On next open everything they marked
is gone.

**Fix:** surface failures, at minimum as a status bar message:
```python
except Exception as exc:
    self.statusBar().showMessage(f"⚠ Could not save prune marks: {exc}", 8000)
```

---

## Low Risk — Good hardening but not urgent

### 9. Partial thumbnail write leaves a corrupt cache file

**File:** `app/thumbnails/disk_cache.py:57`

`put()` calls `image.save(str(p), "PNG")` directly at the final cache path.
If the save is interrupted (disk full, process killed), a partial PNG is written to `p`.
On the next startup, `get()` calls `QImage(str(p))` — QImage returns null for corrupt
data, which returns None (treated as a cache miss), so the thumbnail is re-generated.
This is safe in practice but leaves a stale corrupt file in the cache directory.

Additionally, `put()` skips the write if `p.exists()` — but two threads generating
the same thumbnail simultaneously can both pass the exists check (TOCTOU), and both
call `image.save()`. The docstring acknowledges this as idempotent since both writes
are identical content. Acceptable.

**Fix:** write to a `.tmp` path then `os.replace()`:
```python
tmp = p.with_suffix(".tmp")
image.save(str(tmp), "PNG")
os.replace(str(tmp), str(p))
```
This eliminates both the partial-file and the two-writer race in one shot.

---

### 11. TOCTOU race in `_next_free()` / `_free_path()`  <!-- was mis-numbered; see 10 below -->

**Files:** `app/ops/library.py:116`, `app/ops/separate.py:43`

Both functions check whether a path exists then use it. In theory another process
(or another concurrent export) could create that path between the check and the write,
causing a silent overwrite. Extremely unlikely in normal single-user desktop use.

**Fix if needed:** use `O_CREAT | O_EXCL` for atomic exclusive creation, then fall back to
the next suffix. Not worth implementing now.

---

### 12. No disk space pre-flight

**Files:** `app/ops/library.py`, `app/ops/separate.py`

Neither executor checks available disk space before starting. A large move that fills
the disk halfway through leaves files in a split state — some moved, some not.

**Fix:** before executing, estimate total bytes to copy and compare against
`shutil.disk_usage(dest_root).free`. Abort with a clear error if insufficient space.

---

### 13. No hash/checksum verification after copy

**File:** `app/ops/library.py:344`

The cross-device move branch verifies `st_size` only. A bit-flip during transfer
produces a corrupt dest file at the correct size — undetected.

**Fix:** optionally compute SHA-256 of src before copy and verify after. Expensive
for large RAW files; could be opt-in or limited to the cross-device path only.

---

### 14. Source existence not re-verified before execute

**Files:** `app/ops/library.py:333`, `app/ops/separate.py:143`

If a source file is deleted externally between scan and execute, `src.stat()` raises
`FileNotFoundError`, which is caught generically and reported as a failed op. This is
correct behavior but the error message ("No such file or directory") is confusing
in context.

**Fix:** check `src.exists()` first and emit a clearer message:
```python
if not src.exists():
    failed.append((op, f"source file no longer exists: {src.name}"))
    continue
```

---

## Priority Order for Implementation

| # | Issue | Effort | Impact |
|---|-------|--------|--------|
| 2 | Empty-folder cleanup walks past source root | Small | Critical — wrong by design |
| 1 | Non-atomic sidecar writes | Small | Critical — data loss on crash |
| 4+5 | Atomic copy via temp+replace (closes partial-write AND overwrite-without-backup) | Small | Critical — the only fix that guarantees no photo file is lost |
| 3 | `shutil.move()` lacks post-copy verification in Sort | Small | Critical — cross-device Sort can delete src after partial copy |
| 8 | Silent sidecar save failures | Trivial | High — silent data loss |
| 5 | Atomic overwrite (temp + rename) | Medium | High |
| 6 | Stale conflict re-check at execute time | Small | Moderate |
| 7 | `send2trash` fallback warning | Small | Moderate |
| 9 | Partial thumbnail write (corrupt cache file) | Trivial | Low — cache only, auto-recovers |
| 12 | Disk space pre-flight | Medium | Low-moderate |
| 11 | TOCTOU in _next_free | Medium | Low |
| 13 | Hash verification | Large | Low |
| 14 | Source existence pre-check message | Trivial | Low |

## Files Audited

| File | Has file writes | Notes |
|------|----------------|-------|
| `app/ops/library.py` | Yes | Primary executor — issues 1–6 |
| `app/ops/separate.py` | Yes | Sort executor — issue 3 |
| `app/ops/trash.py` | Via send2trash | Issue 7 |
| `app/models/sidecar.py` | Yes | Issues 1, 8 |
| `app/main_window.py` | Yes | Prune sidecar writes — issues 1, 8 |
| `app/thumbnails/disk_cache.py` | Yes | Issue 9 |
| `app/ui/autosort_dialog.py` | Via LibraryPlan | Calls execute(); source_root fix applies here |
| `app/ui/export_dialog.py` | Via LibraryPlan | Calls execute() |
| `app/ui/separate_dialog.py` | Via SeparationPlan | Issue 3 |
| `app/ui/prune_review.py` | Via send2trash | Issue 7 |
| `app/ui/import_dialog.py` | No | Folder picker only |
| `app/ui/folder_bar.py` | No | UI only; QSettings in main_window |
| `app/models/collection.py` | No | In-memory only |
| `app/scanning/scanner.py` | No | Read-only scan |
| `app/thumbnails/generator.py` | No | Generates QImages; disk_cache writes them |
