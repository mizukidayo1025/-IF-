"""native/src/core gives CPython's results, bit for bit: floats (hypot, sum, fsum, max, min,
round), CounterRNG draws, and the bytes and types of JSON values.

exp, expm1, ** and fma are the libm crate pinned in native/Cargo.toml since model version 2, not
the platform's libm CPython calls: their bits are pinned here as fixed vectors, the same on every
OS, and compared with CPython only for where it raises and for a distance of one ulp.

Each case is drawn from a fixed seed. SHELTERSIM_PARITY_CASES runs more of the CPython cases.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import random
import struct
import sys
from enum import StrEnum

import pytest
import sheltersim_native

from sheltersim import native
from sheltersim.core.rng import CounterRNG
from sheltersim.core.state_mirror import TrackedDict, TrackedList
from sheltersim.domain.models import Activity

CASES = int(os.environ.get("SHELTERSIM_PARITY_CASES", "20000"))

EDGES = [
    0.0,
    -0.0,
    1.0,
    -1.0,
    0.5,
    -0.5,
    2.0,
    5e-324,
    -5e-324,
    2.2250738585072014e-308,
    2.225073858507201e-308,
    1.7976931348623157e308,
    -1.7976931348623157e308,
    709.782712893384,
    709.7827128933841,
    -745.2,
    1e-300,
    1e300,
    math.inf,
    -math.inf,
    math.nan,
]


@pytest.fixture
def parity():
    return native.rules("parity")


def _floats(rng: random.Random, count: int) -> list[float]:
    """Random bits, the unit interval the models work in, every scale, and the edges."""
    values = list(EDGES)
    while len(values) < count:
        kind = rng.randrange(5)
        if kind == 0:
            values.append(struct.unpack("<d", struct.pack("<Q", rng.getrandbits(64)))[0])
        elif kind == 1:
            values.append(rng.random())
        elif kind == 2:
            values.append(rng.uniform(-50.0, 50.0))
        elif kind == 3:
            values.append(rng.random() * 10.0 ** rng.randint(-320, 300))
        else:
            values.append(round(rng.uniform(-2.0, 2.0), rng.randint(0, 10)))
    return values


def _same(rust: object, python: object) -> bool:
    if type(rust) is not type(python):
        return False
    if isinstance(rust, float):
        if math.isnan(rust):
            return math.isnan(python)
        return struct.pack("<d", rust) == struct.pack("<d", python)
    return rust == python


def _assert_all_same(inputs: list, rust: list, python: list) -> None:
    wrong = [
        (given, ours, theirs)
        for given, ours, theirs in zip(inputs, rust, python, strict=True)
        if not _same(ours, theirs)
    ]
    assert not wrong, f"{len(wrong)} of {len(inputs)} differ, first: {wrong[:5]}"


def _raising_to_none(function, *arguments):
    try:
        return function(*arguments)
    except (OverflowError, ValueError, ZeroDivisionError):
        return None


# ---- exp, expm1, ** and fma: the pinned libm crate ----------------------------------------------


def _portable_floats(rng: random.Random, count: int) -> list[float]:
    """Like _floats, but made without libm (`10.0 ** k` calls the platform's pow): the same
    inputs on every OS."""
    values = list(EDGES)
    while len(values) < count:
        kind = rng.randrange(5)
        if kind == 0:
            values.append(struct.unpack("<d", struct.pack("<Q", rng.getrandbits(64)))[0])
        elif kind == 1:
            values.append(rng.random())
        elif kind == 2:
            values.append(rng.uniform(-50.0, 50.0))
        elif kind == 3:
            values.append(rng.random() * float(f"1e{rng.randint(-320, 300)}"))
        else:
            values.append(round(rng.uniform(-2.0, 2.0), rng.randint(0, 10)))
    return values


def _digest(results: list) -> str:
    """The bits of each result, or what it raises; a NaN as one token, since its sign and
    payload are the CPU's."""
    digest = hashlib.sha256()
    for result in results:
        if isinstance(result, float) and math.isnan(result):
            digest.update(b"nan;")
        elif isinstance(result, float):
            digest.update(struct.pack("<d", result).hex().encode() + b";")
        else:
            digest.update(repr(tuple(result)).encode() + b";")
    return digest.hexdigest()[:16]


def _exp_inputs() -> list[float]:
    rng = random.Random(1)
    return _portable_floats(rng, 4000) + [rng.uniform(-1.0, 0.0) for _ in range(4000)]


def _power_inputs() -> list[tuple[float, float]]:
    rng = random.Random(2)
    values = _portable_floats(rng, 2000)
    pairs = [(value, exponent) for exponent in (2.0, 0.5, 3.0, -1.0, 0.25, 1.5) for value in values]
    bases = _portable_floats(rng, 2000)
    exponents = _portable_floats(rng, 2000)
    rng.shuffle(exponents)
    pairs += list(zip(bases, exponents, strict=True))
    pairs += [(rng.uniform(-20.0, 20.0), 2.0) for _ in range(2000)]
    return pairs + [(base, exponent) for base in EDGES for exponent in EDGES]


# A few results by hand: (input, bits). Where CPython on glibc gives another last bit, it says so.
EXP_VECTORS = [
    (1.0, "4005bf0a8b14576a"),  # glibc: 4005bf0a8b145769
    (-1.0, "3fd78b56362cef38"),
    (0.5, "3ffa61298e1e069c"),
    (-0.5, "3fe368b2fc6f960a"),
    (1e-10, "3ff000000006df38"),
    (-0.001, "3feff7cfe56f1a9e"),
    (0.1, "3ff1aec7b35a00d4"),
    (-2.0, "3fc152aaa3bf81cc"),
    (5.0, "40628d389970338f"),
    (-30.0, "3d3a56e0c2ac7f75"),
    (100.0, "48f3494a9b171bf5"),
    (-700.0, "00d14f2b0fb9307f"),
    (-745.0, "0000000000000001"),
    (-41.35324102717517, "3c34401f7fd4326e"),  # glibc: 3c34401f7fd4326f
    (0.493957811, "3ffa387b415db56e"),  # glibc: 3ffa387b415db56d
    (0.4456685163423838, "3ff8fc0ad2565e82"),  # glibc: 3ff8fc0ad2565e83
]
EXPM1_VECTORS = [
    (1.0, "3ffb7e151628aed2"),
    (-1.0, "bfe43a54e4e98864"),
    (1e-10, "3ddb7cdfd9dda4e3"),
    (-1e-05, "bee4f8aea9acf2c9"),
    (0.1, "3fbaec7b35a00d3a"),
    (-0.3, "bfd0966f2c7907f6"),
    (-2.0, "bfebab5557101f8d"),
    (5.0, "40626d389970338f"),
    (-40.0, "bff0000000000000"),
    (100.0, "48f3494a9b171bf5"),
    (0.9820766375385342, "3ffab84cc86cf9e7"),  # glibc: 3ffab84cc86cf9e8
    (0.42463793426560636, "3fe0edde63b50fc7"),  # glibc: 3fe0edde63b50fc6
    (-0.8358092180524663, "bfe22093461664b8"),  # glibc: bfe22093461664b9
    (-0.3357286480574361, "bfd2407000ab4f22"),  # glibc: bfd2407000ab4f23
]
POW_VECTORS = [
    ((3.0, 2.0), "4022000000000000"),
    ((0.7, 0.5), "3feac5eb3f7ab2f8"),
    ((1.1, 3.0), "3ff54bc6a7ef9db4"),
    ((7.0, -1.0), "3fc2492492492492"),
    ((0.3, 0.25), "3fe7aec222340adf"),
    ((2.5, 1.5), "400f9f6e4990f227"),
    ((10.0, -3.0), "3f50624dd2f1a9fc"),
    ((-2.0, 3.0), "c020000000000000"),
    ((0.9, 100.5), "3efa6c21cdecf184"),
    ((1.6658, 2.0), "400632f956ee7945"),  # glibc: 400632f956ee7944
    ((9.99695801291439e-05, 2.0), "3e457636c5425285"),  # glibc: 3e457636c5425284
    ((5.008410626306549e41, 3.0), "59e7c182f1b390ac"),  # glibc: 59e7c182f1b390ab
    ((1.52828, 3.0), "400c8e5c0f9f3e98"),  # glibc: 400c8e5c0f9f3e97
]
FMA_VECTORS = [
    ((0.1, 10.0, -1.0), "3c90000000000000"),
    ((1.0000000000000002, 0.9999999999999998, -1.0), "b970000000000000"),
    ((3.0, 0.3333333333333333, -1.0), "bc90000000000000"),
    ((1e308, 10.0, -1e308), "7ff0000000000000"),
    ((2.0, 3.0, 4.0), "4024000000000000"),
    ((-0.0, 0.0, 0.0), "0000000000000000"),
]
# What the libm crate raises, as Python raised it: (class name, text).
RANGE_ERROR = ("OverflowError", "math range error")


def _bits(value: float) -> str:
    return struct.pack(">d", value).hex()


def test_exp_and_expm1_are_the_pinned_bits(parity) -> None:
    for value, bits in EXP_VECTORS:
        assert _bits(parity.exp([value])[0]) == bits, value
    for value, bits in EXPM1_VECTORS:
        assert _bits(parity.expm1([value])[0]) == bits, value
    assert parity.exp([709.7827128933841, 1e300, -math.inf]) == [RANGE_ERROR, RANGE_ERROR, 0.0]
    assert parity.expm1([709.7827128933841, -1e300]) == [RANGE_ERROR, -1.0]
    values = _exp_inputs()
    assert _digest(parity.exp(values)) == "095d49fb13350259"
    assert _digest(parity.expm1(values)) == "1114bc8319a1b677"


def test_power_is_the_pinned_bits(parity) -> None:
    for (base, exponent), bits in POW_VECTORS:
        assert _bits(parity.pow([(base, exponent)])[0]) == bits, (base, exponent)
    assert parity.pow([(0.0, -1.0), (-8.0, 0.5), (10.0, 400.0), (-10.0, 401.0)]) == [
        ("ZeroDivisionError", "0.0 cannot be raised to a negative power"),
        ("ValueError", "negative number cannot be raised to a fractional power"),
        ("OverflowError", "(34, 'Numerical result out of range')"),
        ("OverflowError", "(34, 'Numerical result out of range')"),
    ]
    # A subnormal result is kept; CPython raised where glibc set ERANGE.
    assert parity.pow([(2.0, -1074.0)]) == [5e-324]
    assert _digest(parity.pow(_power_inputs())) == "169b17e183ada608"


def test_fma_rounds_once(parity) -> None:
    for (x, y, z), bits in FMA_VECTORS:
        assert _bits(parity.fma([(x, y, z)])[0]) == bits, (x, y, z)


# ---- hypot, fsum, sum, round, repr and draws: the same bits on every OS -------------------------
# The tests below compare these with CPython on the OS they run on; these digests pin them to one
# set of bits, so a platform whose CPython differs too (C fma in hypot, dtoa) still fails here.


def _portable_rows(rng: random.Random) -> list[list[float]]:
    values = _portable_floats(rng, 2000)
    rows = [[values[i], values[(i * 7 + 1) % len(values)]] for i in range(len(values))]
    rows += [[rng.uniform(-30.0, 30.0), rng.uniform(-30.0, 30.0)] for _ in range(2000)]
    rows += [[rng.random() * 2e-310, rng.random() * 2e-310] for _ in range(200)]
    rows += [[rng.random() * 1e308, rng.random() * 1e308] for _ in range(200)]
    rows += [[rng.uniform(-5.0, 5.0) for _ in range(rng.randint(3, 6))] for _ in range(500)]
    return rows + [[x, y] for x in EDGES for y in EDGES] + [[value] for value in EDGES] + [[]]


def test_hypot_fsum_and_sum_are_the_pinned_bits(parity) -> None:
    rows = _portable_rows(random.Random(21))
    assert _bits(parity.hypot([[3.0, 4.0]])[0]) == "4014000000000000"
    assert _bits(parity.hypot([[0.1, 0.2, 0.3]])[0]) == "3fd7f254dab9cc3a"
    assert _digest(parity.hypot(rows)) == "188d1b6d61a6fb66"
    finite = [[value for value in row if math.isfinite(value)] for row in rows]
    sums = [math.nan if value is None else value for value in parity.fsum(finite)]
    assert _digest(sums) == "11599aeafc4f961c"
    assert _digest(parity.sum_floats(finite)) == "3892f0b878ff84bf"


def test_round_and_repr_are_the_pinned_bits(parity) -> None:
    rng = random.Random(22)
    values = _portable_floats(rng, 4000)
    values += [(2 * rng.randrange(1, 100_000) + 1) / 512 for _ in range(500)]
    rounded = [math.nan if v is None else v for v in parity.round_digits(values, 8)]
    assert _digest(rounded) == "4c8dbb0458b5385e"
    integers = parity.round_int(values)
    assert hashlib.sha256(repr(integers).encode()).hexdigest()[:16] == "7cdab211b29eb809"
    finite = [value for value in values if math.isfinite(value)]
    written = "\n".join(sheltersim_native.float_repr(value) for value in finite)
    assert hashlib.sha256(written.encode()).hexdigest()[:16] == "6dff661ec0f8de33"
    assert parity.value_json([0.1, 1e16, 1e-7, 5e-324, 1.7976931348623157e308, -0.0]) == (
        b"[0.1,1e+16,1e-07,5e-324,1.7976931348623157e+308,-0.0]"
    )


def test_draws_are_the_pinned_bits(parity) -> None:
    keys = [["rumor-acceptance-v1", minute, f"agent-{minute % 97}", "m1"] for minute in range(2000)]
    draws = parity.draws("20261108", keys)
    assert _digest(draws) == "ff5986936ee5229a"


def _ulps(first: float, second: float) -> int:
    def ordered(value: float) -> int:
        bits = struct.unpack("<q", struct.pack("<d", value))[0]
        return bits if bits >= 0 else -(bits & 0x7FFFFFFFFFFFFFFF)

    return abs(ordered(first) - ordered(second))


def _near_cpython(inputs: list, rust: list, python: list) -> None:
    """Where CPython raises the crate does, and elsewhere the two are at most one ulp apart."""
    wrong = []
    for given, ours, theirs in zip(inputs, rust, python, strict=True):
        if theirs is None or isinstance(ours, tuple):
            if not (theirs is None and isinstance(ours, tuple)):
                wrong.append((given, ours, theirs))
        elif math.isnan(theirs):
            if not math.isnan(ours):
                wrong.append((given, ours, theirs))
        elif _ulps(ours, theirs) > 1 or (ours == 0.0 and _bits(ours) != _bits(theirs)):
            wrong.append((given, ours, theirs))
    assert not wrong, f"{len(wrong)} of {len(inputs)} differ, first: {wrong[:5]}"


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="measured against glibc only")
@pytest.mark.parametrize("name", ["exp", "expm1"])
def test_exp_and_expm1_are_within_one_ulp_of_cpython(parity, name) -> None:
    rng = random.Random(1)
    values = _floats(rng, CASES) + [rng.uniform(-1.0, 0.0) for _ in range(CASES)]
    python = [_raising_to_none(getattr(math, name), value) for value in values]
    _near_cpython(values, getattr(parity, name)(values), python)


