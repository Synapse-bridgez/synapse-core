use chrono::{Datelike, Utc, Weekday};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettlementSchedule {
    Hourly,
    Daily,
    Weekly,
}

impl SettlementSchedule {
    pub fn is_eligible_now(&self) -> bool {
        let now = Utc::now();
        match self {
            SettlementSchedule::Hourly => true,
            SettlementSchedule::Daily => {
                let last_settlement = now - chrono::Duration::hours(23);
                now.date_naive() != last_settlement.date_naive()
            }
            SettlementSchedule::Weekly => now.weekday() == Weekday::Mon,
        }
    }

    pub fn should_settle(&self, last_settlement_time: Option<i64>) -> bool {
        match self {
            SettlementSchedule::Hourly => true,
            SettlementSchedule::Daily => {
                if let Some(last_time) = last_settlement_time {
                    let last_settlement =
                        chrono::DateTime::<Utc>::from_timestamp(last_time, 0).unwrap_or(Utc::now());
                    let now = Utc::now();
                    now.date_naive() != last_settlement.date_naive()
                } else {
                    true
                }
            }
            SettlementSchedule::Weekly => {
                if let Some(last_time) = last_settlement_time {
                    let last_settlement =
                        chrono::DateTime::<Utc>::from_timestamp(last_time, 0).unwrap_or(Utc::now());
                    let now = Utc::now();
                    now.weekday() == Weekday::Mon
                        && now.date_naive() != last_settlement.date_naive()
                } else {
                    Utc::now().weekday() == Weekday::Mon
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hourly_schedule_always_eligible() {
        let schedule = SettlementSchedule::Hourly;
        assert!(
            schedule.should_settle(None),
            "Hourly should always settle without last_settlement_time"
        );
        assert!(
            schedule.should_settle(Some(Utc::now().timestamp() - 60)),
            "Hourly should settle even if settled 1 minute ago"
        );
    }

    #[test]
    fn test_daily_schedule_settles_once_per_day() {
        let schedule = SettlementSchedule::Daily;

        assert!(
            schedule.should_settle(None),
            "Daily should settle without prior settlement"
        );

        let one_hour_ago = Utc::now().timestamp() - 3600;
        assert!(
            schedule.should_settle(Some(one_hour_ago)),
            "Daily should not settle if already settled today"
        );

        let yesterday = Utc::now().timestamp() - 86400;
        assert!(
            schedule.should_settle(Some(yesterday)),
            "Daily should settle if last settlement was yesterday"
        );
    }

    #[test]
    fn test_weekly_schedule_settles_on_monday() {
        let schedule = SettlementSchedule::Weekly;

        assert!(
            schedule.should_settle(None),
            "Weekly should settle without prior settlement if today is Monday"
        );

        let now = Utc::now();
        if now.weekday() == Weekday::Mon {
            let last_monday = now.timestamp() - 604800;
            assert!(
                schedule.should_settle(Some(last_monday)),
                "Weekly should settle on Monday if last settlement was last Monday"
            );

            assert!(
                !schedule.should_settle(Some(now.timestamp() - 3600)),
                "Weekly should not settle on Monday if already settled today"
            );
        }
    }

    #[test]
    fn test_settlement_schedule_respects_boundaries() {
        let daily = SettlementSchedule::Daily;
        let now = Utc::now().timestamp();
        let yesterday_eod = now - 86401;

        assert!(
            daily.should_settle(Some(yesterday_eod)),
            "Daily should settle after 24 hours have passed"
        );

        let just_now = now - 1;
        assert!(
            !daily.should_settle(Some(just_now)),
            "Daily should not settle if settled in the last hour"
        );
    }

    #[test]
    fn test_schedule_not_settled_multiple_times_per_day() {
        let schedule = SettlementSchedule::Daily;
        let first_settlement_time = Utc::now().timestamp();

        let should_settle_again = schedule.should_settle(Some(first_settlement_time));
        assert!(
            !should_settle_again,
            "Daily should not settle again on the same day"
        );
    }

    #[test]
    fn test_weekly_does_not_settle_on_tuesday() {
        let schedule = SettlementSchedule::Weekly;
        let now = Utc::now();

        if now.weekday() != Weekday::Mon {
            let should_settle = schedule.is_eligible_now();
            assert!(
                !should_settle,
                "Weekly should not be eligible on non-Monday (today is {:?})",
                now.weekday()
            );
        }
    }
}
