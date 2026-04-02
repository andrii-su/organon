use crate::entity::LifecycleState;

const DORMANT_DAYS: i64 = 30;
const ARCHIVE_DAYS: i64 = 90;

pub fn compute_state(accessed_at: i64, now: i64) -> LifecycleState {
    let days_since_access = (now - accessed_at) / 86400;
    match days_since_access {
        0..=1 => LifecycleState::Active,
        2..=DORMANT_DAYS => LifecycleState::Active,
        d if d <= ARCHIVE_DAYS => LifecycleState::Dormant,
        _ => LifecycleState::Archived,
    }
}
