//! Percentiles and the summary of a set of durations.
use std::time::Duration;

/// The nearest-rank percentile `p` (0 to 100) of sorted values: the smallest value
/// with at least `p` percent of the values at or below it.
pub fn percentile(sorted: &[Duration], p: f64) -> Option<Duration> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

/// The median, p95, p99 and maximum of a set of durations.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
    pub max: Duration,
}

impl Summary {
    pub fn of(mut values: Vec<Duration>) -> Summary {
        values.sort_unstable();
        let at = |p| percentile(&values, p).unwrap_or_default();
        Summary {
            count: values.len(),
            p50: at(50.0),
            p95: at(95.0),
            p99: at(99.0),
            max: values.last().copied().unwrap_or_default(),
        }
    }
}

/// A duration in microseconds, for the report.
pub fn micros(d: Duration) -> String {
    format!("{:.1} µs", d.as_secs_f64() * 1e6)
}

/// The signed difference `a - b` in microseconds.
pub fn delta_micros(a: Duration, b: Duration) -> String {
    format!("{:+.1} µs", (a.as_secs_f64() - b.as_secs_f64()) * 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(values: impl IntoIterator<Item = u64>) -> Vec<Duration> {
        values.into_iter().map(Duration::from_millis).collect()
    }

    /// The values 1 to 100 ms: the nearest rank of p is p itself.
    #[test]
    fn percentiles_of_one_to_a_hundred() {
        let s = Summary::of(ms((1..=100).rev()));
        assert_eq!(s.count, 100);
        assert_eq!(s.p50, Duration::from_millis(50));
        assert_eq!(s.p95, Duration::from_millis(95));
        assert_eq!(s.p99, Duration::from_millis(99));
        assert_eq!(s.max, Duration::from_millis(100));
    }

    #[test]
    fn percentiles_of_few_values() {
        let three = ms([30, 10, 20]);
        let mut sorted = three.clone();
        sorted.sort();
        // ceil(0.5 * 3) = 2nd value; ceil(0.99 * 3) = 3rd.
        assert_eq!(percentile(&sorted, 50.0), Some(Duration::from_millis(20)));
        assert_eq!(percentile(&sorted, 99.0), Some(Duration::from_millis(30)));
        assert_eq!(percentile(&sorted, 0.0), Some(Duration::from_millis(10)));
        assert_eq!(percentile(&[], 50.0), None);
        assert_eq!(Summary::of(Vec::new()), Summary::default());
    }

    #[test]
    fn durations_are_written_in_microseconds() {
        assert_eq!(micros(Duration::from_nanos(1_250)), "1.2 µs");
        assert_eq!(
            delta_micros(Duration::from_micros(30), Duration::from_micros(10)),
            "+20.0 µs"
        );
        assert_eq!(
            delta_micros(Duration::from_micros(10), Duration::from_micros(30)),
            "-20.0 µs"
        );
    }
}
