//! Command-line interface.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};
use comfy_table::Table;
use comfy_table::presets::UTF8_FULL_CONDENSED;
use oxigraph::model::{NamedNode, Term, Triple};

use super::lmstudio::{DEFAULT_BASE_URL, DEFAULT_MODEL, LmStudioExtractor};
use super::metricflow::MetricFlowCli;
use super::render;
use super::warehouse::DuckDbWarehouse;
use crate::core::domain::{Confidence, Quarter, SubmissionId, TrustPolicy};
use crate::core::graph::{Graph, submission_iri};
use crate::core::knowledge::{
    Catalog, KnowledgeExtractor, LlmCandidates, MockExtractor, UW, extract,
};
use crate::core::questions::{
    Comparison, GoldStore, MetricsLayer, Q1Row, Q4, SET_QUESTIONS, SetQuestion, compare, graph_q1,
};
use crate::core::reasoner::{Reasoning, explain, reason};
use crate::core::seed::{GroundTruth, ReferenceData, generate, write_bronze};
use crate::core::silver::{SilverData, SilverSource};

#[derive(Debug, Parser)]
#[command(
    name = "s2r",
    version,
    about = "Semantic to Reasoning: Gold-layer business truth vs. triple-store relationship truth"
)]
pub struct Cli {
    /// Data directory (Bronze CSVs, warehouse, LLM candidates).
    #[arg(long, global = true, env = "S2R_DATA_DIR", default_value = "data")]
    pub data_dir: PathBuf,
    /// `DuckDB` warehouse built by dbt. Defaults to <data-dir>/warehouse.duckdb.
    #[arg(long, global = true, env = "S2R_WAREHOUSE")]
    pub warehouse: Option<PathBuf>,
    /// dbt + `MetricFlow` project directory.
    #[arg(
        long,
        global = true,
        env = "S2R_ANALYTICS_DIR",
        default_value = "analytics"
    )]
    pub analytics_dir: PathBuf,
    /// `MetricFlow` executable. Defaults to the analytics project's virtualenv.
    #[arg(long, global = true, env = "S2R_MF_BIN")]
    pub mf_bin: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Trust {
    /// Human-curated dictionary only.
    Curated,
    /// Curated dictionary plus LLM candidates above --min-confidence.
    Llm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Which {
    Q1,
    Q2,
    Q3,
    Q4,
    All,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExtractorKind {
    /// LM Studio (OpenAI-compatible API).
    Lmstudio,
    /// Deterministic offline stand-in.
    Mock,
}

#[derive(Debug, clap::Args, Clone, Copy)]
pub struct TrustArgs {
    /// Which knowledge the reasoner may use.
    #[arg(long, value_enum, default_value_t = Trust::Llm)]
    pub trust: Trust,
    /// Minimum confidence for LLM candidates.
    #[arg(long, default_value_t = 0.8)]
    pub min_confidence: f64,
}

impl TrustArgs {
    /// # Errors
    /// Returns an error for a confidence outside [0, 1].
    pub fn policy(self) -> anyhow::Result<TrustPolicy> {
        Ok(match self.trust {
            Trust::Curated => TrustPolicy::CuratedOnly,
            Trust::Llm => TrustPolicy::CuratedPlusLlm {
                min_confidence: Confidence::new(self.min_confidence)?,
            },
        })
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Generate synthetic Bronze CSVs (and the ground truth of planted scenarios).
    Seed {
        #[arg(long, default_value_t = 42)]
        seed: u64,
    },
    /// Ask the LLM for knowledge the curated dictionary lacks; cache the validated candidates.
    Extract {
        #[arg(long, value_enum, default_value_t = ExtractorKind::Lmstudio)]
        extractor: ExtractorKind,
        #[arg(long, env = "LMSTUDIO_BASE_URL", default_value = DEFAULT_BASE_URL)]
        base_url: String,
        #[arg(long, env = "LMSTUDIO_MODEL", default_value = DEFAULT_MODEL)]
        model: String,
        #[arg(long, default_value_t = 600)]
        timeout_secs: u64,
    },
    /// Ask the demo questions of both the Gold layer and the graph.
    Ask {
        #[arg(value_enum, default_value_t = Which::All)]
        question: Which,
        #[command(flatten)]
        trust: TrustArgs,
        /// Quarter for Q1.
        #[arg(long, default_value = "2026-Q3")]
        quarter: Quarter,
        /// How many sample ids to print per row.
        #[arg(long, default_value_t = 4)]
        show: usize,
    },
    /// Explain every finding on one submission, down to its source facts.
    Explain {
        submission: String,
        #[command(flatten)]
        trust: TrustArgs,
    },
    /// Run Q1-Q4 and explain one case Gold cannot see.
    Demo {
        #[arg(long, default_value = "2026-Q3")]
        quarter: Quarter,
    },
    /// Write the reasoned graph (asserted + inferred) as N-Triples.
    Export {
        #[command(flatten)]
        trust: TrustArgs,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

/// Resolved file locations.
#[derive(Debug, Clone)]
pub struct Paths {
    pub bronze: PathBuf,
    pub warehouse: PathBuf,
    pub candidates: PathBuf,
    pub truth: PathBuf,
    pub analytics: PathBuf,
    pub mf_bin: PathBuf,
}

impl Paths {
    #[must_use]
    pub fn resolve(cli: &Cli) -> Self {
        let venv_mf = if cfg!(windows) {
            cli.analytics_dir.join(".venv/Scripts/mf.exe")
        } else {
            cli.analytics_dir.join(".venv/bin/mf")
        };
        Self {
            bronze: cli.data_dir.join("bronze"),
            warehouse: cli
                .warehouse
                .clone()
                .unwrap_or_else(|| cli.data_dir.join("warehouse.duckdb")),
            candidates: cli.data_dir.join("llm_candidates.json"),
            truth: cli.data_dir.join("bronze/_ground_truth.json"),
            analytics: cli.analytics_dir.clone(),
            mf_bin: cli.mf_bin.clone().unwrap_or(venv_mf),
        }
    }
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_owned())
}

/// Loads LLM candidates when the policy needs them.
///
/// # Errors
/// Returns an error when the policy accepts LLM knowledge but no cache exists.
pub fn candidates_for(policy: TrustPolicy, path: &Path) -> anyhow::Result<Option<LlmCandidates>> {
    match policy {
        TrustPolicy::CuratedOnly => Ok(None),
        TrustPolicy::CuratedPlusLlm { .. } => LlmCandidates::load(path)
            .map(Some)
            .with_context(|| {
                format!(
                    "no LLM candidates at {}: run `s2r extract` (or `s2r extract --extractor mock`), or use --trust curated",
                    path.display()
                )
            }),
    }
}

/// Builds and reasons the graph.
///
/// # Errors
/// Returns an error when building or reasoning fails.
pub fn reasoned_graph(
    silver: &SilverData,
    candidates: Option<&LlmCandidates>,
    policy: TrustPolicy,
) -> anyhow::Result<(Graph, Reasoning)> {
    let catalog = Catalog::bundled()?;
    let mut graph = Graph::build(silver, &catalog, candidates, policy)?;
    let reasoning = reason(&mut graph)?;
    Ok((graph, reasoning))
}

/// Everything `ask` printed, plus the raw comparisons.
pub struct Report {
    pub text: String,
    pub q1_match: Option<bool>,
    pub comparisons: Vec<Comparison>,
}

fn selected(which: Which) -> Vec<SetQuestion> {
    SET_QUESTIONS
        .into_iter()
        .filter(|q| match which {
            Which::All => true,
            Which::Q2 => q.key == "q2",
            Which::Q3 => q.key == "q3",
            Which::Q4 => q.key == "q4",
            Which::Q1 => false,
        })
        .collect()
}

/// Produces the side-by-side report.
///
/// # Errors
/// Returns an error when a query fails.
#[allow(clippy::too_many_arguments)] // a flat signature keeps the call sites readable
pub fn report(
    which: Which,
    quarter: Quarter,
    q1_gold: Option<Vec<Q1Row>>,
    gold: &dyn GoldStore,
    graph: &Graph,
    reasoning: &Reasoning,
    truth: Option<&GroundTruth>,
    show: usize,
) -> anyhow::Result<Report> {
    let mut text = format!("{}\n\n", render::graph_summary(graph, reasoning));
    let mut q1_match = None;
    if let Some(gold_rows) = q1_gold {
        let (t, ok) = render::q1(&quarter.to_string(), &gold_rows, &graph_q1(graph, quarter)?);
        text.push_str(&t);
        text.push('\n');
        q1_match = Some(ok);
    }
    let mut comparisons = Vec::new();
    for q in selected(which) {
        let c = compare(q, gold, graph)?;
        text.push_str(&render::comparison(&c, truth, show));
        text.push('\n');
        comparisons.push(c);
    }
    Ok(Report {
        text,
        q1_match,
        comparisons,
    })
}

/// Explains every finding on a submission.
///
/// # Errors
/// Returns an error for an invalid id or a failed query.
pub fn explain_submission(
    graph: &Graph,
    reasoning: &Reasoning,
    id: &str,
    only: Option<&str>,
) -> anyhow::Result<String> {
    let id = SubmissionId::new(id)?;
    let subject = NamedNode::new(submission_iri(id.as_str()))?;
    let has_finding = NamedNode::new(format!("{UW}hasFinding"))?;
    let mut findings = graph.objects(&subject.clone().into(), &has_finding)?;
    if let Some(only) = only {
        let wanted = format!("<{UW}{only}>");
        findings.retain(|f| f.to_string() == wanted);
    }
    findings.sort_by_key(ToString::to_string);
    let mut out = format!("Findings for {id} ({}):\n", graph.policy);
    if findings.is_empty() {
        out.push_str("    none\n");
    }
    for f in findings {
        let t = Triple::new(subject.clone(), has_finding.clone(), f);
        out.push('\n');
        out.push_str(&render::explanation(
            &explain(graph, reasoning, &t),
            reasoning,
        ));
    }
    Ok(out)
}

/// Picks the most telling submission to explain in the demo: one Gold
/// missed for Q4 whose evidence came through the LLM, else any Gold missed.
#[must_use]
pub fn demo_pick(
    comparisons: &[Comparison],
    graph: &Graph,
    reasoning: &Reasoning,
) -> Option<String> {
    let q4 = comparisons.iter().find(|c| c.question == Q4)?;
    let missed = q4.graph_only();
    let finding = |id: &str| {
        Triple::new(
            NamedNode::new_unchecked(submission_iri(id)),
            NamedNode::new_unchecked(format!("{UW}hasFinding")),
            Term::from(NamedNode::new_unchecked(format!("{UW}{}", Q4.finding))),
        )
    };
    missed
        .iter()
        .find(|id| explain(graph, reasoning, &finding(id)).depends_on_llm())
        .or_else(|| missed.iter().next())
        .cloned()
}

fn extraction_summary(c: &LlmCandidates, path: &Path) -> String {
    let mut t = Table::new();
    t.load_style(UTF8_FULL_CONDENSED);
    t.set_header(vec!["Kind", "Input", "Proposed", "Confidence"]);
    for m in &c.mappings {
        t.add_row(vec![
            "drug text → drug".to_owned(),
            m.text.clone(),
            m.drug.clone().unwrap_or_else(|| "(none)".into()),
            format!("{:.2}", m.confidence.value()),
        ]);
    }
    for i in &c.indications {
        t.add_row(vec![
            "drug → treats".to_owned(),
            i.drug.clone(),
            i.condition.clone().unwrap_or_else(|| "(none)".into()),
            format!("{:.2}", i.confidence.value()),
        ]);
    }
    let mut out = format!(
        "LLM candidates from {} (validated against the allow-lists):\n{t}\n",
        c.model
    );
    for r in &c.rejected {
        let _ = writeln!(out, "    rejected: {r}");
    }
    let _ = writeln!(out, "Saved to {}", path.display());
    out
}

fn metricflow(paths: &Paths) -> MetricFlowCli {
    // Absolute paths: the subprocess runs inside the analytics directory.
    let bin = if paths.mf_bin.components().count() > 1 {
        absolute(&paths.mf_bin)
    } else {
        paths.mf_bin.clone() // a bare name is looked up on PATH
    };
    let mut mf = MetricFlowCli::new(bin, absolute(&paths.analytics));
    mf.envs.push((
        "S2R_WAREHOUSE".into(),
        absolute(&paths.warehouse).display().to_string(),
    ));
    mf
}

/// Executes a command and returns what to print.
///
/// # Errors
/// Returns an error when the command fails.
#[allow(clippy::too_many_lines)] // one arm per subcommand
pub fn run(cli: &Cli) -> anyhow::Result<String> {
    let paths = Paths::resolve(cli);
    match &cli.command {
        Command::Seed { seed } => {
            let generated = generate(*seed);
            let reference = ReferenceData {
                drug_condition: Catalog::bundled()?.reference_rows(),
            };
            write_bronze(&generated, &reference, &paths.bronze, *seed)?;
            let s = &generated.silver;
            Ok(format!(
                "Wrote Bronze CSVs to {} (seed {seed}): {} parties, {} submissions, {} prescriptions, {} relationships.\nNext: cd {} && dbt build",
                paths.bronze.display(),
                s.parties.len(),
                s.submissions.len(),
                s.rx_fills.len(),
                s.relationships.len(),
                paths.analytics.display(),
            ))
        }
        Command::Extract {
            extractor,
            base_url,
            model,
            timeout_secs,
        } => {
            let silver = DuckDbWarehouse::open(&paths.warehouse)?.load_silver()?;
            let texts: Vec<String> = silver
                .rx_fills
                .iter()
                .map(|r| r.drug_text.clone())
                .collect();
            let catalog = Catalog::bundled()?;
            let ex: Box<dyn KnowledgeExtractor> = match extractor {
                ExtractorKind::Mock => Box::new(MockExtractor),
                ExtractorKind::Lmstudio => Box::new(LmStudioExtractor::new(
                    base_url,
                    model,
                    Duration::from_secs(*timeout_secs),
                )),
            };
            let candidates = extract(ex.as_ref(), &catalog, &texts)?;
            candidates.save(&paths.candidates)?;
            Ok(extraction_summary(&candidates, &paths.candidates))
        }
        Command::Ask {
            question,
            trust,
            quarter,
            show,
        } => {
            let policy = trust.policy()?;
            // MetricFlow first: it needs the DuckDB file lock before we open it read-only.
            let q1_gold = if matches!(question, Which::Q1 | Which::All) {
                Some(metricflow(&paths).q1(*quarter)?)
            } else {
                None
            };
            let wh = DuckDbWarehouse::open(&paths.warehouse)?;
            let silver = wh.load_silver()?;
            let candidates = candidates_for(policy, &paths.candidates)?;
            let (graph, reasoning) = reasoned_graph(&silver, candidates.as_ref(), policy)?;
            let truth = GroundTruth::load(&paths.truth).ok();
            Ok(report(
                *question,
                *quarter,
                q1_gold,
                &wh,
                &graph,
                &reasoning,
                truth.as_ref(),
                *show,
            )?
            .text)
        }
        Command::Explain { submission, trust } => {
            let policy = trust.policy()?;
            let silver = DuckDbWarehouse::open(&paths.warehouse)?.load_silver()?;
            let candidates = candidates_for(policy, &paths.candidates)?;
            let (graph, reasoning) = reasoned_graph(&silver, candidates.as_ref(), policy)?;
            explain_submission(&graph, &reasoning, submission, None)
        }
        Command::Demo { quarter } => {
            let q1_gold = metricflow(&paths).q1(*quarter)?;
            let wh = DuckDbWarehouse::open(&paths.warehouse)?;
            let silver = wh.load_silver()?;
            let llm = TrustArgs {
                trust: Trust::Llm,
                min_confidence: 0.8,
            }
            .policy()?;
            let (policy, candidates) = match candidates_for(llm, &paths.candidates) {
                Ok(c) => (llm, c),
                Err(_) => (TrustPolicy::CuratedOnly, None),
            };
            let (graph, reasoning) = reasoned_graph(&silver, candidates.as_ref(), policy)?;
            let truth = GroundTruth::load(&paths.truth).ok();
            let r = report(
                Which::All,
                *quarter,
                Some(q1_gold),
                &wh,
                &graph,
                &reasoning,
                truth.as_ref(),
                4,
            )?;
            let mut out = r.text;
            if let Some(id) = demo_pick(&r.comparisons, &graph, &reasoning) {
                let _ = writeln!(
                    out,
                    "Why does the graph flag {id}? Gold has every one of these facts, but not the meaning that connects them.\n"
                );
                out.push_str(&explain_submission(
                    &graph,
                    &reasoning,
                    &id,
                    Some(Q4.finding),
                )?);
            }
            Ok(out)
        }
        Command::Export { trust, out } => {
            let policy = trust.policy()?;
            let silver = DuckDbWarehouse::open(&paths.warehouse)?.load_silver()?;
            let candidates = candidates_for(policy, &paths.candidates)?;
            let (graph, reasoning) = reasoned_graph(&silver, candidates.as_ref(), policy)?;
            let path = out.clone().unwrap_or_else(|| cli.data_dir.join("graph.nt"));
            std::fs::write(&path, graph.to_ntriples()?)?;
            Ok(format!(
                "Wrote {} triples ({} inferred) to {}",
                graph.len()?,
                reasoning.inferred(),
                path.display()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::warehouse::tests::in_memory_warehouse;
    use crate::core::questions::{Q2, Q3};

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("s2r").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn argument_parsing_and_paths() {
        let cli = parse(&["--data-dir", "d", "ask", "q2", "--trust", "curated"]);
        let p = Paths::resolve(&cli);
        assert_eq!(p.warehouse, Path::new("d").join("warehouse.duckdb"));
        assert_eq!(p.candidates, Path::new("d").join("llm_candidates.json"));
        assert!(p.mf_bin.to_string_lossy().contains(".venv"));
        let Command::Ask {
            question, trust, ..
        } = cli.command
        else {
            panic!()
        };
        assert_eq!(question, Which::Q2);
        assert_eq!(trust.policy().unwrap(), TrustPolicy::CuratedOnly);
        let cli = parse(&["--warehouse", "w.db", "--mf-bin", "mf", "demo"]);
        let p = Paths::resolve(&cli);
        assert_eq!(p.warehouse, Path::new("w.db"));
        assert_eq!(p.mf_bin, Path::new("mf"));
        assert!(
            TrustArgs {
                trust: Trust::Llm,
                min_confidence: 2.0
            }
            .policy()
            .is_err()
        );
        assert!(Cli::try_parse_from(["s2r", "ask", "--quarter", "2026-Q9"]).is_err());
    }

    #[test]
    fn candidates_are_required_only_for_llm_trust() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none.json");
        assert!(
            candidates_for(TrustPolicy::CuratedOnly, &missing)
                .unwrap()
                .is_none()
        );
        let llm = TrustArgs {
            trust: Trust::Llm,
            min_confidence: 0.8,
        }
        .policy()
        .unwrap();
        let err = candidates_for(llm, &missing).unwrap_err();
        assert!(format!("{err:#}").contains("s2r extract"));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn end_to_end_on_an_in_memory_warehouse() {
        let g = generate(42);
        let wh = in_memory_warehouse(&g.silver);
        let silver = wh.load_silver().unwrap();
        let catalog = Catalog::bundled().unwrap();
        let texts: Vec<String> = silver
            .rx_fills
            .iter()
            .map(|r| r.drug_text.clone())
            .collect();
        let cands = extract(&MockExtractor, &catalog, &texts).unwrap();
        let policy = TrustArgs {
            trust: Trust::Llm,
            min_confidence: 0.8,
        }
        .policy()
        .unwrap();
        let (graph, reasoning) = reasoned_graph(&silver, Some(&cands), policy).unwrap();

        let quarter: Quarter = "2026-Q3".parse().unwrap();
        let q1_gold = graph_q1(&graph, quarter).unwrap();
        let r = report(
            Which::All,
            quarter,
            Some(q1_gold),
            &wh,
            &graph,
            &reasoning,
            Some(&g.truth),
            3,
        )
        .unwrap();
        assert_eq!(r.q1_match, Some(true));
        assert_eq!(r.comparisons.len(), 3);

        let by = |q: SetQuestion| r.comparisons.iter().find(|c| c.question == q).unwrap();
        let labels = |ids: &std::collections::BTreeSet<String>, idx: usize| {
            ids.iter()
                .map(|id| g.truth.label(id).unwrap()[idx].clone())
                .collect::<std::collections::BTreeSet<_>>()
        };

        // Q2: Gold's only disagreement is false alarms on chains it cannot follow.
        let q2 = by(Q2);
        assert_eq!(q2.graph, g.truth.q2_insurable_interest_not_established);
        assert!(q2.graph_only().is_empty());
        assert_eq!(
            labels(&q2.gold_only(), 0),
            ["GRANDPARENT", "TRUST_SELF", "TRUST_SPOUSE"]
                .map(String::from)
                .into()
        );

        // Q3: Gold finds a subset; it misses brands, hierarchy and LLM knowledge.
        let q3 = by(Q3);
        assert!(q3.gold_only().is_empty());
        assert!(!q3.both().is_empty());
        assert_eq!(
            labels(&q3.graph_only(), 1),
            ["ND_BRAND", "ND_HIERARCHY", "ND_LLM"]
                .map(String::from)
                .into()
        );

        // Q4: Gold catches lab and disclosure cases, misses implied conditions.
        let q4 = by(Q4);
        assert!(q4.gold_only().is_empty());
        assert!(!q4.graph_only().is_empty());
        for label in labels(&q4.graph_only(), 1) {
            assert!(
                [
                    "ND_GENERIC",
                    "ND_BRAND",
                    "ND_HIERARCHY",
                    "ND_LLM",
                    "CARDIO_NO_QUESTION"
                ]
                .contains(&label.as_str()),
                "{label}"
            );
        }

        assert!(
            r.text
                .contains("Q2  Which applications lack an insurable interest")
        );
        assert!(r.text.contains("Both paths agree. Business truth"));

        let pick = demo_pick(&r.comparisons, &graph, &reasoning).unwrap();
        assert_eq!(g.truth.label(&pick).unwrap()[1], "ND_LLM");
        let text = explain_submission(&graph, &reasoning, &pick, None).unwrap();
        assert!(text.contains("UW-REILL-PREFERRED"));
        assert!(text.contains("UW-NONDISCLOSURE"));
        assert!(text.contains("LLM candidate"));
        let only = explain_submission(&graph, &reasoning, &pick, Some(Q4.finding)).unwrap();
        assert!(!only.contains("<= UW-NONDISCLOSURE"));
        assert!(explain_submission(&graph, &reasoning, "bad", None).is_err());
        assert!(
            explain_submission(&graph, &reasoning, "S999999", None)
                .unwrap()
                .contains("none")
        );

        let only_q3 = report(Which::Q3, quarter, None, &wh, &graph, &reasoning, None, 3).unwrap();
        assert_eq!(only_q3.comparisons.len(), 1);
        assert_eq!(only_q3.q1_match, None);
        assert!(selected(Which::Q1).is_empty());
        assert_eq!(selected(Which::Q2)[0], Q2);
        assert_eq!(selected(Which::Q4)[0], Q4);
        assert!(demo_pick(&only_q3.comparisons, &graph, &reasoning).is_none());

        let summary = extraction_summary(&cands, Path::new("c.json"));
        assert!(summary.contains("empagliflozin"));
        assert!(summary.contains("Saved to c.json"));
    }

    #[test]
    fn seed_command_writes_bronze() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_string_lossy().to_string();
        let out = run(&parse(&["--data-dir", &d, "seed", "--seed", "5"])).unwrap();
        assert!(out.contains("1000 submissions"));
        assert!(dir.path().join("bronze/submissions.csv").exists());
        // Commands that need the warehouse fail with a helpful message.
        for args in [
            vec!["--data-dir", &d, "ask", "q2", "--trust", "curated"],
            vec!["--data-dir", &d, "explain", "S000001"],
            vec!["--data-dir", &d, "extract", "--extractor", "mock"],
            vec!["--data-dir", &d, "export"],
        ] {
            let err = run(&parse(&args)).unwrap_err();
            assert!(format!("{err:#}").contains("dbt build"), "{args:?}");
        }
        let err = run(&parse(&[
            "--data-dir",
            &d,
            "--mf-bin",
            "no-such-mf-s2r",
            "demo",
        ]))
        .unwrap_err();
        assert!(format!("{err:#}").contains("no-such-mf-s2r"));
    }

    #[test]
    fn commands_against_a_warehouse_file() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_string_lossy().to_string();
        run(&parse(&["--data-dir", &d, "seed"])).unwrap();
        {
            let conn = duckdb::Connection::open(dir.path().join("warehouse.duckdb")).unwrap();
            crate::api::warehouse::tests::populate(&conn, &generate(42).silver);
        }
        let out = run(&parse(&["--data-dir", &d, "ask", "q2", "--trust", "llm"]));
        assert!(format!("{:#}", out.unwrap_err()).contains("s2r extract"));

        let out = run(&parse(&[
            "--data-dir",
            &d,
            "extract",
            "--extractor",
            "mock",
        ]))
        .unwrap();
        assert!(out.contains("mock-extractor"));
        assert!(dir.path().join("llm_candidates.json").exists());

        let out = run(&parse(&["--data-dir", &d, "ask", "q4"])).unwrap();
        assert!(out.contains("Q4  Which signed illustrations"));
        assert!(out.contains("ND_LLM"), "ground truth labels are shown");

        let out = run(&parse(&[
            "--data-dir",
            &d,
            "explain",
            "S000001",
            "--trust",
            "curated",
        ]))
        .unwrap();
        assert!(out.starts_with("Findings for S000001 (curated)"));

        let nt = dir.path().join("g.nt");
        let out = run(&parse(&[
            "--data-dir",
            &d,
            "export",
            "--out",
            &nt.to_string_lossy(),
        ]))
        .unwrap();
        assert!(out.contains("inferred"));
        assert!(std::fs::metadata(&nt).unwrap().len() > 1000);

        let lm = run(&parse(&[
            "--data-dir",
            &d,
            "extract",
            "--base-url",
            "http://127.0.0.1:9/v1",
            "--timeout-secs",
            "1",
        ]));
        assert!(format!("{:#}", lm.unwrap_err()).contains("LM Studio"));
    }
}
