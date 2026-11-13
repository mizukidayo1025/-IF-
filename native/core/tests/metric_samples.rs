//! The metric samples of a Run without Python, packed (`plain::MetricSamples`, #252 PR 6-4): each
//! sample comes back as it was taken and writes the line it wrote, the IDs behind its counts packed
//! when their text comes back the same and kept as text otherwise; the digest is the one of the
//! lines.

use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::engine::metric_line;
use sheltersim_core::plain::MetricSamples;
use sheltersim_core::rules::metrics::MetricSample;

fn sample(ids: &str, seed: f64) -> MetricSample {
    MetricSample {
        toilet_queue_length: 3,
        toilet_active_users: usize::MAX,
        toilet_working_units: -2,
        toilet_wait_count: seed,
        toilet_wait_total_minutes: -0.0,
        toilet_wait_mean_minutes: 0.1 + 0.2,
        toilet_wait_max_minutes: 1e22,
        stress_mean: 5e-324,
        stress_high_count: 0,
        staff_assigned_task_count: 7,
        staff_busy_count: 1,
        staff_load_ratio: 1.0 / 3.0,
        unresolved_support_request_count: 38,
        source_ids_json: ids.to_owned(),
        support_no_qualified_worker_count: 2,
        support_no_qualified_worker_request_minutes: 12.5,
        support_fairness_promoted_assignment_count: 47.0,
        support_wait_staff_busy_request_minutes: 10287.0,
        support_wait_staff_off_shift_request_minutes: 1042.0,
    }
}

#[test]
fn each_sample_comes_back_and_writes_its_line() {
    let texts = [
        // Canonical JSON: packed as a value.
        r#"{"busy_staff":["staff_005"],"realized_events":["action-lifecycle-129640","toilet-complete-129514"],"support_wait_by_agent":{"A001":{"reason_codes":["staff_resting"],"request_id":"support-491654b59eeff3f42042de16","waited_minutes":46}}}"#,
        "{}",
        // Not what the value writes again (a space, keys out of order, a float as an int): kept as text.
        r#"{"b": 1}"#,
        r#"{"b":1,"a":2}"#,
        r#"{"x":1.50}"#,
        // Not JSON at all.
        "not json",
        "",
    ];
    let mut samples = MetricSamples::default();
    let mut lines = Vec::new();
    for (minute, text) in texts.iter().enumerate() {
        let taken = sample(text, minute as f64);
        lines.push(metric_line(&taken, minute as i64).unwrap());
        samples.push(&taken, minute as i64).unwrap();
        let (again, at) = samples.sample(minute).unwrap().unwrap();
        assert_eq!(again, taken);
        assert_eq!(at, minute as i64);
        assert_eq!(again.toilet_wait_total_minutes.to_bits(), (-0.0f64).to_bits());
    }
    assert_eq!(samples.len(), texts.len());
    for (index, line) in lines.iter().enumerate() {
        assert_eq!(samples.line(index).unwrap().as_ref(), Some(line));
    }
    assert!(samples.line(texts.len()).unwrap().is_none());
    assert_eq!(samples.digest(), sha256_hex(&lines.join(&b'\n')));
    assert_eq!(MetricSamples::default().digest(), sha256_hex(b""));
    assert!(samples.size() < lines.iter().map(Vec::len).sum::<usize>());
}
