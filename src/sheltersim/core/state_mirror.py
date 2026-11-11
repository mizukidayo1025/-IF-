"""The tree unported Python reads and writes for a namespace Rust owns.

A world keeps its Rust state in `world.__dict__["_core"]`, made only with the Rust kernels on. A
namespace is owned when it is listed in OWNED_NAMESPACES: Rust holds it in a typed row, and
`world.model_state[name]` is a tree of TrackedDict and TrackedList. Any other key of model_state
whose name is a str is owned the same way when it is first stored, with a ValueRow, a `V` that any
JSON fits (lane 5-3 of Issue #152): its tree stays, and a value that does not fit V taints that key
alone. These are real dicts and lists, so isinstance, json, == and every read work as before and
run the inherited C code. Every mutator also tells the tree's sink, which keeps the row equal to
the tree. A plain container is converted when it is inserted, so a write through a name it had
before the insert reaches neither. See docs/rust-port/lanes/p4-world-core.md for what a lane does.

A namespace in DETACHED_NAMESPACES has no tree: its row alone holds it, and model_state holds a
placeholder under its name (_DetachingTable). Reading it from Python builds the tree again from
the row, and writes through that tree reach the row as for any owned namespace; the next tick
(core/engine.py, detach_unread) drops the tree again. Whole-world readers (WorldState's canonical
JSON, same_state and validation, Checkpoint capture, deepcopy) read the row instead, through
plain_model_state, read_only_state and the row's bytes, and build no tree.

The maps of entities are IdMap, which keep the Rust rows of the entities equal to the objects
(docs/rust-port/lanes/p6-entity-windows.md).

A sink is sheltersim_native.NamespaceMirror, or a test's object with the same methods. A node's
_sink is None out of any tree: it then behaves as the plain container, like the value `pop`
returns, and an insert moves it in as itself.
"""

from __future__ import annotations

import contextlib
import copy
import json
import operator
from collections.abc import Callable, Iterator, Mapping, Sequence
from typing import Any

from sheltersim import native

_MISSING = object()


class DetachedNodeError(RuntimeError):
    """A write to a node of a detached namespace, which could reach neither the row nor a tree."""


class _DeadSink:
    """The sink of a node whose namespace was detached. Reading such a node is reading the past."""

    def __getattr__(self, name: str) -> Any:
        raise DetachedNodeError(
            "a tree node was kept across a detach of its namespace and then written; "
            "read the node again from world.model_state"
        )


_DEAD_SINK = _DeadSink()


class TrackedDict(dict):
    """A dict whose mutators tell its sink. Reads are the inherited C code."""

    __slots__ = ("_sink", "_node", "__weakref__")

    def __new__(cls, *_arguments: Any, **_keywords: Any) -> TrackedDict:
        self = super().__new__(cls)
        self._sink = None
        self._node = None
        return self

    def __init__(self, *arguments: Any, **keywords: Any) -> None:
        if self._sink is None:
            dict.__init__(self, *arguments, **keywords)
        else:
            self.update(*arguments, **keywords)

    def __setitem__(self, key: Any, value: Any) -> None:
        sink = self._sink
        if sink is None:
            dict.__setitem__(self, key, value)
        else:
            sink.dict_set(self, self._node, key, value)

    def __delitem__(self, key: Any) -> None:
        sink = self._sink
        if sink is None:
            dict.__delitem__(self, key)
            return
        forward = sink.forward
        # pop raises the same KeyError(key) as del.
        old = dict.pop(self, key)
        sink.release(old)
        forward(self, self._node, "del", key)

    def pop(self, key: Any, *default: Any) -> Any:
        sink = self._sink
        if sink is None:
            return dict.pop(self, key, *default)
        forward = sink.forward
        value = dict.pop(self, key, _MISSING)
        if value is _MISSING:
            return dict.pop(self, key, *default)
        sink.release(value)
        forward(self, self._node, "del", key)
        return value

    def popitem(self) -> tuple[Any, Any]:
        sink = self._sink
        if sink is None:
            return dict.popitem(self)
        forward = sink.forward
        key, value = dict.popitem(self)
        sink.release(value)
        forward(self, self._node, "del", key)
        return key, value

    def setdefault(self, key: Any, default: Any = None) -> Any:
        if self._sink is None or key in self:
            return dict.setdefault(self, key, default)
        self[key] = default
        # The stored node, not the plain default: this is what makes setdefault's result safe.
        return dict.__getitem__(self, key)

    def update(self, *arguments: Any, **keywords: Any) -> None:
        if self._sink is None:
            dict.update(self, *arguments, **keywords)
            return
        for key, value in dict(*arguments, **keywords).items():
            self[key] = value

    def clear(self) -> None:
        sink = self._sink
        if sink is None:
            dict.clear(self)
            return
        forward = sink.forward
        old = list(dict.values(self))
        dict.clear(self)
        for value in old:
            sink.release(value)
        forward(self, self._node, "clear")

    def __ior__(self, other: Any) -> TrackedDict:
        self.update(other)
        return self

    def copy(self) -> dict[Any, Any]:
        return dict(self)

    def __copy__(self) -> dict[Any, Any]:
        return dict(self)

    def __deepcopy__(self, memo: dict[int, Any]) -> dict[Any, Any]:
        result: dict[Any, Any] = {}
        memo[id(self)] = result
        for key, value in dict.items(self):
            result[copy.deepcopy(key, memo)] = copy.deepcopy(value, memo)
        return result

    def __reduce_ex__(self, _protocol: Any) -> tuple[Any, ...]:
        return (dict, (dict(self),))


