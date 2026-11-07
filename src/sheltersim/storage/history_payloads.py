"""The stored form of a history index row's payload (#80).

A row's payload is its canonical JSON text, and payload_hash is the SHA-256 of that text.
Rows written by index version 3 store it as a BLOB: one format byte, then the UTF-8 text as
raw deflate (RFC 1951, a window of 2**15 bytes, no zlib header or checksum) primed with the
preset dictionary that the format byte names. Rows stored as TEXT, as every row before
version 3 was, are the text itself. SQLite keeps the two apart by storage class (typeof), so
one file may hold both, and an old row is never written again.

The bytes of a compressed payload depend on the zlib build that wrote them, so a payload is
compared, hashed and read only as decoded text. A dictionary never changes once rows use it:
each one is a file of this package pinned by its SHA-256, checked when it is first loaded.
A new dictionary takes a new format byte, and the old ones stay readable.
"""

from __future__ import annotations

import functools
import hashlib
import zlib
from importlib import resources
from typing import Any

from sheltersim.storage.projects import StorageError

# Format byte -> (dictionary file in this package, its SHA-256). Built from synthetic rows by
# scripts/build_history_dictionary.py; never edit or rebuild a listed file.
DICTIONARIES = {
    1: (
        "history_payloads_1.zdict",
        "816feb82c64f4cc9646f216e7b0492331a1633c458fbcf3468bfd4c334d1a94a",
    ),
}
# The format new rows are written in.
WRITE_FORMAT = 1
# The indexing process sets the pace of a long computation, so its compression time adds to
# the computation's. Level 3 compresses the payloads of a generated Run about 6 times; level 6
# shrinks them about a tenth more for about 1.5 times the time (#80).
LEVEL = 3
_WINDOW_BITS = -15


@functools.cache
def dictionary(format_byte: int) -> bytes:
    """The preset dictionary of a format, checked against its pinned SHA-256."""
    entry = DICTIONARIES.get(format_byte)
    if entry is None:
        raise StorageError("history_index_payload_format_unsupported")
    name, expected = entry
    data = resources.files(__package__).joinpath(name).read_bytes()
    if hashlib.sha256(data).hexdigest() != expected:
        raise StorageError("history_index_dictionary_mismatch")
    return data


@functools.cache
def _primed(format_byte: int) -> Any:
    # Priming with a dictionary costs as much as compressing a row; copy a primed one.
    return zlib.compressobj(
        LEVEL,
        zlib.DEFLATED,
        _WINDOW_BITS,
        zlib.DEF_MEM_LEVEL,
        zlib.Z_DEFAULT_STRATEGY,
        zdict=dictionary(format_byte),
    )


def encode(text: str) -> bytes:
    """The stored form of a payload's canonical JSON text."""
    compressor = _primed(WRITE_FORMAT).copy()
    return bytes((WRITE_FORMAT,)) + compressor.compress(text.encode()) + compressor.flush()


def decode(stored: str | bytes) -> str:
    """The canonical JSON text of a stored payload, whichever form it was stored in."""
    if isinstance(stored, str):
        return stored
    if not stored:
        raise StorageError("history_index_record_corrupt: rebuild from immutable source")
    decompressor = zlib.decompressobj(_WINDOW_BITS, zdict=dictionary(stored[0]))
    try:
        data = decompressor.decompress(stored[1:]) + decompressor.flush()
        if not decompressor.eof or decompressor.unused_data:
            raise ValueError("incomplete or trailing data")
        return data.decode()
    except (zlib.error, ValueError) as exc:
        raise StorageError("history_index_record_corrupt: rebuild from immutable source") from exc
