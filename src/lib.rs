use std::{
    collections::{BTreeMap, HashMap},
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    ops::Deref,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread::{JoinHandle, Thread},
    time::{Duration, Instant},
};

#[cfg(feature = "formatting")]
mod formatting;

#[cfg(feature = "formatting")]
pub use formatting::*;
use parking_lot::{Condvar, Mutex};

pub struct Timings<Metric> {
    sender: flume::Sender<(Label, Metric, Duration)>,
}

impl<Metric> Clone for Timings<Metric> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

impl<Metric> Timings<Metric>
where
    Metric: Send + Ord + Hash + Eq + Display + Debug + Clone + 'static,
{
    pub fn new() -> (Self, JoinHandle<BTreeMap<Metric, MetricSummary>>) {
        let (sender, receiver) = flume::unbounded();
        let stats_handle = std::thread::Builder::new()
            .name(String::from("measurements"))
            .spawn(move || stats_thread(receiver))
            .unwrap();
        (Self { sender }, stats_handle)
    }

    pub fn begin(&self, label: impl Into<Label>, metric: Metric) -> Measurement<'_, Metric> {
        Measurement {
            target: &self.sender,
            label: label.into(),
            metric,
            start: Instant::now(),
        }
    }
}

pub struct Measurement<'a, Metric> {
    target: &'a flume::Sender<(Label, Metric, Duration)>,
    label: Label,
    metric: Metric,
    start: Instant,
}

impl<'a, Metric> Measurement<'a, Metric> {
    pub fn finish(self) {
        let duration = Instant::now()
            .checked_duration_since(self.start)
            .expect("time went backwards. Restart benchmarks.");
        self.target
            .send((self.label, self.metric, duration))
            .unwrap();
    }
}

fn stats_thread<Metric: Ord + Eq + Hash + Display + Debug + Clone>(
    metric_receiver: flume::Receiver<(Label, Metric, Duration)>,
) -> BTreeMap<Metric, MetricSummary> {
    let mut all_results: BTreeMap<Metric, BTreeMap<Label, Vec<u64>>> = BTreeMap::new();
    let mut accumulated_label_stats: BTreeMap<Label, Duration> = BTreeMap::new();
    let mut longest_by_metric = HashMap::new();
    while let Ok((label, metric, duration)) = metric_receiver.recv() {
        let metric_results = all_results.entry(metric.clone()).or_default();
        let label_results = metric_results.entry(label.clone()).or_default();
        let nanos = u64::try_from(duration.as_nanos()).unwrap();
        label_results.push(nanos);
        let label_duration = accumulated_label_stats.entry(label).or_default();
        longest_by_metric
            .entry(metric)
            .and_modify(|existing: &mut Duration| {
                *existing = (*existing).max(duration);
            })
            .or_insert(duration);
        *label_duration += duration;
    }

    let mut operations = BTreeMap::new();
    for (metric, label_metrics) in all_results {
        let label_stats = label_metrics
            .iter()
            .map(|(label, stats)| {
                let mut sum = 0;
                let mut min = u64::MAX;
                let mut max = 0;
                for &nanos in stats {
                    sum += nanos;
                    min = min.min(nanos);
                    max = max.max(nanos);
                }
                let average = sum as f64 / stats.len() as f64;
                let stddev = stddev(stats, average);

                let mut outliers = Vec::new();
                let mut plottable_stats = Vec::new();
                let mut min_plottable = u64::MAX;
                let mut max_plottable = 0;
                for &nanos in stats {
                    let diff = (nanos as f64 - average).abs();
                    let diff_magnitude = diff / stddev;
                    if stats.len() == 1 || diff_magnitude < 3. {
                        plottable_stats.push(nanos);
                        min_plottable = min_plottable.min(nanos);
                        max_plottable = max_plottable.max(nanos);
                    } else {
                        // Outlier
                        outliers.push(diff_magnitude);
                    }
                }

                (
                    label,
                    MetricStats {
                        average,
                        min,
                        max,
                        stddev,
                        outliers,
                        plottable_stats,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        for (label, metrics) in label_stats.into_iter() {
            let report = operations
                .entry(metric.clone())
                .or_insert_with(|| MetricSummary {
                    invocations: label_metrics.values().next().unwrap().len(),
                    labels: BTreeMap::new(),
                });
            report.labels.insert(label.clone(), metrics);
        }
    }
    operations
}

#[derive(Clone)]
pub enum Label {
    Static(&'static str),
    Owned(Arc<String>),
}

impl Debug for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Static(s) => Debug::fmt(s, f),
            Self::Owned(s) => Debug::fmt(s, f),
        }
    }
}

impl Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self)
    }
}

