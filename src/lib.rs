#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![warn(
    clippy::cargo,
    missing_docs,
    clippy::pedantic,
    future_incompatible,
    rust_2018_idioms
)]
#![allow(
    clippy::option_if_let_else,
    clippy::module_name_repetitions,
    clippy::missing_errors_doc
)]

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fmt::{Debug, Display},
    hash::Hash,
    marker::PhantomData,
    ops::Deref,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[cfg(feature = "formatting")]
mod formatting;

#[cfg(feature = "formatting")]
pub use formatting::*;
use parking_lot::{Condvar, Mutex};

/// A collection of [`MetricSummary`] ordered by `Metric`.
pub type MetricSummaries<Metric> = BTreeMap<Metric, MetricSummary>;

/// Measures the time `Metric`s take to execute.
#[derive(Debug)]
pub struct Timings<Metric> {
    sender: Option<flume::Sender<(Label, Metric, Duration)>>,
    stats_thread: Arc<Mutex<Option<JoinHandle<MetricSummaries<Metric>>>>>,
}

impl<Metric> Clone for Timings<Metric> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            stats_thread: self.stats_thread.clone(),
        }
    }
}

impl<Metric> Default for Timings<Metric>
where
    Metric: Send + Ord + Hash + Eq + Display + Debug + Clone + 'static,
{
    fn default() -> Self {
        let (sender, receiver) = flume::unbounded();
        let stats_handle = std::thread::Builder::new()
            .name(String::from("measurements"))
            .spawn(move || stats_thread(&receiver))
            .unwrap();
        Self {
            sender: Some(sender),
            stats_thread: Arc::new(Mutex::new(Some(stats_handle))),
        }
    }
}

impl<Metric> Timings<Metric>
where
    Metric: Send + Ord + Hash + Eq + Display + Debug + Clone + 'static,
{
    /// Begin a [`Timing`] of `metric` for `label`. Labels are used group
    /// multiple invocations of `metric`. For example, if a benchmark is
    /// measuring 3 different crates, `label` could be the crate name and
    /// `metric` would be the operation being measured across the three crates.
    pub fn begin(&self, label: impl Into<Label>, metric: Metric) -> Timing<'_, Metric> {
        Timing {
            target: self.sender.as_ref().expect("already finished"),
            label: label.into(),
            metric,
            start: Instant::now(),
        }
    }

    /// Waits for all measurements to conclude and returns an organized summary.
    ///
    /// # Panics
    ///
    /// This function must only be called once. If more than one clone calls
    /// this function, a panic will occur.
    ///
    /// This will also panic if the thread that is collecting the summaries
    /// panics.
    #[must_use]
    pub fn wait_for_stats(mut self) -> MetricSummaries<Metric> {
        self.sender = None;
        let mut stats_thread = self.stats_thread.lock();
        let join_handle = stats_thread.take().expect("wait_for_stats already called");
        join_handle
            .join()
            .expect("stats thread could not be joined")
    }
}

/// A [`Timings`] instance that uses the same label for all metrics measured.
#[derive(Debug)]
pub struct LabeledTimings<Metric> {
    label: Label,
    timings: Timings<Metric>,
}

impl<Metric> LabeledTimings<Metric> {
    /// Begin a [`Timing`] of `metric`.
    pub fn begin(&self, metric: Metric) -> Timing<'_, Metric> {
        Timing {
            target: self.timings.sender.as_ref().expect("already finished"),
            label: self.label.clone(),
            metric,
            start: Instant::now(),
        }
    }
}

impl<Metric> Clone for LabeledTimings<Metric> {
    fn clone(&self) -> Self {
        Self {
            label: self.label.clone(),
            timings: self.timings.clone(),
        }
    }
}

/// An ongoing timing measurement of a `Metric`.
pub struct Timing<'a, Metric> {
    target: &'a flume::Sender<(Label, Metric, Duration)>,
    label: Label,
    metric: Metric,
    start: Instant,
}

impl<'a, Metric> Timing<'a, Metric> {
    /// Completes the measurement of this metric.
    ///
    /// # Panics
    ///
    /// Panics if time goes backwards or if there is an error sending the
    /// measurement to the collection thread.
    pub fn finish(self) {
        let duration = Instant::now()
            .checked_duration_since(self.start)
            .expect("time went backwards. Restart benchmarks.");
        self.target
            .send((self.label, self.metric, duration))
            .unwrap();
    }
}

