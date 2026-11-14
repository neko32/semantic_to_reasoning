//! A small forward-chaining reasoner with provenance.
//!
//! * OWL/RDFS axioms in the ontology are compiled into Horn rules
//!   (subClassOf, subPropertyOf, inverseOf, `SymmetricProperty`,
//!   `TransitiveProperty`, propertyChainAxiom).
//! * Business rules come from `rules/underwriting.toml`.
//! * Each rule body is evaluated as a SPARQL SELECT. New conclusions are
//!   inserted until nothing changes (a fixpoint), stratum by stratum.
//! * Every inferred triple records the rule and the premises that produced
//!   it, so any conclusion can be explained down to Silver rows, curated
//!   knowledge, or LLM candidates.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use anyhow::{Context, bail};
use oxigraph::model::{Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use oxigraph::sparql::QuerySolution;
use serde::Deserialize;

use super::graph::{Graph, Origin, PREFIXES, compact};

pub const BUSINESS_RULES_TOML: &str = include_str!("../../rules/underwriting.toml");

/// Safety net against runaway rule sets.
pub const MAX_ITERATIONS: usize = 100;

/// A token in a triple pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    Var(String),
    Const(Term),
}

impl Tok {
    fn sparql(&self) -> String {
        match self {
            Self::Var(v) => format!("?{v}"),
            Self::Const(t) => t.to_string(),
        }
    }

    fn bind(&self, row: &QuerySolution) -> Option<Term> {
        match self {
            Self::Var(v) => row.get(v.as_str()).cloned(),
            Self::Const(t) => Some(t.clone()),
        }
    }
}

pub type Pattern = [Tok; 3];

fn expand_prefixed(token: &str) -> anyhow::Result<NamedNode> {
    if let Some(iri) = token.strip_prefix('<').and_then(|t| t.strip_suffix('>')) {
        return Ok(NamedNode::new(iri)?);
    }
    let (prefix, local) = token
        .split_once(':')
        .with_context(|| format!("expected a prefixed name, got '{token}'"))?;
    let ns = PREFIXES
        .iter()
        .find(|(p, _)| *p == prefix)
        .map(|(_, ns)| *ns)
        .with_context(|| format!("unknown prefix '{prefix}:'"))?;
    Ok(NamedNode::new(format!("{ns}{local}"))?)
}

fn parse_token(token: &str) -> anyhow::Result<Tok> {
    Ok(match token {
        "true" => Tok::Const(Literal::from(true).into()),
        "false" => Tok::Const(Literal::from(false).into()),
        t if t.starts_with('?') && t.len() > 1 => Tok::Var(t[1..].to_owned()),
        t => Tok::Const(expand_prefixed(t)?.into()),
    })
}

/// Parses `"?s uw:p ?o"`.
///
/// # Errors
/// Returns an error for a malformed pattern.
pub fn parse_pattern(text: &str) -> anyhow::Result<Pattern> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let [s, p, o] = parts.as_slice() else {
        bail!("pattern must have 3 tokens: '{text}'");
    };
    let pattern = [parse_token(s)?, parse_token(p)?, parse_token(o)?];
    if matches!(&pattern[1], Tok::Const(Term::Literal(_))) {
        bail!("predicate cannot be a literal: '{text}'");
    }
    Ok(pattern)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleSource {
    Ontology,
    Business,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub stratum: u8,
    pub description: String,
    pub source: RuleSource,
    pub when: Vec<Pattern>,
    pub filter: Vec<String>,
    pub unless: Vec<Pattern>,
    pub then: Vec<Pattern>,
}

impl Rule {
    fn sparql(&self) -> String {
        let mut q = String::from("SELECT * WHERE {\n");
        for p in &self.when {
            let _ = writeln!(
                q,
                "  {} {} {} .",
                p[0].sparql(),
                p[1].sparql(),
                p[2].sparql()
            );
        }
        for f in &self.filter {
            let _ = writeln!(q, "  FILTER({f})");
        }
        if !self.unless.is_empty() {
            q.push_str("  FILTER NOT EXISTS {\n");
            for p in &self.unless {
                let _ = writeln!(
                    q,
                    "    {} {} {} .",
                    p[0].sparql(),
                    p[1].sparql(),
                    p[2].sparql()
                );
            }
            q.push_str("  }\n");
        }
        q.push('}');
        q
    }
}