impl Deref for Label {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        match self {
            Label::Static(str) => str,
            Label::Owned(string) => string,
        }
    }
}

impl Hash for Label {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write(self.as_bytes());
    }
}

impl Eq for Label {}

impl PartialEq for Label {
    fn eq(&self, other: &Self) -> bool {
        // Compare the contained strings.
        **self == **other
    }
}

impl Ord for Label {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Compare the contained strings.
        (&**self).cmp(other)
    }
}

impl PartialOrd for Label {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl From<&'static str> for Label {
    fn from(label: &'static str) -> Self {
        Self::Static(label)
    }
}

impl From<String> for Label {
    fn from(label: String) -> Self {
        Self::Owned(Arc::new(label))
    }
}

fn stddev(data: &[u64], average: f64) -> f64 {
    if data.is_empty() {
        0.
    } else {
        let variance = data
            .iter()
            .map(|value| {
                let diff = average - (*value as f64);

                diff * diff
            })
            .sum::<f64>()
            / data.len() as f64;

        variance.sqrt()
    }
}

#[derive(Debug)]
pub struct MetricStats {
    pub average: f64,
    pub min: u64,
    pub max: u64,
    pub stddev: f64,
    pub plottable_stats: Vec<u64>,
    pub outliers: Vec<f64>,
}

#[derive(Debug)]
pub struct MetricSummary {
    pub invocations: usize,
    pub labels: BTreeMap<Label, MetricStats>,
}

pub struct Benchmark<Metric, Config, Error> {
    threads: Vec<usize>,
    configs: Vec<Config>,
    thread_start_timeout: Duration,
    functions: Vec<Arc<dyn AnyBenchmarkImplementation<Metric, Config, Error>>>,
}

impl<Metric, Error> Benchmark<Metric, (), Error> {
    pub fn new() -> Self {
        Self {
            threads: Vec::new(),
            configs: vec![()],
            thread_start_timeout: Duration::from_secs(15),
            functions: Vec::new(),
        }
    }
}

impl<Metric, Error> Default for Benchmark<Metric, (), Error> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Metric, Config, Error> Benchmark<Metric, Config, Error>
where
    Error: Send + Sync + 'static,
    Config: Send + Clone + 'static,
    Metric: Send + Sync + 'static,
{
    pub fn for_config(config: Config) -> Self {
        Self {
            threads: Vec::new(),
            configs: vec![config],
            thread_start_timeout: Duration::from_secs(15),
            functions: Vec::new(),
        }
    }

    pub fn for_each_config(configs: Vec<Config>) -> Self {
        Self {
            threads: Vec::new(),
            configs,
            thread_start_timeout: Duration::from_secs(15),
            functions: Vec::new(),
        }
    }

    pub fn with_number_of_threads(mut self, threads: usize) -> Self {
        self.threads = vec![threads];
        self
    }

    pub fn with_thread_start_timeout(mut self, thread_start_timeout: Duration) -> Self {
        self.thread_start_timeout = thread_start_timeout;
        self
    }

    pub fn with_each_number_of_threads<ThreadCounts: IntoIterator<Item = usize>>(
        mut self,
        threads: ThreadCounts,
    ) -> Self {
        self.threads = threads.into_iter().collect();
        self
    }

    pub fn with<Implementation: BenchmarkImplementation<Metric, Config, Error>>(mut self) -> Self {
        self.functions
            .push(Arc::new(BenchmarkImpl::<Implementation>::default()));
        self
    }

    pub fn run(self, timings: &Timings<Metric>) -> Result<(), Error> {
        let threads = if self.threads.is_empty() {
            vec![1]
        } else {
            self.threads
        };

        for thread_count in threads {
            for config in &self.configs {
                for function in &self.functions {
                    function.reset(false)?;
                    let starter = ThreadSync::new(thread_count, self.thread_start_timeout);
                    let thread_handles = function.measure(
                        thread_count,
                        config,
                        starter.signal().clone(),
                        timings,
                    )?;

                    if starter.signal_threads().is_err() {
                        eprintln!("Benchmark thread failed to start in time.");
                    }

                    for handle in thread_handles {
                        if let Err(err) = handle.join() {
                            eprintln!("Benchmark thread panic: {err:?}");
                        }
                    }
                    function.reset(true)?;
                }
            }
        }

        Ok(())
    }
}