class TrackedList(list):
    """A list whose mutators tell its sink. Reads are the inherited C code."""

    __slots__ = ("_sink", "_node", "__weakref__")

    def __new__(cls, *_arguments: Any, **_keywords: Any) -> TrackedList:
        self = super().__new__(cls)
        self._sink = None
        self._node = None
        return self

    def __init__(self, *arguments: Any) -> None:
        if self._sink is None:
            list.__init__(self, *arguments)
        else:
            self[:] = list(*arguments)

    def __setitem__(self, index: Any, value: Any) -> None:
        sink = self._sink
        if sink is None:
            list.__setitem__(self, index, value)
        elif isinstance(index, slice):
            self._set_slice(sink, index, value)
        else:
            self._set_index(sink, index, value)

    def _set_index(self, sink: Any, index: Any, value: Any) -> None:
        size = list.__len__(self)
        if not hasattr(index, "__index__") or not -size <= operator.index(index) < size:
            list.__setitem__(self, index, value)  # raises list's own error
        position = operator.index(index)
        position = position + size if position < 0 else position
        old = list.__getitem__(self, position)
        if old is value:
            return
        forward = sink.forward
        stored = sink.adopt(self._node, None, value)
        list.__setitem__(self, position, stored)
        sink.release(old)
        forward(self, self._node, "set", position, stored)

    def _set_slice(self, sink: Any, index: slice, value: Any) -> None:
        forward = sink.forward
        if not hasattr(value, "__iter__") and not hasattr(value, "__getitem__"):
            list.__setitem__(self, index, value)  # raises list's own error
        items = list(value)
        size = list.__len__(self)
        old = list.__getitem__(self, index)
        # Stored as they are first, so a wrong size raises list's own error before any change.
        list.__setitem__(self, index, items)
        for item in old:
            sink.release(item)
        start, stop, step = index.indices(size)
        positions = range(start, start + len(items)) if step == 1 else range(start, stop, step)
        stored = []
        for position in positions:
            node = sink.adopt(self._node, None, list.__getitem__(self, position))
            list.__setitem__(self, position, node)
            stored.append(node)
        forward(self, self._node, "slice_set", index, stored)

    def __delitem__(self, index: Any) -> None:
        sink = self._sink
        if sink is None:
            list.__delitem__(self, index)
            return
        forward = sink.forward
        size = list.__len__(self)
        if isinstance(index, slice):
            start, stop, step = index.indices(size)
            old = list.__getitem__(self, index)
            list.__delitem__(self, index)
            if not old:
                return  # the usual `del history[:-N]` of a list within its limit
            for item in old:
                sink.release(item)
            if step == 1:
                forward(self, self._node, "del_range", start, stop)
            else:
                forward(self, self._node, "replace_all")
            return
        if not hasattr(index, "__index__") or not -size <= operator.index(index) < size:
            list.__delitem__(self, index)  # raises list's own error
        position = operator.index(index)
        position = position + size if position < 0 else position
        old = list.__getitem__(self, position)
        list.__delitem__(self, position)
        sink.release(old)
        forward(self, self._node, "del", position)

    def append(self, value: Any) -> None:
        sink = self._sink
        if sink is None:
            list.append(self, value)
        else:
            sink.list_append(self, self._node, value)

    def extend(self, values: Any) -> None:
        sink = self._sink
        if sink is None:
            list.extend(self, values)
            return
        forward = sink.forward
        stored = [sink.adopt(self._node, None, item) for item in list(values)]
        list.extend(self, stored)
        forward(self, self._node, "extend", stored)

    def insert(self, index: Any, value: Any) -> None:
        sink = self._sink
        if sink is None:
            list.insert(self, index, value)
            return
        size = list.__len__(self)
        position = operator.index(index)
        position = max(position + size, 0) if position < 0 else min(position, size)
        forward = sink.forward
        stored = sink.adopt(self._node, None, value)
        list.insert(self, position, stored)
        forward(self, self._node, "insert", position, stored)

    def remove(self, value: Any) -> None:
        if self._sink is None or value not in self:
            list.remove(self, value)
        else:
            del self[list.index(self, value)]

    def pop(self, index: Any = -1) -> Any:
        sink = self._sink
        if sink is None:
            return list.pop(self, index)
        forward = sink.forward
        position = operator.index(index)
        if position < 0:
            position += list.__len__(self)
        value = list.pop(self, index)
        sink.release(value)
        forward(self, self._node, "del", position)
        return value

    def clear(self) -> None:
        sink = self._sink
        if sink is None:
            list.clear(self)
            return
        forward = sink.forward
        old = list(list.__iter__(self))
        list.clear(self)
        for item in old:
            sink.release(item)
        forward(self, self._node, "clear")

    def sort(self, *, key: Any = None, reverse: bool = False) -> None:
        sink = self._sink
        if sink is None:
            list.sort(self, key=key, reverse=reverse)
            return
        forward = sink.forward
        list.sort(self, key=key, reverse=reverse)
        forward(self, self._node, "replace_all")

    def reverse(self) -> None:
        sink = self._sink
        if sink is None:
            list.reverse(self)
            return
        forward = sink.forward
        list.reverse(self)
        forward(self, self._node, "reverse")

    def __iadd__(self, values: Any) -> TrackedList:
        self.extend(values)
        return self

    def __imul__(self, count: Any) -> TrackedList:
        if not hasattr(count, "__index__"):
            # The operator's own text; list.__imul__ called by name words it differently.
            kind = type(count).__name__
            raise TypeError(f"can't multiply sequence by non-int of type '{kind}'")
        sink = self._sink
        if sink is None:
            return list.__imul__(self, count)
        forward = sink.forward
        size = list.__len__(self)
        if operator.index(count) <= 0:
            self.clear()
            return self
        list.__imul__(self, count)
        # Each repeat is an item of its own: a node has one place.
        for position in range(size, list.__len__(self)):
            list.__setitem__(
                self, position, sink.adopt(self._node, None, list.__getitem__(self, position))
            )
        forward(self, self._node, "replace_all")
        return self

    def copy(self) -> list[Any]:
        return list(self)

    def __copy__(self) -> list[Any]:
        return list(self)

    def __deepcopy__(self, memo: dict[int, Any]) -> list[Any]:
        result: list[Any] = []
        memo[id(self)] = result
        for value in list.__iter__(self):
            result.append(copy.deepcopy(value, memo))
        return result

    def __reduce_ex__(self, _protocol: Any) -> tuple[Any, ...]:
        return (list, (list(self),))


class NamespaceTable(dict):
    """world.model_state while a namespace is owned. Reads are the inherited C code.

    Each owned namespace is a tree with a mirror. A str key no lane owns gets a mirror with a
    ValueRow from _core when it is first stored (_value_mirror), and keeps it while the key comes
    and goes; any other key is stored as it is. Writing a whole owned namespace rebuilds its tree
    and row, so it costs O(namespace): the code doing it per tick moves with the namespace's port.
    """

    __slots__ = ("_mirrors", "_core", "__weakref__")

    def __new__(cls, *_arguments: Any, **_keywords: Any) -> NamespaceTable:
        self = super().__new__(cls)
        self._mirrors = {}
        self._core = None
        return self

    def __setitem__(self, key: Any, value: Any) -> None:
        mirror = self._mirrors.get(key)
        if mirror is None:
            mirror = _value_mirror(self, key)
        if mirror is None:
            dict.__setitem__(self, key, value)
        elif dict.get(self, key, _MISSING) is not value:
            _store_owned(self, key, mirror, value)

    def __delitem__(self, key: Any) -> None:
        dict.__delitem__(self, key)
        mirror = self._mirrors.get(key)
        if mirror is not None:
            mirror.clear_root()

    def pop(self, key: Any, *default: Any) -> Any:
        mirror = self._mirrors.get(key)
        if mirror is None:
            return dict.pop(self, key, *default)
        value = dict.pop(self, key, _MISSING)
        if value is _MISSING:
            return dict.pop(self, key, *default)
        mirror.clear_root()
        return value

    def popitem(self) -> tuple[Any, Any]:
        key, value = dict.popitem(self)
        mirror = self._mirrors.get(key)
        if mirror is not None:
            mirror.clear_root()
        return key, value

    def setdefault(self, key: Any, default: Any = None) -> Any:
        if key not in self:
            self[key] = default
            # A namespace of DETACHED_NAMESPACES is stored in its row alone: the caller writes
            # to what it gets, so it gets the tree, built again from the row.
            return self[key]
        return dict.__getitem__(self, key)

    def update(self, *arguments: Any, **keywords: Any) -> None:
        for key, value in dict(*arguments, **keywords).items():
            self[key] = value

    def clear(self) -> None:
        names = list(dict.keys(self))
        dict.clear(self)
        for name in names:
            mirror = self._mirrors.get(name)
            if mirror is not None:
                mirror.clear_root()

    def __ior__(self, other: Any) -> NamespaceTable:
        self.update(other)
        return self

    def copy(self) -> dict[Any, Any]:
        return dict(self)

    def __copy__(self) -> dict[Any, Any]:
        return dict(self)

    def __deepcopy__(self, memo: dict[int, Any]) -> dict[Any, Any]:
        result: dict[Any, Any] = {}
        memo[id(self)] = result
        for key, value in dict(self).items():
            result[copy.deepcopy(key, memo)] = copy.deepcopy(value, memo)
        return result

    def __reduce_ex__(self, _protocol: Any) -> tuple[Any, ...]:
        return (dict, (dict(self),))