def _power(base: float, exponent: float) -> float | None:
    result = _raising_to_none(lambda: base**exponent)
    return None if isinstance(result, complex) else result


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="measured against glibc only")
def test_power_is_within_one_ulp_of_cpython(parity) -> None:
    rng = random.Random(4)
    values = _floats(rng, CASES)
    pairs = [(value, exponent) for exponent in (2.0, 0.5, 3.0, -1.0, 0.25, 1.5) for value in values]
    exponents = _floats(rng, CASES)
    rng.shuffle(exponents)
    pairs += list(zip(values, exponents, strict=True))
    pairs += [(base, exponent) for base in EDGES for exponent in EDGES]
    rust = parity.pow(pairs)
    python = [_power(*pair) for pair in pairs]
    # glibc set ERANGE for some subnormal results and CPython raised; the crate keeps them.
    kept = [
        index
        for index, value in enumerate(rust)
        if not (isinstance(value, float) and 0.0 < abs(value) < 2.2250738585072014e-308)
    ]
    _near_cpython(
        [pairs[index] for index in kept],
        [rust[index] for index in kept],
        [python[index] for index in kept],
    )


def test_hypot_is_cpythons_algorithm(parity) -> None:
    rng = random.Random(5)
    values = _floats(rng, CASES)
    rows = [[values[i], values[(i * 7 + 1) % len(values)]] for i in range(len(values))]
    rows += [[rng.uniform(-30.0, 30.0), rng.uniform(-30.0, 30.0)] for _ in range(CASES)]
    rows += [[rng.random() * 2e-310, rng.random() * 2e-310] for _ in range(1000)]
    rows += [[rng.random() * 1e-320, rng.random() * 1e-320] for _ in range(1000)]
    rows += [[rng.random() * 1e308, rng.random() * 1e308] for _ in range(1000)]
    rows += [[rng.random(), rng.random() * 1e-200] for _ in range(1000)]
    rows += [[x, y] for x in EDGES for y in EDGES]
    rows += [[value] for value in EDGES] + [[]]
    rows += [[rng.uniform(-5.0, 5.0) for _ in range(rng.randint(3, 6))] for _ in range(1000)]
    _assert_all_same(rows, parity.hypot(rows), [math.hypot(*row) for row in rows])


