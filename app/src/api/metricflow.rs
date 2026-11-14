//! `MetricFlow` adapter: answers Q1 through the dbt semantic layer by running
//! `mf query` as a subprocess. Arguments are fixed; nothing user-typed is
//! interpolated except a validated [`Quarter`].

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

use crate::core::domain::Quarter;
use crate::core::questions::{MetricsLayer, Q1Row};

/// Runs a program and reports success or the captured stderr.
pub trait Runner {
    /// # Errors
    /// Returns an error when the program cannot start or exits non-zero.
    fn run(
        &self,
        program: &Path,
        args: &[String],
        cwd: &Path,
        envs: &[(String, String)],
    ) -> anyhow::Result<String>;
}

/// Real subprocess runner.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(
        &self,
        program: &Path,
        args: &[String],
        cwd: &Path,
        envs: &[(String, String)],
    ) -> anyhow::Result<String> {
        let out = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env(
                "DBT_PROFILES_DIR",
                std::path::absolute(cwd).unwrap_or_else(|_| cwd.to_owned()),
            )
            .envs(envs.iter().map(|(k, v)| (k, v)))
            .output()
            .with_context(|| format!("starting {}", program.display()))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() {
            bail!(
                "{} failed ({}):\n{}{}",
                program.display(),
                out.status,
                stdout,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(stdout)
    }
}

pub const METRICS: [&str; 3] = ["submissions", "placement_rate", "avg_face_amount"];
pub const GROUP_BY: [&str; 2] = ["submission__channel", "submission__product_type"];

/// The `MetricFlow` CLI inside the analytics project.
pub struct MetricFlowCli<R: Runner = ProcessRunner> {
    pub mf_bin: PathBuf,
    pub project_dir: PathBuf,
    /// Extra environment, e.g. `S2R_WAREHOUSE` when the warehouse is not in the default place.
    pub envs: Vec<(String, String)>,
    pub runner: R,
}

impl MetricFlowCli<ProcessRunner> {
    #[must_use]
    pub fn new(mf_bin: PathBuf, project_dir: PathBuf) -> Self {
        Self {
            mf_bin,
            project_dir,
            envs: Vec::new(),
            runner: ProcessRunner,
        }
    }
}

impl<R: Runner> MetricFlowCli<R> {
    /// Arguments for the Q1 query, writing CSV to `csv`.
    #[must_use]
    pub fn q1_args(quarter: Quarter, csv: &Path) -> Vec<String> {
        vec![
            "query".into(),
            "--metrics".into(),
            METRICS.join(","),
            "--group-by".into(),
            GROUP_BY.join(","),
            "--where".into(),
            format!(
                "{{{{ TimeDimension('metric_time', 'day') }}}} >= '{}' and {{{{ TimeDimension('metric_time', 'day') }}}} < '{}'",
                quarter.start_date(),
                quarter.end_date_exclusive()
            ),
            "--order".into(),
            GROUP_BY.join(","),
            "--decimals".into(),
            "10".into(),
            "--quiet".into(),
            "--csv".into(),
            csv.display().to_string(),
        ]
    }
}

fn column(headers: &csv::StringRecord, suffix: &str) -> anyhow::Result<usize> {
    headers
        .iter()
        .position(|h| h.trim().to_lowercase().ends_with(suffix))
        .with_context(|| format!("MetricFlow CSV has no column ending in '{suffix}'"))
}

/// Parses `mf query --csv` output.
///
/// # Errors
/// Returns an error when columns are missing or values are not numbers.
pub fn parse_q1_csv(text: &str) -> anyhow::Result<Vec<Q1Row>> {
    let mut reader = csv::Reader::from_reader(text.as_bytes());
    let headers = reader.headers()?.clone();
    let channel = column(&headers, "channel")?;
    let ptype = column(&headers, "product_type")?;
    let subs = column(&headers, "submissions")?;
    let rate = column(&headers, "placement_rate")?;
    let avg = column(&headers, "avg_face_amount")?;
    let mut out = Vec::new();
    for record in reader.records() {
        let r = record?;
        let get = |i: usize| r.get(i).unwrap_or_default().trim().to_owned();
        let n: f64 = get(subs).parse().context("submissions")?;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        out.push(Q1Row {
            channel: get(channel),
            product_type: get(ptype),
            submissions: n as u64,
            placement_rate: get(rate).parse().context("placement_rate")?,
            avg_face_amount: get(avg).parse().context("avg_face_amount")?,
        });
    }
    out.sort_by(|a, b| (&a.channel, &a.product_type).cmp(&(&b.channel, &b.product_type)));
    Ok(out)
}

impl<R: Runner> MetricsLayer for MetricFlowCli<R> {
    fn q1(&self, quarter: Quarter) -> anyhow::Result<Vec<Q1Row>> {
        let dir = tempfile::tempdir()?;
        let csv = dir.path().join("q1.csv");
        self.runner.run(
            &self.mf_bin,
            &Self::q1_args(quarter, &csv),
            &self.project_dir,
            &self.envs,
        )?;
        let text = std::fs::read_to_string(&csv)
            .with_context(|| format!("MetricFlow wrote no CSV at {}", csv.display()))?;
        parse_q1_csv(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str =
        "submission__channel,submission__product_type,submissions,placement_rate,avg_face_amount
DIRECT,TERM,4,0.5,1250000.0
BANK,UL,2,1.0,800000
";

    #[test]
    fn parses_and_sorts_csv() {
        let rows = parse_q1_csv(SAMPLE).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].channel, "BANK");
        assert_eq!(rows[1].submissions, 4);
        assert!((rows[1].placement_rate - 0.5).abs() < 1e-12);
    }

    #[test]
    fn rejects_bad_csv() {
        assert!(parse_q1_csv("a,b\n1,2\n").is_err());
        let bad = SAMPLE.replace("0.5", "half");
        assert!(parse_q1_csv(&bad).is_err());
    }

    #[test]
    fn query_arguments_are_fixed() {
        let q: Quarter = "2026-Q3".parse().unwrap();
        let args = MetricFlowCli::<ProcessRunner>::q1_args(q, Path::new("out.csv"));
        assert_eq!(args[0], "query");
        assert!(args.contains(&"submissions,placement_rate,avg_face_amount".to_owned()));
        let w = &args[args.iter().position(|a| a == "--where").unwrap() + 1];
        assert_eq!(
            w,
            "{{ TimeDimension('metric_time', 'day') }} >= '2026-07-01' and {{ TimeDimension('metric_time', 'day') }} < '2026-10-01'"
        );
        assert_eq!(args.last().unwrap(), "out.csv");
    }

    struct FakeMf(&'static str);
    impl Runner for FakeMf {
        fn run(
            &self,
            _: &Path,
            args: &[String],
            _: &Path,
            _: &[(String, String)],
        ) -> anyhow::Result<String> {
            if !self.0.is_empty() {
                std::fs::write(args.last().unwrap(), self.0)?;
            }
            Ok(String::new())
        }
    }

    #[test]
    fn q1_via_fake_runner() {
        let mf = MetricFlowCli {
            mf_bin: "mf".into(),
            project_dir: ".".into(),
            envs: vec![],
            runner: FakeMf(SAMPLE),
        };
        assert_eq!(mf.q1("2026-Q3".parse().unwrap()).unwrap().len(), 2);
        let silent = MetricFlowCli {
            mf_bin: "mf".into(),
            project_dir: ".".into(),
            envs: vec![],
            runner: FakeMf(""),
        };
        assert!(silent.q1("2026-Q3".parse().unwrap()).is_err());
    }

    #[test]
    fn process_runner_reports_failures() {
        let dir = tempfile::tempdir().unwrap();
        let err = ProcessRunner
            .run(
                Path::new("definitely-not-a-real-binary-s2r"),
                &[],
                dir.path(),
                &[],
            )
            .unwrap_err();
        assert!(err.to_string().contains("starting"));
        let ok = ProcessRunner
            .run(
                Path::new("cargo"),
                &["--version".into()],
                dir.path(),
                &[("A".into(), "B".into())],
            )
            .unwrap();
        assert!(ok.starts_with("cargo"));
        let failed = ProcessRunner
            .run(
                Path::new("cargo"),
                &["--no-such-flag-s2r".into()],
                dir.path(),
                &[],
            )
            .unwrap_err();
        assert!(failed.to_string().contains("failed"));
        let mf = MetricFlowCli::new("definitely-not-a-real-binary-s2r".into(), dir.path().into());
        assert!(mf.q1("2026-Q3".parse().unwrap()).is_err());
    }
}