class _DetachingTable(NamespaceTable):
    """model_state while a namespace is detached: its key stays, holding a placeholder.

    Keys, len and `in` read the right thing as they are. A read of a value builds its tree first,
    so the placeholder never gets out to Python. Comparing, copying and pickling the table read
    the rows instead and build nothing. Kernels read the table through the C API and see the
    placeholder: each one that reads a detached namespace reads its row, or asks for the tree
    (core/tracked.rs). The class goes back to NamespaceTable, with C-speed reads, when nothing is
    detached any more.
    """

    __slots__ = ()

    def _build(self, key: Any) -> None:
        if dict.get(self, key, None) is _DETACHED:
            _rebuild(self, key)

    def _build_all(self) -> None:
        for key, value in list(dict.items(self)):
            if value is _DETACHED:
                _rebuild(self, key)

    def __getitem__(self, key: Any) -> Any:
        self._build(key)
        return dict.__getitem__(self, key)

    def get(self, key: Any, default: Any = None) -> Any:
        self._build(key)
        return dict.get(self, key, default)

    # Without this, dict(table) takes its C path and reads the placeholder. keys() stays right.
    def __iter__(self) -> Iterator[Any]:
        return dict.__iter__(self)

    def values(self) -> Any:
        self._build_all()
        return dict.values(self)

    def items(self) -> Any:
        self._build_all()
        return dict.items(self)

    def pop(self, key: Any, *default: Any) -> Any:
        self._build(key)
        return NamespaceTable.pop(self, key, *default)

    def popitem(self) -> tuple[Any, Any]:
        self._build_all()
        return dict.popitem(self)

    def setdefault(self, key: Any, default: Any = None) -> Any:
        self._build(key)
        return NamespaceTable.setdefault(self, key, default)

    # A whole write replaces the row, and a delete drops it: neither needs the old tree.
    def __setitem__(self, key: Any, value: Any) -> None:
        NamespaceTable.__setitem__(self, key, value)
        _settle(self)

    def __delitem__(self, key: Any) -> None:
        NamespaceTable.__delitem__(self, key)
        _settle(self)

    def __eq__(self, other: object) -> bool:
        if not isinstance(other, dict):
            return NotImplemented
        return _same_tables(self, other)

    def __ne__(self, other: object) -> bool:
        if not isinstance(other, dict):
            return NotImplemented
        return not _same_tables(self, other)

    __hash__ = None  # type: ignore[assignment]

    def __or__(self, other: Any) -> Any:
        self._build_all()
        return dict.__or__(self, other)

    def __ror__(self, other: Any) -> Any:
        self._build_all()
        return dict.__ror__(self, other)

    def __repr__(self) -> str:
        return dict.__repr__(plain_model_state(self))

    # A shallow copy shares the trees, so it needs them.
    def copy(self) -> dict[Any, Any]:
        self._build_all()
        return dict(dict.items(self))

    __copy__ = copy

    def __deepcopy__(self, memo: dict[int, Any]) -> dict[Any, Any]:
        # A detached namespace's document is new already.
        result: dict[Any, Any] = {}
        memo[id(self)] = result
        for key, value in dict.items(self):
            result[copy.deepcopy(key, memo)] = (
                self._mirrors[key].document() if value is _DETACHED else copy.deepcopy(value, memo)
            )
        return result

    def __reduce_ex__(self, _protocol: Any) -> tuple[Any, ...]:
        return (dict, (plain_model_state(self),))


_DETACHED = object()

# The owned namespaces their rows alone hold (L14 stage 1, docs/rust-port/step4-plan.md section
# 5). Nothing reads them outside the tick but whole-world readers, which read the rows, and inside
# it only kernels, which read the rows too; so a world keeps no tree for them. A whole write loads
# the row directly; a Python read builds a tree, which detach_unread drops at the next tick.
DETACHED_NAMESPACES = frozenset(
    {
        "psychological_distress_v1",
        "social_propagation_v1",
        "temporary_spaces_v1",
        "social_network_v1",
        "population_presence_v1",
        "population_lifecycle_v1",
        "population_dynamics_v1",
        "geometry_overlay_v1",
        "movement_v1",
        "agent_dynamics_v1",
        "information_spread_v1",
        "staff_dynamics_v1",
        "facility_queues_v1",
        "effect_program_v1",
        "body_state_v1",
        "shared_operations_v1",
        "resource_ledger_v1",
        "resource_service_v1",
        "staff_operations_v1",
    }
)

# Called with a namespace's name each time a read builds its tree again: for the tests and the
# benchmarks that find where Python still reads a detached namespace. None in a Run.
_REBUILD_OBSERVER: Callable[[str], None] | None = None


def _rebuild(table: NamespaceTable, key: Any) -> None:
    if _REBUILD_OBSERVER is not None:
        _REBUILD_OBSERVER(key)
    dict.__setitem__(table, key, table._mirrors[key].rebuild())
    _settle(table)


def _settle(table: NamespaceTable) -> None:
    """Give the table the class that reads what it holds."""
    detached = any(value is _DETACHED for value in dict.values(table))
    table.__class__ = _DetachingTable if detached else NamespaceTable


def _value_mirror(table: NamespaceTable, key: Any) -> Any:
    """A new mirror with a ValueRow for a model_state key no lane owns, or None to store it plain.

    Made the first time the key is stored, and kept in the table while the key is absent, as the
    mirrors of OWNED_NAMESPACES are; deleting the key drops its tree and row (clear_root). A table
    with no core (a plain holder), a key that is not a str, and the name of an owned field of the
    world, whose slot the core has already, store the value as it is.
    """
    core = table._core
    if core is None or type(key) is not str or core.mirror(key) is not None:
        return None
    mirror = core.own_value(key)
    table._mirrors[key] = mirror
    return mirror


def _store_owned(table: NamespaceTable, key: Any, mirror: Any, value: Any) -> None:
    """table[key] = value for an owned namespace: its row, and a tree unless it stays detached."""
    if key in DETACHED_NAMESPACES and isinstance(value, dict):
        if mirror.load_detached(value, _DEAD_SINK):
            dict.__setitem__(table, key, _DETACHED)
            table.__class__ = _DetachingTable
            return
    # A value that does not fit the row is a tree, and the namespace is tainted, as before.
    dict.__setitem__(table, key, mirror.replace_root(value))


def detach(world: Any, name: str) -> bool:
    """Drop the tree of an owned namespace with no per-tick Python reader left.

    The row alone then holds it, and the next Python read rebuilds the tree. A node kept from
    before raises DetachedNodeError on a write, and its reads are of the past. Returns False for a
    tainted or absent namespace, which stays as it is.
    """
    table = world.model_state
    mirror = table._mirrors[name]
    held = dict.get(table, name, _MISSING)
    if held is _DETACHED:
        return True
    if held is _MISSING or not mirror.detach(_DEAD_SINK):
        return False
    dict.__setitem__(table, name, _DETACHED)
    table.__class__ = _DetachingTable
    return True