#[derive(Debug, Deserialize)]
struct RuleFile {
    rule: Vec<RuleSpec>,
}

#[derive(Debug, Deserialize)]
struct RuleSpec {
    id: String,
    stratum: u8,
    description: String,
    when: Vec<String>,
    #[serde(default)]
    filter: Vec<String>,
    #[serde(default)]
    unless: Vec<String>,
    then: Vec<String>,
}

fn vars(patterns: &[Pattern]) -> Vec<&str> {
    patterns
        .iter()
        .flatten()
        .filter_map(|t| match t {
            Tok::Var(v) => Some(v.as_str()),
            Tok::Const(_) => None,
        })
        .collect()
}

/// Parses and validates business rules.
///
/// # Errors
/// Returns an error for malformed TOML or rules that break the safety checks.
pub fn parse_business_rules(toml_text: &str) -> anyhow::Result<Vec<Rule>> {
    let file: RuleFile = toml::from_str(toml_text).context("parsing rules TOML")?;
    let mut rules = Vec::new();
    for spec in file.rule {
        let parse_all = |v: &[String]| -> anyhow::Result<Vec<Pattern>> {
            v.iter().map(|p| parse_pattern(p)).collect()
        };
        let rule = Rule {
            id: spec.id.clone(),
            stratum: spec.stratum,
            description: spec.description,
            source: RuleSource::Business,
            when: parse_all(&spec.when).with_context(|| format!("rule {}", spec.id))?,
            filter: spec.filter,
            unless: parse_all(&spec.unless).with_context(|| format!("rule {}", spec.id))?,
            then: parse_all(&spec.then).with_context(|| format!("rule {}", spec.id))?,
        };
        if !(1..=2).contains(&rule.stratum) {
            bail!("rule {}: stratum must be 1 or 2", rule.id);
        }
        if !rule.unless.is_empty() && rule.stratum != 2 {
            bail!(
                "rule {}: 'unless' (negation) is only allowed in stratum 2",
                rule.id
            );
        }
        if rule.when.is_empty() || rule.then.is_empty() {
            bail!("rule {}: 'when' and 'then' must not be empty", rule.id);
        }
        // Range restriction: every head variable must be bound by the body.
        let body = vars(&rule.when);
        if let Some(v) = vars(&rule.then).into_iter().find(|v| !body.contains(v)) {
            bail!(
                "rule {}: head variable ?{v} is not bound in 'when'",
                rule.id
            );
        }
        rules.push(rule);
    }
    Ok(rules)
}

fn v(name: &str) -> Tok {
    Tok::Var(name.to_owned())
}

fn c(node: &NamedNode) -> Tok {
    Tok::Const(node.clone().into())
}

fn ontology_rule(id: String, description: String, when: Vec<Pattern>, then: Pattern) -> Rule {
    Rule {
        id,
        stratum: 1,
        description,
        source: RuleSource::Ontology,
        when,
        filter: Vec::new(),
        unless: Vec::new(),
        then: vec![then],
    }
}

fn named(row: &QuerySolution, var: &str) -> Option<NamedNode> {
    match row.get(var) {
        Some(Term::NamedNode(n)) => Some(n.clone()),
        _ => None,
    }
}

