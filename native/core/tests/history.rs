//! The KPI history of a Run's stored samples (#266; `sheltersim_core::history`): the rows the
//! history index gives, paged as its query pages them. `native/wasm/tests/node_api_answers.mjs`
//! checks the whole answer against the Python server's on a Run; this checks the paging's edges.

use sheltersim_core::events::packed::Packer;
use sheltersim_core::history::{kpi_row, review_metrics, KpiQuery, KpiTable};
use sheltersim_core::plain::MetricSamples;
use sheltersim_core::rules::metrics::MetricSample;

fn sample(stress: f64) -> MetricSample {
    MetricSample {
        toilet_queue_length: 2,
        toilet_active_users: 1,
        toilet_working_units: 3,
        toilet_wait_count: 1.0,
        toilet_wait_total_minutes: 4.5,
        toilet_wait_mean_minutes: 4.5,
        toilet_wait_max_minutes: 4.5,
        stress_mean: stress,
        stress_high_count: 0,
        staff_assigned_task_count: 1,
        staff_busy_count: 1,
        staff_load_ratio: 0.5,
        unresolved_support_request_count: 0,
        source_ids_json: r#"{"busy_staff":["staff_001"]}"#.to_owned(),
        support_no_qualified_worker_count: 0,
        support_no_qualified_worker_request_minutes: 0.0,
        support_fairness_promoted_assignment_count: 0.0,
        support_wait_staff_busy_request_minutes: 0.0,
        support_wait_staff_off_shift_request_minutes: 0.0,
    }
}

/// A table of samples at the minutes 0 to 29, stored in blocks of the given size, as a Run in the
/// browser stores them, and read back.
fn table() -> KpiTable {
    let mut samples = MetricSamples::default();
    for minute in 0..30 {
        samples.push(&sample(minute as f64 / 100.0), minute, Some(&[[minute; 6], [-minute; 6]])).unwrap();
    }
    let (packer, blocks) = samples.store();
    let mut tables = Vec::new();
    packer.write_tables(0, 0, &mut tables);
    let mut table = KpiTable::new();
    table.read_tables(&tables).unwrap();
    for block in blocks.blocks() {
        table.read_block(block).unwrap();
    }
    assert_eq!(table.len(), 30);
    table
}

fn query<'a>(start: i64, end: i64, step: i64, limit: usize, after: Option<(i64, &'a str)>) -> KpiQuery<'a> {
    KpiQuery { start, end, step, limit, after, filtered: false, run_id: "r", branch_id: "main" }
}

fn minutes(items: &[u8]) -> Vec<i64> {
    let rows: Vec<serde_json::Value> = serde_json::from_slice(items).unwrap();
    rows.iter().map(|row| row["minute_index"].as_i64().unwrap()).collect()
}

#[test]
fn a_page_is_the_index_s() {
    let table = table();
    let page = table.page(&query(3, 20, 4, 3, None)).unwrap();
    assert_eq!(minutes(&page.items), [3, 7, 11]);
    assert_eq!((page.last, page.more), (Some(11), true));
    let next = table.page(&query(3, 20, 4, 3, Some((11, "11")))).unwrap();
    assert_eq!(minutes(&next.items), [15, 19]);
    assert!(!next.more);
    // The cursor's key compares as text with the row's minute: "5" comes after "10".
    assert_eq!(minutes(&table.page(&query(0, 29, 1, 3, Some((5, "10")))).unwrap().items), [5, 6, 7]);
    assert_eq!(minutes(&table.page(&query(0, 29, 1, 3, Some((5, "5")))).unwrap().items), [6, 7, 8]);
    // Past the samples, a step past the range, a filter.
    assert_eq!(table.page(&query(30, 40, 1, 5, None)).unwrap().items, b"[]");
    assert_eq!(minutes(&table.page(&query(2, 29, 1000, 5, None)).unwrap().items), [2]);
    let filtered = KpiQuery { filtered: true, ..query(0, 29, 1, 5, None) };
    assert_eq!(table.page(&filtered).unwrap().items, b"[]");
    assert_eq!(table.coverage(3, 40), Some((27, 3, 29)));
    assert_eq!(table.coverage(30, 40), None);
}

#[test]
fn a_row_is_canonical_json_with_the_ledger_columns() {
    let row = kpi_row(&sample(0.25), 7, Some(&[[1, 2, 3, 4, 5, 6], [7, 8, 9, 10, 11, 12]]), "r", "main").unwrap();
    let text = String::from_utf8(row).unwrap();
    assert!(text.starts_with(r#"{"branch_id":"main","food_meal_units_delivered":8,"food_meal_units_initial":7,"#), "{text}");
    assert!(text.contains(r#""minute_index":7,"record_kind":"kpis","run_id":"r","staff_assigned_task_count":1,"staff_busy_count":1,"staff_load_ratio":0.5,"#), "{text}");
    assert!(text.contains(r#""time_semantics":"end_of_tick","toilet_active_users":1,"#), "{text}");
    assert!(text.ends_with(r#""water_ml_reserved":6,"water_ml_served":3}"#), "{text}");
    assert!(!text.contains("source_ids_json"));
    let without = String::from_utf8(kpi_row(&sample(0.25), 7, None, "r", "main").unwrap()).unwrap();
    assert!(!without.contains("water_ml"));
    let metrics = String::from_utf8(review_metrics(&sample(0.25)).unwrap()).unwrap();
    assert!(metrics.contains(r#""toilet_working_units":3.0,"#) && !metrics.contains("minute_index"), "{metrics}");
}

#[test]
fn stored_samples_that_do_not_read_back_are_errors() {
    let mut table = KpiTable::new();
    assert!(table.read_block(&[3, 0, 0]).is_err());
    assert!(table.read_block(&[1]).is_err());
    let mut tables = Vec::new();
    Packer::new().write_tables(0, 0, &mut tables);
    table.read_tables(&tables).unwrap();
    assert!(table.read_block(&[0xff; 4]).is_err());
}