def detach_unread(world: Any) -> None:
    """Drop the trees a read built again for the namespaces of DETACHED_NAMESPACES.

    Called where a tick starts and ends (core/engine.py), so a read between ticks costs one
    rebuild and the kernels of the next tick find the row alone. A tainted namespace keeps its tree.
    """
    table = world.__dict__.get("model_state")
    if not isinstance(table, NamespaceTable):
        return
    for name in DETACHED_NAMESPACES:
        held = dict.get(table, name, _MISSING)
        if held is not _MISSING and held is not _DETACHED and name in table._mirrors:
            detach(world, name)


def _held_value(table: Any, key: Any, held: Any) -> Any:
    """What a read of table[key] gives, without building a tree: a detached one's document."""
    return table._mirrors[key].document() if held is _DETACHED else held


def plain_model_state(
    table: Any, known: Any = (), emptied: Mapping[str, Sequence[str]] | None = None
) -> Any:
    """model_state with each detached namespace's row as a plain document, building no tree.

    The documents are new and nothing follows them: read them, never write. A name in known keeps
    the placeholder, for a caller that has that namespace's bytes and reads nothing else of it
    (checkpoint_cache.documents). A detached namespace in emptied has an empty list under each
    of its keys there that holds a list, and those items are not made: for a caller that reads
    them by NamespaceMirror.items_json. Anything but a table with a detached namespace is
    returned as it is.
    """
    if not isinstance(table, _DetachingTable):
        return table
    emptied = emptied or {}
    return {
        key: held
        if key in known
        else table._mirrors[key].document_emptied(list(emptied[key]))
        if held is _DETACHED and key in emptied
        else _held_value(table, key, held)
        for key, held in dict.items(table)
    }


def read_only_state(world: Any, name: str, default: Any = None) -> Any:
    """world.model_state.get(name, default) for a reader that never writes to it.

    A detached namespace comes back as a new document from its row, and no tree is built, so a
    write to the result reaches nothing (decision D3 of step4-plan.md: the validators in Python
    read the namespace this way).
    """
    table = world.model_state
    if not isinstance(table, dict):
        return table.get(name, default)
    held = dict.get(table, name, _MISSING)
    if held is _MISSING:
        return default
    return _held_value(table, name, held)


def read_only_part(world: Any, name: str, key: str, default: Any = None) -> Any:
    """world.model_state.get(name, {}).get(key, default) for a reader that never writes to it.

    A detached namespace gives a new document of that one field of its row, and neither the tree
    nor the rest of the document is made: for a reader of a part of a large namespace, such as a
    frame (inspection/frames.py). An attached one gives its tree's value, as before.
    """
    table = world.model_state
    if not isinstance(table, dict):
        return table.get(name, {}).get(key, default)
    held = dict.get(table, name, _MISSING)
    if held is _MISSING:
        return default
    if held is _DETACHED:
        part = table._mirrors[name].document_part(key)
        return default if part is None else part[0]
    return held.get(key, default) if isinstance(held, dict) else default


class RowItems(Sequence[Any]):
    """A list of a detached namespace's row, read only: each read makes only the items it reads.

    len() makes nothing; an index or a slice makes new documents of those items, as
    read_only_state would hold there. For a reader of the newest items of a long history (a
    tick's check, a frame), which would otherwise make all 20,000.
    """

    __slots__ = ("_mirror", "_key", "_count")

    def __init__(self, mirror: Any, key: str, count: int) -> None:
        self._mirror = mirror
        self._key = key
        self._count = count

    def __len__(self) -> int:
        return self._count

    def __getitem__(self, index: Any) -> Any:
        if isinstance(index, slice):
            start, stop, step = index.indices(self._count)
            if step != 1:
                return self._mirror.part_slice(self._key, 0, self._count)[index]
            return self._mirror.part_slice(self._key, start, max(start, stop))
        position = operator.index(index)
        if position < 0:
            position += self._count
        if not 0 <= position < self._count:
            raise IndexError("list index out of range")
        return self._mirror.part_slice(self._key, position, position + 1)[0]

    def __iter__(self) -> Iterator[Any]:
        return iter(self._mirror.part_slice(self._key, 0, self._count))

    def __repr__(self) -> str:
        return f"RowItems({self._key!r}, {self._count} items)"


def read_only_items(world: Any, name: str, key: str, default: Any = None) -> Any:
    """read_only_part for a list: a detached namespace gives RowItems, which makes no item until
    it is read. An attached one gives its tree's list, as before. Never write to it."""
    table = world.model_state
    if isinstance(table, dict) and dict.get(table, name, _MISSING) is _DETACHED:
        mirror = table._mirrors[name]
        count = mirror.part_len(key)
        if count is not None:
            return RowItems(mirror, key, count)
    return read_only_part(world, name, key, default)


def holds_detached(world: Any, name: str) -> bool:
    """Whether model_state holds this namespace detached: its row alone, and no tree."""
    table = world.__dict__.get("model_state")
    return isinstance(table, dict) and dict.get(table, name, _MISSING) is _DETACHED


def detached_row(world: Any, name: str) -> Any:
    """The mirror of a namespace of DETACHED_NAMESPACES whose row holds its state, or None.

    Its tree may be there too, after a Python read, but the row is what the namespace is: a
    save reads that (NamespaceMirror.items_json), so whether a frame read it between two saves
    changes nothing of what the save reads.
    """
    if name not in DETACHED_NAMESPACES:
        return None
    table = world.__dict__.get("model_state")
    if not isinstance(table, NamespaceTable):
        return None
    mirror = table._mirrors.get(name)
    if mirror is None or not mirror.has_row or not _table_holds_row_of(table, mirror, name):
        return None
    return mirror


def validated_state(world: Any, name: str, history_memo: Any = None, default: Any = None) -> Any:
    """What a validation reads of a namespace: model_state.get(name, default), building no tree.

    A detached namespace is a document: the one a save made already (history_memo, which
    core/checkpoint_cache.py fills with the documents of its histories), or else a new one from
    the row (read_only_state). Never write to it.
    """
    if history_memo is not None:
        state = history_memo.states.get(name, _MISSING)
        if state is not _MISSING:
            return state
    return read_only_state(world, name, default)


def same_detached(
    mine: Any, theirs: Any, name: str, known: Mapping[str, int] | None = None
) -> bool | None:
    """Whether two tables hold the same value under name, if one of them holds it detached.

    None if neither does: the caller compares the values as always. The same row bytes are the
    same value; other bytes are compared as documents, as == compares the trees. known holds, by
    key, how many leading items of a list of the namespace are known to be equal, as
    WorldState.same_state's known_equal_histories: against a plain value those are not read.
    """
    first = dict.get(mine, name, _MISSING)
    second = dict.get(theirs, name, _MISSING)
    if first is not _DETACHED and second is not _DETACHED:
        return None
    if first is _DETACHED and second is _DETACHED:
        try:
            if mine._mirrors[name].row_json() == theirs._mirrors[name].row_json():
                return True
        except native.Unsupported:
            pass
    if first is _MISSING or second is _MISSING:
        return False
    if known and (first is _DETACHED) != (second is _DETACHED):
        table, value = (mine, second) if first is _DETACHED else (theirs, first)
        return _same_row_and_value(table._mirrors[name], value, known)
    return bool(_held_value(mine, name, first) == _held_value(theirs, name, second))


