"""The stored form of a history index payload (#80): text stored before version 3 reads as
itself, compressed bytes read back as the same text, and a dictionary that is not the
pinned one, an unknown format or damaged bytes are refused, never read as something else.
"""

from __future__ import annotations

import hashlib
import zlib

import pytest

from sheltersim.storage import history_payloads
from sheltersim.storage.projects import StorageError, canonical


@pytest.fixture
def fresh_dictionaries():
    # Each test loads the dictionaries itself, and leaves none it changed behind.
    history_payloads.dictionary.cache_clear()
    history_payloads._primed.cache_clear()
    yield
    history_payloads.dictionary.cache_clear()
    history_payloads._primed.cache_clear()


ROWS = [
    {},
    {"event_type": "social_interaction", "minute": 3, "agent_id": "A001", "status": "applied"},
    {"virtual_utterance_example": "少し安心できた気がします。", "minute": 1440},
    {"values": list(range(2_000)), "minute": 0},
]


@pytest.mark.parametrize("row", ROWS, ids=["empty", "event", "japanese", "long"])
def test_a_payload_reads_back_as_its_canonical_text(row):
    text = canonical(row)
    stored = history_payloads.encode(text)
    assert type(stored) is bytes and stored[0] == history_payloads.WRITE_FORMAT
    assert history_payloads.decode(stored) == text
    assert history_payloads.decode(text) == text


def test_a_typical_row_is_compressed_well_by_the_dictionary():
    row = {
        "event_type": "agent_action_lifecycle",
        "event_id": "action-lifecycle-17",
        "minute": 931,
        "status": "applied",
        "source_id": "applied-action-lifecycle-v2",
        "time_semantics": "recorded_event_minute",
        "provenance": {"policy_version": "applied-action-lifecycle-v2"},
    }
    text = canonical(row)
    assert len(history_payloads.encode(text)) * 3 < len(text.encode())


def test_the_pinned_dictionary_fits_the_deflate_window(fresh_dictionaries):
    name, expected = history_payloads.DICTIONARIES[history_payloads.WRITE_FORMAT]
    data = history_payloads.dictionary(history_payloads.WRITE_FORMAT)
    assert 0 < len(data) <= 32 * 1024
    assert hashlib.sha256(data).hexdigest() == expected
    # Built from synthetic rows: whole canonical JSON objects of the generated Scenario.
    assert data.startswith(b"{") and data.endswith(b"}")


def test_a_dictionary_other_than_the_pinned_one_is_refused(fresh_dictionaries, monkeypatch):
    stored = history_payloads.encode(canonical(ROWS[1]))
    history_payloads.dictionary.cache_clear()
    history_payloads._primed.cache_clear()
    name, _ = history_payloads.DICTIONARIES[1]
    monkeypatch.setitem(history_payloads.DICTIONARIES, 1, (name, "0" * 64))
    for action in (lambda: history_payloads.decode(stored), lambda: history_payloads.encode("{}")):
        with pytest.raises(StorageError, match="history_index_dictionary_mismatch"):
            action()


def _damaged() -> dict[str, bytes]:
    good = history_payloads.encode(canonical(ROWS[3]))
    compressor = zlib.compressobj(6, zlib.DEFLATED, -15, zdict=history_payloads.dictionary(1))
    return {
        "empty": b"",
        "format_only": b"\x01",
        "not_deflate": b"\x01\xff\xff",
        "truncated": good[:-5],
        "trailing": good + b"\x00",
        "not_utf8": b"\x01" + compressor.compress(b"\xff\xfe") + compressor.flush(),
    }


@pytest.mark.parametrize("damage", list(_damaged()))
def test_damaged_bytes_are_refused_as_a_corrupt_record(damage):
    with pytest.raises(StorageError, match="history_index_record_corrupt"):
        history_payloads.decode(_damaged()[damage])


def test_an_unknown_format_is_refused():
    with pytest.raises(StorageError, match="history_index_payload_format_unsupported"):
        history_payloads.decode(b"\x02" + history_payloads.encode("{}")[1:])
    with pytest.raises(StorageError, match="history_index_payload_format_unsupported"):
        history_payloads.decode(b"\x00")
