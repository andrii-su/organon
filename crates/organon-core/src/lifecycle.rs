use crate::entity::LifecycleState;

pub const DEFAULT_DORMANT_DAYS: i64 = 30;
pub const DEFAULT_ARCHIVE_DAYS: i64 = 90;

/// Compute lifecycle state given access timestamp, current time, and configurable thresholds.
pub fn compute_state(accessed_at: i64, now: i64, dormant_days: i64, archive_days: i64) -> LifecycleState {
    let days_since_access = (now - accessed_at) / 86400;
    if days_since_access <= dormant_days {
        LifecycleState::Active
    } else if days_since_access <= archive_days {
        LifecycleState::Dormant
    } else {
        LifecycleState::Archived
    }
}

/// Convenience wrapper using default thresholds (for callers without config).
pub fn compute_state_default(accessed_at: i64, now: i64) -> LifecycleState {
    compute_state(accessed_at, now, DEFAULT_DORMANT_DAYS, DEFAULT_ARCHIVE_DAYS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86400;
    const D: i64 = DEFAULT_DORMANT_DAYS;
    const A: i64 = DEFAULT_ARCHIVE_DAYS;

    fn cs(accessed_days_ago: i64) -> LifecycleState {
        let now = 1_000_000_000i64;
        compute_state(now - accessed_days_ago * DAY, now, D, A)
    }

    #[test]
    fn accessed_today_is_active()        { assert_eq!(cs(0),  LifecycleState::Active); }
    #[test]
    fn accessed_yesterday_is_active()    { assert_eq!(cs(1),  LifecycleState::Active); }
    #[test]
    fn boundary_30_days_is_active()      { assert_eq!(cs(30), LifecycleState::Active); }
    #[test]
    fn accessed_31_days_ago_is_dormant() { assert_eq!(cs(31), LifecycleState::Dormant); }
    #[test]
    fn boundary_90_days_is_dormant()     { assert_eq!(cs(90), LifecycleState::Dormant); }
    #[test]
    fn accessed_91_days_ago_is_archived(){ assert_eq!(cs(91), LifecycleState::Archived); }

    #[test]
    fn custom_thresholds() {
        let now = 1_000_000_000i64;
        // dormant at 7 days, archive at 14 days
        assert_eq!(compute_state(now - 8 * DAY, now, 7, 14), LifecycleState::Dormant);
        assert_eq!(compute_state(now - 15 * DAY, now, 7, 14), LifecycleState::Archived);
    }
}