def _same_row_and_value(mirror: Any, value: Any, known: Mapping[str, int]) -> bool:
    """The row's document == value, making no item of a list known equal (same_detached)."""
    if not isinstance(value, dict):
        return False
    lists = [key for key in known if mirror.part_len(key) is not None]
    document = mirror.document_emptied(lists)
    if document.keys() != value.keys():
        return False
    for key, held in document.items():
        other = value[key]
        if key not in lists:
            if held != other:
                return False
            continue
        size = mirror.part_len(key)
        count = min(known[key], size)
        if not isinstance(other, list) or len(other) != size:
            return False
        if count < size and mirror.part_slice(key, count, size) != other[count:]:
            return False
    return True


def _same_tables(mine: Any, theirs: Any) -> bool:
    """mine == theirs for a table with a detached namespace, building no tree."""
    if dict.__len__(mine) != dict.__len__(theirs):
        return False
    for key, held in dict.items(mine):
        if not dict.__contains__(theirs, key):
            return False
        detached = same_detached(mine, theirs, key)
        if detached is None:
            if held != dict.__getitem__(theirs, key):
                return False
        elif not detached:
            return False
    return True


# ---- The entities of a world (native/src/state/entities.rs) --------------------------------------


class IdMap(dict):
    """world.agents and the other maps of entities. Reads are the inherited C code.

    An entity stored here is attached: its row is in the world's EntityStore and its own writes go
    there (domain/models.py). An entity that leaves works on as a plain object, and a map that
    leaves its world (`_store` None) is a plain dict of plain objects.
    """

    __slots__ = ("_store", "_table", "__weakref__")

    def __new__(cls, *_arguments: Any, **_keywords: Any) -> IdMap:
        self = super().__new__(cls)
        self._store = None
        self._table = 0
        return self

    def __init__(self, *arguments: Any, **keywords: Any) -> None:
        if self._store is None:
            dict.__init__(self, *arguments, **keywords)
        else:
            self.update(*arguments, **keywords)

    def __setitem__(self, key: Any, value: Any) -> None:
        store = self._store
        if store is None:
            dict.__setitem__(self, key, value)
        else:
            store.map_set(self, self._table, key, value)

    def __delitem__(self, key: Any) -> None:
        store = self._store
        if store is None:
            dict.__delitem__(self, key)
            return
        # pop raises the same KeyError(key) as del.
        store.map_removed(self._table, key, dict.pop(self, key))

    def pop(self, key: Any, *default: Any) -> Any:
        store = self._store
        if store is None:
            return dict.pop(self, key, *default)
        value = dict.pop(self, key, _MISSING)
        if value is _MISSING:
            return dict.pop(self, key, *default)
        store.map_removed(self._table, key, value)
        return value

    def popitem(self) -> tuple[Any, Any]:
        key, value = dict.popitem(self)
        store = self._store
        if store is not None:
            store.map_removed(self._table, key, value)
        return key, value

    def setdefault(self, key: Any, default: Any = None) -> Any:
        if key not in self:
            self[key] = default
        return dict.__getitem__(self, key)

    def update(self, *arguments: Any, **keywords: Any) -> None:
        if self._store is None:
            dict.update(self, *arguments, **keywords)
            return
        for key, value in dict(*arguments, **keywords).items():
            self[key] = value

    def clear(self) -> None:
        store = self._store
        if store is None:
            dict.clear(self)
            return
        items = list(dict.items(self))
        dict.clear(self)
        for key, value in items:
            store.map_removed(self._table, key, value)

    def __ior__(self, other: Any) -> IdMap:
        self.update(other)
        return self

    def copy(self) -> dict[Any, Any]:
        return dict(self)

    def __copy__(self) -> dict[Any, Any]:
        return dict(self)

    def __deepcopy__(self, memo: dict[int, Any]) -> dict[Any, Any]:
        result: dict[Any, Any] = {}
        memo[id(self)] = result
        for key, value in dict.items(self):
            result[copy.deepcopy(key, memo)] = copy.deepcopy(value, memo)
        return result

    def __reduce_ex__(self, _protocol: Any) -> tuple[Any, ...]:
        return (dict, (dict(self),))


# world.<name> -> the table of the EntityStore: the numbers of native/src/state/entities.rs. The
# zones and zone edges have a taint of their own (EntityStore.zones_tainted).
ENTITY_FIELDS = {
    "agents": 0,
    "facilities": 2,
    "staff": 3,
    "support_requests": 4,
    "active_tasks": 5,
    "zones": 6,
    "zone_edges": 7,
}

# The scalars of a world that its core holds (Scalars in native/core/src/world.rs). Each is written
# from WorldState.__setattr__; one written past it (world.__dict__[name] = ...) is not seen.
SCALAR_FIELDS = ("scenario_id", "seed", "duration_minutes", "minute_index", "schema_version")


class _RowWatch:
    """A debug run's check of the entity rows against their objects at the end of each tick."""

    on = False
    checked = 0
    mismatches = 0
    first: str | None = None


def watch_entity_rows(on: bool) -> None:
    """Check at the end of every tick that each object holds what its row holds (debug runs).

    The rows are the truth and the objects follow them when a kernel's borrow ends
    (native/src/state/entities.rs). The all-path gate turns this on with --verify-rows.
    """
    _RowWatch.on = on
    _RowWatch.checked = 0
    _RowWatch.mismatches = 0
    _RowWatch.first = None


def entity_rows_watched() -> bool:
    return _RowWatch.on


def entity_row_mismatches() -> dict[str, Any]:
    """How many ticks were checked since watch_entity_rows(True), how many differed, the first."""
    return {
        "checked": _RowWatch.checked,
        "mismatches": _RowWatch.mismatches,
        "first": _RowWatch.first,
    }


def check_entity_rows(world: Any, where: str) -> None:
    """Count a tick whose rows and objects differ (EntityStore.verify). Never raises for that."""
    core = world.__dict__.get("_core")
    if core is None:
        return
    _RowWatch.checked += 1
    found = core.entities().verify(world)
    if found is not None:
        _RowWatch.mismatches += 1
        if _RowWatch.first is None:
            _RowWatch.first = f"{where}: {found}"


def _attach_entities(world: Any, core: Any) -> None:
    store = core.entities()
    state = world.__dict__
    for name, table in ENTITY_FIELDS.items():
        state[name] = store.assign(table, None, state[name])


def entities_assigned(world: Any, name: str, value: Any) -> Any:
    """What world.<name> stores when a caller assigns a whole map of entities to a world."""
    current = world.__dict__.get(name)
    if value is current:
        return value
    return world.__dict__["_core"].entities().assign(ENTITY_FIELDS[name], current, value)


def field_assigned(world: Any, name: str, value: Any) -> Any:
    """What world.<name> stores when a caller assigns a whole field of WHOLE_FIELDS to a world.

    A dict becomes the field's new tree; whoever holds the old one keeps it, as a plain dict with
    its old values. Anything else is stored as it is, for validation to say what is wrong with it,
    and the row is dropped.
    """
    if value is world.__dict__.get(name):
        return value
    mirror = world.__dict__["_core"].mirror(name)
    if mirror is None:
        return value
    if isinstance(value, dict):
        return mirror.replace_root(value)
    mirror.clear_root()
    return value