def test_round_to_eight_digits_is_half_even_on_the_exact_value(parity) -> None:
    rng = random.Random(6)
    values = _floats(rng, CASES)
    values += [(2 * rng.randrange(1, 100_000) + 1) / 512 for _ in range(2000)]  # exact ties
    values += [-(2 * rng.randrange(1, 100_000) + 1) / 512 for _ in range(2000)]
    values += [-1e-10, 1e-10, 5e-9, -5e-9, 0.000000005, 1.5e-8, 2.5e-8]
    python = [round(value, 8) for value in values]
    _assert_all_same(values, parity.round_digits(values, 8), python)


@pytest.mark.parametrize("ndigits", [0, 1, 2, 3, 6, 12, 17, 300, 400])
def test_round_to_other_digits(parity, ndigits) -> None:
    rng = random.Random(7 + ndigits)
    values = _floats(rng, CASES // 4)
    values += [(2 * rng.randrange(1, 100_000) + 1) / 2 ** (ndigits + 1) for _ in range(500)]
    python = [round(value, ndigits) for value in values]
    _assert_all_same(values, parity.round_digits(values, ndigits), python)


def test_round_to_an_int_is_half_even(parity) -> None:
    rng = random.Random(8)
    values = _floats(rng, CASES) + [rng.randrange(-1000, 1000) + 0.5 for _ in range(2000)]
    python = [_raising_to_none(round, value) for value in values]
    python = [None if value is not None and abs(value) >= 2**63 else value for value in python]
    rust = parity.round_int(values)
    # Python raises for an infinity with OverflowError and a NaN with ValueError: both None.
    _assert_all_same(values, rust, python)


def test_max_and_min_of_floats_keep_the_first_argument_on_a_tie(parity) -> None:
    rng = random.Random(9)
    values = _floats(rng, CASES)
    pairs = [(values[i], values[(i * 3 + 1) % len(values)]) for i in range(len(values))]
    pairs += [(x, y) for x in EDGES for y in EDGES]
    _assert_all_same(pairs, parity.max_floats(pairs), [max(x, y) for x, y in pairs])
    _assert_all_same(pairs, parity.min_floats(pairs), [min(x, y) for x, y in pairs])
    near = [(x, x * (1.0 + 1e-9 * step)) for x in values for step in (0.5, 1.0, 1.5)]
    pairs += [pair for pair in near if math.isfinite(pair[1])]
    assert parity.is_close(pairs) == [math.isclose(x, y) for x, y in pairs]


def test_max_of_an_int_and_a_float_returns_the_type_python_returns(parity) -> None:
    assert type(parity.max_num(0, -0.5)) is int
    assert type(parity.max_num(0, 0.0)) is int
    assert type(parity.max_num(0, -0.0)) is int
    assert type(parity.max_num(0, math.nan)) is int
    assert type(parity.max_num(0, 0.5)) is float
    rng = random.Random(10)
    numbers: list[int | float] = [0, 1, -1, 2**53, 2**53 + 1, -(2**53) - 1, 2**62, -(2**63)]
    numbers += [2**63 - 1, 9007199254740992.0, 9007199254740994.0, 9.223372036854775808e18]
    numbers += [-9.223372036854775808e18, 1e300, -1e300, 0.5, -0.5] + EDGES
    numbers += _floats(rng, 100) + [rng.randint(-10, 10) for _ in range(100)]
    pairs = [(x, y) for x in numbers for y in numbers]
    pairs += [(0, value) for value in _floats(rng, CASES)]
    for name, function in (("max_num", max), ("min_num", min)):
        kernel = getattr(parity, name)
        _assert_all_same(
            pairs, [kernel(x, y) for x, y in pairs], [function(x, y) for x, y in pairs]
        )


def test_sum_of_floats_is_the_compensated_sum_of_312(parity) -> None:
    rng = random.Random(11)
    values = _floats(rng, CASES)
    rows = [rng.sample(values, rng.randint(1, 8)) for _ in range(CASES // 2)]
    rows += [[rng.random() * 10.0 ** rng.randint(-20, 20) for _ in range(6)] for _ in range(CASES)]
    rows += [[1.0, 1e100, 1.0, -1e100], [-0.0], [-0.0, -0.0], [1e308, 1e308, -1e308], [0.1] * 10]
    _assert_all_same(rows, parity.sum_floats(rows), [sum(row) for row in rows])


def test_sum_of_ints_and_floats_keeps_python_types(parity) -> None:
    rng = random.Random(12)
    rows: list[list[int | float]] = [[], [0], [1, 2], [1, 0.5], [0.5, 1], [-0.0], [0, -0.0]]
    rows += [[2**53, 1, 1.0], [1.0, 2**53, 1], [1e100, 1, -1e100], [1.0, 1e100, 1, -1e100]]
    for _ in range(CASES // 2):
        rows.append(
            [
                rng.randint(-(2**60), 2**60)
                if rng.random() < 0.4
                else rng.choice(EDGES[:18])
                if rng.random() < 0.1
                else rng.uniform(-1e6, 1e6)
                for _ in range(rng.randint(0, 7))
            ]
        )
    if sys.platform == "win32":
        # CPython's sum() leaves its fast path for an int past C long, 32 bits on Windows; the
        # model takes 64 bits on every OS (test_sum_takes_64_bit_ints_on_every_os).
        rows = [
            row
            for row in rows
            if all(type(item) is not int or -(2**31) <= item < 2**31 for item in row)
            and -(2**31) <= sum(item for item in row if type(item) is int) < 2**31
        ]
    _assert_all_same(rows, [parity.sum_nums(row) for row in rows], [sum(row) for row in rows])


def test_sum_takes_64_bit_ints_on_every_os(parity) -> None:
    # As CPython on Linux and macOS. On Windows the int ends CPython's compensated sum, which
    # gives 1.0001099511627776e16.
    assert parity.sum_nums([1e16, 1.0, 2**40, 1.0]) == 1.0001099511627778e16


def test_sum_leaving_i64_is_left_to_python(parity) -> None:
    with pytest.raises(native.Unsupported):
        parity.sum_nums([2**63 - 1, 1])


def test_fsum(parity) -> None:
    rng = random.Random(13)
    rows = [[rng.random() * 10.0 ** rng.randint(-20, 20) for _ in range(8)] for _ in range(CASES)]
    rows += [[1e308, 1e308, -1e308], [1e16, 1.0, 1e-16, -1e16], []]
    python = [_raising_to_none(math.fsum, row) for row in rows]
    # A partial sum that leaves the finite range is left to Python.
    rust = parity.fsum(rows)
    kept = [(row, r, p) for row, r, p in zip(rows, rust, python, strict=True) if r is not None]
    _assert_all_same([k[0] for k in kept], [k[1] for k in kept], [k[2] for k in kept])
    assert len(kept) >= len(rows) - 1


TEXTS = [
    "a",
    "resident",
    "A001",
    "",
    'q"uo\\te',
    "\n\t\r\b\f",
    "\x00\x1f\x7f",
    "避難所",
    "😀 é",
    "é\u2028",
]


def _key_parts(rng: random.Random) -> list[str | int]:
    parts: list[str | int] = []
    for _ in range(rng.randint(1, 6)):
        kind = rng.randrange(3)
        if kind == 0:
            parts.append(rng.choice(TEXTS))
        elif kind == 1:
            parts.append(rng.randint(-(2**62), 2**62))
        else:
            parts.append(f"agent-{rng.randrange(1000)}")
    return parts


@pytest.mark.parametrize("seed", [0, 1, 42, 20261031, 2**63 - 1, 2**64 + 7, 10**40])
def test_keyed_draws_are_counter_rng_draws(parity, seed) -> None:
    rng = random.Random(seed % 1000)
    keys = [_key_parts(rng) for _ in range(CASES // 4)]
    reference = CounterRNG(seed)
    python = [reference.random(*key) for key in keys]
    _assert_all_same(keys, parity.draws(str(seed), keys), python)


def test_keys_written_from_a_mark_are_the_same_draws(parity) -> None:
    reference = CounterRNG(7)
    python = [reference.random("social-contact-bucket-v1", 9600, slot) for slot in range(100)]
    rust = parity.draws_from_mark("7", "social-contact-bucket-v1", 9600, 100)
    _assert_all_same(list(range(100)), rust, python)


def _json_value(rng: random.Random, depth: int = 0) -> object:
    kind = rng.randrange(10 if depth < 5 else 7)
    if kind == 0:
        return None
    if kind == 1:
        return rng.choice([True, False])
    if kind == 2:
        return rng.choice([0, 1, -1, 2**53, 2**63 - 1, -(2**63), 2**63, -(2**80), 10**30])
    if kind in (3, 4):
        value = _floats(rng, len(EDGES) + 1)[-1] if rng.random() < 0.5 else rng.choice(EDGES[:18])
        return value if math.isfinite(value) else 0.25
    if kind in (5, 6):
        return rng.choice(TEXTS)
    if kind == 7:
        return [_json_value(rng, depth + 1) for _ in range(rng.randrange(5))]
    keys = rng.sample(TEXTS + ["z", "b", "aa", "é", "￿", "😀"], rng.randrange(6))
    return {key: _json_value(rng, depth + 1) for key in keys}


def _same_value(x: object, y: object) -> bool:
    if type(x) is not type(y):
        return False
    if isinstance(x, dict):
        return list(x) == list(y) and all(_same_value(x[key], y[key]) for key in x)
    if isinstance(x, list):
        return len(x) == len(y) and all(_same_value(a, b) for a, b in zip(x, y, strict=True))
    return _same(x, y)


def test_values_write_the_canonical_bytes_and_come_back_the_same(parity) -> None:
    rng = random.Random(14)
    for _ in range(CASES // 10):
        value = _json_value(rng)
        expected = json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False
        ).encode()
        assert parity.value_json(value) == expected
        assert native.canonical_json(value) == expected
        assert _same_value(parity.value_round_trip(value), value)


class _Subclass(dict):
    pass


class _SubclassList(list):
    pass


class _Text(StrEnum):
    A = "a"


@pytest.mark.parametrize(
    "value", [(1,), {1: 2}, _Text.A, {"a": (1,)}, object(), b"x", {"a": {"b": [(1,)]}}]
)
def test_values_that_would_not_come_back_the_same_are_unsupported(parity, value) -> None:
    with pytest.raises(native.Unsupported):
        parity.value_round_trip(value)


def test_nan_and_infinity_are_left_to_json(parity) -> None:
    for value in (math.nan, math.inf, [{"a": -math.inf}]):
        with pytest.raises(native.Unsupported):
            parity.value_json(value)


@pytest.mark.parametrize("value", [_Subclass(a=1), _SubclassList([1]), {"a": [_Subclass()]}])
def test_values_in_other_container_subclasses_are_unsupported(parity, value) -> None:
    with pytest.raises(native.Unsupported):
        parity.value_json(value)


def test_values_read_through_tracked_containers(parity) -> None:
    value = TrackedDict(b=TrackedList([1, 2.0]), a=None)
    assert parity.value_json(value) == b'{"a":null,"b":[1,2.0]}'
    back = parity.value_round_trip(value)
    assert type(back) is dict and type(back["b"]) is list and list(back) == ["b", "a"]


def test_an_id_has_one_python_object(parity) -> None:
    first, second, third = parity.interned(["A001", "A002", "".join(["A0", "01"])])
    assert first == "A001" and second == "A002"
    assert third is first


def test_enum_members_come_back_as_the_singletons(parity) -> None:
    members = list(Activity)
    found = parity.enum_members(Activity, [*members, str(members[0].value)])
    for index, member in enumerate(members):
        assert found[index][0] == index
        assert found[index][1] is member
    assert found[-1] is None


def test_lane_modules_are_found_by_name(parity) -> None:
    assert callable(parity.exp)
    with pytest.raises(AttributeError):
        native.rules("no_such_rule_module")
    with pytest.raises(AttributeError):
        native.state("no_such_state_module")
