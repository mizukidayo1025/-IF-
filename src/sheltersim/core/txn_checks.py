"""Debug checks of the transactions of a tick (lane 6-3 of Issue #152).

poison_replaced gives every object a transaction takes out of the world (the old maps of entities,
the old trees, the old model_state table) a class that raises on any use and counts it: code that
kept such an object across the transaction, expecting it to stay as it was or to be the world's, is
found. A rollback gives the objects their classes back. The all-path gate turns it on with --poison.
The rules' transactions run in the core only since lane P9-1b (policy E16): there is no copy of the
world to refuse into any more.

Not poisoned: objects of sheltersim.events (the events, which the dispatcher's snapshot of due
events keeps on purpose, the queue and the log), builtin containers (which can't change class), the
world's core and model_configs (see _KEPT). A kernel that reads a container through the C API
(PyDict_GetItem) is not seen; one that checks a container's exact type refuses it (Unsupported).
"""

from __future__ import annotations

import enum
import traceback
from collections.abc import Callable, Iterable, Mapping
from typing import Any


class _TxnDebug:
    poison = False
    poisoned = 0
    uses = 0
    first: str | None = None
    classes: dict[type, type] = {}


def poison_replaced(on: bool) -> None:
    """While on, objects a transaction takes out of the world raise on any use."""
    _TxnDebug.poison = on
    _TxnDebug.poisoned = 0
    _TxnDebug.uses = 0
    _TxnDebug.first = None


def poisoning() -> bool:
    return _TxnDebug.poison


def poison_uses() -> dict[str, Any]:
    """How many objects were poisoned since poison_replaced(True), how many uses, the first."""
    return {"poisoned": _TxnDebug.poisoned, "uses": _TxnDebug.uses, "first": _TxnDebug.first}


_POISONED_METHODS = (
    "__getattribute__",
    "__setattr__",
    "__delattr__",
    "__getitem__",
    "__setitem__",
    "__delitem__",
    "__iter__",
    "__reversed__",
    "__len__",
    "__contains__",
    "__eq__",
    "__ne__",
    "__lt__",
    "__le__",
    "__gt__",
    "__ge__",
    "__hash__",
    "__bool__",
    "__repr__",
    "__str__",
    "__format__",
    "__or__",
    "__ior__",
    "__add__",
    "__iadd__",
    "__mul__",
    "__imul__",
    "__reduce__",
    "__reduce_ex__",
    "__copy__",
    "__deepcopy__",
)


def _poison_used(cls: type, name: str) -> None:
    _TxnDebug.uses += 1
    if _TxnDebug.first is None:
        stack = "".join(traceback.format_stack(limit=12)[:-2])
        _TxnDebug.first = f"{cls.__qualname__}.{name}\n{stack}"


def _poisoned_class(cls: type) -> type:
    if cls not in _TxnDebug.classes:

        def refusing(name: str) -> Callable[..., Any]:
            def refuse(self: Any, *args: Any, **kwargs: Any) -> Any:
                _poison_used(cls, name)
                raise RuntimeError(
                    f"an object taken out of the world was used: {cls.__name__}.{name}"
                )

            return refuse

        namespace: dict[str, Any] = {name: refusing(name) for name in _POISONED_METHODS}
        namespace["__slots__"] = ()
        namespace["__module__"] = __name__
        _TxnDebug.classes[cls] = type(f"Poisoned{cls.__name__}", (cls,), namespace)
    return _TxnDebug.classes[cls]


_ATOMS = (str, int, float, bool, bytes, type(None), type)


def _reachable(roots: Iterable[Any]) -> dict[int, Any]:
    """Every object reachable from roots through containers and the attributes of sheltersim's
    objects, read past their classes (so that a poisoned object is not used)."""
    seen: dict[int, Any] = {}
    stack = list(roots)
    while stack:
        value = stack.pop()
        if isinstance(value, _ATOMS) or id(value) in seen:
            continue
        seen[id(value)] = value
        if isinstance(value, dict):
            stack.extend(dict.values(value))
        elif isinstance(value, (list, tuple)):
            stack.extend(list.__iter__(value) if isinstance(value, list) else tuple.__iter__(value))
        elif isinstance(value, (set, frozenset)):
            stack.extend(value)
        else:
            module = type(value).__module__ or ""
            if module.startswith("sheltersim.") and not module.startswith("sheltersim.events"):
                try:
                    fields = object.__getattribute__(value, "__dict__")
                except AttributeError:
                    continue
                stack.extend(dict.values(fields))
    return seen


def _poisonable(value: Any) -> bool:
    cls = type(value)
    module = cls.__module__ or ""
    return (
        module.startswith("sheltersim.")
        and not module.startswith("sheltersim.events")
        and not isinstance(value, type)
        and cls.__flags__ & (1 << 9) != 0  # a heap type: one whose instances can change class
        and cls not in _TxnDebug.classes.values()
        and not isinstance(value, enum.Enum)
    )


# Not poisoned: the world's core, and model_configs, parts of which the controller's policies keep
# from the Run's start (social.py's source_expressiveness_by_agent is a node of it). Nothing writes
# model_configs inside a tick (the gate counts such writes), so the old tree holds what the new one
# does.
_KEPT = frozenset({"_core", "model_configs"})


def poison(old: Mapping[str, Any], new: Mapping[str, Any]) -> list[tuple[Any, type]]:
    """Poison what old's values reach and new's don't (_KEPT aside); the classes to give back."""
    gone = _reachable(value for key, value in old.items() if key not in _KEPT)
    for key in _reachable(value for key, value in new.items() if key not in _KEPT):
        gone.pop(key, None)
    done = []
    for value in gone.values():
        if _poisonable(value):
            cls = type(value)
            object.__setattr__(value, "__class__", _poisoned_class(cls))
            done.append((value, cls))
    _TxnDebug.poisoned += len(done)
    return done


def unpoison(done: list[tuple[Any, type]] | None) -> None:
    for value, cls in done or ():
        object.__setattr__(value, "__class__", cls)