# ---- What is owned, and what Rust does instead of Python ----------------------------------------

# name of a namespace -> (the module of native/src/state/, the name of its row type). A lane adds
# its line here and nothing to core/world.py. Empty, a world's model_state stays a plain dict.
OWNED_NAMESPACES: dict[str, tuple[str, str]] = {
    "psychological_distress_v1": ("psychological_distress", "ROW"),
    "staff_dynamics_v1": ("staff_dynamics", "ROW"),
    "information_spread_v1": ("information", "ROW"),
    "social_propagation_v1": ("social_propagation", "ROW"),
    "movement_v1": ("movement", "ROW"),
    "body_state_v1": ("body_state", "ROW"),
    "social_network_v1": ("social_network", "ROW"),
    "agent_dynamics_v1": ("agent_dynamics", "ROW"),
    "staff_operations_v1": ("staff_operations", "ROW"),
    "shared_operations_v1": ("shared_operations", "ROW"),
    "resource_ledger_v1": ("resource_ledger", "ROW"),
    "population_presence_v1": ("population", "ROW"),
    "population_lifecycle_v1": ("population", "ROW"),
    "population_dynamics_v1": ("population", "ROW"),
    "temporary_spaces_v1": ("population", "ROW"),
    "geometry_overlay_v1": ("population", "ROW"),
    "resource_service_v1": ("resource_service", "ROW"),
    "effect_program_v1": ("effect_program", "ROW"),
    "facility_queues_v1": ("facility_queues", "ROW"),
}

# The same for a field of WorldState that is not in model_state. A whole assignment to such a field
# is not caught, unless the field is in WHOLE_FIELDS: the field then holds a plain dict, the
# kernels say Unsupported (field_mirror_of in world_core.rs) and the Python rules run on it.
OWNED_FIELDS: dict[str, tuple[str, str]] = {
    "metric_state": ("metric_state", "ROW"),
    "model_configs": ("population", "ROW"),
    "resources": ("world_fields", "RESOURCES"),
    "id_counters": ("world_fields", "ID_COUNTERS"),
}

# The owned fields whose whole assignment (world.resources = {...}) makes a new tree of the value,
# as the Rust rule's new dict does (rules/resources.rs).
WHOLE_FIELDS = frozenset({"resources", "id_counters"})

# name of a namespace -> the kernel that accepts it: kernel(world) is True only if the namespace
# passes every rule its Python validation checks; anything else, or Unsupported, runs the rules.
# Filled by register_rust_validator from the lane's module, read at each validation site through
# rust_validated.
_RUST_VALIDATORS: dict[str, Callable[[Any], bool]] = {}


def register_rust_validator(name: str, kernel: Callable[[Any], bool] | None) -> None:
    """Say a kernel can accept this namespace; None takes that back."""
    if kernel is None:
        _RUST_VALIDATORS.pop(name, None)
    else:
        _RUST_VALIDATORS[name] = kernel


def namespace_stamp(world: Any, name: str) -> tuple[Any, int] | None:
    """What stays the same while nothing writes to this namespace, or None if no mirror follows it.

    Two stamps are the same state when same_stamp says so. dict.__setitem__ on a node of the tree
    passes the mirror (docs/rust-port/lanes/t-tick-validation.md); a validation of the whole world
    still reads the node.
    """
    table = world.__dict__.get("model_state")
    if not isinstance(table, NamespaceTable):
        return None
    mirror = table._mirrors.get(name)
    value = dict.get(table, name, _MISSING)
    if mirror is None or value is _MISSING:
        return None
    generation = mirror.generation_of(value)
    if generation is None:
        return None
    # Every world holds the one placeholder: a detached namespace is told apart by its mirror.
    return (mirror if value is _DETACHED else value), generation


def field_stamp(world: Any, name: str) -> tuple[Any, int] | None:
    """namespace_stamp of a field in OWNED_FIELDS. A field assigned whole has no stamp."""
    state = world.__dict__
    core = state.get("_core")
    mirror = None if core is None else core.mirror(name)
    if mirror is None:
        return None
    value = state[name]
    generation = mirror.generation_of(value)
    return None if generation is None else (value, generation)


def same_stamp(first: tuple[Any, int] | None, second: tuple[Any, int] | None) -> bool:
    """Whether both stamps exist and say the same tree at the same generation."""
    return (
        first is not None and second is not None and first[0] is second[0] and first[1] == second[1]
    )


def rust_validated(world: Any, name: str) -> bool:
    """Whether Rust accepts this namespace now, so its Python validation can be skipped.

    Called where that validation runs, so the errors of the others keep their order. A false says
    nothing about the state. A kernel reads its row through with_row, which raises Unsupported for
    a tainted namespace, so that one is always false. A world that holds something else than the
    rows follow is false too: see _rows_are_what_the_world_holds.
    """
    if not _RUST_VALIDATORS:
        return False
    kernel = _RUST_VALIDATORS.get(name)
    return kernel is not None and rust_accepts(world, name, kernel)


def rust_accepts(world: Any, name: str, kernel: Callable[[Any], bool]) -> bool:
    """Whether a kernel accepts a part of this namespace, whose Python rules can then be skipped.

    For a namespace with rules that read what its row doesn't hold (domain/body_state.py), which
    has no validator for the whole. A false or Unsupported says nothing about the state.
    """
    if not _rows_are_what_the_world_holds(world, name):
        return False
    try:
        return kernel(world) is True
    except native.Unsupported:
        return False


def _rows_are_what_the_world_holds(world: Any, name: str) -> bool:
    """Whether a kernel would read what the Python rules read, as far as identity shows that.

    Storing past NamespaceTable and IdMap (world.__dict__["model_state"] = ...,
    dict.__setitem__(world.model_state, name, ...), world.__dict__["staff"] = ...,
    dict.pop(world.staff, key)) leaves a row that nothing writes any more, which would pass for
    the state. The Python rules read what is really there. dict.__setitem__ on a node of the tree
    is not seen here: see docs/rust-port/lanes/t-tick-validation.md.
    """
    state = world.__dict__
    core = state.get("_core")
    table = state.get("model_state")
    if core is None or not isinstance(table, NamespaceTable) or not core.entities().follows(state):
        return False
    mirror = table._mirrors.get(name)
    if mirror is None:
        # Not owned: the kernel says Unsupported itself.
        return True
    if dict.get(table, name, _MISSING) is _MISSING:
        # Truly absent the kernel answers; a row left behind answers for nothing.
        return mirror.row_json() is None
    return _table_holds_row_of(table, mirror, name)


def _table_holds_row_of(table: Any, mirror: Any, name: str) -> bool:
    """Whether what the table holds under this name is the tree of this row, or the row alone."""
    value = dict.get(table, name, _MISSING)
    return value is _DETACHED or (value is not _MISSING and mirror.holds(value))


def _attach_owned(world: Any, core: Any) -> None:
    table = NamespaceTable()
    table._core = core
    for name, (module, row) in OWNED_NAMESPACES.items():
        table._mirrors[name] = core.own(name, getattr(native.state(module), row))
    current = world.__dict__["model_state"]
    # Anything but a mapping stays as it is, for validation to say what is wrong with it.
    if isinstance(current, dict):
        _fill_table(table, current)
        world.__dict__["model_state"] = table


