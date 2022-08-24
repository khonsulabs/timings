//! This test is split on its own due to its sensitivity in timing. The regular
//! test harness runs each test in its own thread, which causes contention
//! between threads which can mess up the accuracy of the thread starter.
//!
//! All that being said, there seems to be no truly guaranteed way to get
//! guaranteed sub-microsecond wakeup times.

use std::time::{Duration, Instant};

use timings::ThreadSync;

fn main() {
    // We need at least 2 threads to test this. By using only half of the CPUs,
    // we are more likely to get a favorable test result.
    let number_of_threads = (std::thread::available_parallelism().unwrap().get() / 2).max(2);

    let (sender, receiver) = flume::bounded(number_of_threads);

    ThreadSync::synchronize_spawn_with(
        number_of_threads,
        Duration::from_secs(1),
        &sender,
        |sender| {
            sender.send(Instant::now()).unwrap();
        },
    );
    drop(sender);

    let mut start_times = Vec::with_capacity(number_of_threads);
    for _ in 0..number_of_threads {
        start_times.push(receiver.recv().unwrap());
    }
    assert!(receiver.try_recv().is_err());

    let earliest_start = *start_times.iter().min().unwrap();
    let latest_start = *start_times.iter().max().unwrap();
    let max_delta = latest_start.duration_since(earliest_start).as_nanos();
    let total_deltas: u64 = start_times
        .iter()
        .map(|time| u64::try_from(time.duration_since(earliest_start).as_nanos()).unwrap())
        .sum();
    let average_delta = total_deltas as f64 / number_of_threads as f64;
    let mut all_deltas = start_times
        .iter()
        .map(|time| u64::try_from(time.duration_since(earliest_start).as_nanos()).unwrap())
        .collect::<Vec<_>>();
    all_deltas.sort();
    let stddev = timings::stddev(&all_deltas, average_delta);
    println!(
        "Sync start instant avg delta: {average_delta:.03}ns, max delta: {max_delta}ns, stddev {stddev:03}ns ({number_of_threads} threads)",
    );
    println!("All deltas (nanoseconds): {all_deltas:?}",);

    // This test is a failure if max_dela is > 1ms.
    assert!(max_delta < 1_000_000);
}
