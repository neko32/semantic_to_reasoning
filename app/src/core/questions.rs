//! The four demo questions, asked of both paths:
//!
//! * **Gold**: star schema in `DuckDB` (Q2–Q4 as best-effort SQL) and the
//!   `MetricFlow` semantic layer (Q1).
//! * **Graph**: Oxigraph + ontology + reasoner.

use std::collections::{BTreeMap, BTreeSet};

use oxigraph::model::Term;

use super::domain::Quarter;
use super::graph::{BASE, Graph};
use super::seed::GroundTruth;

pub const GOLD_Q2_SQL: &str = include_str!("../../queries/gold/q2_insurable_interest.sql");
pub const GOLD_Q3_SQL: &str = include_str!("../../queries/gold/q3_non_disclosure.sql");
pub const GOLD_Q4_SQL: &str = include_str!("../../queries/gold/q4_reillustration.sql");

/// Port to the Gold tables.
pub trait GoldStore {
    /// Runs a query whose first column is a submission id.
    ///
    /// # Errors
    /// Returns an error when the query fails.
    fn submission_ids(&self, sql: &str) -> anyhow::Result<BTreeSet<String>>;
}

/// Port to the semantic layer.
pub trait MetricsLayer {
    /// # Errors
    /// Returns an error when the semantic layer cannot answer.
    fn q1(&self, quarter: Quarter) -> anyhow::Result<Vec<Q1Row>>;
}

/// One row of Q1: placement by channel and product type.
#[derive(Debug, Clone, PartialEq)]
pub struct Q1Row {
    pub channel: String,
    pub product_type: String,
    pub submissions: u64,
    pub placement_rate: f64,
    pub avg_face_amount: f64,
}

impl Q1Row {
    /// Equal up to rounding.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.channel == other.channel
            && self.product_type == other.product_type
            && self.submissions == other.submissions
            && (self.placement_rate - other.placement_rate).abs() < 1e-6
            && (self.avg_face_amount - other.avg_face_amount).abs() < 0.01
    }
}