#[allow(clippy::cast_precision_loss)]
fn stats_thread<Metric: Ord + Eq + Hash + Display + Debug + Clone>(
    metric_receiver: &flume::Receiver<(Label, Metric, Duration)>,
) -> MetricSummaries<Metric> {
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
                        plottable_stats,
                        outliers,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        for (label, metrics) in label_stats {
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

/// A cheap-to-clone string type. Internally this can be an `&'static str` or a
/// `String` stored within an [`Arc`].
#[derive(Clone)]
pub enum Label {
    /// A static reference to a string slice.
    Static(&'static str),
    /// A reference-counted String value.
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

#[allow(clippy::cast_precision_loss)]
#[doc(hidden)]
#[must_use]
pub fn stddev<
    'a,
    Data: IntoIterator<Item = &'a u64, IntoIter = Iter>,
    Iter: Iterator<Item = &'a u64> + ExactSizeIterator,
>(
    data: Data,
    average: f64,
) -> f64 {
    let data = data.into_iter();
    let data_points = data.len();
    if data_points == 0 {
        0.
    } else {
        let variance = data
            .map(|value| {
                let diff = average - (*value as f64);

                diff * diff
            })
            .sum::<f64>()
            / data_points as f64;

        variance.sqrt()
    }
}

/// Statistics gathered for a metric. All timings are in nanoseconds.
#[derive(Debug)]
pub struct MetricStats {
    /// The average nanoseconds elapsed for each execution.
    pub average: f64,
    /// The minimum nanoseconds elapsed for each execution.
    pub min: u64,
    /// The maximum nanoseconds elapsed for each execution.
    pub max: u64,
    /// The [standard deviation][stddev] nanoseconds elapsed for each execution.
    ///
    /// [stddev]: https://en.wikipedia.org/wiki/Standard_deviation
    pub stddev: f64,
    /// The list of all measurements taken, in nanoseconds.
    pub plottable_stats: Vec<u64>,
    /// The list of measurements that are considered outliers. The heuristic
    /// that identifies outliers currently looks to see if the difference
    /// between a measurement and the average measurement is more than 3x the
    /// [standard deviation](Self::stddev), but the heuristic may change.
    pub outliers: Vec<f64>,
}

/// A summary of statistics gathered for a single metric across one or more
/// [`Label`]s.
#[derive(Debug)]
pub struct MetricSummary {
    /// The number of invocations each label observed.
    pub invocations: usize,
    /// The [`MetricStats`] for each label.
    pub labels: BTreeMap<Label, MetricStats>,
}

/// A benchmark that helps execute benchmarks with multiple implementations and
/// configuration options.
#[must_use]
pub struct Benchmark<Metric, Config, Error> {
    threads: Vec<usize>,
    configs: Vec<Config>,
    thread_start_timeout: Duration,
    functions: Vec<Arc<dyn AnyBenchmarkImplementation<Metric, Config, Error>>>,
}

#[allow(clippy::mismatching_type_param_order)] // https://github.com/rust-lang/rust-clippy/issues/9367
impl<Metric, Error> Default for Benchmark<Metric, (), Error> {
    fn default() -> Self {
        Self {
            threads: Vec::new(),
            configs: vec![()],
            thread_start_timeout: Duration::from_secs(15),
            functions: Vec::new(),
        }
    }
}

impl<Metric, Config, Error> Benchmark<Metric, Config, Error>
where
    Error: Send + Sync + 'static,
    Config: Send + Clone + 'static,
    Metric: Send + Sync + 'static,
{
    /// Returns a new instance using `config` as the configuration parameter for
    /// each [`BenchmarkImplementation`].
    pub fn for_config(config: Config) -> Self {
        Self {
            threads: Vec::new(),
            configs: vec![config],
            thread_start_timeout: Duration::from_secs(15),
            functions: Vec::new(),
        }
    }

    /// Returns a new instance that will invoke each [`BenchmarkImplementation`]
    /// once for each `config`.
    pub fn for_each_config(configs: Vec<Config>) -> Self {
        Self {
            threads: Vec::new(),
            configs,
            thread_start_timeout: Duration::from_secs(15),
            functions: Vec::new(),
        }
    }

    /// Updates the timeout used to detect thread start failures.
    pub fn with_thread_start_timeout(mut self, thread_start_timeout: Duration) -> Self {
        self.thread_start_timeout = thread_start_timeout;
        self
    }

    /// Executes the benchmark using `number_of_threads`.
    pub fn with_number_of_threads(mut self, number_of_threads: usize) -> Self {
        self.threads = vec![number_of_threads];
        self
    }

    /// Executes the benchmark once for each value in `threads`. For example, if
    /// `[1, 2, 3]` were passed in for `threads`, the benchmark will execute
    /// once with 1 thread, again with 2 threads, and finally one last time with
    /// 3 threads.
    pub fn with_each_number_of_threads<ThreadCounts: IntoIterator<Item = usize>>(
        mut self,
        threads: ThreadCounts,
    ) -> Self {
        self.threads = threads.into_iter().collect();
        self
    }

    /// Registers a [`BenchmarkImplementation`] to run.
    pub fn with<Implementation: BenchmarkImplementation<Metric, Config, Error>>(mut self) -> Self {
        self.functions
            .push(Arc::new(BenchmarkImpl::<Implementation>::default()));
        self
    }

    /// Executes the benchmark using all [`BenchmarkImplementation`]s,
    /// configurations, and thread counts, using `timings` to gather all
    /// measurements.
    pub fn run(self, timings: &Timings<Metric>) -> Result<(), Error> {
        let threads = if self.threads.is_empty() {
            vec![1]
        } else {
            self.threads
        };

        for thread_count in threads {
            for config in &self.configs {
                for function in &self.functions {
                    let timings = LabeledTimings {
                        label: function.label(thread_count, config),
                        timings: timings.clone(),
                    };
                    println!("Running {} on {thread_count} threads", timings.label);
                    function.reset(false)?;
                    let starter = ThreadSync::new(self.thread_start_timeout);
                    let thread_handles =
                        function.measure(thread_count, config, starter.new_signal(), &timings)?;

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

/// An implementation of logic for a [`Benchmark`].
pub trait BenchmarkImplementation<Metric, Config, Error>: Sized + Send + Sync + 'static {
    /// A configuration type that is shared across all threads within a single
    /// benchmark execution.
    type SharedConfig: Clone + Send + Sync + 'static;

    /// The unique label of this implementation.
    fn label(number_of_threads: usize, config: &Config) -> Label;

    /// Initializes a [`Self::SharedConfig`] based on the `number_of_threads`
    /// and `config` provided.
    fn initialize_shared_config(
        number_of_threads: usize,
        config: &Config,
    ) -> Result<Self::SharedConfig, Error>;

    /// Called between benchmark runs to allow cleaning up resources, if needed.
    /// When `shutting_down` is true, no more benchmarks will be executed.
    fn reset(shutting_down: bool) -> Result<(), Error>;

    /// Initializes an instance of this benchmark implementation using the
    /// shared configuration.
    fn initialize(number_of_threads: usize, config: Self::SharedConfig) -> Result<Self, Error>;

    /// Perform the measurement. This function will be invoked by each of the
    /// threads that are spawned for the benchmark run.
    fn measure(&mut self, measurements: &LabeledTimings<Metric>) -> Result<(), Error>;
}

trait AnyBenchmarkImplementation<Metric, Config, Error>: Sync + Send + 'static {
    fn label(&self, number_of_threads: usize, config: &Config) -> Label;
    fn reset(&self, shutting_down: bool) -> Result<(), Error>;
    fn measure(
        &self,
        number_of_threads: usize,
        data: &Config,
        thread_sync: ThreadSignal,
        measurements: &LabeledTimings<Metric>,
    ) -> Result<Vec<JoinHandle<Result<(), Error>>>, Error>;
}

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
    fn label(&self, number_of_threads: usize, config: &Config) -> Label {
        T::label(number_of_threads, config)
    }

    fn reset(&self, shutting_down: bool) -> Result<(), Error> {
        T::reset(shutting_down)
    }

    fn measure(
        &self,
        number_of_threads: usize,
        config: &Config,
        thread_sync: ThreadSignal,
        measurements: &LabeledTimings<Metric>,
    ) -> Result<Vec<JoinHandle<Result<(), Error>>>, Error> {
        let mut thread_handles = Vec::with_capacity(number_of_threads);
        let shared_config = T::initialize_shared_config(number_of_threads, config)?;
        for _ in 0..number_of_threads {
            let config = shared_config.clone();
            let measurements = measurements.clone();
            let mut thread_sync = thread_sync.clone();
            thread_handles.push(std::thread::spawn(move || {
                let mut data = T::initialize(number_of_threads, config)?;
                thread_sync.wait();
                T::measure(&mut data, &measurements)
            }));
        }
        Ok(thread_handles)
    }
}

/// Synchronizes multiple threads to ensure all waiting threads wake up as close
/// to simultaneously as possible.
#[must_use]
#[derive(Debug)]
pub struct ThreadSync {
    signal: ThreadSignal,
}

impl ThreadSync {
    /// Returns a new instance that will synchronize the start of
    /// `number_of_threads` threads.
    pub fn new(thread_start_timeout: Duration) -> Self {
        let signal = ThreadSignal {
            data: Arc::new(ThreadSyncData {
                countdown: AtomicUsize::default(),
                thread_start_timeout,
                parked_threads: Mutex::default(),
                parked_sync: Condvar::new(),
                start_sync: Condvar::new(),
                epoch: Instant::now(),
                spinlock_target: AtomicU64::default(),
            }),
            started: false,
        };
        Self { signal }
    }

    /// Returns a new signal that will be started when [`Self::signal_threads`]
    /// is called.
    pub fn new_signal(&self) -> ThreadSignal {
        self.signal.clone()
    }

    /// Spawns `number_of_threads` threads, calling `thread_main` from each
    /// thread from all threads as simultaneously as possible.
    ///
    /// If available, this function will use CPU core affinity to pin each
    /// spawned thread to a separate core.
    ///
    /// # Panics
    ///
    /// Panics if `thread-start_timeout` elapses before all threads are ready.
    pub fn synchronize_spawn<F: Fn() + Clone + Send + 'static>(
        number_of_threads: usize,
        thread_start_timeout: Duration,
        thread_main: F,
    ) {
        Self::synchronize_spawn_with(number_of_threads, thread_start_timeout, &(), move |_| {
            thread_main();
        });
    }

    /// Spawns `number_of_threads` threads, calling `thread_main` from each
    /// thread from all threads as simultaneously as possible. This function
    /// differs from [`Self::synchronize_spawn`] by cloning `context` and
    /// passing it into `thread_main`.
    ///
    /// If available, this function will use CPU core affinity to pin each
    /// spawned thread to a separate core.
    ///
    /// # Panics
    ///
    /// Panics if `thread-start_timeout` elapses before all threads are ready.
    pub fn synchronize_spawn_with<T: Clone + Send + 'static, F: Fn(T) + Clone + Send + 'static>(
        number_of_threads: usize,
        thread_start_timeout: Duration,
        context: &T,
        thread_main: F,
    ) {
        let starter = Self::new(thread_start_timeout);
        let mut core_ids = core_affinity::get_core_ids()
            .map(Vec::into_iter)
            .into_iter()
            .flatten()
            .collect::<VecDeque<_>>();
        for _ in 0..number_of_threads {
            let core_id_to_assign = core_ids.pop_front();
            if let Some(core_id) = core_id_to_assign {
                core_ids.push_back(core_id);
            }
            let thread_main = thread_main.clone();
            let mut sync = starter.new_signal();
            let context = context.clone();
            std::thread::spawn(move || {
                if let Some(core_id) = core_id_to_assign {
                    core_affinity::set_for_current(core_id);
                }
                sync.wait();
                thread_main(context);
            });
        }

        starter.signal_threads().unwrap();
    }

    /// Signals all waiting threads to start simultaneously.
    ///
    /// This function will wait for all threads to reach the spin-lock phase of
    /// synchronization, but does not wait for the spinlock to expire before
    /// returning. This means that this function may return before the threads
    /// waiting on [`ThreadSignal::wait`] return.
    ///
    /// # Errors
    ///
    /// Returns `Err(self)` if a timeout occurs.
    pub fn signal_threads(self) -> Result<(), Self> {
        let deadline = Instant::now() + self.signal.data.thread_start_timeout;

        let mut parked_threads = self.signal.data.parked_threads.lock();

        // Wait for threads to park.
        if self
            .signal
            .data
            .parked_sync
            .wait_while_until(
                &mut parked_threads,
                |threads| threads.parked < threads.expected,
                deadline,
            )
            .timed_out()
        {
            drop(parked_threads);
            return Err(self);
        }

        // Set the countdown to the number of threads parked.
        self.signal
            .data
            .countdown
            .store(parked_threads.parked, Ordering::SeqCst);

        // Signal the thread start.
        self.signal.data.start_sync.notify_all();

        Ok(())
    }
}

/// A signal that attempts to wake up at the same time as all other signals.
#[derive(Debug)]
#[must_use]
pub struct ThreadSignal {
    data: Arc<ThreadSyncData>,
    started: bool,
}

impl Clone for ThreadSignal {
    fn clone(&self) -> Self {
        let mut state = self.data.parked_threads.lock();
        state.expected += 1;
        Self {
            data: self.data.clone(),
            started: false,
        }
    }
}

impl Drop for ThreadSignal {
    fn drop(&mut self) {
        if !self.started {
            let mut state = self.data.parked_threads.lock();
            state.expected -= 1;
        }
    }
}

#[derive(Debug)]
struct ThreadSyncData {
    parked_threads: Mutex<ParkState>,
    parked_sync: Condvar,
    start_sync: Condvar,
    countdown: AtomicUsize,
    spinlock_target: AtomicU64,
    thread_start_timeout: Duration,
    epoch: Instant,
}

#[derive(Debug, Default)]
struct ParkState {
    expected: usize,
    parked: usize,
}

impl ThreadSignal {
    /// Waits for [`ThreadSync::signal_threads()`], and after waking up uses a
    /// spin lock to synchronize all signaled threads. After all threads have
    /// woken up, this thread will return.
    ///
    /// # Panics
    ///
    /// Panics if called more than once.
    ///
    /// This function does not consume self, as invoking Drop will execute
    /// additional code that affects atomic variables (`Arc`). This can impact
    /// the effectiveness of the spinlock. By deferring the drop to the calling
    /// function, cleanup can happen after the synchronized code is executed.
    pub fn wait(&mut self) {
        let mut parked_threads = self.data.parked_threads.lock();
        parked_threads.parked += 1;
        self.data.parked_sync.notify_one();

        // Mark this instance as having started, which prevents Drop from
        // cleaning up the countdown.
        self.started = true;

        // wait for the start signal
        self.data.start_sync.wait(&mut parked_threads);

        // Allow all other threads to wake up as well.
        drop(parked_threads);

        // Once we're unparked, enter a spinlock waiting for countdown to reach 0.
        let remaining_threads = self.data.countdown.fetch_sub(1, Ordering::SeqCst) - 1;
        let spinlock_target = if remaining_threads == 0 {
            // This thread is the last thread to wake up. Set the spinlock
            // target. 100 microseconds is an eternity since at this stage we've
            // guaranteed all the threads have unparked and are in one of the
            // loops in the other branch of this if statement. Despite this
            // guarantee, it's still possible for the scheduler to pause a
            // thread and not wake it back up until after this spinlock expires.
            let spinlock_target = self.nanos_since_epoch() + 100_000; // 100us
            self.data
                .spinlock_target
                .store(spinlock_target, Ordering::Relaxed);
            spinlock_target
        } else {
            // Wait for the spinlock target to be published.
            loop {
                let spinlock_target = self.data.spinlock_target.load(Ordering::Acquire);
                if spinlock_target > 0 {
                    break spinlock_target;
                }
            }
        };

        // Wait until the target timestamp.
        let target_instant = self.data.epoch + Duration::from_nanos(spinlock_target);
        while Instant::now() <= target_instant {}
    }

    #[inline]
    fn nanos_since_epoch(&self) -> u64 {
        self.data
            .epoch
            .elapsed()
            .as_nanos()
            .try_into()
            .expect("too much time elapsed (585 years)")
    }
}
