//! BIP113 median-time-past of an already-collected timestamp window.

/// Median of `times` (unsorted OK). An empty window is an error.
pub fn median_time_past_times(times: &[u32]) -> Result<u32, &'static str> {
    if times.is_empty() {
        return Err("invariant: empty median time");
    }
    let mut sorted = times.to_vec();
    sorted.sort_unstable();
    Ok(sorted[sorted.len() / 2])
}