/// Compiles the OWL/RDFS axioms found in the graph into rules.
///
/// # Errors
/// Returns an error when a query fails or a property chain is malformed.
#[allow(clippy::too_many_lines)]
pub fn compile_ontology(graph: &Graph) -> anyhow::Result<Vec<Rule>> {
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let mut rules = Vec::new();

    for row in graph.select("SELECT ?c ?d WHERE { ?c rdfs:subClassOf ?d FILTER(isIRI(?d)) }")? {
        let (Some(sub), Some(sup)) = (named(&row, "c"), named(&row, "d")) else {
            continue;
        };
        let (a, b) = (compact(sub.as_str()), compact(sup.as_str()));
        rules.push(ontology_rule(
            format!("RDFS-SUBCLASS {a} ⊑ {b}"),
            format!("Every {a} is a {b}."),
            vec![[v("x"), c(&rdf_type), c(&sub)]],
            [v("x"), c(&rdf_type), c(&sup)],
        ));
    }
    for row in graph.select("SELECT ?p ?q WHERE { ?p rdfs:subPropertyOf ?q }")? {
        let (Some(p), Some(q)) = (named(&row, "p"), named(&row, "q")) else {
            continue;
        };
        let (a, b) = (compact(p.as_str()), compact(q.as_str()));
        rules.push(ontology_rule(
            format!("RDFS-SUBPROPERTY {a} ⊑ {b}"),
            format!("{a} implies {b}."),
            vec![[v("x"), c(&p), v("y")]],
            [v("x"), c(&q), v("y")],
        ));
    }
    for row in graph.select("SELECT ?p ?q WHERE { ?p owl:inverseOf ?q }")? {
        let (Some(p), Some(q)) = (named(&row, "p"), named(&row, "q")) else {
            continue;
        };
        let (a, b) = (compact(p.as_str()), compact(q.as_str()));
        rules.push(ontology_rule(
            format!("OWL-INVERSE {a} → {b}"),
            format!("{a} is the inverse of {b}."),
            vec![[v("x"), c(&p), v("y")]],
            [v("y"), c(&q), v("x")],
        ));
        rules.push(ontology_rule(
            format!("OWL-INVERSE {b} → {a}"),
            format!("{b} is the inverse of {a}."),
            vec![[v("x"), c(&q), v("y")]],
            [v("y"), c(&p), v("x")],
        ));
    }
    for row in graph.select("SELECT ?p WHERE { ?p a owl:SymmetricProperty }")? {
        let Some(p) = named(&row, "p") else { continue };
        let a = compact(p.as_str());
        rules.push(ontology_rule(
            format!("OWL-SYMMETRIC {a}"),
            format!("{a} holds in both directions."),
            vec![[v("x"), c(&p), v("y")]],
            [v("y"), c(&p), v("x")],
        ));
    }
    for row in graph.select("SELECT ?p WHERE { ?p a owl:TransitiveProperty }")? {
        let Some(p) = named(&row, "p") else { continue };
        let a = compact(p.as_str());
        rules.push(ontology_rule(
            format!("OWL-TRANSITIVE {a}"),
            format!("{a} chains: x→y and y→z give x→z."),
            vec![[v("x"), c(&p), v("y")], [v("y"), c(&p), v("z")]],
            [v("x"), c(&p), v("z")],
        ));
    }

    let first = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#first");
    let rest = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#rest");
    let nil = "http://www.w3.org/1999/02/22-rdf-syntax-ns#nil";
    for row in graph.select("SELECT ?q ?list WHERE { ?q owl:propertyChainAxiom ?list }")? {
        let Some(q) = named(&row, "q") else { continue };
        let Some(mut cursor) = row.get("list").cloned() else {
            continue;
        };
        let mut chain = Vec::new();
        loop {
            let node: NamedOrBlankNode = match cursor {
                Term::NamedNode(n) if n.as_str() == nil => break,
                Term::NamedNode(n) => n.into(),
                Term::BlankNode(b) => b.into(),
                Term::Literal(_) => bail!("malformed property chain for {q}"),
            };
            let item = graph.objects(&node, &first)?;
            let next = graph.objects(&node, &rest)?;
            match (item.first(), next.first()) {
                (Some(Term::NamedNode(p)), Some(n)) => {
                    chain.push(p.clone());
                    cursor = n.clone();
                }
                _ => bail!("malformed property chain for {q}"),
            }
            if chain.len() > 16 {
                bail!("property chain for {q} is too long");
            }
        }
        if chain.len() < 2 {
            bail!("property chain for {q} needs at least two properties");
        }
        let when: Vec<Pattern> = chain
            .iter()
            .enumerate()
            .map(|(i, p)| [v(&format!("x{i}")), c(p), v(&format!("x{}", i + 1))])
            .collect();
        let names: Vec<String> = chain.iter().map(|p| compact(p.as_str())).collect();
        let a = compact(q.as_str());
        rules.push(ontology_rule(
            format!("OWL-CHAIN {} ⊑ {a}", names.join(" ∘ ")),
            format!("Following {} implies {a}.", names.join(" then ")),
            when,
            [v("x0"), c(&q), v(&format!("x{}", chain.len()))],
        ));
    }
    rules.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(rules)
}

