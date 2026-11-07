"""A small in-memory cache of worlds that read requests computed past a saved Checkpoint.

A seek or replay between two saves restores the Checkpoint before it and ticks forward
(experiments/timeline.py). The browser's replay asks for consecutive short windows, so
each window would restore the same Checkpoint and tick the same minutes again. Here the
reader leaves the world it ended with, so that the next window ticks on from it.

An entry is opaque to this module: the reader's cursor (its world, metrics, controller,
recorded router and history cache), the minute its world is at, and the branch head it
was read under. take() hands an entry over to one reader and removes it, so no two
readers ever hold the same world; put() returns it after the reader has advanced it.
Nothing here is ever given to a computation, and a cursor is never restored from a
computing branch's world: the reader restores it from a saved Checkpoint.

The worlds are large (a 7-day world holds several hundred MiB), so the cache keeps at
most ``per_branch`` cursors of one branch and ``total`` overall, the least recently used
going first, and drops everything once no read has used it for ``idle_seconds``.
An entry read under another head of its branch (the branch was computed on, or
replaced) is dropped when the branch is next read, and the store drops a Run's entries
when it saves a branch of it or deletes it.
"""

from __future__ import annotations

import gc
import os
import threading
import time
from collections import OrderedDict
from typing import Any

# The cursors one branch keeps: the replay's and one more (a seek elsewhere in the Run).
PER_BRANCH = 2
# The cursors the process keeps: two branches compared, each with two.
TOTAL = 4
IDLE_SECONDS = 60.0
# Sets TOTAL for a server process; 0 turns the cache off.
TOTAL_ENVIRONMENT = "SHELTERSIM_READ_WORLDS"


def _total_from_environment() -> int:
    raw = os.environ.get(TOTAL_ENVIRONMENT)
    if raw is None or raw.strip() == "":
        return TOTAL
    try:
        value = int(raw)
    except ValueError as exc:
        raise ValueError(f"{TOTAL_ENVIRONMENT} must be a non-negative integer") from exc
    if value < 0:
        raise ValueError(f"{TOTAL_ENVIRONMENT} must be a non-negative integer")
    return value


class ReadWorldCache:
    """Cursors of read-only recomputation, by (project, run, branch) and the branch head."""

    def __init__(
        self,
        *,
        per_branch: int = PER_BRANCH,
        total: int | None = None,
        idle_seconds: float = IDLE_SECONDS,
    ) -> None:
        self.per_branch = per_branch
        self.total = _total_from_environment() if total is None else total
        self.idle_seconds = idle_seconds
        self._lock = threading.Lock()
        # (key, serial) -> (head checkpoint hash, minute, cursor); oldest use first.
        self._entries: OrderedDict[tuple[tuple[str, str, str], int], tuple[str, int, Any]] = (
            OrderedDict()
        )
        self._serial = 0
        self._used = 0.0
        self._timer: threading.Timer | None = None

    def __len__(self) -> int:
        with self._lock:
            return len(self._entries)

    def take(
        self, key: tuple[str, str, str], head: str, lowest: int, highest: int
    ) -> tuple[int, Any] | None:
        """Remove and return (minute, cursor) of the branch's cursor read under head at the
        latest minute in [lowest, highest], or None. Cursors of another head are dropped."""
        dropped = []
        with self._lock:
            best = None
            for entry_key, (entry_head, minute, _) in list(self._entries.items()):
                if entry_key[0] != key:
                    continue
                if entry_head != head:
                    dropped.append(self._entries.pop(entry_key))
                elif lowest <= minute <= highest and (best is None or minute > best[1]):
                    best = (entry_key, minute)
            taken = None
            if best is not None:
                _, minute, cursor = self._entries.pop(best[0])
                taken = (minute, cursor)
                self._note_use()
        del dropped
        return taken

    def put(self, key: tuple[str, str, str], head: str, minute: int, cursor: Any) -> None:
        """Keep a cursor a reader has finished with; the least recently used go first."""
        if self.total <= 0 or self.per_branch <= 0:
            return
        evicted = []
        with self._lock:
            self._serial += 1
            self._entries[(key, self._serial)] = (head, minute, cursor)
            mine = [entry_key for entry_key in self._entries if entry_key[0] == key]
            for entry_key in mine[: max(0, len(mine) - self.per_branch)]:
                evicted.append(self._entries.pop(entry_key))
            while len(self._entries) > self.total:
                evicted.append(self._entries.popitem(last=False)[1])
            self._note_use()
        del evicted

    def drop(self, project_id: str, run_id: str, branch_id: str | None = None) -> None:
        """Forget the cursors of a Run, or of one of its branches."""
        with self._lock:
            dropped = [
                self._entries.pop(entry_key)
                for entry_key in list(self._entries)
                if entry_key[0][:2] == (project_id, run_id)
                and (branch_id is None or entry_key[0][2] == branch_id)
            ]
        del dropped

    def clear(self) -> None:
        with self._lock:
            dropped, self._entries = self._entries, OrderedDict()
        del dropped

    def _note_use(self) -> None:
        """Note a use and make sure the entries expire; holds _lock."""
        self._used = time.monotonic()
        if self._timer is None:
            self._start_timer(self.idle_seconds)

    def _start_timer(self, delay: float) -> None:
        self._timer = threading.Timer(delay, self._expire)
        self._timer.daemon = True
        self._timer.start()

    def _expire(self) -> None:
        with self._lock:
            remaining = self._used + self.idle_seconds - time.monotonic()
            if remaining > 0 and self._entries:
                self._start_timer(remaining)
                return
            dropped, self._entries, self._timer = self._entries, OrderedDict(), None
        del dropped
        # Return the worlds' memory now; an idle server allocates nothing that would
        # trigger a collection.
        gc.collect()
