use std::{
    collections::{BTreeMap, HashMap},
    fmt::{Debug, Display},
    hash::Hash,
    ops::Deref,
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant},
};

#[cfg(feature = "formatting")]
mod formatting;

#[cfg(feature = "formatting")]
pub use formatting::*;

#[derive(Clone)]
pub struct Timings<Metric> {
    sender: flume::Sender<(Label, Metric, Duration)>,
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
