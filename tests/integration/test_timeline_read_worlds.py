"""Reads between saves tick on from the world the previous read of the branch ended with.

The browser's replay asks for consecutive short windows; each used to restore the save
before it again. ProjectStore.read_worlds keeps the world a read ended with, and the
frames must be the same as the frames read without it (seek, iter_worlds).
"""

import copy
from types import SimpleNamespace

import pytest

from sheltersim.core.checkpoint_cache import CheckpointHistoryCache
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import TimelineStore
from sheltersim.inspection.frames import world_frame
from sheltersim.storage.projects import ProjectStore
from sheltersim.web.replay import replay_frames
from sheltersim.web.workspace import ProjectWorkspace

FORK = 31
COMPUTED = 70


@pytest.fixture(scope="module")
def saved_run(tmp_path_factory, generated_scenario_preview, saved_timeline):
    """main saved at 30, 60 and 70; child forked at FORK and computed to COMPUTED."""
    scenario = ScenarioSpec.model_validate(copy.deepcopy(generated_scenario_preview["scenario"]))
    root = tmp_path_factory.mktemp("read-worlds")
    store, timeline = saved_timeline(root, scenario)
    timeline.advance("main", COMPUTED)
    timeline.fork("main", FORK, "child")
    timeline.advance("child", COMPUTED)
    return root


def _uncached_replay(store, branch, start, end):
    """The frames replay_frames rendered before the cache: iter_worlds from the save."""
    timeline = TimelineStore(store, "p", "r")
    reference = store.branch_reference("p", "r", branch)
    history = CheckpointHistoryCache()
    frames = []
    for world in timeline.iter_worlds(branch, start, end, history_cache=history):
        world.validate()
        document, _, encoder = history.documents(world, None)
        frames.append(
            copy.deepcopy(
                world_frame(
                    world,
                    run_id="r",
                    branch_id=branch,
                    computed_until=reference.computed_until,
                    state_hash=encoder.world_tree_digest(document),
                )
            )
        )
    return frames


def _uncached_frame(store, branch, minute):
    """The timeline frame between saves before the cache: seek() and render."""
    runtime = TimelineStore(store, "p", "r").seek(branch, minute)
    reference = store.branch_reference("p", "r", branch)
    return world_frame(
        runtime.world, run_id="r", branch_id=branch, computed_until=reference.computed_until
    )


def _count_restores(store, monkeypatch):
    calls = []
    original = store.load_restored_checkpoint

    def counted(*args, **kwargs):
        calls.append(args[2])
        return original(*args, **kwargs)

    monkeypatch.setattr(store, "load_restored_checkpoint", counted)
    return calls


@pytest.mark.parametrize("branch", ["main", "child"])
def test_consecutive_windows_tick_on_and_match_uncached_frames(saved_run, branch, monkeypatch):
    store = ProjectStore(saved_run)
    workspace = ProjectWorkspace(SimpleNamespace(store=store))
    restores = _count_restores(store, monkeypatch)
    # Across the save at 60, as playback crosses saves.
    windows = [(52, 55), (56, 59), (60, 63), (64, 66)]
    for start, end in windows:
        batch = replay_frames(store, workspace, "p", "r", [branch], start, end)
        expected = _uncached_replay(ProjectStore(saved_run), branch, start, end)
        assert batch["samples"] == expected
    # Only the first window restored a save.
    assert len(restores) == 1
    # A timeline read forward of the replay ticks on too.
    frame = TimelineStore(store, "p", "r").frame(branch, 68)
    assert len(restores) == 1
    assert frame == _uncached_frame(ProjectStore(saved_run), branch, 68)


def test_timeline_reads_between_saves_match_seek_forward_and_back(saved_run, monkeypatch):
    store = ProjectStore(saved_run)
    restores = _count_restores(store, monkeypatch)
    timeline = TimelineStore(store, "p", "r")
    reference = ProjectStore(saved_run)
    for minute, restored in [(35, 1), (35, 1), (44, 1), (40, 2), (62, 3), (69, 3)]:
        assert timeline.frame("main", minute) == _uncached_frame(reference, "main", minute)
        assert len(restores) == restored, minute


def test_two_branch_windows_match_and_keep_heads(saved_run):
    store = ProjectStore(saved_run)
    workspace = ProjectWorkspace(SimpleNamespace(store=store))
    heads = {
        key: store.branch_reference("p", "r", key).checkpoint_hash for key in ("main", "child")
    }
    for start in (40, 48):
        batch = replay_frames(store, workspace, "p", "r", ["main", "child"], start, start + 7)
        for index, branch in enumerate(("main", "child")):
            expected = _uncached_replay(ProjectStore(saved_run), branch, start, start + 7)
            assert [sample["frames"][index] for sample in batch["samples"]] == expected
    assert len(store.read_worlds) == 2
    assert {
        key: store.branch_reference("p", "r", key).checkpoint_hash for key in ("main", "child")
    } == heads


def test_a_new_head_drops_the_kept_world(tmp_path, generated_scenario, saved_timeline, monkeypatch):
    store, timeline = saved_timeline(tmp_path, generated_scenario)
    timeline.advance("main", 40)
    timeline.frame("main", 35)
    assert len(store.read_worlds) == 1
    # The branch is computed on: its save forgets the kept world of the old head.
    timeline.advance("main", 45)
    assert len(store.read_worlds) == 0
    restores = _count_restores(store, monkeypatch)
    assert timeline.frame("main", 36) == _uncached_frame(store, "main", 36)
    assert len(restores) >= 1


def test_a_world_kept_under_another_head_is_not_used(saved_run, monkeypatch):
    """Another process computed the branch on: the pointer names a head this store
    did not save, and the kept world of the old head is dropped, not ticked on."""
    store = ProjectStore(saved_run)
    timeline = TimelineStore(store, "p", "r")
    timeline.frame("main", 35)
    head = store.branch_reference("p", "r", "main").checkpoint_hash
    minute, cursor = store.read_worlds.take(("p", "r", "main"), head, 0, 99)
    store.read_worlds.put(("p", "r", "main"), "0" * 64, minute, cursor)
    restores = _count_restores(store, monkeypatch)
    assert timeline.frame("main", 36) == _uncached_frame(ProjectStore(saved_run), "main", 36)
    assert len(restores) == 1
    assert len(store.read_worlds) == 1


def test_a_read_stopped_early_keeps_nothing(saved_run):
    store = ProjectStore(saved_run)
    reads = TimelineStore(store, "p", "r").read_worlds("main", 33, 38)
    world, _ = next(reads)
    assert world.minute_index == 33
    reads.close()
    assert len(store.read_worlds) == 0


def test_cache_off_by_environment_restores_every_read(saved_run, monkeypatch):
    monkeypatch.setenv("SHELTERSIM_READ_WORLDS", "0")
    store = ProjectStore(saved_run)
    restores = _count_restores(store, monkeypatch)
    timeline = TimelineStore(store, "p", "r")
    for minute in (33, 34):
        assert timeline.frame("main", minute) == _uncached_frame(
            ProjectStore(saved_run), "main", minute
        )
    assert len(restores) == 2
    assert len(store.read_worlds) == 0
