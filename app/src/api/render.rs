//! Terminal rendering. Pure functions returning strings, so they are testable.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Cell, CellAlignment, ContentArrangement, Table};

use crate::core::graph::{Graph, show_triple};
use crate::core::questions::{Comparison, Q1Row, by_scenario};
use crate::core::reasoner::{Explanation, Reasoning};
use crate::core::seed::GroundTruth;

fn table() -> Table {
    let mut t = Table::new();
    t.load_style(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic);
    t
}

fn right(text: impl ToString) -> Cell {
    Cell::new(text).set_alignment(CellAlignment::Right)
}

/// Header line describing the graph and the reasoning run.
#[must_use]
pub fn graph_summary(graph: &Graph, reasoning: &Reasoning) -> String {
    let ontology_rules = reasoning
        .rules
        .iter()
        .filter(|r| r.source == crate::core::reasoner::RuleSource::Ontology)
        .count();
    format!(
        "Graph: {} asserted triples + {} inferred ({} ontology rules, {} business rules, fixpoint after {} iterations). Trust: {}.",
        graph.asserted_count(),
        reasoning.inferred(),
        ontology_rules,
        reasoning.rules.len() - ontology_rules,
        reasoning.iterations,
        graph.policy,
    )
}

fn pct(v: f64) -> String {
    format!("{:.1}%", v * 100.0)
}

fn usd(v: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let whole = v.round() as u64;
    let digits = whole.to_string();
    let mut out = String::from("$");
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Q1 side by side. Returns the text and whether every row matched.
#[must_use]
pub fn q1(quarter: &str, gold: &[Q1Row], graph: &[Q1Row]) -> (String, bool) {
    let mut t = table();
    t.set_header(vec![
        "Channel",
        "Product",
        "Submissions\nGold | Graph",
        "Placement rate\nGold | Graph",
        "Avg face amount\nGold | Graph",
        "Same?",
    ]);
    let mut keys: BTreeSet<(&str, &str)> = BTreeSet::new();
    keys.extend(
        gold.iter()
            .map(|r| (r.channel.as_str(), r.product_type.as_str())),
    );
    keys.extend(
        graph
            .iter()
            .map(|r| (r.channel.as_str(), r.product_type.as_str())),
    );
    let find = |rows: &'_ [Q1Row], k: (&str, &str)| {
        rows.iter()
            .find(|r| r.channel == k.0 && r.product_type == k.1)
            .cloned()
    };
    let mut all_match = true;
    for k in keys {
        let (a, b) = (find(gold, k), find(graph, k));
        let same = matches!((&a, &b), (Some(x), Some(y)) if x.matches(y));
        all_match &= same;
        let pair = |f: &dyn Fn(&Q1Row) -> String| {
            format!(
                "{} | {}",
                a.as_ref().map_or("-".into(), f),
                b.as_ref().map_or("-".into(), f)
            )
        };
        t.add_row(vec![
            Cell::new(k.0),
            Cell::new(k.1),
            right(pair(&|r| r.submissions.to_string())),
            right(pair(&|r| pct(r.placement_rate))),
            right(pair(&|r| usd(r.avg_face_amount))),
            Cell::new(if same { "yes" } else { "NO" }),
        ]);
    }
    let verdict = if all_match {
        "Both paths agree. Business truth is the Gold layer's home ground."
    } else {
        "The paths disagree. Check the metric definitions."
    };
    (
        format!(
            "Q1  Placement rate and average face amount by channel and product, {quarter}\n    Gold: MetricFlow semantic layer (mf query)    Graph: SPARQL aggregate\n{t}\n    => {verdict}\n"
        ),
        all_match,
    )
}

fn breakdown(counts: &BTreeMap<String, usize>) -> String {
    counts
        .iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn sample(ids: &BTreeSet<String>, n: usize) -> String {
    let mut s: Vec<&str> = ids.iter().take(n).map(String::as_str).collect();
    if ids.len() > n {
        s.push("…");
    }
    s.join(" ")
}

/// A set question side by side.
#[must_use]
pub fn comparison(c: &Comparison, truth: Option<&GroundTruth>, show: usize) -> String {
    let q = c.question;
    let mut t = table();
    let mut header = vec!["", "Count", "Sample ids"];
    if truth.is_some() {
        header.push("Planted scenario");
    }
    t.set_header(header);
    for (label, ids) in [
        ("Found by both", c.both()),
        ("Gold only (false alarm)", c.gold_only()),
        ("Graph only (Gold missed)", c.graph_only()),
    ] {
        let mut row = vec![
            Cell::new(label),
            right(ids.len()),
            Cell::new(sample(&ids, show)),
        ];
        if let Some(truth) = truth {
            row.push(Cell::new(breakdown(&by_scenario(&ids, truth, q))));
        }
        t.add_row(row);
    }
    let verdict = match (c.gold_only().len(), c.graph_only().len()) {
        (0, 0) => "Both paths agree.".to_owned(),
        (fp, 0) => format!(
            "Gold raises {fp} false alarm(s): the relationship that clears them is not a row in any table."
        ),
        (0, miss) => format!(
            "Gold misses {miss} case(s): the facts are in Silver, but the meaning that connects them is not."
        ),
        (fp, miss) => format!("Gold raises {fp} false alarm(s) and misses {miss} case(s)."),
    };
    format!(
        "{}  {}\n    Gold: {} flagged by SQL on the star schema    Graph: {} flagged by the reasoner\n{t}\n    => {verdict}\n",
        q.key.to_uppercase(),
        q.title,
        c.gold.len(),
        c.graph.len(),
    )
}

fn explain_into(
    out: &mut String,
    e: &Explanation,
    reasoning: &Reasoning,
    prefix: &str,
    last: bool,
    root: bool,
) {
    let (branch, next) = if root {
        ("", format!("{prefix}  "))
    } else if last {
        ("└─ ", format!("{prefix}   "))
    } else {
        ("├─ ", format!("{prefix}│  "))
    };
    match e {
        Explanation::Asserted { triple, origin } => {
            let origin = origin
                .as_ref()
                .map_or("asserted".to_owned(), ToString::to_string);
            let _ = writeln!(out, "{prefix}{branch}{}   [{origin}]", show_triple(triple));
        }
        Explanation::Inferred {
            triple,
            rule,
            premises,
        } => {
            // Business rules carry a plain-English reason; OWL rule ids speak for themselves.
            let why = reasoning
                .rule(rule)
                .filter(|r| r.source == crate::core::reasoner::RuleSource::Business)
                .map(|r| format!(" — {}", r.description))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "{prefix}{branch}{}   <= {rule}{why}",
                show_triple(triple)
            );
            for (i, p) in premises.iter().enumerate() {
                explain_into(out, p, reasoning, &next, i + 1 == premises.len(), false);
            }
        }
    }
}