pub trait BenchmarkImplementation<Metric, Config, Error>: Sized + Send + Sync + 'static {
    type SharedConfig: Clone + Send + Sync + 'static;

    fn initialize_shared_config(
        number_of_threads: usize,
        config: &Config,
    ) -> Result<Self::SharedConfig, Error>;

    fn reset(shutting_down: bool) -> Result<(), Error>;

    fn initialize(number_of_threads: usize, config: Self::SharedConfig) -> Result<Self, Error>;

    fn measure(&mut self, measurements: &Timings<Metric>) -> Result<(), Error>;
}

trait AnyBenchmarkImplementation<Metric, Config, Error>: Sync + Send + 'static {
    fn reset(&self, shutting_down: bool) -> Result<(), Error>;
    fn measure(
        &self,
        number_of_threads: usize,
        data: &Config,
        thread_sync: ThreadSignal,
        measurements: &Timings<Metric>,
    ) -> Result<Vec<JoinHandle<Result<(), Error>>>, Error>;
}

// impl<T, Metric, Config, Error> AnyBenchmarkImplementation<Metric, Config, Error> for T
// where
//     T: BenchmarkImplementation<Metric, Config, Error>,
// {
//     fn measure(
//         config: Config,
//         thread_sync: ThreadSync,
//         measurements: &Timings<Metric>,
//     ) -> Result<(), Error> {
//         let mut data = Self::initialize(config)?;
//         thread_sync.wait_for_signal();
//         T::measure(&mut data, measurements)
//     }
// }

struct BenchmarkImpl<T>(PhantomData<T>);

impl<T> Default for BenchmarkImpl<T> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T, Metric, Config, Error> AnyBenchmarkImplementation<Metric, Config, Error>
    for BenchmarkImpl<T>
where
    T: BenchmarkImplementation<Metric, Config, Error> + Send + Sync,
    Error: Send + 'static,
    Metric: Send + 'static,
{
    fn measure(
        &self,
        number_of_threads: usize,
        config: &Config,
        thread_sync: ThreadSignal,
        measurements: &Timings<Metric>,
    ) -> Result<Vec<JoinHandle<Result<(), Error>>>, Error> {
        let mut thread_handles = Vec::with_capacity(number_of_threads);
        let shared_config = T::initialize_shared_config(number_of_threads, config)?;
        for _ in 0..number_of_threads {
            let config = shared_config.clone();
            let measurements = measurements.clone();
            let thread_sync = thread_sync.clone();
            thread_handles.push(std::thread::spawn(move || {
                let mut data = T::initialize(number_of_threads, config)?;
                thread_sync.wait();
                T::measure(&mut data, &measurements)
            }));
        }
        Ok(thread_handles)
    }

    fn reset(&self, shutting_down: bool) -> Result<(), Error> {
        T::reset(shutting_down)
    }
}