def _fill_table(table: NamespaceTable, current: Any) -> None:
    # In the order of the document, so a world's order does not depend on what is owned.
    for key, value in current.items():
        mirror = table._mirrors.get(key)
        if mirror is None:
            mirror = _value_mirror(table, key)
        if mirror is None:
            dict.__setitem__(table, key, value)
        else:
            _store_owned(table, key, mirror, value)


def _attach_fields(world: Any, core: Any) -> None:
    for name, (module, row) in OWNED_FIELDS.items():
        mirror = core.own(name, getattr(native.state(module), row), field=True)
        current = world.__dict__[name]
        # Anything but a mapping stays as it is; the kernels then say Unsupported.
        if isinstance(current, dict):
            world.__dict__[name] = mirror.replace_root(current)


# A namespace whose fields the Checkpoint hash tree reads apart (_WORLD_TREE in
# core/checkpoint_cache.py) has no use for the bytes of the whole: 8 MB of history at each save.
_HASHED_BY_PARTS = frozenset(
    {"body_state_v1", "resource_ledger_v1", "resource_service_v1", "shared_operations_v1"}
)


def owned_row_bytes(
    world: Any,
) -> tuple[dict[str, list[bytes]], dict[str, bytes], dict[str, bytes]]:
    """The canonical JSON the Rust rows of a validated world write, without reading its objects.

    Returns the items of each entity field, each owned namespace and each owned world field. What
    has no bytes here has no row that holds it (tainted, a field assigned whole, a value canonical
    JSON refuses, a map or a namespace stored past WorldState.__setattr__, IdMap or NamespaceTable,
    as rust_validated sees them), and the caller encodes the Python objects as always. A row needs
    nothing but
    state_row! (or a Row with write_json) and its line in OWNED_NAMESPACES or OWNED_FIELDS.
    """
    core = world.__dict__.get("_core")
    if core is None:
        return {}, {}, {}
    store = core.entities()
    entities: Any = {}
    # A map stored past WorldState.__setattr__, or written past IdMap, is not what the rows hold.
    if store.follows(world.__dict__):
        try:
            entities = store.rows_json(world.schema_version)
        except native.Unsupported:
            pass
    namespaces = {}
    table = world.__dict__.get("model_state")
    # Without the table the namespaces are in a dict no row follows.
    for name in OWNED_NAMESPACES if isinstance(table, NamespaceTable) else ():
        if name in _HASHED_BY_PARTS:
            continue
        mirror = core.mirror(name)
        try:
            raw = (
                mirror.row_json()
                if mirror is not None and _table_holds_row_of(table, mirror, name)
                else None
            )
        except native.Unsupported:
            raw = None
        if raw is not None:
            namespaces[name] = raw
    owned_fields = {}
    for name in OWNED_FIELDS:
        mirror = core.mirror(name)
        try:
            raw = (
                mirror.row_json()
                if mirror is not None and mirror.holds(world.__dict__[name])
                else None
            )
        except native.Unsupported:
            raw = None
        if raw is not None:
            owned_fields[name] = raw
    return dict(entities), namespaces, owned_fields


def start_core(world: Any) -> None:
    """Give a new world its core, and own what is listed. Only with the kernels on."""
    core = native.world_core()
    state = world.__dict__
    state["_core"] = core
    for name in SCALAR_FIELDS:
        core.set_scalar(name, state.get(name))
    _attach_entities(world, core)
    if OWNED_NAMESPACES:
        _attach_owned(world, core)
    _attach_fields(world, core)


def model_state_assigned(world: Any, value: Any) -> Any:
    """What world.model_state stores when a caller assigns value to a world with owned namespaces.

    The table it replaces becomes a plain holder of plain containers, as a name of it would see.
    """
    table = world.__dict__.get("model_state")
    if value is table or not isinstance(table, NamespaceTable):
        return value
    table.items()  # builds whatever is detached
    mirrors, table._mirrors = table._mirrors, {}
    core, table._core = table._core, None
    for mirror in mirrors.values():
        mirror.clear_root()
    new_table = NamespaceTable()
    new_table._mirrors = mirrors
    new_table._core = core
    if isinstance(value, dict):
        _fill_table(new_table, value)
        return new_table
    # Not a mapping at all: validation will say so.
    return value


def deepcopy_world(world: Any, memo: dict[int, Any]) -> Any:
    """deepcopy(world): the state is copied as plain containers, and owned again by a new core."""
    result = object.__new__(type(world))
    memo[id(world)] = result
    state = result.__dict__
    for key, value in world.__dict__.items():
        if key != "_core":
            state[key] = copy.deepcopy(value, memo)
    if "_core" in world.__dict__:
        start_core(result)
    return result


# ---- Transactions (lane 6-2 of Issue #152, policy E5) --------------------------------------------
#
# A transaction stands for the copy through canonical JSON a rule changes and swaps in
# (WorldState.from_dict(world.to_dict()), or _effect_candidate of events/effects.py), on the world
# itself. Its start validates the world, keeps a snapshot of the core (copy on write), puts the
# core's rows in canonical order (World::canonicalize in native/core/src/world.rs) and makes the
# Python containers again in the order and with the types the copy had: new maps of the entities
# (the same objects, by ID), new trees of the attached namespaces and owned fields from their rows,
# a new model_state table with its keys sorted, the pending events again in queue order under new
# serials, rng_state and any value no row holds through canonical JSON. It then validates the world
# as from_dict validated the copy. A rollback gives back the snapshot and every old container,
# object and tree; a commit drops them. BranchRuntime.apply and schedule_effect_program use it
# through transaction_or_copy (lane 6-4): see docs/rust-port/step5-plan.md, PR 6-2 and PR 6-4. The
# functions starting with _txn_ are the native host's (native/src/core/txn.rs).


class Transaction:
    """An open transaction on a world (transaction()): commit() or rollback() ends it early."""

    __slots__ = ("_world", "open")

    def __init__(self, world: Any) -> None:
        self._world = world
        self.open = True

    def commit(self) -> None:
        self._end(True)

    def rollback(self) -> None:
        self._end(False)

    def _end(self, commit: bool) -> None:
        if not self.open:
            raise RuntimeError("the transaction is over")
        self.open = False
        core = self._world.__dict__["_core"]
        if commit:
            core.commit_transaction(self._world)
        else:
            core.rollback_transaction(self._world)


@contextlib.contextmanager
def transaction(world: Any, kind: str) -> Iterator[Transaction]:
    """`with transaction(world, "full" | "effect") as txn:` changes the world in place.

    The block's writes are kept when it ends, and rolled back when it raises (the error passes on)
    or calls txn.rollback(). "effect" stands for _effect_candidate, "full" for
    from_dict(to_dict()); both validate the world first (its error as it is), then the world in
    canonical order (its error wrapped as from_dict wraps it). Transactions don't nest.
    native.Unsupported, with nothing changed, for a world whose parts its core does not all hold.
    """
    core = world.__dict__.get("_core")
    if core is None:
        raise native.Unsupported("the world has no core")
    core.begin_transaction(world, kind)
    txn = Transaction(world)
    try:
        yield txn
    except BaseException:
        if txn.open:
            txn.rollback()
        raise
    if txn.open:
        txn.commit()


