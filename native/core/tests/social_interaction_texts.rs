//! `SocialInteractionHandler`'s suppressed and failed texts, in the Python rule's order of checks
//! (Issue #225, lane M1; moved from `tests/integration/test_social_interaction_texts.py`).
//!
//! These texts are saved: the dispatcher writes them to the event's record as `detail`. Each case
//! gives an event that fails one check, often together with later ones, and expects the text of
//! the first check that fails. The core's handler (`dispatch::apply_alone`) must raise it itself
//! and change nothing (`support/cases.rs`). The worlds are the gate's generated Scenario at minute 5,
//! and with its rumors at minute 40, as the Python side edited them (`tests/support/core_texts.py`:
//! two people in one zone and household, a request opened, applied contacts in the log...). The
//! Python rule is gone (lane L10, `kernel_only`): the texts and their classes are written down from
//! it, as are the writes of an applied interaction (the case's `after` world). A social action
//! between two people runs every tick, so its kernel checks in its own order on the way to an
//! applied action and finds Python's first failing check only when one fails.

mod support;

use support::cases::check_group;

const FILE: &str = "event_texts";
const RUMORS: &str = "rumor_texts";

#[test]
fn an_untyped_social_event_s_texts_are_python_s() {
    check_group(FILE, "an_untyped_social_event_s_texts_are_python_s", 10);
}

/// 57 texts in order, then the reassurance applies twice (without and with a captured context)
/// with the Python rule's writes: the participant is relieved.
#[test]
fn a_social_action_s_texts_come_in_python_s_order() {
    check_group(FILE, "a_social_action_s_texts_come_in_python_s_order", 59);
}

#[test]
fn a_social_action_whose_participant_moved_is_suppressed_first() {
    check_group(FILE, "a_social_action_whose_participant_moved_is_suppressed_first", 3);
}

#[test]
fn family_care_and_voluntary_support_texts_come_after_the_duration() {
    check_group(FILE, "family_care_and_voluntary_support_texts_come_after_the_duration", 4);
}

#[test]
fn complaint_response_texts_are_python_s() {
    check_group(FILE, "complaint_response_texts_are_python_s", 25);
}

/// The texts, then the cooperation applies with the Python rule's writes: both are a little calmer
/// and trust a little more.
#[test]
fn cooperation_texts_are_python_s() {
    check_group(FILE, "cooperation_texts_are_python_s", 25);
}

/// The texts, then the reaction applies with the Python rule's writes: the listener's query
/// pressure rises by the claim's.
#[test]
fn information_reaction_texts_are_python_s() {
    check_group(RUMORS, "information_reaction_texts_are_python_s", 21);
}

#[test]
fn a_reaction_without_the_information_state_is_suppressed() {
    check_group(FILE, "a_reaction_without_the_information_state_is_suppressed", 1);
}

#[test]
fn another_interaction_type_is_failed_after_the_common_checks() {
    check_group(FILE, "another_interaction_type_is_failed_after_the_common_checks", 8);
}
