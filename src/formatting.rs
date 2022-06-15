use std::{
    collections::BTreeMap,
    fmt::Display,
    io::{stdout, Write},
};

use tabled::{Header, Table, Tabled};

use crate::{Label, MetricStats, MetricSummary};

pub fn print_table_summaries<Metric: Display>(
    report: &BTreeMap<Metric, MetricSummary>,
) -> std::io::Result<()> {
    write_table_summaries(stdout(), report)
}

pub fn write_table_summaries<Writer: Write, Metric: Display>(
    mut writer: Writer,
    report: &BTreeMap<Metric, MetricSummary>,
) -> std::io::Result<()> {
    for (metric, summary) in report {
        writer.write_all(
            Table::new(&summary.labels)
                .with(Header(metric.to_string()))
                .to_string()
                .as_bytes(),
        )?;
    }
    Ok(())
}

impl Tabled for Label {
    const LENGTH: usize = 1;

    fn fields(&self) -> Vec<String> {
        vec![self.to_string()]
    }

    fn headers() -> Vec<String> {
        vec![String::from("Label")]
    }
}

impl Tabled for MetricStats {
    const LENGTH: usize = 5;

    fn fields(&self) -> Vec<String> {
        let total_metrics = self.plottable_stats.len() + self.outliers.len();
        let outlier_percent = if total_metrics > 0 {
            format_float(self.outliers.len() as f64 / total_metrics as f64, "%")
        } else {
            String::default()
        };
        vec![
            format_nanoseconds(self.average),
            format_nanoseconds(self.min as f64),
            format_nanoseconds(self.max as f64),
            format_nanoseconds(self.stddev),
            outlier_percent,
        ]
    }

    fn headers() -> Vec<String> {
        vec![
            String::from("avg"),
            String::from("min"),
            String::from("max"),
            String::from("stddev"),
            String::from("out%"),
        ]
    }
}

fn format_nanoseconds(nanoseconds: f64) -> String {
    if nanoseconds <= f64::EPSILON {
        String::from("0s")
    } else if nanoseconds < 1_000. {
        format_float(nanoseconds, "ns")
    } else if nanoseconds < 1_000_000. {
        format_float(nanoseconds / 1_000., "us")
    } else if nanoseconds < 1_000_000_000. {
        format_float(nanoseconds / 1_000_000., "ms")
    } else if nanoseconds < 1_000_000_000_000. {
        format_float(nanoseconds / 1_000_000_000., "s")
    } else {
        // this hopefully is unreachable...
        format_float(nanoseconds / 1_000_000_000. / 60., "m")
    }
}

fn format_float(value: f64, suffix: &str) -> String {
    if value < 10. {
        format!("{:.3}{}", value, suffix)
    } else if value < 100. {
        format!("{:.2}{}", value, suffix)
    } else {
        format!("{:.1}{}", value, suffix)
    }
}