/// Renders an explanation tree.
#[must_use]
pub fn explanation(e: &Explanation, reasoning: &Reasoning) -> String {
    let mut out = String::new();
    explain_into(&mut out, e, reasoning, "    ", true, true);
    if e.depends_on_llm() {
        out.push_str("    !! This conclusion depends on an LLM candidate (see the leaves marked 'LLM candidate').\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::domain::TrustPolicy;
    use crate::core::graph::Origin;
    use crate::core::questions::Q2;
    use oxigraph::model::{NamedNode, Triple};

    fn row(ch: &str, n: u64) -> Q1Row {
        Q1Row {
            channel: ch.into(),
            product_type: "TERM".into(),
            submissions: n,
            placement_rate: 0.5,
            avg_face_amount: 1_234_567.4,
        }
    }

    #[test]
    fn number_formats() {
        assert_eq!(usd(1_234_567.4), "$1,234,567");
        assert_eq!(usd(999.0), "$999");
        assert_eq!(pct(0.4567), "45.7%");
    }

    #[test]
    fn q1_detects_agreement_and_disagreement() {
        let (text, ok) = q1("2026-Q3", &[row("BANK", 3)], &[row("BANK", 3)]);
        assert!(ok);
        assert!(text.contains("$1,234,567 | $1,234,567"));
        assert!(text.contains("Both paths agree"));
        let (text, ok) = q1(
            "2026-Q3",
            &[row("BANK", 3)],
            &[row("BANK", 4), row("DIRECT", 1)],
        );
        assert!(!ok);
        assert!(text.contains("NO"));
        assert!(text.contains("- | 1"));
    }

    #[test]
    fn comparison_verdicts() {
        let ids = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<BTreeSet<_>>();
        let mut truth = GroundTruth::default();
        truth
            .scenarios
            .insert("S000001".into(), ["GRANDPARENT".into(), "HEALTHY".into()]);
        let c = Comparison {
            question: Q2,
            gold: ids(&["S000001", "S000002"]),
            graph: ids(&["S000002"]),
        };
        let text = comparison(&c, Some(&truth), 1);
        assert!(text.contains("GRANDPARENT 1"));
        assert!(text.contains("false alarm"));
        let miss = Comparison {
            gold: ids(&[]),
            ..c.clone()
        };
        assert!(comparison(&miss, None, 5).contains("misses 1"));
        let same = Comparison {
            gold: c.graph.clone(),
            ..c.clone()
        };
        assert!(comparison(&same, None, 5).contains("Both paths agree"));
        let both = Comparison {
            gold: ids(&["S000001"]),
            ..c
        };
        assert!(comparison(&both, None, 5).contains("and misses"));
        assert_eq!(sample(&ids(&["a", "b", "c"]), 2), "a b …");
    }

    #[test]
    fn explanation_tree() {
        let n = |s: &str| NamedNode::new(format!("https://example.org/s2r/uw#{s}")).unwrap();
        let t = |a, b, c| Triple::new(n(a), n(b), n(c));
        let e = Explanation::Inferred {
            triple: t("x", "p", "z"),
            rule: "R1".into(),
            premises: vec![
                Explanation::Asserted {
                    triple: t("x", "q", "y"),
                    origin: Some(Origin::Silver("t")),
                },
                Explanation::Asserted {
                    triple: t("y", "q", "z"),
                    origin: Some(Origin::Llm {
                        model: "m".into(),
                        confidence: 0.9,
                    }),
                },
                Explanation::Asserted {
                    triple: t("y", "r", "z"),
                    origin: None,
                },
            ],
        };
        let text = explanation(&e, &Reasoning::default());
        assert!(text.contains("uw:x uw:p uw:z   <= R1"));
        assert!(text.contains("      ├─ uw:x uw:q uw:y   [silver.t]"));
        assert!(text.contains("└─ uw:y uw:r uw:z   [asserted]"));
        assert!(text.contains("depends on an LLM candidate"));
    }

    #[test]
    fn summary_line() {
        let g = Graph::empty(TrustPolicy::CuratedOnly).unwrap();
        let s = graph_summary(&g, &Reasoning::default());
        assert!(s.contains("0 asserted"));
        assert!(s.contains("Trust: curated"));
    }
}
