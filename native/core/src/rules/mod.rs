//! The kernels moved to the core: pure functions over `&World`, whose PyO3 crate's file of the same
//! name gathers what Python holds and makes the Python values (lane 5-4, step5-plan.md).
//!
//! One line per file, in the block of the lane that moves it (design-p8.md, section 3.3), so that
//! lanes running at the same time each add a line in their own block.

// ---- Shared: the helpers the rules call each other for (P8-0a), and the pilot (5-4) ----------
pub mod kit;
pub mod metrics;

// ---- P8a: people rules -------------------------------------------------------------------------
pub mod action_lifecycle;
pub mod agent_dynamics;
pub mod body_state;
pub mod person_context;
pub mod psychology;
pub mod social_dialogue;
pub mod social_network;
pub mod trace;
pub mod world_view;
pub mod zone_environment;

// ---- P8b: operations ---------------------------------------------------------------------------
pub mod care_receipts;
pub mod common_operations;
pub mod resources;
pub mod staff_dynamics;

// ---- P8c: population and movement --------------------------------------------------------------
pub mod facility_queues;
pub mod movement;
pub mod queues;

// ---- P8d: events and checks --------------------------------------------------------------------
pub mod information_events;
pub mod social_interaction;

// ---- P8e: proposals ----------------------------------------------------------------------------