@contextlib.contextmanager
def transaction_or_copy(world: Any, kind: str, make_copy: Callable[[Any], Any]) -> Iterator[Any]:
    """`with transaction_or_copy(world, kind, make_copy) as target:` changes target and keeps it.

    target is the world itself, in transaction(world, kind), where the world's core holds a
    transaction. Where it says Unsupported (no core, or _txn_check: a tainted entity store, a
    queue outside Rust, ...), target is make_copy(world), the copy kind stands for, and it takes
    the world's place when the block ends without an error: the copy-and-swap every change made
    before lane 6-4. Either way the world is validated as the copy validates it before the block
    runs, and an error in the block leaves the world as it was and passes on.
    """
    with contextlib.ExitStack() as stack:
        try:
            stack.enter_context(transaction(world, kind))
        except native.Unsupported:
            target = make_copy(world)
        else:
            target = world
        yield target
        if target is not world:
            # The only copy-and-swap of a WorldState left in src/
            # (tests/unit/repo/test_world_swap_lint.py).
            world.__dict__.clear()
            world.__dict__.update(target.__dict__)


def _txn_check(world: Any) -> None:
    """Unsupported unless the world's core holds every part a transaction gives back."""
    state = world.__dict__
    core = state["_core"]
    table = state.get("model_state")
    if not isinstance(table, NamespaceTable) or table._core is not core:
        raise native.Unsupported("model_state is not the table of the world's core")
    if any(type(key) is not str for key in dict.keys(table)):
        raise native.Unsupported("a key of model_state is not a str")
    if not isinstance(getattr(state.get("event_queue"), "_store", None), _QueueStore):
        raise native.Unsupported("the event queue is not held in Rust")
    if not hasattr(getattr(state.get("event_log"), "_store", None), "txn_begin"):
        raise native.Unsupported("the EventLog is not held in Rust")
    store = core.entities()
    if store.tainted is not None or store.zones_tainted is not None:
        raise native.Unsupported("the entity store is tainted")
    if not store.follows(state):
        raise native.Unsupported("a map of entities is not the rows' map")
    for name in OWNED_FIELDS:
        if core.mirror(name) is None:
            raise native.Unsupported("an owned field has no mirror")


def _txn_validate(world: Any, candidate: bool) -> None:
    """world.validate(); for the candidate, its error wrapped as from_dict wraps it."""
    if not candidate:
        world.validate()
        return
    from sheltersim.core.world import StateInvariantError

    try:
        world.validate()
    except (KeyError, TypeError, ValueError) as exc:
        if isinstance(exc, StateInvariantError):
            raise
        raise StateInvariantError(f"invalid WorldState document: {exc}") from exc


def _canonical_copy(value: Any) -> Any:
    """json.loads of the canonical JSON of value: what a copy through canonical JSON holds."""
    from sheltersim.core.world import WorldState

    return json.loads(WorldState._canonical_text(value))


def _canonical_event(event: Any) -> Any:
    """The event as from_dict reads it back from the world's document."""
    from sheltersim.events.models import ScheduledEvent

    return ScheduledEvent(**_canonical_copy(event.to_dict()))


def _txn_canonicalized(world: Any, kept: dict[str, Any]) -> None:
    """The Python containers in canonical order, after the core's rows (World::canonicalize).

    What is made through canonical JSON is made first, so an error there changes nothing; each
    change is then written down in kept as it is made, for _txn_restored.
    """
    state = world.__dict__
    core = state["_core"]
    table = state["model_state"]
    rng_state = _canonical_copy(state["rng_state"])
    plain_fields = {}
    for name in OWNED_FIELDS:
        mirror = core.mirror(name)
        value = state[name]
        if not (mirror.has_row and mirror.holds(value)):
            plain_fields[name] = _canonical_copy(value)
    keys = sorted(dict.keys(table))
    plain = {}
    for key in keys:
        held = dict.__getitem__(table, key)
        if held is _DETACHED:
            continue
        mirror = table._mirrors.get(key)
        if mirror is None or not mirror.has_row or not mirror.holds(held):
            plain[key] = _canonical_copy(held)
    queue = state["event_queue"]._store
    log = state["event_log"]._store
    events = [_canonical_event(event) for event in queue.object_only_events()]

    kept["state"] = dict(state)
    core.entities().txn_begin(world)
    kept["entities"] = True
    queue.txn_begin(events)
    kept["queue"] = queue
    log.txn_begin()
    kept["log"] = log
    state["rng_state"] = rng_state
    roots: dict[str, Any] = {}
    kept["roots"] = roots
    for name in OWNED_FIELDS:
        mirror = core.mirror(name)
        roots[name] = mirror.take_root()
        if name not in plain_fields:
            state[name] = mirror.root_from_row()
        elif isinstance(plain_fields[name], dict):
            state[name] = mirror.replace_root(plain_fields[name])
        else:
            mirror.clear_root()
            state[name] = plain_fields[name]
    new = NamespaceTable()
    new._core = core
    new._mirrors, table._mirrors = table._mirrors, {}
    table._core = None
    kept["table"] = (table, new)
    state["model_state"] = new
    for key in keys:
        held = dict.__getitem__(table, key)
        if held is _DETACHED:
            dict.__setitem__(new, key, _DETACHED)
            continue
        mirror = new._mirrors.get(key)
        if mirror is not None:
            roots[key] = mirror.take_root()
        if key in plain:
            if mirror is None:
                dict.__setitem__(new, key, plain[key])
            else:
                _store_owned(new, key, mirror, plain[key])
        elif key in DETACHED_NAMESPACES:
            mirror.detach(_DEAD_SINK)
            dict.__setitem__(new, key, _DETACHED)
        else:
            dict.__setitem__(new, key, mirror.root_from_row())
    _settle(new)
    kept["roots_done"] = True


def _txn_restored(world: Any, kept: dict[str, Any]) -> None:
    """The containers the transaction started from, after the core's snapshot came back."""
    state = world.__dict__
    core = state["_core"]
    if "log" in kept:
        kept["log"].txn_rollback()
    if "queue" in kept:
        kept["queue"].txn_rollback()
    if "entities" in kept:
        core.entities().txn_rollback(world)
    if "table" in kept:
        old, new = kept["table"]
        old._mirrors, new._mirrors = new._mirrors, {}
        old._core, new._core = core, None
    roots = kept.get("roots", {})
    # Every tree a mirror has now leaves, and the one it had comes back; without roots_done only
    # the mirrors the start reached had theirs taken.
    names = core.owned_names() if "roots_done" in kept else list(roots)
    for name in names:
        core.mirror(name).put_root(roots.get(name))
    saved = kept.get("state")
    if saved is not None:
        for key in [key for key in state if key not in saved]:
            del state[key]
        for key, value in saved.items():
            if state.get(key, _MISSING) is not value:
                state[key] = value


def _txn_committed(world: Any, kept: dict[str, Any]) -> None:
    """The transaction's writes stay: what it kept goes, and so does the tick's memo, as a world
    swapped in had none."""
    kept["log"].txn_commit()
    kept["queue"].txn_commit()
    world.__dict__["_core"].entities().txn_commit()
    world.__dict__.pop("_tick_memo", None)
    kept.clear()


_QueueStore = native.state("events").QueueStore

native.install_tracked_types(TrackedDict, TrackedList, NamespaceTable)
native.install_id_map_type(IdMap)
native.install_detached_types(_DetachingTable, _DETACHED)
