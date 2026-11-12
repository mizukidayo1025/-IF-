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
pub mod behavior;
pub mod body_state;
pub mod decisions;
pub mod information;
pub mod person_context;
pub mod psychology;
pub mod social_dialogue;
pub mod social_network;
pub mod social_propagation;
pub mod trace;
pub mod world_view;
pub mod zone_environment;

// ---- P8b: operations ---------------------------------------------------------------------------
pub mod care_receipts;
pub mod common_operations;
pub mod resources;
pub mod staff_dynamics;
// lane b2
pub mod staff_operation_events;
pub mod staff_order;
// lane b3
pub mod staff;
// lane b4
pub mod support;

// ---- P8c: population and movement --------------------------------------------------------------
pub mod facility_queues;
pub mod movement;
pub mod overlay;
pub mod population;
pub mod queues;
pub mod service;
pub mod toilet;
// lane c6
pub mod proximity;
pub mod replan;
pub mod travel;
// lane c7
pub mod population_lifecycle;
pub mod travel_completion;

// ---- P8d: events and checks --------------------------------------------------------------------
pub mod after_tick;
pub mod information_events;
pub mod social_interaction;
pub mod tick_validation;
// lane d2
pub mod handlers;
// lane d3
pub mod effects;

// ---- P8e: proposals ----------------------------------------------------------------------------
pub mod proposals;

// ---- P9-2a: the tick's phases 2-6 ----------------------------------------------------------------
pub mod tick;
