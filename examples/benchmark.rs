//! Shows how to use `timings::Benchmark` to measure `parking_lot`'s Mutex vs
//! `std::sync::Mutex`.

use std::convert::Infallible;

use timings::{Benchmark, BenchmarkImplementation, Label, LabeledTimings, Timings};

const ITERATIONS: usize = 1000;

fn main() {
    let timings = Timings::default();

    Benchmark::default()
        .with_each_number_of_threads([1, 4, 8, 16])
        .with::<ParkingLotMutex>()
        .with::<StdMutex>()
        .run(&timings)
        .unwrap();

    let report = timings.wait_for_stats();
    timings::print_table_summaries(&report).unwrap();
}

struct ParkingLotMutex {
    mutex: parking_lot::Mutex<()>,
    metric: Label,
}

impl BenchmarkImplementation<Label, (), Infallible> for ParkingLotMutex {
    type SharedConfig = Label;

    fn label(_number_of_threads: usize, _config: &()) -> Label {
        Label::from("parking_lot")
    }

    fn initialize_shared_config(
        number_of_threads: usize,
        _config: &(),
    ) -> Result<Self::SharedConfig, Infallible> {
        Ok(Label::from(format!("lock-{number_of_threads:02}t")))
    }

    fn reset(_shutting_down: bool) -> Result<(), Infallible> {
        Ok(())
    }

    fn initialize(
        _number_of_threads: usize,
        metric: Self::SharedConfig,
    ) -> Result<Self, Infallible> {
        Ok(Self {
            mutex: parking_lot::Mutex::new(()),
            metric,
        })
    }

    fn measure(&mut self, measurements: &LabeledTimings<Label>) -> Result<(), Infallible> {
        for _ in 0..ITERATIONS {
            let timing = measurements.begin(self.metric.clone());
            drop(self.mutex.lock());
            timing.finish();
        }
        Ok(())
    }
}

struct StdMutex {
    mutex: std::sync::Mutex<()>,
    metric: Label,
}

impl BenchmarkImplementation<Label, (), Infallible> for StdMutex {
    type SharedConfig = Label;

    fn label(_number_of_threads: usize, _config: &()) -> Label {
        Label::from("std")
    }

    fn initialize_shared_config(
        number_of_threads: usize,
        _config: &(),
    ) -> Result<Self::SharedConfig, Infallible> {
        Ok(Label::from(format!("lock-{number_of_threads:02}t")))
    }

    fn reset(_shutting_down: bool) -> Result<(), Infallible> {
        Ok(())
    }

    fn initialize(
        _number_of_threads: usize,
        metric: Self::SharedConfig,
    ) -> Result<Self, Infallible> {
        Ok(Self {
            mutex: std::sync::Mutex::new(()),
            metric,
        })
    }

    fn measure(&mut self, measurements: &LabeledTimings<Label>) -> Result<(), Infallible> {
        for _ in 0..ITERATIONS {
            let timing = measurements.begin(self.metric.clone());
            drop(self.mutex.lock().unwrap());
            timing.finish();
        }
        Ok(())
    }
}

#[test]
fn runs() {
    main()
}