fn number(term: Option<&Term>) -> f64 {
    match term {
        Some(Term::Literal(l)) => l.value().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn text(term: Option<&Term>) -> String {
    match term {
        Some(Term::Literal(l)) => l.value().to_owned(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Q1 on the graph: same definition as the `MetricFlow` metrics.
///
/// # Errors
/// Returns an error when the query fails.
pub fn graph_q1(graph: &Graph, quarter: Quarter) -> anyhow::Result<Vec<Q1Row>> {
    let rows = graph.select(&format!(
        r#"SELECT ?channel ?ptype (COUNT(?s) AS ?n)
                  (SUM(IF(?status = "PLACED", 1, 0)) AS ?placed)
                  (AVG(?face) AS ?avg)
           WHERE {{
             ?s a uw:Submission ; uw:channel ?channel ; uw:status ?status ;
                uw:faceAmount ?face ; uw:receivedOn ?d ; uw:forProduct ?p .
             ?p uw:productType ?ptype .
             FILTER(?d >= "{}"^^xsd:date && ?d < "{}"^^xsd:date)
           }}
           GROUP BY ?channel ?ptype
           ORDER BY ?channel ?ptype"#,
        quarter.start_date(),
        quarter.end_date_exclusive()
    ))?;
    Ok(rows
        .iter()
        .map(|r| {
            let n = number(r.get("n"));
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Q1Row {
                channel: text(r.get("channel")),
                product_type: text(r.get("ptype")),
                submissions: n as u64,
                placement_rate: if n > 0.0 {
                    number(r.get("placed")) / n
                } else {
                    0.0
                },
                avg_face_amount: number(r.get("avg")),
            }
        })
        .collect())
}

/// Submission ids with a given finding.
///
/// # Errors
/// Returns an error when the query fails.
pub fn graph_findings(graph: &Graph, finding: &str) -> anyhow::Result<BTreeSet<String>> {
    let prefix = format!("{BASE}submission/");
    Ok(graph
        .select(&format!(
            "SELECT DISTINCT ?s WHERE {{ ?s uw:hasFinding uw:{finding} }}"
        ))?
        .iter()
        .filter_map(|r| match r.get("s") {
            Some(Term::NamedNode(n)) => n.as_str().strip_prefix(&prefix).map(str::to_owned),
            _ => None,
        })
        .collect())
}

/// A set-valued question (Q2–Q4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetQuestion {
    pub key: &'static str,
    pub title: &'static str,
    pub finding: &'static str,
    pub gold_sql: &'static str,
}

pub const Q2: SetQuestion = SetQuestion {
    key: "q2",
    title: "Which applications lack an insurable interest (possible STOLI)?",
    finding: "InsurableInterestNotEstablished",
    gold_sql: GOLD_Q2_SQL,
};
pub const Q3: SetQuestion = SetQuestion {
    key: "q3",
    title: "Which applications contradict the applicant's prescription history?",
    finding: "PotentialNonDisclosure",
    gold_sql: GOLD_Q3_SQL,
};
pub const Q4: SetQuestion = SetQuestion {
    key: "q4",
    title: "Which signed illustrations assume a rate class the case cannot get?",
    finding: "RequiresReillustration",
    gold_sql: GOLD_Q4_SQL,
};
pub const SET_QUESTIONS: [SetQuestion; 3] = [Q2, Q3, Q4];

/// Gold vs. graph answers to a set-valued question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub question: SetQuestion,
    pub gold: BTreeSet<String>,
    pub graph: BTreeSet<String>,
}

impl Comparison {
    #[must_use]
    pub fn both(&self) -> BTreeSet<String> {
        self.gold.intersection(&self.graph).cloned().collect()
    }

    #[must_use]
    pub fn gold_only(&self) -> BTreeSet<String> {
        self.gold.difference(&self.graph).cloned().collect()
    }

    #[must_use]
    pub fn graph_only(&self) -> BTreeSet<String> {
        self.graph.difference(&self.gold).cloned().collect()
    }
}

/// Runs a set question on both paths.
///
/// # Errors
/// Returns an error when either path fails.
pub fn compare(
    question: SetQuestion,
    gold: &dyn GoldStore,
    graph: &Graph,
) -> anyhow::Result<Comparison> {
    Ok(Comparison {
        question,
        gold: gold.submission_ids(question.gold_sql)?,
        graph: graph_findings(graph, question.finding)?,
    })
}

/// Counts ids by planted scenario label (ownership for Q2, health otherwise).
#[must_use]
pub fn by_scenario(
    ids: &BTreeSet<String>,
    truth: &GroundTruth,
    question: SetQuestion,
) -> BTreeMap<String, usize> {
    let idx = usize::from(question.key != "q2");
    let mut out = BTreeMap::new();
    for id in ids {
        let label = truth
            .label(id)
            .map_or_else(|| "UNKNOWN".to_owned(), |l| l[idx].clone());
        *out.entry(label).or_default() += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::domain::{Confidence, TrustPolicy};
    use crate::core::knowledge::{Catalog, LlmCandidates, MockExtractor, extract};
    use crate::core::reasoner::{Explanation, explain, reason};
    use crate::core::seed::{Generated, generate};
    use oxigraph::model::{NamedNode, Triple};

    fn setup() -> (Generated, Catalog, LlmCandidates) {
        let g = generate(42);
        let cat = Catalog::bundled().unwrap();
        let texts: Vec<String> = g
            .silver
            .rx_fills
            .iter()
            .map(|r| r.drug_text.clone())
            .collect();
        let cands = extract(&MockExtractor, &cat, &texts).unwrap();
        (g, cat, cands)
    }

    fn llm() -> TrustPolicy {
        TrustPolicy::CuratedPlusLlm {
            min_confidence: Confidence::new(0.8).unwrap(),
        }
    }

    #[test]
    fn reasoner_matches_the_independent_oracle() {
        let (g, cat, cands) = setup();
        for policy in [TrustPolicy::CuratedOnly, llm()] {
            let mut graph = Graph::build(&g.silver, &cat, Some(&cands), policy).unwrap();
            let r = reason(&mut graph).unwrap();
            assert!(r.inferred() > 0);
            let q2 = graph_findings(&graph, Q2.finding).unwrap();
            assert_eq!(
                q2, g.truth.q2_insurable_interest_not_established,
                "{policy}"
            );
            let (q3, q4) = match policy {
                TrustPolicy::CuratedOnly => (
                    &g.truth.q3_potential_non_disclosure.curated,
                    &g.truth.q4_requires_reillustration.curated,
                ),
                TrustPolicy::CuratedPlusLlm { .. } => (
                    &g.truth.q3_potential_non_disclosure.curated_plus_llm,
                    &g.truth.q4_requires_reillustration.curated_plus_llm,
                ),
            };
            assert_eq!(&graph_findings(&graph, Q3.finding).unwrap(), q3, "{policy}");
            assert_eq!(&graph_findings(&graph, Q4.finding).unwrap(), q4, "{policy}");
        }
    }

    #[test]
    fn llm_dependent_findings_are_explained_as_such() {
        let (g, cat, cands) = setup();
        let mut graph = Graph::build(&g.silver, &cat, Some(&cands), llm()).unwrap();
        let r = reason(&mut graph).unwrap();
        let t = &g.truth;
        let only_llm: Vec<&String> = t
            .q3_potential_non_disclosure
            .curated_plus_llm
            .difference(&t.q3_potential_non_disclosure.curated)
            .collect();
        assert!(!only_llm.is_empty());
        let finding = |id: &str| {
            Triple::new(
                NamedNode::new(crate::core::graph::submission_iri(id)).unwrap(),
                NamedNode::new(format!("{}hasFinding", crate::core::knowledge::UW)).unwrap(),
                NamedNode::new(format!(
                    "{}PotentialNonDisclosure",
                    crate::core::knowledge::UW
                ))
                .unwrap(),
            )
        };
        for id in only_llm {
            assert!(explain(&graph, &r, &finding(id)).depends_on_llm(), "{id}");
        }
        for id in &t.q3_potential_non_disclosure.curated {
            let e = explain(&graph, &r, &finding(id));
            assert!(matches!(e, Explanation::Inferred { .. }));
            assert!(!e.depends_on_llm(), "{id}");
        }
    }

    #[test]
    fn graph_q1_counts_every_submission_in_the_quarter() {
        let (g, cat, _) = setup();
        let graph = Graph::build(&g.silver, &cat, None, TrustPolicy::CuratedOnly).unwrap();
        let q: Quarter = "2026-Q3".parse().unwrap();
        let rows = graph_q1(&graph, q).unwrap();
        let expected = g
            .silver
            .submissions
            .iter()
            .filter(|s| {
                let d = s.received_date.to_string();
                d >= q.start_date() && d < q.end_date_exclusive()
            })
            .count() as u64;
        assert_eq!(rows.iter().map(|r| r.submissions).sum::<u64>(), expected);
        assert!(rows.iter().all(|r| (0.0..=1.0).contains(&r.placement_rate)));
        assert!(rows[0].matches(&rows[0].clone()));
        let mut other = rows[0].clone();
        other.submissions += 1;
        assert!(!rows[0].matches(&other));
    }

    struct FixedGold(BTreeSet<String>);
    impl GoldStore for FixedGold {
        fn submission_ids(&self, _: &str) -> anyhow::Result<BTreeSet<String>> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn comparison_set_algebra_and_labels() {
        let (g, cat, _) = setup();
        let mut graph = Graph::build(&g.silver, &cat, None, TrustPolicy::CuratedOnly).unwrap();
        reason(&mut graph).unwrap();
        let truth_ids = g.truth.q2_insurable_interest_not_established.clone();
        let mut gold_ids: BTreeSet<String> = truth_ids.iter().skip(2).cloned().collect();
        gold_ids.insert("S999999".into());
        let c = compare(Q2, &FixedGold(gold_ids), &graph).unwrap();
        assert_eq!(c.both().len(), truth_ids.len() - 2);
        assert_eq!(c.graph_only().len(), 2);
        assert_eq!(c.gold_only(), BTreeSet::from(["S999999".to_owned()]));
        let labels = by_scenario(&c.gold_only(), &g.truth, Q2);
        assert_eq!(labels["UNKNOWN"], 1);
        let labels = by_scenario(&c.graph, &g.truth, Q2);
        assert_eq!(labels.values().sum::<usize>(), 12);
        assert!(labels.keys().all(|k| k.starts_with("STOLI")));
        let health = by_scenario(&c.graph, &g.truth, Q3);
        assert_eq!(health.values().sum::<usize>(), 12);
    }

    #[test]
    fn helpers_tolerate_missing_values() {
        assert!(number(None).abs() < f64::EPSILON);
        assert_eq!(text(None), "");
        let n: Term = NamedNode::new("https://e.org/a").unwrap().into();
        assert_eq!(text(Some(&n)), "<https://e.org/a>");
        assert!(number(Some(&n)).abs() < f64::EPSILON);
    }
}
