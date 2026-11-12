"""The tick phases of OfflineSimulationController, one module per phase.

Each module holds one mixin of the controller. Its docstring gives the phase's steps in their order,
which the Rust driver runs (driver.py); each step with a Python entry is a named method whose
docstring states its contract (segments.py).
"""
