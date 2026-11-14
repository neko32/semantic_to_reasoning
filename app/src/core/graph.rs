//! The knowledge graph: Silver facts projected to RDF in an in-memory
//! Oxigraph store, plus the ontology and the knowledge the trust policy allows.
//! Every asserted triple remembers where it came from.

use std::collections::HashMap;
use std::fmt;
use std::fmt::Write as _;

use anyhow::Context;
use oxigraph::io::RdfFormat;
use oxigraph::model::vocab::{rdf, xsd};
use oxigraph::model::{GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term, Triple};
use oxigraph::sparql::{QueryResults, QuerySolution, SparqlEvaluator};
use oxigraph::store::Store;

use super::domain::TrustPolicy;
use super::knowledge::{COND, CURATED_DRUGS_TTL, Catalog, DRUG, LlmCandidates, ONTOLOGY_TTL, UW};
use super::silver::{PartyType, RelationshipType, SilverData};

pub const BASE: &str = "https://example.org/s2r/";

/// Prefixes used by rules, queries and rendering.
pub const PREFIXES: [(&str, &str); 9] = [
    ("uw", UW),
    ("cond", COND),
    ("drug", DRUG),
    ("rc", "https://example.org/s2r/rate-class/"),
    ("ex", BASE),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("skos", "http://www.w3.org/2004/02/skos/core#"),
];

/// SPARQL prologue declaring [`PREFIXES`].
#[must_use]
pub fn sparql_prologue() -> String {
    let mut out = String::new();
    for (p, iri) in PREFIXES {
        let _ = writeln!(out, "PREFIX {p}: <{iri}>");
    }
    out.push_str("PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\n");
    out
}

/// Shortens an IRI with the known prefixes.
#[must_use]
pub fn compact(iri: &str) -> String {
    // Longest namespace first so `ex:` does not shadow the more specific ones.
    let mut best: Option<(&str, &str)> = None;
    for (p, ns) in PREFIXES {
        if iri.starts_with(ns) && best.is_none_or(|(_, b)| ns.len() > b.len()) {
            best = Some((p, ns));
        }
    }
    match best {
        Some((p, ns)) => format!("{p}:{}", &iri[ns.len()..]),
        None => format!("<{iri}>"),
    }
}

/// Renders a term compactly.
#[must_use]
pub fn show_term(term: &Term) -> String {
    match term {
        Term::NamedNode(n) => compact(n.as_str()),
        Term::Literal(l) => match l.datatype().as_str() {
            "http://www.w3.org/2001/XMLSchema#string" => format!("\"{}\"", l.value()),
            _ => l.value().to_owned(),
        },
        Term::BlankNode(b) => b.to_string(),
    }
}

/// Renders a triple compactly.
#[must_use]
pub fn show_triple(t: &Triple) -> String {
    format!(
        "{} {} {}",
        show_term(&t.subject.clone().into()),
        compact(t.predicate.as_str()),
        show_term(&t.object)
    )
}

/// Where an asserted triple came from.
#[derive(Debug, Clone, PartialEq)]
pub enum Origin {
    Ontology,
    Curated,
    Silver(&'static str),
    Llm { model: String, confidence: f64 },
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ontology => f.write_str("ontology"),
            Self::Curated => f.write_str("curated dictionary"),
            Self::Silver(table) => write!(f, "silver.{table}"),
            Self::Llm { model, confidence } => {
                write!(f, "LLM candidate ({model}, confidence {confidence:.2})")
            }
        }
    }
}

/// In-memory knowledge graph with per-triple provenance for asserted facts.
pub struct Graph {
    store: Store,
    origins: HashMap<Triple, Origin>,
    pub policy: TrustPolicy,
}

fn node(iri: impl Into<String>) -> NamedNode {
    NamedNode::new_unchecked(iri)
}

fn uw(local: &str) -> NamedNode {
    node(format!("{UW}{local}"))
}

fn res(kind: &str, id: &str) -> NamedNode {
    node(format!("{BASE}{kind}/{id}"))
}

fn party(id: &str) -> NamedNode {
    res("party", id)
}

/// IRI of a submission resource.
#[must_use]
pub fn submission_iri(id: &str) -> String {
    format!("{BASE}submission/{id}")
}

impl Graph {
    /// # Errors
    /// Returns an error when the store cannot be created.
    pub fn empty(policy: TrustPolicy) -> anyhow::Result<Self> {
        Ok(Self {
            store: Store::new()?,
            origins: HashMap::new(),
            policy,
        })
    }

