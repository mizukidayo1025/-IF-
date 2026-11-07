"""ReadWorldCache: bounded, handed to one reader at a time, dropped on a new head."""

import pytest

from sheltersim.storage.read_worlds import ReadWorldCache

MAIN = ("p", "r", "main")
CHILD = ("p", "r", "child")


def test_take_hands_over_the_latest_cursor_in_range_once():
    cache = ReadWorldCache(per_branch=2, total=4)
    cache.put(MAIN, "h", 35, "at35")
    cache.put(MAIN, "h", 44, "at44")
    assert cache.take(MAIN, "h", 30, 40) == (35, "at35")
    # Taken: no second reader gets the same world.
    assert cache.take(MAIN, "h", 30, 40) is None
    assert cache.take(MAIN, "h", 45, 59) is None
    assert cache.take(MAIN, "h", 30, 50) == (44, "at44")
    assert len(cache) == 0


def test_another_head_drops_the_branch_cursors_only():
    cache = ReadWorldCache(per_branch=2, total=4)
    cache.put(MAIN, "old", 35, "main")
    cache.put(CHILD, "c", 35, "child")
    assert cache.take(MAIN, "new", 30, 40) is None
    assert len(cache) == 1
    assert cache.take(CHILD, "c", 30, 40) == (35, "child")


def test_bounds_per_branch_and_in_total_drop_the_least_recently_kept():
    cache = ReadWorldCache(per_branch=2, total=3)
    for minute in (1, 2, 3):
        cache.put(MAIN, "h", minute, f"main{minute}")
    assert len(cache) == 2
    assert cache.take(MAIN, "h", 1, 1) is None
    cache.put(CHILD, "c", 5, "child5")
    cache.put(CHILD, "c", 6, "child6")
    assert len(cache) == 3
    # main2 was kept first of what remained.
    assert cache.take(MAIN, "h", 2, 2) is None
    assert cache.take(MAIN, "h", 3, 3) == (3, "main3")


def test_drop_forgets_a_run_or_a_branch():
    cache = ReadWorldCache(per_branch=2, total=8)
    cache.put(MAIN, "h", 1, "a")
    cache.put(CHILD, "c", 1, "b")
    cache.put(("p", "other", "main"), "h", 1, "c")
    cache.drop("p", "r", "child")
    assert len(cache) == 2
    cache.drop("p", "r")
    assert len(cache) == 1


def test_idle_cursors_expire():
    cache = ReadWorldCache(per_branch=2, total=4, idle_seconds=0.0)
    cache.put(MAIN, "h", 1, "a")
    cache._expire()
    assert len(cache) == 0


@pytest.mark.parametrize(("raw", "total"), [("0", 0), ("6", 6), ("", 4)])
def test_total_from_environment(monkeypatch, raw, total):
    monkeypatch.setenv("SHELTERSIM_READ_WORLDS", raw)
    cache = ReadWorldCache()
    assert cache.total == total
    cache.put(MAIN, "h", 1, "a")
    assert len(cache) == min(total, 1)


@pytest.mark.parametrize("raw", ["-1", "many"])
def test_invalid_total_from_environment_is_refused(monkeypatch, raw):
    monkeypatch.setenv("SHELTERSIM_READ_WORLDS", raw)
    with pytest.raises(ValueError, match="SHELTERSIM_READ_WORLDS"):
        ReadWorldCache()
