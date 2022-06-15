use std::time::Duration;

use timings::Timings;

fn main() {
    // Timings doesn't force any particular benchmarking style on you, and
    // because of that, it can be adapted to a wide variety of use cases.
    //
    // This example is meant to showcase the simplicity. Let's say that we have
    // a process that has three steps, and we want to measure each step's
    // performance when using different backends.
    let (timings, stats_handle) = Timings::new();

    // Measure the operations
    for algorithm in ["backend a", "backend b", "backend c"] {
        for iteration in 0..100 {
            // Perform the three steps, all which are just faked by
            // thread::sleep()
            let timing = timings.begin(algorithm, "step 1");
            perform_step_one(algorithm, iteration);
            timing.finish();

            let timing = timings.begin(algorithm, "step 2");
            perform_step_two(algorithm, iteration);
            timing.finish();

            let timing = timings.begin(algorithm, "step 3");
            perform_step_three(algorithm, iteration);
            timing.finish();
        }
    }

    // The stats thread will not return its report until all instances of
    // `Measurements` have been dropped.
    drop(timings);

    // Retrieve the statistics report, which includes summaries as well as the
    // raw measurements collected.
    let stats = stats_handle.join().unwrap();

    // Print the summary.
    timings::print_table_summaries(&stats).unwrap();
}

fn perform_step_one(algorithm: &str, iter: u64) {
    std::thread::sleep(Duration::from_micros(fake_algorithm_duration(
        algorithm, 1, iter,
    )));
}

fn perform_step_two(algorithm: &str, iter: u64) {
    std::thread::sleep(Duration::from_micros(fake_algorithm_duration(
        algorithm, 2, iter,
    )));
}

fn perform_step_three(algorithm: &str, iter: u64) {
    std::thread::sleep(Duration::from_micros(fake_algorithm_duration(
        algorithm, 3, iter,
    )));
}

fn fake_algorithm_duration(algorithm: &str, step: usize, iter: u64) -> u64 {
    // "backend a" will have the lowest standard deviation, and the others will
    // have higher variation in sleep times.
    match (algorithm, step) {
        ("backend a", 1) => 200,
        ("backend a", 2) => 300,
        ("backend a", _) => 400,
        ("backend b", 1) => 450 + 300 * (iter % 3) - 450,
        ("backend b", 2) => 500 + 200 * (iter % 4) - 400,
        ("backend b", _) => 600 + 100 * (iter % 10) - 500,
        (_, 1) => 250 + 400 * iter % 3 - 150,
        (_, 2) => 700 + 300 * iter % 4 - 600,
        (_, _) => 1200 + 200 * (iter % 10) - 1000,
    }
}
