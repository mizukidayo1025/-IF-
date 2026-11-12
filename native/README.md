# sheltersim_native

Rust kernels for 避難所IF！, merged into `main` on 2026-11-07. The port moves the
state the tick computes on into Rust in stages; each stage must keep the world,
events and KPI hashes of a seven-day Run (`benchmarks/investigate_performance.py
--expect`). The plan and the measurements are in issue #272.

The extension is required: importing `sheltersim` without it raises ImportError
(`docs/development.md` gives the build). Where a kernel raises `Unsupported`, the
caller runs its Python code, which stays until lane L10 of
`docs/rust-port/step4-plan.md` removes it. `tests/integration/test_native_kernels.py`
compares each kernel with that Python code at every tick. CI builds and installs the
extension on Linux, Windows and macOS. Its test jobs build with the profile `ci` of
`native/Cargo.toml` (thin LTO, 16 codegen units; the same bytes, built sooner), and its gate
job with `--release` (#217).

## Build and use without installing

```shell
cd native
../.venv/bin/maturin build --release -i ../.venv/bin/python
mkdir -p pylib && ../.venv/bin/python -m zipfile -e target/wheels/sheltersim_native-*.whl pylib
cd .. && PYTHONPATH=src:native/pylib SHELTERSIM_LLM_MODE=offline .venv/bin/python -m pytest -q -n auto
```

`native/tests/parity_check.py` compares the kernels with Python byte for byte
(random and edge-case floats, strings, structures, and optionally a saved
Checkpoint document) and times them.

## The core's feature `parallel` and the wasm32 build

`sheltersim_core` has one feature, `parallel`, on by default. It brings in rayon for the
helper threads of `parallel.rs` (`in_order`, `parts_in_order`) and the Checkpoint kernels of
`checkpoint.rs` (`sensitive_candidates`, `chunk_hashes`). Without it, the same functions run
on the calling thread, in the same parts and the same order, and give the same bytes
(`sensitive_candidates` the same sets: their iteration order was never fixed, and Python reads
them as sets); `parallel_state` is then `(0, 0)`. The wrapper names the feature in `native/Cargo.toml`, so the
extension keeps its threads. The build for the browser (#132, #152) leaves the feature out:

```shell
rustup target add wasm32-unknown-unknown
cargo build --manifest-path native/Cargo.toml -p sheltersim_core --target wasm32-unknown-unknown --no-default-features
cargo test --manifest-path native/Cargo.toml -p sheltersim_core --no-default-features
```

CI's lint job runs these, after `cargo test -p sheltersim_core` with the feature.
`core/tests/parallel.rs` compares both builds with one frozen digest, and
`core/tests/no_platform.rs` fails if the core's source names `std::time`, `fs`, `env` or
`thread` (they are missing on wasm32 and would tie results to the machine; `thread_local!`,
`Mutex` and `OnceLock` work on one thread and are allowed).

## The workspace: the core and the wrapper (Issue #152)

`native/Cargo.toml` is a workspace of two crates. `docs/rust-port/step5-plan.md` records the
split PR by PR; its last section sums up what is where.

- **`core/` (`sheltersim_core`)**: the simulation without Python. Its `cargo tree` has no pyo3
  (CI checks `cargo tree -p sheltersim_core -e normal,build`). It holds the World (`world.rs`:
  the scalars, the entity rows, the slots of the owned fields and namespaces, the event queue and
  the EventLog), the rows (`row.rs`, `entities.rs`, `state/`, `events/`), the typed configs
  (`config/`), the floor plan and the routes (`geometry.rs`, `routes.rs`), the transactions
  (`txn.rs`), the Run (`run/`: the `Policies` read once from the controller, `Scratch`, the
  dispatch table), every rule of the tick (`rules/`) and the tick itself:
  `rules::tick::run_tick` runs the dispatch, complete, update, propose, arbitrate, sample and
  metrics phases in `TICK_PHASE_ORDER` on a `WorldCell`, a `RunCell` and one `Host`, and
  `rules::tick::run_phases` some of them in that order.
- **`src/` (`sheltersim_native`)**: the thin PyO3 wrapper. It converts between Python and the
  core (`src/state/`, `src/core/`), holds a Python world's core (`WorldCore`) and a controller's
  `Run`, parses the config sections a Python contract interprets (`src/core/configs.rs`,
  `src/core/geometry.rs`), implements `Host` for Python (`src/host.rs`, `PyHost`) and keeps the
  `#[pyfunction]` entries of `src/rules/`. A rules file of the wrapper reads its inputs, calls one
  core function, converts the result and does nothing else.
  `benchmarks/rust_port/glue_check.py` (CI's lint job) fails on state shims, rule arithmetic,
  Python calls or a `with_world` closure of more than one core call there, unless an exemption
  of an allowed class (`CONVERSION`, `ARGUMENT`, `HOOK`) names it. `parity.rs` (tests) and the E6
  validators `after_tick.rs` and `tick_validation.rs` are exempt files.
- **`Host`** (`core/src/host.rs`) is the core's only way out: the core has no `extern`, no pyo3
  and no other callback type. Its methods are the whole-world validation and the transaction's
  views (policy E6), the pydantic input contracts (`contract`, E15), the LLM boundary (`llm`,
  `prioritize_support_request`, `consider_*`), the sinks (metrics, decisions, traces, the phase
  observer), what Python still owns (`holding`, `fill_section`, `floor_plan`, `find_record`, the
  queue's Python events) and the test hooks (`apply_foreign`, `draw`). The section of PR P9-3b
  (2/2) in `step5-plan.md` lists each method with why it stays. The native `run_tick`
  (`src/rules/tick.rs`) is the E10 check (a patched step or rule fails the tick with
  `KernelRefusal`; there is no Python path to run it instead, #221) and one call of the core's
  `run_phases` with a `PyHost`.
- **A tick without Python**: `cargo test -p sheltersim_core --test tick_core` builds the Worlds
  of `core/tests/data/tick_core.json` (`core/tests/support/load.rs`, test code only, decision 4)
  and runs their ticks with a `TestHost` (no LLM; the whole-world validation answers Ok, E6): a
  small world without a floor plan for 1,440 ticks, and a generated world with a floor plan, every
  extension, the typed namespaces and moved facilities for 360 ticks, in which people walk and
  `update_movement` replans their trips (#214). Their digests are the native path's
  (`tests/integration/test_core_fixtures.py`).
- **Typed rows from JSON** (#225): every `Field` reads itself from the `V` of its canonical JSON
  (`Field::from_value`, written by `state_row!` and `str_enum!`, by hand for the leaf types and
  the hand-written fields), with the checks of the PyO3 crate's `from_py`. No tick uses it; the
  test loader reads each typed namespace of `model_state` with it (`core/tests/support/typed.rs`,
  the namespace's row), and `cargo test -p sheltersim_core --test typed_rows` reads the worlds of
  `core/tests/data/typed_rows.json` and writes them back to the same bytes.
- **Out of the core** (decision 4): building a world from a Scenario and restoring a Checkpoint
  stay in Python and the wrapper. The whole-world validation texts (E6) and the pydantic
  contracts (E15) stay Python behind `Host` until stage 4.

## Layout for lanes

- `core/` (`sheltersim_core`): the simulation core without Python (Issue #152,
  `docs/rust-port/step5-plan.md`). It must never depend on pyo3; CI checks its `cargo tree`
  and runs `cargo test -p sheltersim_core`. It holds what moved so far: `json.rs` (JSON strings
  and floats as Python writes them, and `Encoder`, which writes plain values), `value.rs` (`V`),
  `error.rs` (`Error`, one variant per Python class with its text, and `Misfit`), `mathx.rs`,
  `rng.rs` (`KeyedDraws`, and `Draws`, the `CounterRNG` of a seed), `census.rs`, `parallel.rs`, `interner.rs`, `contacts.rs`, `checkpoint.rs` (the
  Checkpoint kernels over bytes), `events/` (`EventRow`, the queue's `Queue`, the `EventLog`
  with its indexes and the saved-record checks, and `writer`, which schedules and records rows
  with the Python errors), `row.rs` (`Field`, `Row`, `Items`, `state_row!`, `str_enum!`),
  `entities.rs` (the entity rows `AgentRow`…`TaskRow`, `ZoneRow` and `EdgeRow` with their
  `EntityField`, whose `write_json` takes the WorldState version, `Table` and `Entities`, which
  also holds the taints: one for the people's five tables, one for the zones and zone edges),
  `state/` (the rows of the namespaces, with their checks that read the row alone or the entity
  rows), `world.rs` (`World`: one world's state, as `CowShard` parts: `scalars` (`Scalars`: each
  value a `Fit`, a misfit when Python holds another type; the seed is a `Seed`, decimal text of
  an int of any size), `entities`, and `slots`, one `Slot` per owned field or namespace with its
  row, taint, detach flag, generation and counters; every other str key of `model_state` has a
  `ValueRow` slot, `Place::Value`, read by `Slots::value`; the event `queue` and the `log`, while
  the world's Python queue and log are stores attached to its core (`queue_held`, `log_held`;
  `hold_queue`, `release_queue` and the same of the log move them in and out, nothing copied);
  typed readers such as `minute_index`, `resources`, `id_counter`, `rows` (the entity rows,
  refused while tainted), `row::<T>(name)`, `metric_state`, `queue`, `log` (refused while not
  held), `stamp_of` and `configs`, each a borrow of its part; `snapshot` and `restore` (the parts'
  `Arc`s, copied on the next write; the log by its mark), `canonicalize` (the order a copy through
  canonical JSON gives: entities by ID, every map's keys sorted, `Row::canonicalize`, the queue in
  queue order under new serials) and `fork`
  (a new `World` sharing every part, stamps included, until either writes it; lane 6-6); and
  `WorldCell`, what holds a world and lends it whole to a core function, `with(|world| ...)`),
  `txn.rs` (`Txn`: a transaction in place, begin (validate, snapshot, canonicalize, validate the
  candidate), commit and rollback, with a `Host` for the views and the validation; lane 6-2),
  `host.rs` (`Host`, the world's outside as the core's rules see it: the transaction's views and
  the whole world's validation (E6), the pydantic contracts of inputs (`contract`, E15), the LLM
  boundary, the sinks (metrics, decisions, traces, the phase observer) and the space's Python
  views; `HostFailure` gives an error's Python class; lane P8-0b; the validation with an event in
  flight (`validate_in_flight`), the EventLog's records by ID (`find_record`) and the queue's slots
  a rule changed (`slots_changed`), lane P8 d2; the floor plan read again (`floor_plan`), and
  `HostFailure::text` and `is_outcome` for a rule that wraps an error's text in its own, lane P8 d3;
  what the world holds (`holding`, a `Holding` of every namespace and owned field, read again by
  the native host after a call of its own runs Python), the configs' sections a rule found empty
  (`fill_section` by `config::SectionId`), a handler the core does not run (`apply_foreign`) and a
  decision a Python sink records (`record_decision_python`), lane P9-0; `rules/kit.rs` reads the
  first two through the host: `holding`, `section` and `section_value`),
  `rules/` (the kernels moved
  to the core, pure functions over `&World`, listed in `rules/mod.rs` by lane block:
  `metrics.rs`, the columns of `MetricSnapshot` with the open requests' waits of `staff.rs`, and
  `capture`, the tick's metrics phase that gives the sample to `Host::capture_metric` (lane P8 b4),
  and `kit.rs`, the helpers the rules share —
  events as rows written through `Events` to the world's queue, log and event counter, metric
  counts, a person's activity, the readers of payloads and of a few namespaces, `place_in_bed`,
  `check_waiting_for_admission` with the rules it still asks through `Admission`, `Counts`, `Held`
  (whether the Python world holds a namespace, which the PyO3 crate reads and a kernel says
  where it reads the namespace), `namespace_absent`, `Kept` (a config section the glue read for a
  kernel, raised where the rule reads it; lane P8 a2) and `kit/movement.rs` (what the queues ask
  of the movement rules: the queue slots, the walk to a facility, the minutes of a walk in a room,
  `Changes` of the row of `movement_v1` and `plan_start`; lane c1); `facility_queues.rs`, the
  independent FIFO of each service point on the row of `facility_queues_v1`, and `queues.rs`, a
  facility's physical queue (`prepare_facility_queue`, the walks to the queue's places and to
  service, those who give up), which write the rows alone (lane c1); `movement.rs`, the movement rules of a tick
  (`advance_movement` with its walk in order, `start_movement`, `finish_movement`,
  `staff_position_snapshots`, the readers of where a person is, `route_with_movement_time`'s
  minutes and the validation), on the zone and edge rows, the floor plan and the overlay's value,
  writing the row of `movement_v1` alone (lane c2);
  the P8b-1 operations: `staff_dynamics.rs` (shift, rest and fatigue), `resources.rs` (the
  ledger's operations, which return the new balance for the PyO3 crate's new `world.resources`),
  `common_operations.rs` (`record_shared_operation`, and the receipts typed as `Receipt` with the
  Rust check of their contracts) and `care_receipts.rs` (a care transition's record and receipt,
  the other receipts' applied records, the person's context); the leaves of the people rules
  (lane P8 a2): `agent_dynamics.rs`, `action_lifecycle.rs` (with `ActiveActions`, a controller's
  open actions as rows, kept in its `Run`), `body_state.rs`, `social_network.rs` and
  `social_dialogue.rs`, which read and write the slot rows alone and take an event as its fields
  and the payload's fields they read, as a `V`; `world_view.rs`, the phases' view (`ViewData`, its
  audit hash, misinformation, stressors, the facilities' event context) from the rows, with
  `person_context.rs` and `zone_environment.rs`, and the `ViewCache` the `Run` keeps (lane P8 a1);
  the event handlers of lane P8 d1: `information_events.rs` (a rumor, a correction and a contact,
  written to the row of `information_spread_v1`; a state of another schema is upgraded through
  `Host::upgrade_information_state`) and `social_interaction.rs` (`SocialInteractionHandler`),
  which take the event's row;
  `psychology.rs` (each person's minute from the view, the rows and the model the `Run`'s
  `Policies` read, as `Delta`s and the trace's rows; the distress monitor and the long-term
  distress; writing the final deltas) and `trace.rs` (the trace documents as `V`, the psychology
  document going out through `Host::trace`) (lane P8 a3);
  the services of lane c3: `service.rs` (the water and meal service: `serve` a queue's head,
  `complete`, `request` with the world's validation through `Host::validate_world`, `cancel`,
  `interrupt`, the stall records; the ledger's refusal comes back as `Failure::Ledger` for the
  PyO3 crate to write with Python's `repr`) and `toilet.rs` (`queue_up`/`request`, `start` with
  the duration drawn from the Run's `CounterRNG`, or from `Host::draw` for another random source,
  and `complete`, which returns `ScheduleReturn`: the glue then runs the core's walk home on the
  same borrow), which write
  the rows alone (a tainted `resource_service_v1` is refused); `behavior.rs` (the proposals of the
  behavior policies the `Run`'s `Policies` read, the trigger texts with `format(x, "g")`, and the
  observed state as a `V`) and `decisions.rs` (a decision's record: the indexed sink's JSON bytes,
  the trace's document, through `Host::decision`; then the lifecycle and the memory, called
  directly) (lane P8 a5); `overlay.rs` (the temporary spaces, capacity, beds and room intake,
  installed facilities, and the floor-plan changes with their checks, on the rows of
  `temporary_spaces_v1` and `geometry_overlay_v1`, a facility's `zone_id` and an edge's `closed`;
  the parameters' contracts through `Host::contract`, policy E15) (lane P8 c4); `travel.rs` (the
  routers of a Run from the zone and edge tables, a route with its capacity waits through the
  world's writer, a trip planned and written, the walk home, the choice of a facility, the edge
  loads), `proximity.rs` (the proximity sampler and its step, from the Run's `Policies`) and
  `replan.rs` (a replanned person's stop and new route, the old events settled on the world's
  queue) (lane P8 c6); `population.rs` (arrivals, admission, approach completion, retries,
  departures and the validation of `population_dynamics_v1`, on the rows of the population
  namespaces and the entity tables: an admitted person is a new `agents` row that `build_agent`
  makes as the Python constructors did; the input contracts through `Host::contract`) (lane P8 c5);
  `staff_operation_events.rs` (a staff operation's arrival, relief, completion and interruption,
  and the stop of trips over a closed route, on the row of `staff_operations_v1` and the entities,
  with the staff positions, `staff_dynamics` and the receipts called directly; an intake's
  admission stays the glue's between the plan and the writes) and `staff_order.rs` (an operator's
  order of a routed task) (lane P8 b2); `staff.rs` (the support requests' assignment with the
  controller's own route and trip called directly, the complaints that come due through
  `Host::consider_complaint_response`, the waits the metrics read, the zones' sanitation, the
  routed tasks and their relief, on the rows; the service's parameters from the Run's `Policies`)
  (lane P8 b3); `support.rs` (a support request's plan and writes, its timeout, the end of a staff
  member's support with the care of the selected formula, which the glue reads where the care
  needs it, and the care receipts around a request, on the rows) (lane P8 b4);
  `handlers.rs` (the rest of `events/handlers.py`: proximity contacts, the resource ledger's events,
  facility outages and recoveries, deliveries, water, staff unavailable and restored with the stop
  of what the member was doing, the population primitives' checks and the infrastructure events, on
  the rows of `operational_events_v1`, `infrastructure_v1` and the entities; events through
  `kit::Events`; the end of an operational transaction, `settle`, with the world's validation, the
  continuity receipt's ingest and the records it reads through the `Host`; the infrastructure's spec
  through `Host::contract`) (lane P8 d2);
  `effects.rs` (the rules of an EffectProgram's events: the guard, the five handlers' checks on the
  world and their writes on the rule transaction's world, the reads of the modifier stacks and the
  tick's and the history's check of `effect_program_v1`; the parameters' and a guard's contracts
  through `Host::contract`, the changed world's validation through `Host::validate_world`, and the
  rules of other files (temporary spaces, geometry changes, an installed facility's place and
  announcement, the staff positions, the staff's availability) called directly, a refusal of one
  being its `KernelRefusal`) (lane P8 d3); `proposals.rs` (`_apply_effect`, the join of the rules
  above: a decision's bed, trip, queue, request, information record and social event; a request's
  LLM hooks through `Host::prioritize_support_request` and `Host::consider_support_request`, with
  the world let go of) (lane P8 e1);
  the file of the same name in `src/rules/` is their glue, see step5-plan.md, PR 5-4, P8-0a, c1,
  P8b-1, P8 a2, P8 a1, P8 d1, P8 a3, P8 c3, P8 a5, P8 c4, P8 c6, P8 c5, P8 b2, P8 b3, P8 b4,
  P8 d2, P8 d3 and P8 e1),
  `text.rs` (`casefold`, CPython 3.12's full fold from a generated table, `text/casefold.rs`
  by `tools/casefold_table.py`, tested at every code point against `tests/data/casefold.txt`;
  `g_text`, `format(value, "g")`; `py_repr`, `repr` of a str, from the table of what
  `str.isprintable` says no to, `text/printable.rs` by `tools/repr_table.py`, tested at every code
  point against `tests/data/repr.txt`),
  `cell.rs` (`Shard`, a part borrowed on its own; a second borrow of a part is an
  `Error::Runtime`, never a panic; `CowShard`, the same over an `Arc`, copied on write while a
  snapshot shares it), `stamp.rs` (`Stamp`, process-unique numbers from one
  counter: a slot's generation is one) and `config/` (`Configs`, the sections the kernels read
  of one state of `model_configs`, each parsed at its first read as a value or a `Fault`, kept in
  the world's `derived` part by the stamp of the `model_configs` slot; `config/geometry.rs` is
  the section of `facility_geometry` and `spatial_constraints`; `config/read.rs` reads a section
  from the configs as a `V` with the kernels' refusal texts, and `staff.rs`, `body.rs`,
  `information.rs`, `agent_dynamics.rs`, `environment.rs` and `proximity.rs` hold the sections of
  lane 7-5b and their parses; `movement.rs` (the movement model and the route capacities),
  `queues.rs` (the two bundled queue policies), `overlay.rs`, `service.rs` and `population.rs`
  those of lane 7-5a), `geometry.rs` (the floor plan
  `Geo`, its `Layout` and `Constraints`, the geometry of `domain/movement.py`, and `GeoCache`, the
  plans one `Configs` keeps by the overlay's stamp, then by the bytes of its moves and partitions),
  `routes.rs` (`Router`, a zone graph's open edges with `ShortestTravelTimePolicy.route` and the
  edges a plan closes, built from the zone and zone-edge tables with `ZoneGraph`'s checks and
  words, and `Routes`, a Run's people's and staff's routers by epoch) and `run/`
  (`Run`, what one controller keeps beside its World: `RunConfig`, read when the controller
  starts, with `route_capacities`; `Routes`; `Policies`, what the rules read of the controller's
  policies and managers, one file per part (`psychology.rs`, `behavior.rs`, `stress.rs`,
  `information.rs`, `proximity.rs`, `social.rs`, `services.rs`, `sinks.rs`), each made from what
  the PyO3 crate read by the part's `Spec` (`held.rs`: classes by name and whether the name finds
  them, values as `V`, faults kept for the read site); and `Scratch`, one tick's values by type;
  lane P8-0b; `dispatch.rs`, the dispatcher's handlers by event type, each a `HandlerKind` the core
  runs with what it reads of the handler, or foreign, lane P9-0). `tests/support/` holds
  `TestHost`, the host of a build without Python, which fills every section of the configs from
  outside the crate; `load.rs`, a test-only loader of a World and a Run's `Policies` from JSON
  (lane P10-1b; not part of the crate), which puts each typed namespace of `model_state` in its
  slot as the row `typed.rs` names for it; `sections.rs`, which gives a `TestHost` the
  Python-parsed sections and the floor plan of an exported world (#225, #214); `dumps.rs`,
  the text of Python's `json.dumps` for the digests the pytest files took of it; `canonical.rs`,
  a World's `canonical_json()` bytes, part by part; and `cases.rs`, the files of events given to
  one handler of an exported world (worlds stored as the changes from another world of the file)
  with the outcome or the world the Python rule left, checked through `dispatch::apply_alone`,
  the kernel a handler's PyO3 entry calls (#225); and `texts.rs`, for the tests that call a rule
  directly: the canonical text of a world in a cell, the outcome a rule raised, and events
  changed as the pytest files changed them. `answers.rs`
  chains a test's answers and the world's canonical parts after each, for the digests a moved
  test records again from the core. `steps.rs` is for the tests that call one step of a tick on
  an exported world: `Case` (the world, its Run and a following host; the step's `Tick`; edits of
  the world's canonical document between steps) and the inputs a builder wrote down. The JSON under
  `tests/data/` (worlds the Python side built) is written by the builders that
  `tests/support/core_fixtures.py` registers (`benchmarks/core_fixtures.py`), and
  `tests/integration/test_core_fixtures.py` checks that each file is what its builder makes
  today. A test reads one exported world with `load::entry(file, world)`, then `load::loaded`
  (the World, the Run and its policies) and `load::host` (the sections and the floor plan), or
  `load::following_host` for a Run's ticks: a host that also does what the PyO3 crate's host does
  to the world it owns (the floor plan of the overlay at each call, a namespace a rule starts,
  `world.resources` after a ledger operation, the records of the world's log as `find_record`
  finds them).
  `tests/tick_core.rs` runs the ticks of each world of `tests/data/tick_core.json` with `TestHost`
  alone and reaches the digests the native path gives on the same world;
  `tests/typed_rows.rs` reads every typed namespace of `tests/data/typed_rows.json` and writes
  it back to the same bytes, and refuses what `from_py` refuses; `tests/fixtures.rs` checks the
  sections and the floor plan of an exported world against what the native path read of it, and
  the `json.dumps` writer against Python's bytes; `tests/event_texts.rs`,
  `tests/social_interaction_texts.rs` and `tests/travel_completion_texts.rs` check the handlers'
  saved texts (`tests/data/event_texts.json`, `rumor_texts.json`, `travel_texts.json`);
  `tests/resource_service_texts.rs`, `tests/effect_program_texts.rs` and `tests/population_texts.rs`
  check the texts the service's, the EffectProgram handlers' and the population's rules save, on
  the worlds of the pytest files of the same names, with the world unchanged where a check comes
  before any write; `tests/service_rules.rs`, `tests/requests_at_once.rs` and
  `tests/staff_assignment.rs` call `_apply_effect` (`tick::propose::apply_alone`), `_route`,
  `_schedule_travel`, the water and meal service and the staff's assignment, relief and support
  waits on the worlds of the pytest files of the same names (`tests/data/service_rules.json`,
  `generated_texts.json`, `staff_assignment.json`); `tests/replan_journeys.rs`,
  `tests/movement_walk_order.rs` and `tests/movement_rules.rs` call `_replan_journeys`
  (`tick::update::replan_journeys`), the walk of `advance_movement` (`advance_through`) and the
  other rules of `domain/movement.py` on the worlds of the pytest files of the same names
  (`tests/data/replan_journeys.json`, `movement_walk_order.json`, `movement_rules.json`).
  `tests/handler_rules.rs` checks the handlers' kernels and the steps around them (the toilet's
  queue and service, the care receipts, a seek, a request's and a support's end, the walk home,
  the care formulas, the social events, the draws of a claim) on the worlds of
  `tests/data/handler_rules.json`, calling each kernel as its PyO3 entry does
  (`dispatch::apply_alone_made` for a handler's records, `tick::propose::apply_alone` for
  `_apply_effect`).
  `tests/staff_rules.rs` checks the staff's rules of a tick and the shared
  operations' receipts on the worlds of `tests/data/staff_rules.json`.
  `tests/service_rules.rs`, `tests/requests_at_once.rs` and
  `tests/staff_assignment.rs` call `_apply_effect` (`tick::propose::apply_alone`), `_route`,
  `_schedule_travel`, the water and meal service and the staff's assignment, relief and support
  waits on the worlds of the pytest files of the same names (`tests/data/service_rules.json`,
  `generated_texts.json`, `staff_assignment.json`).
  The files
  of the same names in
  `src/core/` re-export them under their old paths and add only what needs Python (exceptions,
  Python numbers and strings, `#[pyfunction]` entries): `src/core/errors.rs` maps an `Error` to
  its Python class (`to_py_err`; `core_call().or_py()?`), `src/core/value.rs` reads and makes
  Python values (`VPy`), `lib.rs` walks Python values into an `Encoder` (`PyEncoder`), and
  `world_core.rs` makes a Python call's error a `Misfit` (`or_misfit()`). The pyclasses over
  the events, `QueueStore` (`src/state/events.rs`) and `EventStore` (`src/eventlog.rs`), are
  frozen handles of the core's `Queue` and `EventLog` (`Home`: their own while they belong to no
  world, or the `World::queue`/`World::log` of the core they are attached to) and keep the Python
  objects beside them in side tables, by slot serial and by record position; `EventRowPy` reads a
  row from a `ScheduledEvent` and makes one.
  A namespace moved to the core keeps its `src/state/<name>.rs`, which re-exports the row, gives
  it its Python side (`py_row!`) and holds `register` and the validators. An `Error` can't be
  turned into a `PyErr` with `?`, because neither type is the wrapper's. `native/Cargo.toml` is
  the workspace of both crates; its lints and `clippy.toml` apply to both
  (`cargo clippy --workspace`).
- `src/core/`: what every lane shares. `value.rs` (`V`, a JSON value that writes canonical
  bytes through `Encoder` and comes back to Python the same), `mathx.rs`, `rng.rs`,
  `interner.rs` (IDs and enum members as small numbers in rows, one Python object for each;
  the parts without Python are in `core/`).
- `src/state/*.rs` and `src/rules/*.rs`: `build.rs` makes each file a module and a Python
  submodule, so a lane adds a file and edits neither `lib.rs` nor `sheltersim/native.py`. A file
  exports `pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()>`, and Python reaches it with
  `native.rules("movement")` or `native.state("movement")`. Catch `native.Unsupported` around a call; the Python code then decides.
  `Unsupported` is a type of its own in `lib.rs`: building one notes its line for
  `benchmarks/rule_shadow/refusal_census.py` (`src/core/census.rs`), off unless that tool turns it
  on. New refusals are counted without doing anything.
  An event's expected outcome is not a refusal. A kernel raises `EventSuppressed` or
  `EventApplicationError` itself, with the Python rule's text and in the rule's order of checks,
  through `src/core/errors.rs` (`errors::suppressed`, `errors::failed`; the type tests of a check,
  such as `errors::int_not_bool`, say Unsupported only where Rust can't tell). The text is saved, as
  the event record's `detail`. Tests compare each one with the class and text written down from
  the Python rule before it went (docs/rust-port/lanes/l8-texts-events.md).
  The same file has the exits for the rules' other errors: where the Python rule raises a
  `ValueError`, a `StateInvariantError` or a class of a contract, a kernel raises that class with
  that text, in the rule's order of checks and before any
  change, instead of `Unsupported`. A Python call the rule makes too (a contract's
  `model_validate`) has its error relayed as it is (`docs/rust-port/lanes/l8-texts-rest.md`).
  `corrupt(namespace, check)` is the one exit for a state the Python rules' checks never let
  through; its text is not Python's. `kernel_refusal(label, error)` is the one exit of a refusal no
  Python rule stands behind (lane L10): `KernelRefusal` of `core/kernels.py`, whose text names the
  kernel and gives its reason, as `kernel_only` raises it.
  `rules/parity.rs` is the example and exposes `core` to the tests. The contact kernels are in the
  core (`core/src/rules/social_propagation.rs` and `information.rs`, lane P8 a4); what they share is
  in `core/src/contacts.rs`, and `contacts.rs` here reads an exact int and gives `fsum` to the tests.
- `src/core/world_core.rs`: `WorldCore`, one per world, in `world.__dict__["_core"]`. It holds the
  core's `World` and its handles: a `NamespaceMirror` per owned field or namespace, the
  `EntityStore`, and the `QueueStore` and `EventStore` whose queue and log are the `World`'s
  (`attach_events`; a store of another live world's core stays there). It holds its world by a weak
  reference (`adopt`, from `state_mirror.start_core`, `deepcopy_world` and `owned_again`);
  `with_world` refuses a world whose core is another's (`NOT_OWNER`), and when the world dies the
  callback lets its objects go (`owner_died`: the entity objects and maps become plain, the queue
  and the log go back to their stores). `own_value` gives a key no lane owns its `ValueRow` slot when
  `state_mirror.NamespaceTable` first stores it; the tree stays attached, and the slot stays when the
  key goes, with no row. A mirror is the core, its slot's index and the Python side: the tracked tree,
  whose sink it is (`sheltersim/core/state_mirror.py`), the node table and the bytes a save was
  given. The row and its taint, detach flag, generation and counters are the slot's. Each handle
  holds the core and the core holds it: `__traverse__` and `__clear__` let the collector break the
  cycle. A shim (`with_row`, `with_row_tree`, `with_value`, `EntityStore::with_rows`) borrows the
  core shared and then its one part (R3 of the design in step5-plan.md); the same part twice is a
  `RuntimeError`. `RowType` holds the operations of its row type (`RowType::of::<T>`), which find
  the `PyRow` of the slot's `dyn Row` by `as_any`. `src/state/sample_namespace.rs` is the small
  test-only namespace; `docs/rust-port/lanes/p4-world-core.md` explains the mechanism. Rows hold no
  Python handles. A `PyRow`'s `apply` runs no Python code and no borrow is held across a callback.
  Read the world's core again after any callback (`core_of`), since a copy-and-swap (where a
  transaction is refused: `rules/transaction.rs`, `state_mirror.transaction_or_copy`) may have moved
  it. `with_world(world, |world| ...)` (`PyWorldCell`, the `WorldCell` of a Python world) borrows
  the `WorldCore` exclusively and gives a core function `&mut World`; nothing inside may call Python
  (R2). `field_held` says whether `world.<name>` is still its slot's tree, for a core function that
  reads a field's row. `NamespaceMirror.generation_of(tree)` changes with every write that reaches a tree, from
  Python or from Rust (it is a new stamp each time), so a check that read a namespace need not read
  it again (`state_mirror.namespace_stamp`, `field_stamp`; `core/src/rules/after_tick.rs`).
- `src/core/txn.rs`: a transaction on a Python world, the core's `Txn` with the native host
  (`state_mirror.transaction` and its `_txn_*` functions): the open one is the `WorldCore`'s. The
  queue and the log are in the core's snapshot (the queue shared, the log by its mark); the host
  moves the objects of the queue's events to the canonical serials and back
  (`QueueStore::txn_remap`, `txn_begin`, `txn_rollback`, `txn_commit`) and cuts the record objects
  at the log's length (`EventStore::cut_to_log`). `EntityStore` and `NamespaceMirror` have the steps
  they take (`txn_begin`, `txn_rollback`, `txn_commit`; `take_root`, `root_from_row`, `put_root`).
  `WorldCore.fork(world)` is `deepcopy(world)` (`state_mirror.deepcopy_world`, lane 6-6): the core
  of the copy, from `World::fork`, with a new mirror per slot and an `EntityStore` whose objects are
  made from the rows in the keys' order (`EntityStore::fork_rows`, `fork_objects`); None for a
  world whose maps are not what the rows say, a tainted store or an open transaction.
- `src/core/configs.rs`: fills the core's `Configs` with the Python contracts
  (`selected_models`, `tick_plan`), called on the same tree at the first read of a section after
  the stamp of `model_configs` changed. A parse never raises there; the read site replays the
  fault as its own refusal, or, where the rule relays the error as it is, as the error of a second
  parse (`reraise`), so the class, the text and the moment of every config error stay the same
  (policies E7 and E14 of `docs/rust-port/step5-plan.md`). Configs no mirror follows, and a patched
  `selected_models`, are parsed at every read. A section the kernels parse themselves is parsed in
  the core from the row of the `model_configs` slot (`kept_value`, `parsed_value`, the core's
  `parse_configs(&World, parse)`); a section a
  Python contract parses (`create_*_formula`, `InformationSpreadConfig.from_mapping`,
  `canonical_proximity_contact_config`, the pydantic zone environment and infrastructure specs)
  keeps the typed values the core defines; a section a Python expression of the configs gives
  (`.get` chains, a truth value, `in {3, 4}`) keeps what the expression raised, which the read site
  raises by evaluating it again (`read_again`). These parses of the sections (`Parser::Host`) are
  all here since lane P10-1c, beside the readers of the sections' tests; `PyHost::fill_section` and
  `read_again` call them, and the glue of `src/rules/` reads a section through the host
  (`PyHost::value`, `host_value`, the core's `kit::section`), as the tick does.
  `config_section(world, name, key=None)` reads a section through its read site's accessor, and
  `config_parses()` counts the parses, for the tests. `watch_config_writes(True)` counts the writes to a
  `model_configs` tree in place inside a tick phase (`config_writes_in_tick()`); the all-path gate
  turns it on and reports any. While it counts, a geometry plan found by the overlay's stamp is
  also checked against the overlay held now (`stale_geometry_hits()`, also reported by the gate).
- `src/host.rs`: `PyHost`, the core's `Host` over a Python world and its controller; each method
  calls the Python the kernels call today (a transaction's host has only the world and what the
  transaction keeps). `HostError` carries the Python error. `host_call(controller, method,
  arguments)` calls one method by name, for the tests. A Python entry of `src/rules/` (the phase
  path, a test, a Python caller) reads what the world holds and its configs' sections through the
  core's readers with `PyHost::of_entry` (`holding_now`: `kit::holding`; `value`, `host_value`:
  `kit::section`), as the tick reads them (lane P10-1c). A reader that runs at each frame or minute
  (an inspection frame, a metric sink) reads the holding of the names it asks alone
  (`holding_of`, with the same answers for them).
- `src/core/geometry.rs`: the floor plan of a world (`Geo::of`): the sections `facility_geometry`
  and `spatial_constraints` parsed by their Python contracts, the plans kept by the overlay's
  stamp, the arrival approach routes and `independent_queues_required` (moved from
  `src/rules/movement_geometry.rs` in lane P10-1c; that file keeps the tests' readers).
- `src/core/actions.rs`: `ActiveActions`, the open actions of a controller (the Run's rows, or a
  plain dict a Python rule asked for), lent to one core call at a time (`on_world`, `read`), and the
  tick's check that they fit their rows (`fits_rows`).
- `src/state/events.rs` also has what the rules' entries ask of the world's events (H6):
  `writable`, `forget_taken`, `applied_row`; `src/eventlog.rs` reads the EventLog's Python records
  as the core reads them (`found_records`, `logged_of`).
- `src/core/run.rs`: `RunCore`, the core's `Run` of one controller (a Run, its continuation or a
  Branch), in `controller._run_core`. The controller makes it in `__init__` from
  `route_capacities` and itself (its `Policies` are read then by the core's `CONTROLLER` spec,
  never raising; `refresh_policies(controller)` reads them again), and each `_refresh_space` calls `refresh_routes(world)`, which starts a new
  epoch and builds (`core/src/rules/travel.rs`, `routers_of`) the people's and the staff's router
  from the world's zone and edge tables right then; the staff's keeps, with a floor plan, the edges
  the core `Geo` finds clear for an independent walker. Where the tables dropped their rows or the
  capacities are no plain dict, and after `refresh_routes()` without a world, the routers are built
  from the controller's world when a kernel first asks in the epoch, which refuses tables that
  dropped their rows (lane P8 c6: `controller.space` and `staff_space` are not read).
- `src/state/population.rs`: the `V` row of `population_presence_v1`, `population_lifecycle_v1`,
  `population_dynamics_v1`, `temporary_spaces_v1`, `geometry_overlay_v1` and `model_configs`
  (`docs/rust-port/lanes/m6-population.md`).
- `src/state/facility_queues.rs`: the Python side of the row of `facility_queues_v1` (the row and its
  rules are the core's): whether the world holds it, and the readers the service, movement and tick
  rules call while they hold other borrows (`with_queues`, `read_fifo`, the records as Python
  documents). The core's kernels write the row alone, and an attached tree follows it (lane P8-0c).
- `src/rules/population.rs`: the glue of `core/src/rules/population.rs` (`domain/population.py`,
  each `kernel_only`): it reads what Python holds, puts a new `population_dynamics_v1` into
  `model_state`, and settles the events a departure cancels.
- `src/rules/population_lifecycle.rs`: the glue of `core/src/rules/population_lifecycle.rs`
  (`domain/population_lifecycle.py`, each `kernel_only`; lane c7): it reads what Python holds and
  the EventLog's records of a command's program, parses the command with its contract, runs the
  command as the body of the core's rule transaction (`txn::rule`, lane P9-1b) with the world's
  validation through the native host, and validates a world from its Python values (policy E6). The core archives and
  restores a person as values and rows; a restored person is a new row at the end of `agents`.
- `src/core/row.rs`: typed rows. The core's `state_row!` (`core/src/row.rs`) writes a record's
  struct, `write_json`, `from_value` (the record read from JSON as a `V`, with the checks of
  `from_py` here) and mirrored setters from its list of fields, and any `Field` is a `Row`;
  a setter writes the tree through `TreeSink`, which `Tree` implements. Here, `py_row!` writes
  the Python side of such a struct from the same names (`PyField`: `from_py`, `to_py`, the handling
  of forwarded writes), and any `PyField` is a `PyRow`, which the slot of a `NamespaceMirror` holds. This
  crate's `state_row!` is both, for a row of this crate only. `src/state/psychological_distress.rs`,
  `src/state/metric_state.rs` and
  `core/src/rules/psychology.rs` are the production template: Rust owns those two, and its rule
  writes their rows, which the trees follow. Start a namespace from `docs/rust-port/lanes/p5-template-namespace.md`.
  `row.rs` also has `Maybe<T>` (a key a record may not have), `str_enum!`, `set_all`,
  `namespace_is_absent` and the `exact_*` readers of a kernel's inputs. A rule other modules call as
  a function uses `core/kernels.py`'s `kernel_only`; `src/state/staff_dynamics.rs` and
  `src/rules/staff_dynamics.rs` (glue over `core/src/rules/staff_dynamics.rs`) are the example of
  a whole module
  (`docs/rust-port/lanes/s-staff-dynamics.md`).
- `src/state/entities.rs`: `EntityStore`, the handle of the typed rows of a world's agents,
  facilities, staff, support requests, active tasks, zones and zone edges (kinds 6 and 7, with a
  taint of their own: `zones_tainted`), from `WorldCore::entities`. The rows,
  `Table` and `Entities` are the core's (`core/src/entities.rs`), held in the core `World`; here
  are the counters, reading a row from an object and making a
  dict of it (`PyEntityField`, `py_entity_row!`), the enum members (`install`, `Member`), the
  links of the objects and the bytes objects of `rows_json`. The rows are the truth (PR 6-1): the
  objects stay the dataclasses of `domain/models.py`, each a view of its row; `IdMap`,
  `__setattr__` and `TrackedList` forward Python's writes. A rule reads and writes the rows only,
  with `with_rows` (a tainted store says `Unsupported`), and puts in or takes out rows by key with
  `insert_entity` and `remove_entity`. When the borrow ends (`with_rows`, `with_world`), the
  objects follow (`EntityStore::flush`): the fields that differ from the view, objects made from new
  rows with no `__init__`, the last values of rows taken out. `verify` checks objects, views,
  rows and key order; `apply_row_ops` drives one table for the tests
  (`docs/rust-port/lanes/p6-entity-windows.md`, `docs/rust-port/step5-plan.md` PR 6-1).
- `src/state/world_fields.rs`: the scalars as `WorldState.__setattr__` writes them
  (`WorldCore.set_scalar`), and the row types of `world.resources` (`IdMap<i64>`) and
  `world.id_counters` (`ValueRow`). A rule writes a counter with `set_id_counter` (the row, then
  the tree) and a new `resources` dict with `replace_resources`; `EventWriter` reads and writes
  the event counter through the row.
- `src/core/tracked.rs`: the only dict and list subclasses a kernel may read through the C API.
  Use `as_readable_dict` and `as_readable_list`, not `downcast` or `downcast_exact`, to accept
  state: a mirrored namespace and a world's `IdMap` are made of those classes, and any other
  subclass may override reads.
- Crates lanes need are already in `Cargo.toml` (`indexmap`, `smallvec`, `rustc-hash`, `itoa`),
  so `Cargo.lock` stays quiet. `HashMap` and `FxHashMap` are for lookups only: anything iterated
  into an output goes through `IndexMap`, or is sorted.

### Float rules

Every float operation of a rule goes through `core::mathx`, and keeps the order of the Python
expression. `tests/integration/test_native_parity.py` compares `mathx` with CPython, and pins the
bits of exp, expm1, pow and fma as fixed vectors.
`cargo clippy --manifest-path native/Cargo.toml -- -A clippy::all -D clippy::disallowed_methods`
enforces the bans in `native/clippy.toml`.

| Python | Rust | Note |
|---|---|---|
| `math.exp(x)`, `math.expm1(x)` | `mathx::exp`, `mathx::expm1` | the `libm` crate. `MathError` where Python raises, as Python's exception |
| `x ** y`, `x ** 2`, `x ** 0.5` | `mathx::pow` | `float_pow`'s special cases, then the `libm` crate. Never `x * x` or `sqrt`: see below. `MathError` where Python raises or returns a complex |
| `math.hypot(*xs)` | `mathx::hypot` | CPython's algorithm, with `libm::fma` |
| `sum(xs)` | `mathx::py_sum`, `sum_nums` | compensated in 3.12. `sum([])` and a sum of ints are ints: `sum_nums` |
| `math.fsum(xs)` | `mathx::py_fsum` | `None` for what is not finite |
| `math.isclose(a, b)` | `mathx::py_is_close` | the default tolerances |
| `max(a, b)`, `min(a, b)` of floats | `mathx::py_max`, `py_min` | first argument on a tie; a NaN compares false |
| `max(0, x)`, any int with a float | `mathx::max_num`, `min_num` | returns the int: JSON writes `0`, not `0.0` |
| `round(x, 8)` | `mathx::py_round` | half to even on the exact binary value |
| `round(x)` | `mathx::py_round_int` | an int, half to even |

`None` means the rule raises `Unsupported`, and Python raises its error or computes the value.

Since model version 2, exp, expm1, pow and fma are the pure Rust `libm` crate at one version
(`=0.2.16` and `Cargo.lock`), not the platform's libm CPython calls, so a rule gives the same bits on
Linux, Windows and macOS. They are within one ulp of glibc's and differ from it in the last bit for
some arguments (`exp(1.0)` among them). `f64::powf`, `f64::exp` and `f64::mul_add` would call the
platform's libm, and LLVM rewrites `powf(x, 2.0)` to `x * x` and `powf(x, 0.5)` to `sqrt`;
`native/clippy.toml` bans them. Python code outside the rules writes `x * x` and `math.sqrt`, which
are correctly rounded everywhere, and not `**` with a float.

## Kernels

- `canonical_json(value)` / `canonical_sha256(value)`: the bytes (and their SHA-256)
  of `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False,
  allow_nan=False).encode()` for plain JSON values. Other values, NaN and infinity
  raise `Unsupported`, so the caller falls back to `json`.
  `core/checkpoint_cache.py` uses `canonical_json` for what a Checkpoint save encodes;
  elsewhere replacing `json` alone saved about 1% (see below).

- `rules/trace.rs`: the glue of the core's trace documents (`core/src/rules/trace.rs`, lane P8 a3):
  `TraceRows` (the core's rows as `PsychologyUpdateResult.traces`), `record_psychology_trace`
  (`_record_psychology_trace`, which reads the rows and the final deltas and calls the core, whose
  document goes to `artifacts.record_psychology_trace` through `Host::trace`). The social and the
  information steps send their documents through `Host::trace` too (lane P8 a4). The writer and its
  `_sanitize` stay in Python.

- `rules/psychology.rs`: the glue of the core's psychology (`core/src/rules/psychology.rs`, lane P8
  a3): each person's minute (`update_psychology`, all the built-in psychology models, with
  `TraceRows` if the Run keeps a trace), the distress monitor and long-term distress
  (`update_long_term`) and writing the final deltas (`apply_deltas`). They replace steps of the
  update phase (`experiments/phases/segments.py`). The model is what the controller's `Policies`
  read (`run::policies_of`); the core reads the entity rows, the view's `ViewData` and the slot rows,
  and writes the rows alone. The `AgentStateDelta`s are made here at the edge, for the social step's
  entry and Python callers, and read back by `apply_deltas`. Every float operation keeps the Python
  order; `max`/`min` return the first argument on a tie and `sum` is CPython 3.12's compensated sum,
  so the results are the same floats. A value `AgentStateDelta`'s checks might refuse raises
  `Unsupported` before anything changes.

- `rules/overlay.rs`: `domain/temporary_spaces.py` and the rules of `domain/geometry_overlay.py`,
  the glue of `core/src/rules/overlay.rs`: capacity, beds and room intake, placing an installed
  facility, checking and applying a floor-plan change, and the checks of both states.
  `core/src/rules/effects.rs`, `rules/proposals.rs`, `rules/replan.rs` and `rules/population.rs`
  call it directly, and the core's end-of-tick check calls the core's capacity. A zone's nominal capacity
  is read from its row where the core holds the world's maps (lane P8 d4), and else from the object.
- `rules/facility_queues.rs`: `domain/facility_queues.py`, the glue of
  `core/src/rules/facility_queues.rs`.
- `rules/effects.rs`: the glue of `core/src/rules/effects.rs` (lane P8 d3), the rules of an
  EffectProgram's events: the guard, the five handlers of `events/effects.py`, the two reads of
  modifier stacks and `validate_effect_program_state`. A handler runs the core's checks on the
  world and its writes in a rule transaction (`rules/transaction.rs`), with the native host for the
  contracts (which takes an event's values frozen, as the Python rule gave them) and the
  validation. Each handler and the guard raise their suppressed and failed texts themselves, in the
  Python rule's order of checks; a value Rust can't judge as Python would says `Unsupported` with
  the world as it was. `rules/staff_order.rs` is
  the glue of `order_staff_operation` with `validate_staff_operation_order`'s checks
  (`core/src/rules/staff_order.rs`), for the `staff_task_order` structure change (the Python
  validator stays for `validate_effect_program`).
  `state/effect_program.rs` is the row of `effect_program_v1`.
- `rules/transaction.rs`: `counts()` of the rule transactions, for the gate. A rule that changes the
  world and keeps the change only if the changed world passes its checks (the effect handlers, the
  operational handlers, population, `apply_presence_command`) runs in the core's rule transaction
  (`core/src/txn.rs` `rule`, lane P9-1b, policy E16): the host's check of its views
  (`Host::txn_check`, `_txn_check`; a refusal is the handler's `KernelRefusal`), `Txn::begin`, the
  body, and the commit or the rollback. There is no copy of the world any more. A rule called inside
  another's open transaction validates the world as a start does and writes it as it is.

- `rules/tick.rs`: `run_tick(controller, world, minute, rng, observer, phases=None)`, one tick,
  through `controller.run_tick` (`experiments/phases/driver.py`), the only path of a controller's
  tick (#221); `phases` (values of `TickPhase`) runs only those, for a test that steps a part of a
  tick. `KernelRefusal` of `tick.run_tick` before anything changes for a tick that a hook would
  change (a patched step or rule in the table of `install`, a toilet, resource or staff manager of
  another class than the installed one), and of `dispatch.run_phase`, with the text the phase's entry
  gives, for a dispatch phase its own checks turn down (`dispatch::ready`: the dispatcher's class,
  the world's stores, the open actions). It reads the
  Run's policies again when the controller holds another manager, proximity config or table of
  neighbors than they were read with (`_tick_marks` on the controller), and the dispatcher's handlers
  when they changed (`_dispatch_marks`), and the open actions into the Run's rows. Then the tick is one
  call of the core's `rules::tick::run_phases` (`core/src/rules/tick/mod.rs`, lane P9-2b) with one
  `PyHost` for all the phases, which has the observer and the random source: the Run's scratch begins,
  and each phase of `TICK_PHASE_ORDER` runs between `Host::phase` before and after, with the clock
  checked after it (`Error::Clock`, `TickExecutionError`). The update phase's movement and replanning
  are still this file's, which the core calls first in that phase. A step's error carries its
  kernel's label, and a refusal is `KernelRefusal` naming it (`only`); the dispatch phase's is
  `dispatch.run_phase`'s. No Python rule stands behind one. `install`, `accept` and `stats()` (what
  ran, and the refusals by reason) stay here.

- `rules/dispatch.rs`: `run_phase(controller, world, minute)`, the dispatch phase. An event type with
  a Rust function (`social_interaction`, `agent_travel_complete`, `toilet_service_complete`,
  `resource_service_complete`, `support_request_timeout`, `staff_support_complete`) runs it when the registered handler
  is that class with its `apply` as defined. The loop calls the other handlers' `apply`, listed in
  `install_table`, with the Python dispatcher's steps around it: that runs their kernels
  (`rules/handlers.rs`, the glue of `core/src/rules/handlers.rs` since lane P8 d2, `rules/information_events.rs`, `rules/staff_operation_events.rs`,
  `docs/rust-port/lanes/m6-handlers-rest.md`); a refusal is their `KernelRefusal`. An EffectProgram's
  marked event has its guard asked in Rust first (`effects::guard_or_suppress`; a false or failing
  guard is recorded with the Python rule's text), then goes on as an unmarked one; a marked event of
  a type with a Rust function, a patched handler and any other event a Rust function refused calls
  `controller._dispatch_one_event` (docs/rust-port/lanes/l8-texts-saved.md). Unsupported only before the first change. `rules/travel_completion.rs`
  is the walk's handler, the glue of `core/src/rules/travel_completion.rs` (lane c7), with `_after_arrival`'s kernel, and `rules/toilet.rs`, `rules/support.rs` and
  `rules/care_receipts.rs` the toilet's service, the support requests and the care receipts
  (`docs/rust-port/lanes/m6-handlers.md`). No borrow is held
  across a Python call. What follows the events is the kernels', called directly; a refusal there
  is the `KernelRefusal` of the rule's Python entry. `stats()` says what ran and why anything was
  refused.
- `rules/decisions.rs`: `propose_rows(controller, view, rng)`, `observed_row(controller, view,
  agent_id)` and `record_outcome(controller, proposal, *, target_id, next_decision_at, committed,
  observed_state)` are the steps `_propose_due`, `_observed_state` and `_record_outcome`, which have
  no Python body (`KERNEL_ONLY_SEGMENTS`), as glue over the core's `rules::behavior` and
  `rules::decisions` (lane P8 a5). A `ProposalRow` holds the core's `Proposal` and answers what
  `_apply_effect` reads; anything else makes the `ActionProposal`. `record_outcome` calls the core's
  record through the `PyHost` (`Host::decision_sink`, `decision_source` and `decision`: the
  `_IndexedMetricSink` record as JSON bytes, or the document of `_decision_document` for
  `RunArtifactWriter` with a trace), then the core's lifecycle and memory. What the core does not
  take (a sink of another class, a proposal Python made, a long observed state, a value the bytes
  can't write) goes, before anything is recorded, to the sink's own `record_decision`, then the same
  rules. A refusal of the lifecycle or the memory is the `KernelRefusal` of its Python entry.
- `rules/behavior.rs`: `propose_due(controller, view, rng)` and `observed_state(controller, view,
  agent_id)` give the core's proposals and the snapshot as the Python `ActionProposal`s and the
  snapshot dict (for tests). They read the core's view, the `Run`'s `Policies` (`refresh_policies`
  after a test puts another `decisions` on the controller), the `CounterRNG`'s seed and the view's
  hash. They run for `UtilityBehaviorPolicy` and `PriorityRuleBehaviorPolicy` (the exact classes,
  which keep only their settings). They write nothing, so `Unsupported` at any point leaves the
  world as it was, and the step raises KernelRefusal: a view Python made (or a partial one), another
  class, and anything `ActionCandidate` and `ActionProposal` would turn down.

- `rules/proximity.rs`: `sample_contacts(controller, minute)` replaces the step
  `_sample_proximity_contacts`, with `DistanceSampledProximityPolicy.sample` on the positions of the
  `movement_v1` row (a tainted row is refused): the glue of `core/src/rules/proximity.rs`, which
  reads the config and the policy from the Run's `Policies`. `sample_of` gives the pairs alone.
- `rules/world_view.rs`: `view_after_movement(controller, world)` and
  `view_for_proposals(controller, world, minute)` are the two steps of the phases that make a
  `WorldView`, which have no Python body (`KERNEL_ONLY_SEGMENTS`). They are glue (lane P8 a1): the
  core's `rules/world_view.rs` builds a snapshot of plain Rust values (`ViewData`) from the entity
  rows and the slot rows (`information_spread_v1`, `psychological_distress_v1`, `body_state_v1`,
  `staff_operations_v1` typed; `operational_events_v1`, `infrastructure_v1`,
  `population_context_v1`, `population_dynamics_v1`, `population_presence_v1`,
  `zone_environment_v1` and `model_configs` as `V`), and writes the audit hash (`state_hash`) as the
  Python rule `WorldView.from_world` did before it went (lane L10). The glue gathers what only Python
  has: `selected_models`, the configs while `world.model_configs` was assigned whole, and
  `simulator_extensions.enabled`. The returned `WorldView`
  makes `agents`, `facilities`, `zone_members` and `group_members` when a Python reader first asks
  (`RustView.python_fields`, through `WorldView.__getattr__`). A person's context comes from
  the core's `rules/person_context.rs` (`PersonContext`) and a zone's conditions from
  `rules/zone_environment.rs` (`zone_environment_view` and its 8 formulas), kept in the controller's
  `Run` (`Run::views`) while the stamps of the slots they were read from stay the same; the
  people's IDs as Python strings are kept on `RunCore`. A holder without `_run_core` keeps nothing.
  A tainted namespace, a value a typed field can't hold, or anything Python would raise on, says
  `Unsupported`, and the step raises KernelRefusal. `view_of(view)` gives a kernel the snapshot; for a view Python
  made it reads the fields the update kernels read and sets `partial`. Nothing refuses a partial
  view for a rule: a rule that reads other fields must check `partial` and say `Unsupported`.

- `rules.social_propagation.update_social_propagation(controller, world, view, deltas, minute, rng)`:
  the update phase's step `_social_propagation` for the three social policies (contact sampling,
  durations, cooldowns, each stress operator). The rules are in the core
  (`core/src/rules/social_propagation.rs`, lane P8 a4): they take the psychology deltas as the core's
  `Delta`s and the policy from the Run's `Policies` (`stress`), read the agents from the entity rows
  and write the row of `social_propagation_v1` (its attached tree follows the row) and the observed
  contacts of `social_network_v1`. The glue reads the Python deltas, makes the new ones at the edge
  (a delta whose stress the model kept is itself), and on the first tick stores the empty namespace
  before the core writes. `CounterRNG` draws, `math.fsum`, `sum`, `max`/`min` ties and float order
  are those of Python. The trace's document goes through `Host::trace`. Another RNG, a policy's
  subclass, and what the deleted Python rule turned down give `Unsupported`, which is
  `KernelRefusal`. `core::rng` can keep each draw of a thread for the tests' comparisons
  (`start_draw_log`, `take_draw_log`).

- `state.social_network.ROW`, `validate`: Rust owns `social_network_v1`
  (`src/state/social_network.rs`). The row lists the records written since the last check, whichever
  way they were written, so the check of a tick reads those and not all of them.

- `rules.social_network.observe_event(world, event, minute)`: what an applied event records, with the
  body's trust copied (`domain/social_network.py`). The social step records its observed
  contacts through the same code (`observed_contacts`, checked before the step writes). `rules.social_dialogue.capture_context`,
  `validate_context`: the state context of a social event, read from the rows. Each is
  `kernel_only`: what the Python rule refused, or a value of another type, gives `Unsupported`
  before any change. The rules are the core's (`core/src/rules/`, lane P8 a2): they read
  `body_state_v1` from its row alone, so a tainted body namespace is refused.

- `rules.social_interaction.apply(handler, world, event)`: `SocialInteractionHandler.apply`, which
  raises the rule's `EventSuppressed` or `EventApplicationError` text for an event that does not
  apply (`docs/rust-port/lanes/m3-social-network.md`, `m3-dispatch.md`). The dispatch loop hands the
  queue's row of the event. `rules.information_events.rumor`, `correction`, `contact`: the
  `apply` of the three information handlers. The rules of both are the core's
  (`core/src/rules/information_events.rs`, `social_interaction.rs`, lane P8 d1): they read the
  event's row, the entity rows and the namespaces' rows, and write the rows alone
  (`information_spread_v1` too). The glue reads the configs, the model selection, whether
  `model_state` holds the information namespace, and the EventLog's record of the contact a
  cooperation or an information reaction answers. A state of schema 1 is upgraded by the host
  (`Host::upgrade_information_state`) where the Python rule upgraded it.

- `state.movement.ROW`: Rust owns `movement_v1` (`src/state/movement.rs`).
- `rules.movement`: the movement rules of a tick (`src/rules/movement.rs`, the glue of
  `core/src/rules/movement.rs`: it reads the arguments, the floor plan, the movement section of
  the configs and the staff's operations where their row does not answer, and makes the
  `ZoneRoute`, `Point2D` and dict values; the floor plan and its
  geometry are the core's, and `src/core/geometry.rs` reads the section `geometry` from
  the Python contracts and gives `Geo::of(world)`, the arrival approach routes and
  `independent_queues_required` of a world's configs): `advance_movement(world, minute)`,
  `start_movement`, `finish_movement`, `route_with_movement_time`, `route_is_physically_clear`,
  `plan_in_room_route`, `queue_slot_position`, `facility_approach_route`,
  `staff_position_snapshots`, `agent_is_at_queue_slot`, `agent_is_at_queue_trip_destination`,
  `agent_is_at_facility_service_point`, and `validate(world)` for `register_rust_validator`.
  `domain/movement.py` calls them through `kernel_only`. A queue trip's replan and a person placed
  in a queue slot depend on the order of the walk: `advance_movement` then plans a second walk that
  reads the positions as it leaves them. `zone_center` is for the tests.
  See `docs/rust-port/lanes/m4-movement.md` and `docs/rust-port/lanes/l3-movement.md`.
- `rules.replan.replan_journeys(controller, state, agent_ids, minute)`: `_replan_journeys` and
  `_replan_staff_support_trip` (`src/rules/replan.rs`, the glue of `core/src/rules/replan.rs`).
  It writes each person's stop before it plans
  their new route, so `Unsupported` comes only before the first person's stop (lane L10: the step
  has no Python body, so any refusal is `KernelRefusal`).
- `rules.queues`: a facility's physical queue (`src/rules/queues.rs`, the glue of
  `core/src/rules/queues.rs`):
  `prepare_facility_queue(world, facility_id, *, resource, minute)`,
  `progress_physical_queue_positions` and `progress_physical_queue_head_to_service` (same
  arguments, optional `queue_id`). `domain/queueing.py` and `domain/movement.py` call them through
  `kernel_only`. Each plans and then writes, in one call.
  A person who gives up on the queue leaves it here too (`_abandon_and_retry`), with the walk
  back and, for an independent FIFO, the queue's record (lane L4).
  See `docs/rust-port/lanes/m4-queues.md`.

- `state.staff_operations.ROW`: Rust owns `staff_operations_v1` (`src/state/staff_operations.rs`;
  the row and its checks of the row and the entity rows are in `core/src/state/`).
  `validate(world)` accepts a world where no staff member has a task; an active record reads the
  pending events, which is the Python rule's.
- `rules.staff`: the staff's assignment of `domain/staff.py` and `domain/staff_operations.py`
  (`src/rules/staff.rs`): `assign_pending(service, minute, validate_state=)`, which is `assign_none`
  (None hands the step to `assign_work`), `update_operations(world, minute)`,
  `assign_operations(world, minute, router)`, `relief(world, minute)`, and
  `wait_snapshot(world, active_support, minute)` for `experiments/metrics.py`. A worker's start plans
  first and then writes one record at a time. They say Unsupported, before any change, for a value
  the rows can't hold and for another staff formula. `format_g` is there for the tests.
  See `docs/rust-port/lanes/m4-staff.md`, `l2-staff.md`.
- `rules.travel.route`: `_route`, with edge capacities too: it counts the trips under way and records
  `zone_edge_capacity_wait` as the Python rule does. A rule that plans (the core's
  `proposals::apply`) holds that record and reserves its sequence on the core's `Events` until it
  writes.
- `rules.metrics`: the KPI columns of a minute (`src/rules/metrics.rs`):
  `snapshot_fields(world, unsettled, minute)` for `MetricSnapshot.from_world` and
  `kpi_values(world, snapshot)` for `operational_kpi_values`. They read the rows and change nothing.
  `capture(world, recorder, minute)` is `MetricsRecorder.capture`; it says Unsupported with nothing
  changed for what the Python rule raises on. The tick's metrics phase (`rules/tick.rs`, for a
  `_MetricSink`) runs the core's `metrics::capture`, whose sample goes to the recorder through the
  native host (`Host::capture_metric`), which makes the `MetricSnapshot`. The open requests are those
  the memo of the last end-of-tick check knows, read in the core.
- `rules.tick_validation`: the per-tick check (`src/rules/tick_validation.rs`, the glue of
  `core/src/rules/tick_validation.rs`, lane P8 d4): `entity_rules(world, resource_config)` answers
  for the entity and event rules of `WorldState.validate` at the end of a tick, of every request,
  reading the entity rows, the field and namespace rows and the queue's event rows (an event that
  is only a Python object gets its row first), for a WorldState v6 with its
  resource config and a v5 one (no resource_service) with None. A write past an object's
  `__setattr__` or past an `IdMap` reaches no row and no rule (policy E12). It has no text: it
  returns the zones whose capacity is still the overlay's rule, or says Unsupported and the Python
  rules run. `population_dynamics(world)` is the validator of that namespace with no one pending,
  `validate_presence(world)` the one of the declared `population_presence_v1`. See
  `docs/rust-port/lanes/m5-metrics.md`.
- `rules.after_tick`: all of `WorldState.validate_after_tick` (`src/rules/after_tick.rs`, the glue
  of `core/src/rules/after_tick.rs`, lane P8 d4), at every tick of a WorldState v5 or v6:
  `after_tick(world, memo)` returns with the memo (`TickMemo`, in `world.__dict__["_tick_memo"]`,
  which holds the core's `Memo`) moved on, or says Unsupported with the memo as it was but for the
  configs' stamp, and `validate(validate_event_history=False)` runs with no memo. `Plan` is what it
  reads of `model_configs`, made by the contracts in `core/tick_validation.py` (`tick_plan`) when the
  configs change and kept as the core's `TickPlan` in the section `tick_plan` of the configs.
  Validators come from `_RUST_VALIDATORS`, asked with the world given back (the core's `Checks`); a
  namespace with none is refused. The memo reads a settled request again when its row is written
  (`Table::marked`, from Rust or Python). `TickMemo.unsettled_requests(world)` serves
  `WorldState.unsettled_support_requests`; `facts()` and `written_requests(world)` are for tests.
  `install` lists what a patch of sends the tick to the Python check. `stats()` says what ran. See `docs/rust-port/lanes/l7-validate.md` and, for v5,
  `l7b-world-v5.md`.
- `state.shared_operations.ROW`: Rust owns `shared_operations_v1`
  (`src/state/shared_operations.rs`, the row in `core/src/state/`): the heads typed, each receipt
  its canonical bytes.
  `validate_tick(world)` accepts a tick's state (no replay of the history) when the kernel that
  appended the recent receipts, or the Python rules through `recent_passed(world)` or
  `history_passed(world)`, checked them; it compares a recent staff operation receipt with the
  retained completions of `staff_operations_v1` itself.
  `rules.common_operations.record_shared_operation(world, record)` appends a receipt of
  `contracts/operations.py`, read as the core's `Receipt` (`core/src/rules/common_operations.rs`).
- `state.resource_ledger.ROW`, `validate`: Rust owns `resource_ledger_v1`
  (`src/state/resource_ledger.rs`; the row and `passes_rules_in` in `core/src/state/`, which reads
  the world's resources, people and zones through a `LedgerWorld`): the accounts typed, the audit trail as the canonical bytes of each
  record, with the newest of each resource kept typed for the check. A record the rules would turn
  down, or a write into a record already there, taints the namespace. `validate` answers for the
  check of a tick; the replay of a save stays in Python. `rules.resources.apply_operation(world,
  event, operation)`, `metric_values(world, *, tick_memo=None)` and `reservation_id_was_used(world,
  id)` are `kernel_only` (`docs/rust-port/lanes/m4-resources.md`).

- `state.resource_service.ROW`, `validate`: Rust owns `resource_service_v1`
  (`src/state/resource_service.rs`): typed, with the two histories as lists of records. `validate`
  answers for the check of a tick; a save still reads the histories in Python.
  `rules.service` (`src/rules/service.rs`, the glue of `core/src/rules/service.rs`):
  `facilities_to_serve(manager)`, `serve_head(manager,
  facility_id, resource_id, minute)`, `complete(manager, event)`, `request(manager, agent_id,
  facility_id, *, minute=None, validate_state=None, queue_id=None)` and
  `record_request_outcome(manager, *, ...)`, each `kernel_only` on `ResourceServiceManager`.
  `serve_head` is one pass of `arbitrate`'s loop after the positions; it returns the started event's
  ID, or None to stop. `serve_independent_head` is the same pass of one independent queue.
- `rules.proposals.apply_effect(controller, proposal)`: `_apply_effect`, which returns
  `(target_id, next_decision_at, committed)`; the step has no Python body (`KERNEL_ONLY_SEGMENTS`).
  The rules are the core's (`core/src/rules/proposals.rs`, lane P8 e1): the bed, the choice of a
  facility and the trip, the queues asked at once, the support request with its care receipts, the
  information model's record of it (on the row of `information_spread_v1`; the tree follows), and a
  social action's event. `src/rules/proposals.rs` reads what Python holds by the action and calls
  the core; a request's LLM hooks go through the native host (`Host::prioritize_support_request`,
  `Host::consider_support_request`) with the world let go of, and the step reads again after them.
  A person the water or meal queue turns down at once is recorded with the queue's text; a capacity
  wait recorded while the facility was chosen is written before the queue's own events.
- `rules.travel`: `route(controller, origin_zone_id, destination_zone_id, agent_id)` and
  `schedule_travel(controller, *, ...)`: `_route` and `_schedule_travel` (`src/rules/travel.rs`).
  The glue of `core/src/rules/travel.rs`. The graph is the core's `Router` of the controller's Run
  (`src/core/run.rs`), built from the world's tables at each refresh of the space. See
  `docs/rust-port/lanes/m4-service.md` and step5-plan.md, PRs 7-3, 7-4 and P8 c6.

- `state.body_state.ROW`: Rust owns `body_state_v1` (`src/state/body_state.rs`): the records and
  relationships typed, the history as the canonical bytes of each item. `rules.body_state.
  record_social_event(world, event, minute)` is `domain/body_state.py`'s rule with the operators of
  `policies/affect.py` and `policies/relationships.py`, in the core since lane P8 a2 (it writes the
  row alone; a history item is its canonical JSON, and a tree Python reads has the item's keys in
  that order); `rules.psychology.apply_deltas` writes the
  row. `state.body_state.rows_valid(world)` accepts the header, records and relationships, reading
  only the relationships written since it last said yes. The config and history rules read what
  the row doesn't hold and stay in Python (`docs/rust-port/lanes/m3-body-state.md`).

- `rules.information.sample_contacts(controller, minute, rng)`: the sample phase's step
  `_sample_information_contacts` for the three information policies (contact sampling, durations,
  forgetting, transfers and their probabilities). The rules are in the core
  (`core/src/rules/information.rs`, lane P8 a4): the model and its config come from the Run's
  `Policies` (`information`), the agents from the entity rows and the household of the view from
  `population_presence_v1`'s row. The core writes the row of `information_spread_v1` (its attached
  tree follows the row) and schedules the events through the world's queue (`kit::Events`), with the
  errors of `ScheduledEvent` for what a row refuses. The trace's document goes through
  `Host::trace`. Another RNG, a policy's subclass and whatever the deleted Python rule turned down
  give `Unsupported`, which is `KernelRefusal`. `planned_contacts` returns the same tick's draws and
  contacts without changing anything, for the tests.

  Seven days, run beside the previous commit: `update_agent_state` 76.4 s -> 26.1 s,
  `sample_endogenous_events` 38.6 s -> 1.8 s, with the other changes of this step
  (docs/rust-port.md).

- `agent_dynamics_valid`, `movement_positions_valid`, `world_agents_valid`: fast
  acceptance of the large loops of validation, read where Python keeps the state. The
  namespaces Rust owns have their `validate` in `state/` instead, which reads the row
  (`docs/rust-port/lanes/t-tick-validation.md`). Each
  returns true only if every entry passes the Python rules; on false the caller runs
  those rules, which accept the value or raise their own error, so results and error
  messages come from one place.

- `state.events.QueueStore` and `EventWriter` (`src/state/events.rs` over
  `core/src/events/queue.rs` and `writer.rs`, `docs/rust-port/lanes/e-events.md`): the pending
  events behind
  `sheltersim.events.queue.EventQueue`, in the order of `pending()`, and what a Rust rule uses to
  allocate a sequence, schedule an `EventRow` and record an outcome with no Python object. Python
  gets a `ScheduledEvent` when it first reads the event. `EventWriter::of` takes the stores from
  the world's core (or, for a copy that holds another world's EventLog, from the world's
  attributes). An order outside i64 moves that queue to Python (`_LargeOrderEventQueue`; the world
  then holds no queue), and `EventWriter::of` then says `Unsupported`.

- `state.agent_dynamics.ROW` and `rules.agent_dynamics`: Rust owns `agent_dynamics_v1` and every
  writer but `remove_agent` and `population_lifecycle.py`, which forward one record each
  (`docs/rust-port/lanes/f-action-lifecycle.md`). `rules.action_lifecycle`: the lifecycle receipts.
  `ActiveActions` is a window on a controller's open actions, the core's rows in its Run
  (`Run::actions`, through `RunCore::with_actions`; a store made without a Run holds its own),
  gives a Python rule a plain dict
  (`as_plain`) and reads it back when it fits; it is not saved. A receipt is an `EventRow` appended
  through the core's `kit::Events`, with no Python object until Python reads the record. The rules
  are the core's (`core/src/rules/`, lane P8 a2); the glue reads the configs they read
  (`configs::kept_section`) and whether the log holds an event applied.

- `EventStore` (stage B1, `src/eventlog.rs` over `core/src/events/log.rs`): the realized
  EventLog behind `sheltersim.events.queue.EventLog`.
  Each record is its canonical JSON bytes, its Python object, or both; the indexes
  (ID, sequence, minute, type, provenance keys, care requests, support kinds, facility
  events) live in Rust. Appending from Python writes the bytes from the frozen fields in the same
  step, and `all_encoded` says that every record has bytes written or checked here: a save's
  restore check then reads the live log instead of parsing the bytes and comparing them
  (`CheckpointHistoryCache.restored_log`). A value the writer refuses, such as a str subclass in a
  payload, leaves that record's bytes to the Python encoder, and the old check runs. `extend_saved` builds the log from saved bytes, parsing and
  checking them in parallel without the GIL: each must be the canonical JSON of a record
  that passes the `EventLogRecord`/`ScheduledEvent` checks. A record object is made
  only when Python reads it. `split_json_array` cuts a saved chunk into item bytes.
  Day 7: Checkpoint load 17.4 s → 4.2 s, a fork near day 7 24.0 s → 6.2 s
  (docs/rust-port.md). Appending reads a record's index fields through interned names, and
  skips the `Mapping` check of a provenance value that is a str.

- `sensitive_candidates(raws)` (stage B3): the distinct object keys of the JSON texts
  `raws`, and their distinct string values the privacy patterns could match, read in
  parallel without the GIL. The patterns stay in Python (`storage/projects.py`); the
  kernel leaves out only strings that can not match them, as explained at the top of
  `core/src/checkpoint.rs`. A Checkpoint save passes all its new chunks at once, and each
  history index row is checked through it.

- `chunk_hashes(items, chunks)` (stage B3): the SHA-256 of `[item,item,...]` for each
  `(start, count)` of a list of item bytes, in parallel and without joining them. These
  are the leaves of the Checkpoint v2 hash tree (`core/checkpoint_cache.py`) and the
  archive's chunk keys.

- `same_saved_records(records, raws, record_type, event_type)` (`src/checkpoint.rs`): whether
  each live EventLog record is exactly what restoring its saved bytes gives, field by field
  with the types, without building the restored record. A save uses it only for a log with a
  record whose bytes the Python encoder made. It reads every field of the bytes,
  and a false says nothing about `==`: the restore check of a save then compares restored
  records as before (`WorldState.same_state`).

- `record_json(record, record_type, event_type)` (`src/checkpoint.rs`): the canonical JSON of
  `record.to_dict()`, read from the frozen record without thawing it, or None for any value
  that is not frozen plain JSON. `same_saved_records` reads the result back on its own path.

- `index_row(row)` (`src/checkpoint.rs`): one pass over a history index row of plain values,
  giving its canonical JSON, the hash, its identities (but for the keys of `NOT_ENTITY_KEYS`, the
  same list as `_NOT_ENTITY_KEYS`, #138) and event references (`_identities`,
  `_event_references` in `storage/history_index.py`), the sorted keys joined by NUL
  and the strings for the privacy check, and the text its tokens are read from. None for any other
  row, which Python reads as before.
- `dumps_length(value)` (`src/checkpoint.rs`): `len(json.dumps(value, ensure_ascii=False).encode())`
  of a plain JSON value, for the size limit of a decision's observed state.
- `EntityStore.rows_json(version)` (`src/state/entities.rs` over `core/src/entities.rs`): each entity row's canonical JSON, by
  field, in the order of `WorldState._canonical_data`. A row nothing reached for writing keeps its
  bytes object, so the same object means the same row (`Table::changed`, set by every way to a row
  for writing). `NamespaceMirror.row_json()` is the same for an owned namespace. A capture takes both
  (`state_mirror.owned_row_bytes`), so a new row needs only `state_row!` and its registry line.
- `HistoryPins` (`src/history.rs`): references to everything inside a history's entries, from when
  they were shown equal to their documents. `unchanged` reads only the containers, and false says
  nothing, so the caller compares with `same_json_items`.

- `same_json_items(current, documents, start, count)` (stage B3, `src/history.rs`): whether
  each of `current[:count]` is exactly `documents[start + i]`, with the types: 1, 1.0 and
  True differ, a tuple and a list differ, and floats compare by their bits. A save compares
  each append-only history with the documents of its last save, and its full validation
  does not read the entries found unchanged (`core/save_validation.py`). Anything that is
  not a plain JSON value compares as different, and the rules read the entry again.

- `EventStore.active_lifecycle_records()`: the `agent_action_lifecycle` records that
  decide each agent's active action, in the order a full pass inserts the agents, from
  an index kept as records are added. The runner calls it at each fork and resume
  instead of making every lifecycle record's object. `EventStore.of_types_from(types,
  start)` reads a type's records from a position, for a reader that keeps its result as
  the log grows.

- `scheduled_event_checked(...)` and `view_state_hash(...)` (`native/src/records.rs`, stage
  B4): `ScheduledEvent._check` with its frozen payload and provenance, and the audit hash
  of a `WorldView` without building its document. Each accepts only what the Python code
  accepts (exact types, stable IDs, finite floats, keys it can write without escaping);
  anything else raises `Unsupported`, and the Python code decides or raises. No model
  formula moved in this stage. `float_json` is the float form of the canonical encoder, which
  writes ryu's digits directly in the range where they equal `float.__repr__`.
  Seven days: docs/rust-port.md.

## Results so far (WSL2, 16 cores, generated Scenario, 100 people)

| Check | Python | Rust |
|---|---:|---:|
| Floats compared with `repr` (random bits, decimals, edges) | — | 799,870, 0 mismatches |
| Day-7 world_state document (352 MB), JSON | 3,257 ms | 1,209 ms (byte-identical) |
| Same, JSON + SHA-256 | 3,864 ms | 1,256 ms |
| One day, project route (develop 4272b40) | 71.7 s | 70.8 s (same world/events/KPI hashes) |
| Full offline tests | — | 1372 passed, 4 skipped |

### Checkpoint save (stage B3)

| Check | Before | After |
|---:|---:|---:|
| One late save, `TimelineStore.record` (near day 7) | 1,076 ms | 420 ms |
| Both hashes | 376 ms | 20 ms |
| Privacy check of the new chunks | 119 ms | 10 ms |
| Seven days, `TimelineStore.record` (336 saves) | 288.8 s | 125.2 s (same world/events/KPI hashes) |

### Psychology update (tick prototype)

| Check | Python | Rust |
|---|---:|---:|
| Deltas compared each tick for one day (100 people, 13 fields) | — | 0 mismatches (now `tests/integration/test_native_kernels.py`, 360 minutes) |
| One day, library route: total | 42.6 s | 35.9 s (same world/events/KPI hashes) |
| Same, `update_agent_state` phase | 15.5 s | 9.2 s |
| Psychology policy alone, one day | 6.66 s | 0.61 s |
| └ of which the Rust kernel | — | 0.07 s (about 95x the Python computation) |
| Full offline tests with both kernels | — | 1372 passed, 4 skipped |

Python without the per-agent traces (`deltas_without_traces`, PR #286 on develop) takes the same policy from 6.54 s to
1.25 s a day and one day from 42.0 s to 36.7 s, close to the Rust kernel's 35.9 s.
Against that Python, this kernel alone saves under a second a day.

Most of the native path's time is building the inputs from `WorldView` and the
`AgentStateDelta` objects in Python. The computation itself is about 95x faster;
keeping the per-agent state in Rust would remove most of the boundary cost.

### Canonical JSON

Python's json encoder is already C, and saves reuse most history bytes, so
replacing JSON alone saves about 1%. The time is in Python-level loops (row
building, validation, model updates); those loops, not leaf functions, have to
move for Rust to pay off.
