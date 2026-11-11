//! The kernels moved to the core: pure functions over `&World`, whose PyO3 crate's file of the same
//! name gathers what Python holds and makes the Python values (lane 5-4, step5-plan.md).
//!
//! One line per file, in the block of the lane that moves it (design-p8.md, section 3.3), so that
//! lanes running at the same time each add a line in their own block.

// ---- Shared: the helpers the rules call each other for (P8-0a), and the pilot (5-4) ----------
pub mod kit;
pub mod metrics;

// ---- P8a: people rules -------------------------------------------------------------------------

// ---- P8b: operations ---------------------------------------------------------------------------

// ---- P8c: population and movement --------------------------------------------------------------

// ---- P8d: events and checks --------------------------------------------------------------------

// ---- P8e: proposals ----------------------------------------------------------------------------