    /// Builds the graph from Silver, the bundled ontology and dictionary, and
    /// the LLM candidates the policy accepts.
    ///
    /// # Errors
    /// Returns an error when the bundled Turtle is invalid or the store fails.
    pub fn build(
        silver: &SilverData,
        catalog: &Catalog,
        candidates: Option<&LlmCandidates>,
        policy: TrustPolicy,
    ) -> anyhow::Result<Self> {
        let mut g = Self::empty(policy)?;
        g.load_turtle(ONTOLOGY_TTL, &Origin::Ontology)?;
        g.load_turtle(CURATED_DRUGS_TTL, &Origin::Curated)?;
        g.add_silver(silver, catalog, candidates)?;
        if let Some(c) = candidates {
            for i in c.accepted_indications(policy) {
                let Some(cond) = &i.condition else { continue };
                g.assert(
                    node(format!("{DRUG}{}", i.drug)),
                    uw("treats"),
                    node(format!("{COND}{cond}")),
                    Origin::Llm {
                        model: c.model.clone(),
                        confidence: i.confidence.value(),
                    },
                )?;
            }
        }
        Ok(g)
    }

    fn load_turtle(&mut self, ttl: &str, origin: &Origin) -> anyhow::Result<()> {
        let tmp = Store::new()?;
        tmp.load_from_slice(RdfFormat::Turtle, ttl)
            .context("parsing bundled Turtle")?;
        for quad in &tmp {
            let quad = quad?;
            let triple = Triple::new(quad.subject, quad.predicate, quad.object);
            self.insert(triple, origin.clone())?;
        }
        Ok(())
    }

    fn insert(&mut self, triple: Triple, origin: Origin) -> anyhow::Result<bool> {
        let quad = Quad::new(
            triple.subject.clone(),
            triple.predicate.clone(),
            triple.object.clone(),
            GraphName::DefaultGraph,
        );
        if self.store.contains(&quad)? {
            return Ok(false);
        }
        self.store.insert(&quad)?;
        self.origins.insert(triple, origin);
        Ok(true)
    }