#[derive(Debug, Clone)]
pub struct ThreadSignal {
    data: Arc<ThreadSyncData>,
}

#[derive(Debug)]
struct ThreadSyncData {
    parked_threads: Mutex<Vec<Thread>>,
    ready_sync: Condvar,
    countdown: AtomicUsize,
    number_of_threads: usize,
    thread_start_timeout: Duration,
}

impl ThreadSignal {
    pub fn wait(self) {
        let mut parked_threads = self.data.parked_threads.lock();
        parked_threads.push(std::thread::current());
        drop(parked_threads);
        self.data.ready_sync.notify_one();
        std::thread::park();

        // Once we're unparked, enter a spinlock waiting for countdown to reach 0.
        let mut remaining_threads = self.data.countdown.fetch_sub(1, Ordering::SeqCst) - 1;
        while remaining_threads > 0 {
            remaining_threads = self.data.countdown.load(Ordering::SeqCst);
        }
    }
}

#[derive(Debug)]
pub struct ThreadSync {
    signal: ThreadSignal,
}

impl ThreadSync {
    pub fn new(number_of_threads: usize, thread_start_timeout: Duration) -> Self {
        let signal = ThreadSignal {
            data: Arc::new(ThreadSyncData {
                countdown: AtomicUsize::new(number_of_threads),
                thread_start_timeout,
                number_of_threads,
                parked_threads: Mutex::default(),
                ready_sync: Condvar::new(),
            }),
        };
        Self { signal }
    }

    pub const fn signal(&self) -> &ThreadSignal {
        &self.signal
    }

    pub fn synchronize_spawn<F: Fn() + Clone + Send + 'static>(
        number_of_threads: usize,
        thread_start_timeout: Duration,
        thread_main: F,
    ) {
        let starter = Self::new(number_of_threads, thread_start_timeout);
        for _ in 0..number_of_threads {
            let thread_main = thread_main.clone();
            let sync = starter.signal().clone();
            std::thread::spawn(move || {
                sync.wait();
                thread_main();
            });
        }

        starter.signal_threads().unwrap();
    }

    pub fn synchronize_spawn_with<T: Clone + Send + 'static, F: Fn(T) + Clone + Send + 'static>(
        number_of_threads: usize,
        thread_start_timeout: Duration,
        context: &T,
        thread_main: F,
    ) {
        let starter = Self::new(number_of_threads, thread_start_timeout);
        for _ in 0..number_of_threads {
            let thread_main = thread_main.clone();
            let sync = starter.signal().clone();
            let context = context.clone();
            std::thread::spawn(move || {
                sync.wait();
                thread_main(context);
            });
        }

        starter.signal_threads().unwrap();
    }

    pub fn signal_threads(self) -> Result<(), Self> {
        let deadline = Instant::now() + self.signal.data.thread_start_timeout;

        let mut parked_threads = self.signal.data.parked_threads.lock();
        if self
            .signal
            .data
            .ready_sync
            .wait_while_until(
                &mut parked_threads,
                |threads| threads.len() < self.signal.data.number_of_threads,
                deadline,
            )
            .timed_out()
        {
            drop(parked_threads);
            return Err(self);
        }

        assert_eq!(self.signal.data.number_of_threads, parked_threads.len());

        for thread in parked_threads.drain(..) {
            thread.unpark();
        }

        Ok(())
    }
}

#[test]
fn thread_start_tolerance() {
    // We need at least 2 threads to test this.
    let number_of_threads = std::thread::available_parallelism().unwrap().get().min(2);

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
    let earliest_start = start_times.iter().min().unwrap();
    let latest_start = start_times.iter().max().unwrap();
    let delta = latest_start
        .checked_duration_since(*earliest_start)
        .unwrap();
    println!("Sync start instant max delta: {:.03}ns", delta.as_nanos());
    assert!(delta < Duration::from_micros(10));
}