/// How an inferred triple was derived.
#[derive(Debug, Clone, PartialEq)]
pub struct Derivation {
    pub rule: String,
    pub premises: Vec<Triple>,
}

/// Result of a reasoning run.
#[derive(Debug, Default)]
pub struct Reasoning {
    pub derivations: HashMap<Triple, Derivation>,
    pub iterations: usize,
    /// Inferred triples per rule id.
    pub per_rule: BTreeMap<String, usize>,
    pub rules: Vec<Rule>,
}

impl Reasoning {
    #[must_use]
    pub fn inferred(&self) -> usize {
        self.derivations.len()
    }

    #[must_use]
    pub fn rule(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }
}

fn instantiate(pattern: &Pattern, row: &QuerySolution) -> Option<Triple> {
    let subject: NamedOrBlankNode = match pattern[0].bind(row)? {
        Term::NamedNode(n) => n.into(),
        Term::BlankNode(b) => b.into(),
        Term::Literal(_) => return None,
    };
    let Term::NamedNode(predicate) = pattern[1].bind(row)? else {
        return None;
    };
    Some(Triple::new(subject, predicate, pattern[2].bind(row)?))
}

/// Runs all rules to a fixpoint, stratum by stratum.
///
/// # Errors
/// Returns an error when a query fails or the fixpoint is not reached within
/// [`MAX_ITERATIONS`].
pub fn run(graph: &mut Graph, rules: Vec<Rule>) -> anyhow::Result<Reasoning> {
    let mut out = Reasoning::default();
    let queries: Vec<String> = rules.iter().map(Rule::sparql).collect();
    for stratum in [1u8, 2] {
        loop {
            out.iterations += 1;
            if out.iterations > MAX_ITERATIONS {
                bail!("no fixpoint after {MAX_ITERATIONS} iterations");
            }
            let mut new = 0usize;
            for (rule, query) in rules
                .iter()
                .zip(&queries)
                .filter(|(r, _)| r.stratum == stratum)
            {
                let rows = graph
                    .select(query)
                    .with_context(|| format!("evaluating rule {}", rule.id))?;
                for row in &rows {
                    let premises: Option<Vec<Triple>> =
                        rule.when.iter().map(|p| instantiate(p, row)).collect();
                    let Some(premises) = premises else { continue };
                    for head in &rule.then {
                        let Some(triple) = instantiate(head, row) else {
                            continue;
                        };
                        if graph.insert_inferred(&triple)? {
                            new += 1;
                            *out.per_rule.entry(rule.id.clone()).or_default() += 1;
                            out.derivations.insert(
                                triple,
                                Derivation {
                                    rule: rule.id.clone(),
                                    premises: premises.clone(),
                                },
                            );
                        }
                    }
                }
            }
            if new == 0 {
                break;
            }
        }
    }
    out.rules = rules;
    Ok(out)
}

/// Compiles the ontology, adds the bundled business rules, and runs them.
///
/// # Errors
/// Returns an error when the rules are invalid or reasoning fails.
pub fn reason(graph: &mut Graph) -> anyhow::Result<Reasoning> {
    let mut rules = compile_ontology(graph)?;
    rules.extend(parse_business_rules(BUSINESS_RULES_TOML)?);
    run(graph, rules)
}

