//! `social_propagation_v1`: the contacts of the last tick and the cooldowns of the social model
//! (`_read_runtime_state` in `policies/social.py`). The rule that writes it is
//! `rules/social_propagation.rs`.

use crate::row::IdMap;
use crate::state_row;

pub const NAME: &str = "social_propagation_v1";
pub const SCHEMA_VERSION: i64 = 1;

state_row! {
    #[derive(Clone, Copy)]
    pub struct ContactRecord {
        last_seen_minute: i64,
        consecutive_minutes: i64,
    }
}

state_row! {
    pub struct SocialRow {
        schema_version: i64,
        contacts: IdMap<ContactRecord>,
        agent_cooldown_until: IdMap<i64>,
    }
}
