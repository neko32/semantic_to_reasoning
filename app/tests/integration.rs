//! End-to-end pipeline with the real external tools:
//! `s2r seed` -> `dbt build` (`DuckDB`) -> `mf query` (`MetricFlow`) -> reasoner.
//!
//! Needs the analytics virtualenv (`cd analytics && uv sync`), so it is ignored
//! by default. Run with: `cargo test --test integration -- --ignored`.

use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Parser;
use s2r::api::cli::{Cli, run};

fn analytics_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../analytics")
}

fn venv_bin(name: &str) -> PathBuf {
    let dir = analytics_dir();
    if cfg!(windows) {
        dir.join(".venv/Scripts").join(format!("{name}.exe"))
    } else {
        dir.join(".venv/bin").join(name)
    }
}

fn s2r(data: &Path, args: &[&str]) -> String {
    let data_dir = data.to_string_lossy().to_string();
    let project = analytics_dir().to_string_lossy().to_string();
    let mut command_line = vec!["s2r", "--data-dir", &data_dir, "--analytics-dir", &project];
    command_line.extend_from_slice(args);
    run(&Cli::try_parse_from(command_line).expect("valid args")).expect("command succeeds")
}

#[test]
#[ignore = "needs the analytics virtualenv with dbt-duckdb and MetricFlow"]
fn full_pipeline_with_dbt_and_metricflow() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path();

    let out = s2r(data, &["seed"]);
    assert!(out.contains("1000 submissions"));

    let status = Command::new(venv_bin("dbt"))
        .args(["build", "--quiet"])
        .current_dir(analytics_dir())
        .env("DBT_PROFILES_DIR", analytics_dir())
        .env("S2R_BRONZE_DIR", data.join("bronze"))
        .env("S2R_WAREHOUSE", data.join("warehouse.duckdb"))
        .status()
        .expect("dbt runs");
    assert!(status.success(), "dbt build failed");

    s2r(data, &["extract", "--extractor", "mock"]);
    let report = s2r(data, &["ask", "all", "--trust", "llm"]);

    // Q1: MetricFlow and SPARQL agree on every row.
    assert!(
        report.contains("Both paths agree. Business truth"),
        "{report}"
    );
    assert!(!report.contains("│ NO"), "{report}");
    // Q2: Gold's false alarms are exactly the chains it cannot follow.
    assert!(
        report.contains("GRANDPARENT 8, TRUST_SELF 10, TRUST_SPOUSE 6"),
        "{report}"
    );
    // Q3: the graph finds brand names, hierarchy and LLM-mapped drugs.
    assert!(
        report.contains("ND_BRAND 6, ND_HIERARCHY 6, ND_LLM 7"),
        "{report}"
    );

    let demo = s2r(data, &["demo"]);
    assert!(demo.contains("depends on an LLM candidate"), "{demo}");
}
