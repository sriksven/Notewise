//! Terminal output helpers.

/// Render a duration in milliseconds as `HH:MM:SS`, or a marker for a live meeting.
pub fn duration(ms: Option<i64>) -> String {
    let Some(ms) = ms else {
        return "  live  ".to_string();
    };

    let total = (ms / 1000).max(0);
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

/// Render an action item with its owner, when one is known.
pub fn action_item(text: &str, owner: Option<&str>) -> String {
    match owner {
        Some(owner) => format!("{text} — {owner}"),
        None => format!("{text} — unassigned"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_hours_minutes_and_seconds() {
        assert_eq!(duration(Some(0)), "00:00:00");
        assert_eq!(duration(Some(45_000)), "00:00:45");
        assert_eq!(duration(Some(90_000)), "00:01:30");
        assert_eq!(duration(Some(3_661_000)), "01:01:01");
    }

    #[test]
    fn a_recording_meeting_shows_as_live() {
        assert_eq!(duration(None).trim(), "live");
    }

    #[test]
    fn durations_stay_column_aligned() {
        // The meeting list is a fixed-width table; a ragged column is unreadable.
        assert_eq!(duration(Some(0)).len(), duration(None).len());
    }

    #[test]
    fn a_negative_duration_does_not_render_as_garbage() {
        // Clock adjustment mid-meeting can produce this; it must not print "-1:-1:-1".
        assert_eq!(duration(Some(-5000)), "00:00:00");
    }

    #[test]
    fn unowned_action_items_are_marked_rather_than_left_blank() {
        assert_eq!(action_item("Write docs", None), "Write docs — unassigned");
        assert_eq!(action_item("Write docs", Some("alex")), "Write docs — alex");
    }
}