    fn assert(
        &mut self,
        s: NamedNode,
        p: NamedNode,
        o: impl Into<Term>,
        origin: Origin,
    ) -> anyhow::Result<()> {
        self.insert(Triple::new(s, p, o.into()), origin)?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn add_silver(
        &mut self,
        s: &SilverData,
        catalog: &Catalog,
        candidates: Option<&LlmCandidates>,
    ) -> anyhow::Result<()> {
        let str_lit = |v: &str| Literal::new_simple_literal(v);
        for p in &s.parties {
            let class = match p.party_type {
                PartyType::Person => "Person",
                PartyType::Trust => "Trust",
                PartyType::Company => "Company",
            };
            let id = party(p.party_id.as_str());
            self.assert(
                id.clone(),
                rdf::TYPE.into_owned(),
                uw(class),
                Origin::Silver("parties"),
            )?;
            self.assert(
                id,
                uw("displayName"),
                str_lit(&p.full_name),
                Origin::Silver("parties"),
            )?;
        }
        for r in &s.relationships {
            let pred = match r.relationship {
                RelationshipType::Spouse => "spouseOf",
                RelationshipType::ParentOf => "parentOf",
                RelationshipType::EmployerOf => "employs",
            };
            self.assert(
                party(r.from.as_str()),
                uw(pred),
                party(r.to.as_str()),
                Origin::Silver("relationships"),
            )?;
        }
        for t in &s.trusts {
            self.assert(
                party(t.trust_id.as_str()),
                uw("grantor"),
                party(t.grantor_id.as_str()),
                Origin::Silver("trusts"),
            )?;
        }
        for p in &s.products {
            let id = res("product", &p.product_code);
            self.assert(
                id.clone(),
                rdf::TYPE.into_owned(),
                uw("Product"),
                Origin::Silver("products"),
            )?;
            self.assert(
                id,
                uw("productType"),
                str_lit(&p.product_type),
                Origin::Silver("products"),
            )?;
        }
        for sub in &s.submissions {
            let id = res("submission", sub.submission_id.as_str());
            let o = Origin::Silver("submissions");
            self.assert(
                id.clone(),
                rdf::TYPE.into_owned(),
                uw("Submission"),
                o.clone(),
            )?;
            self.assert(
                id.clone(),
                uw("insured"),
                party(sub.insured.as_str()),
                o.clone(),
            )?;
            self.assert(
                id.clone(),
                uw("owner"),
                party(sub.owner.as_str()),
                o.clone(),
            )?;
            self.assert(
                id.clone(),
                uw("forProduct"),
                res("product", &sub.product_code),
                o.clone(),
            )?;
            self.assert(
                id.clone(),
                uw("channel"),
                str_lit(sub.channel.code()),
                o.clone(),
            )?;
            self.assert(
                id.clone(),
                uw("status"),
                str_lit(sub.status.code()),
                o.clone(),
            )?;
            self.assert(
                id.clone(),
                uw("faceAmount"),
                Literal::new_typed_literal(sub.face_amount.dollars().to_string(), xsd::INTEGER),
                o.clone(),
            )?;
            self.assert(
                id,
                uw("receivedOn"),
                Literal::new_typed_literal(sub.received_date.to_string(), xsd::DATE),
                o,
            )?;
        }
        for ill in &s.illustrations {
            let sid = ill.submission_id.as_str();
            let id = res("illustration", sid);
            let o = Origin::Silver("illustrations");
            self.assert(
                res("submission", sid),
                uw("hasIllustration"),
                id.clone(),
                o.clone(),
            )?;
            self.assert(
                id,
                uw("assumedRateClass"),
                node(format!(
                    "{BASE}rate-class/{}",
                    ill.assumed_rate_class.iri_local()
                )),
                o,
            )?;
        }
        for d in &s.disclosures {
            let pred = if d.answer {
                "disclosedYes"
            } else {
                "disclosedNo"
            };
            self.assert(
                res("submission", d.submission_id.as_str()),
                uw(pred),
                node(format!("{COND}{}", d.question.condition())),
                Origin::Silver("disclosures"),
            )?;
        }
        for c in &s.cotinine {
            let sid = c.submission_id.as_str();
            let lab = res("lab", &format!("{sid}-COTININE"));
            let o = Origin::Silver("lab_results");
            self.assert(
                res("submission", sid),
                uw("hasLabResult"),
                lab.clone(),
                o.clone(),
            )?;
            self.assert(lab.clone(), uw("labTest"), uw("Cotinine"), o.clone())?;
            self.assert(lab, uw("positive"), Literal::from(c.positive), o)?;
        }
        for r in &s.rx_fills {
            let rx = res("rx", r.rx_id.as_str());
            let o = Origin::Silver("rx_fills");
            self.assert(
                party(r.party_id.as_str()),
                uw("hasPrescription"),
                rx.clone(),
                o.clone(),
            )?;
            self.assert(rx.clone(), uw("drugText"), str_lit(&r.drug_text), o)?;
            if let Some(d) = catalog.match_text(&r.drug_text) {
                self.assert(
                    rx,
                    uw("prescribedDrug"),
                    node(format!("{DRUG}{}", d.id)),
                    Origin::Curated,
                )?;
            } else if let Some(m) =
                candidates.and_then(|c| c.accepted_mapping(&r.drug_text, self.policy))
                && let Some(drug) = &m.drug
            {
                self.assert(
                    rx,
                    uw("prescribedDrug"),
                    node(format!("{DRUG}{drug}")),
                    Origin::Llm {
                        model: candidates.map(|c| c.model.clone()).unwrap_or_default(),
                        confidence: m.confidence.value(),
                    },
                )?;
            }
        }
        Ok(())
    }

    /// Inserts an inferred triple. Returns `true` if it was new.
    ///
    /// # Errors
    /// Returns an error when the store fails.
    pub fn insert_inferred(&mut self, triple: &Triple) -> anyhow::Result<bool> {
        let quad = Quad::new(
            triple.subject.clone(),
            triple.predicate.clone(),
            triple.object.clone(),
            GraphName::DefaultGraph,
        );
        if self.store.contains(&quad)? {
            return Ok(false);
        }
        self.store.insert(&quad)?;
        Ok(true)
    }

    /// # Errors
    /// Returns an error when the store fails.
    pub fn contains(&self, triple: &Triple) -> anyhow::Result<bool> {
        Ok(self.store.contains(&Quad::new(
            triple.subject.clone(),
            triple.predicate.clone(),
            triple.object.clone(),
            GraphName::DefaultGraph,
        ))?)
    }

    #[must_use]
    pub fn origin(&self, triple: &Triple) -> Option<&Origin> {
        self.origins.get(triple)
    }

    #[must_use]
    pub fn asserted_count(&self) -> usize {
        self.origins.len()
    }

    /// Number of triples in the store (asserted + inferred).
    ///
    /// # Errors
    /// Returns an error when the store fails.
    pub fn len(&self) -> anyhow::Result<usize> {
        Ok(self.store.len()?)
    }

    /// # Errors
    /// Returns an error when the store fails.
    pub fn is_empty(&self) -> anyhow::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Runs a SELECT query (the standard prefixes are added automatically).
    ///
    /// # Errors
    /// Returns an error for a syntax error or a non-SELECT query.
    pub fn select(&self, query: &str) -> anyhow::Result<Vec<QuerySolution>> {
        let full = format!("{}{query}", sparql_prologue());
        match SparqlEvaluator::new()
            .parse_query(&full)
            .with_context(|| format!("parsing SPARQL:\n{full}"))?
            .on_store(&self.store)
            .execute()?
        {
            QueryResults::Solutions(s) => Ok(s.collect::<Result<Vec<_>, _>>()?),
            _ => anyhow::bail!("expected a SELECT query"),
        }
    }

    /// Objects of `subject predicate ?o`.
    ///
    /// # Errors
    /// Returns an error when the store fails.
    pub fn objects(
        &self,
        subject: &NamedOrBlankNode,
        predicate: &NamedNode,
    ) -> anyhow::Result<Vec<Term>> {
        self.store
            .quads_for_pattern(Some(subject.as_ref()), Some(predicate.as_ref()), None, None)
            .map(|q| Ok(q?.object))
            .collect()
    }

    /// Serialises every triple as N-Triples (asserted and inferred).
    ///
    /// # Errors
    /// Returns an error when the store fails.
    pub fn to_ntriples(&self) -> anyhow::Result<String> {
        let mut lines: Vec<String> = self
            .store
            .iter()
            .map(|q| q.map(|q| format!("{} {} {} .", q.subject, q.predicate, q.object)))
            .collect::<Result<_, _>>()?;
        lines.sort();
        Ok(lines.join("\n") + "\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::domain::Confidence;
    use crate::core::knowledge::{MockExtractor, extract};
    use crate::core::seed::generate;

    #[test]
    fn compact_prefers_the_longest_namespace() {
        assert_eq!(compact(&format!("{UW}insured")), "uw:insured");
        assert_eq!(compact(&format!("{BASE}party/P000001")), "ex:party/P000001");
        assert_eq!(compact(&format!("{COND}Asthma")), "cond:Asthma");
        assert_eq!(compact("urn:x"), "<urn:x>");
    }

    #[test]
    fn show_terms() {
        assert_eq!(show_term(&Literal::new_simple_literal("a").into()), "\"a\"");
        assert_eq!(show_term(&Literal::from(true).into()), "true");
        let t = Triple::new(uw("a"), uw("b"), uw("c"));
        assert_eq!(show_triple(&t), "uw:a uw:b uw:c");
        assert_eq!(Origin::Silver("x").to_string(), "silver.x");
        assert_eq!(Origin::Ontology.to_string(), "ontology");
        assert_eq!(Origin::Curated.to_string(), "curated dictionary");
        assert_eq!(
            Origin::Llm {
                model: "m".into(),
                confidence: 0.9
            }
            .to_string(),
            "LLM candidate (m, confidence 0.90)"
        );
    }

    #[test]
    fn build_tracks_origins_and_respects_trust() {
        let g = generate(42);
        let cat = Catalog::bundled().unwrap();
        let texts: Vec<String> = g
            .silver
            .rx_fills
            .iter()
            .map(|r| r.drug_text.clone())
            .collect();
        let cands = extract(&MockExtractor, &cat, &texts).unwrap();

        let curated =
            Graph::build(&g.silver, &cat, Some(&cands), TrustPolicy::CuratedOnly).unwrap();
        let llm_policy = TrustPolicy::CuratedPlusLlm {
            min_confidence: Confidence::new(0.8).unwrap(),
        };
        let with_llm = Graph::build(&g.silver, &cat, Some(&cands), llm_policy).unwrap();
        assert!(with_llm.len().unwrap() > curated.len().unwrap());
        assert!(!curated.is_empty().unwrap());
        assert_eq!(curated.asserted_count(), curated.len().unwrap());

        let treats = Triple::new(
            node(format!("{DRUG}apixaban")),
            uw("treats"),
            node(format!("{COND}AtrialFibrillation")),
        );
        assert!(!curated.contains(&treats).unwrap());
        assert!(with_llm.contains(&treats).unwrap());
        assert!(matches!(with_llm.origin(&treats), Some(Origin::Llm { .. })));

        let rows = curated
            .select("SELECT (COUNT(*) AS ?n) WHERE { ?s a uw:Submission }")
            .unwrap();
        assert_eq!(show_term(rows[0].get("n").unwrap()), "1000");
        assert!(curated.select("ASK { ?s ?p ?o }").is_err());
        assert!(curated.select("SELEC nonsense").is_err());

        let nt = curated.to_ntriples().unwrap();
        assert!(nt.contains("<https://example.org/s2r/uw#Person>"));

        let sub = NamedOrBlankNode::from(node(submission_iri("S000001")));
        assert_eq!(curated.objects(&sub, &uw("insured")).unwrap().len(), 1);
    }

    #[test]
    fn inferred_insert_is_idempotent() {
        let mut g = Graph::empty(TrustPolicy::CuratedOnly).unwrap();
        let t = Triple::new(uw("a"), uw("b"), uw("c"));
        assert!(g.insert_inferred(&t).unwrap());
        assert!(!g.insert_inferred(&t).unwrap());
        assert!(g.origin(&t).is_none());
    }
}