/// One node in an explanation tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Explanation {
    Asserted {
        triple: Triple,
        origin: Option<Origin>,
    },
    Inferred {
        triple: Triple,
        rule: String,
        premises: Vec<Explanation>,
    },
}

impl Explanation {
    #[must_use]
    pub fn triple(&self) -> &Triple {
        match self {
            Self::Asserted { triple, .. } | Self::Inferred { triple, .. } => triple,
        }
    }

    /// Every asserted leaf, in order.
    #[must_use]
    pub fn leaves(&self) -> Vec<(&Triple, Option<&Origin>)> {
        match self {
            Self::Asserted { triple, origin } => vec![(triple, origin.as_ref())],
            Self::Inferred { premises, .. } => premises.iter().flat_map(Self::leaves).collect(),
        }
    }

    /// True when any leaf comes from an LLM candidate.
    #[must_use]
    pub fn depends_on_llm(&self) -> bool {
        self.leaves()
            .iter()
            .any(|(_, o)| matches!(o, Some(Origin::Llm { .. })))
    }
}

/// Builds the explanation tree for a triple.
#[must_use]
pub fn explain(graph: &Graph, reasoning: &Reasoning, triple: &Triple) -> Explanation {
    match reasoning.derivations.get(triple) {
        Some(d) => Explanation::Inferred {
            triple: triple.clone(),
            rule: d.rule.clone(),
            premises: d
                .premises
                .iter()
                .map(|p| explain(graph, reasoning, p))
                .collect(),
        },
        None => Explanation::Asserted {
            triple: triple.clone(),
            origin: graph.origin(triple).cloned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::domain::TrustPolicy;
    use crate::core::graph::show_triple;
    use oxigraph::io::RdfFormat;
    use oxigraph::store::Store;

    fn tiny_graph(ttl: &str) -> Graph {
        let mut g = Graph::empty(TrustPolicy::CuratedOnly).unwrap();
        let tmp = Store::new().unwrap();
        tmp.load_from_slice(RdfFormat::Turtle, ttl).unwrap();
        for q in &tmp {
            let q = q.unwrap();
            g.insert_inferred(&Triple::new(q.subject, q.predicate, q.object))
                .unwrap();
        }
        g
    }

    const PFX: &str = "@prefix uw: <https://example.org/s2r/uw#> .
        @prefix owl: <http://www.w3.org/2002/07/owl#> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        @prefix ex: <https://example.org/s2r/> .\n";

    fn has(g: &Graph, s: &str, p: &str, o: &str) -> bool {
        let t = Triple::new(
            NamedNode::new(format!("https://example.org/s2r/{s}")).unwrap(),
            NamedNode::new(format!("https://example.org/s2r/uw#{p}")).unwrap(),
            NamedNode::new(format!("https://example.org/s2r/{o}")).unwrap(),
        );
        g.contains(&t).unwrap()
    }

    #[test]
    fn pattern_parsing() {
        let p = parse_pattern("?s uw:p true").unwrap();
        assert_eq!(p[0], Tok::Var("s".into()));
        assert!(matches!(p[2], Tok::Const(Term::Literal(_))));
        assert!(parse_pattern("?s uw:p").is_err());
        assert!(parse_pattern("?s nope:p ?o").is_err());
        assert!(parse_pattern("?s plain ?o").is_err());
        assert!(parse_pattern("?s true ?o").is_err());
        assert!(parse_pattern("<https://e.org/a> <https://e.org/b> false").is_ok());
    }

    #[test]
    fn business_rule_validation() {
        assert!(parse_business_rules(BUSINESS_RULES_TOML).unwrap().len() >= 8);
        let bad_neg = r#"[[rule]]
            id = "X"
            stratum = 1
            description = "d"
            when = ["?a uw:p ?b"]
            unless = ["?a uw:q ?b"]
            then = ["?a uw:r ?b"]"#;
        assert!(
            parse_business_rules(bad_neg)
                .unwrap_err()
                .to_string()
                .contains("stratum 2")
        );
        let unbound = r#"[[rule]]
            id = "X"
            stratum = 1
            description = "d"
            when = ["?a uw:p ?b"]
            then = ["?a uw:r ?zzz"]"#;
        assert!(
            parse_business_rules(unbound)
                .unwrap_err()
                .to_string()
                .contains("?zzz")
        );
        let stratum = r#"[[rule]]
            id = "X"
            stratum = 3
            description = "d"
            when = ["?a uw:p ?b"]
            then = ["?a uw:r ?b"]"#;
        assert!(parse_business_rules(stratum).is_err());
        let empty = r#"[[rule]]
            id = "X"
            stratum = 1
            description = "d"
            when = []
            then = ["uw:a uw:r uw:b"]"#;
        assert!(parse_business_rules(empty).is_err());
        assert!(parse_business_rules("not toml [").is_err());
        let bad_pattern = r#"[[rule]]
            id = "X"
            stratum = 1
            description = "d"
            when = ["?a"]
            then = ["?a uw:r ?a"]"#;
        assert!(parse_business_rules(bad_pattern).is_err());
    }

    #[test]
    fn owl_constructs_are_compiled_and_applied() {
        let mut g = tiny_graph(&format!(
            "{PFX}
            uw:spouseOf a owl:SymmetricProperty ; rdfs:subPropertyOf uw:familyMemberOf .
            uw:ancestorOf a owl:TransitiveProperty .
            uw:parentOf rdfs:subPropertyOf uw:ancestorOf .
            uw:childOf owl:inverseOf uw:parentOf .
            uw:Person rdfs:subClassOf uw:Party .
            uw:hasInsurableInterestIn owl:propertyChainAxiom ( uw:grantor uw:familyMemberOf ) .
            ex:a uw:parentOf ex:b . ex:b uw:parentOf ex:c .
            ex:x uw:spouseOf ex:y . ex:t uw:grantor ex:y .
            ex:a a uw:Person ."
        ));
        let rules = compile_ontology(&g).unwrap();
        let ids: Vec<&str> = rules.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.iter().any(|i| i.starts_with("OWL-CHAIN")));
        assert!(ids.iter().any(|i| i.starts_with("OWL-SYMMETRIC")));
        let r = run(&mut g, rules).unwrap();
        assert!(has(&g, "a", "ancestorOf", "c"), "transitive");
        assert!(has(&g, "c", "childOf", "b"), "inverse");
        assert!(has(&g, "y", "spouseOf", "x"), "symmetric");
        assert!(
            has(&g, "t", "hasInsurableInterestIn", "x"),
            "chain via symmetric"
        );
        assert!(
            g.contains(&Triple::new(
                NamedNode::new("https://example.org/s2r/a").unwrap(),
                NamedNode::new("http://www.w3.org/1999/02/22-rdf-syntax-ns#type").unwrap(),
                NamedNode::new("https://example.org/s2r/uw#Party").unwrap(),
            ))
            .unwrap()
        );
        assert!(r.iterations >= 2);
        assert!(r.inferred() > 0);

        // The chain conclusion explains down to asserted facts.
        let t = Triple::new(
            NamedNode::new("https://example.org/s2r/t").unwrap(),
            NamedNode::new("https://example.org/s2r/uw#hasInsurableInterestIn").unwrap(),
            NamedNode::new("https://example.org/s2r/x").unwrap(),
        );
        let e = explain(&g, &r, &t);
        assert!(matches!(&e, Explanation::Inferred { rule, .. } if rule.starts_with("OWL-CHAIN")));
        assert_eq!(e.triple(), &t);
        let leaves: Vec<String> = e.leaves().iter().map(|(t, _)| show_triple(t)).collect();
        assert!(leaves.contains(&"ex:t uw:grantor ex:y".to_owned()));
        assert!(leaves.contains(&"ex:x uw:spouseOf ex:y".to_owned()));
        assert!(!e.depends_on_llm());
        assert!(r.rule(&r.derivations[&t].rule).is_some());
        assert!(r.rule("nope").is_none());
    }

    #[test]
    fn malformed_chains_are_rejected() {
        let g = tiny_graph(&format!("{PFX} uw:q owl:propertyChainAxiom ( uw:only ) ."));
        assert!(compile_ontology(&g).is_err());
        let g = tiny_graph(&format!("{PFX} uw:q owl:propertyChainAxiom \"x\" ."));
        assert!(compile_ontology(&g).is_err());
        let g = tiny_graph(&format!("{PFX} uw:q owl:propertyChainAxiom ex:notalist ."));
        assert!(compile_ontology(&g).is_err());
    }

    #[test]
    fn negation_runs_after_the_monotone_stratum() {
        let mut g = tiny_graph(&format!(
            "{PFX}
            ex:s1 uw:owner ex:o1 ; uw:insured ex:i1 .
            ex:s2 uw:owner ex:o2 ; uw:insured ex:i2 .
            ex:o1 uw:link ex:i1 ."
        ));
        let rules = parse_business_rules(
            r#"
            [[rule]]
            id = "LIFT"
            stratum = 1
            description = "link implies interest"
            when = ["?a uw:link ?b"]
            then = ["?a uw:hasInsurableInterestIn ?b"]
            [[rule]]
            id = "NEG"
            stratum = 2
            description = "no interest"
            when = ["?s uw:owner ?o", "?s uw:insured ?i"]
            unless = ["?o uw:hasInsurableInterestIn ?i"]
            then = ["?s uw:hasFinding uw:Flag"]
            "#,
        )
        .unwrap();
        let r = run(&mut g, rules).unwrap();
        let flag = |s: &str| {
            g.contains(&Triple::new(
                NamedNode::new(format!("https://example.org/s2r/{s}")).unwrap(),
                NamedNode::new("https://example.org/s2r/uw#hasFinding").unwrap(),
                NamedNode::new("https://example.org/s2r/uw#Flag").unwrap(),
            ))
            .unwrap()
        };
        assert!(!flag("s1"));
        assert!(flag("s2"));
        assert_eq!(r.per_rule["NEG"], 1);
    }

    #[test]
    fn literal_subjects_are_skipped() {
        let mut g = tiny_graph(&format!("{PFX} ex:a uw:p \"lit\" ."));
        let rules = parse_business_rules(
            r#"[[rule]]
            id = "FLIP"
            stratum = 1
            description = "flip"
            when = ["?a uw:p ?b"]
            then = ["?b uw:q ?a"]"#,
        )
        .unwrap();
        let r = run(&mut g, rules).unwrap();
        assert_eq!(r.inferred(), 0);
    }

    #[test]
    fn runaway_rules_hit_the_iteration_cap() {
        // Each round invents a new node: no fixpoint exists.
        let mut g = tiny_graph(&format!("{PFX} ex:a uw:n 0 ."));
        // The filter smuggles in a BIND that mints a fresh IRI every round.
        let bad = Rule {
            id: "RUNAWAY".into(),
            stratum: 1,
            description: "d".into(),
            source: RuleSource::Business,
            when: vec![[
                v("a"),
                Tok::Const(
                    NamedNode::new("https://example.org/s2r/uw#n")
                        .unwrap()
                        .into(),
                ),
                v("k"),
            ]],
            filter: vec!["true) BIND(IRI(CONCAT(STR(?a), \"x\")) AS ?a2".into()],
            unless: vec![],
            then: vec![[
                v("a2"),
                Tok::Const(
                    NamedNode::new("https://example.org/s2r/uw#n")
                        .unwrap()
                        .into(),
                ),
                v("k"),
            ]],
        };
        let err = run(&mut g, vec![bad]).unwrap_err();
        assert!(err.to_string().contains("no fixpoint"));
    }
}
