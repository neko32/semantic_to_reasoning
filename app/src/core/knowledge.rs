//! Medical knowledge: the curated drug dictionary, the deterministic text
//! matcher, and LLM-extracted candidates with their trust metadata.
//!
//! The LLM never writes to the graph directly. It proposes candidates; this
//! module validates them against allow-lists (catalog drugs and ontology
//! conditions), and the [`TrustPolicy`] decides which ones the reasoner sees.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::Context;
use oxigraph::io::RdfFormat;
use oxigraph::model::Term;
use oxigraph::sparql::{QueryResults, SparqlEvaluator};
use oxigraph::store::Store;
use serde::{Deserialize, Serialize};

use super::domain::{Confidence, TrustPolicy};

pub const ONTOLOGY_TTL: &str = include_str!("../../ontology/uw.ttl");
pub const CURATED_DRUGS_TTL: &str = include_str!("../../knowledge/curated_drugs.ttl");

pub const UW: &str = "https://example.org/s2r/uw#";
pub const COND: &str = "https://example.org/s2r/condition/";
pub const DRUG: &str = "https://example.org/s2r/drug/";

/// One drug in the curated catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogDrug {
    /// Local name in the `drug:` namespace, e.g. `metformin`.
    pub id: String,
    /// Generic name plus curated brand names, lower case.
    pub names: Vec<String>,
    /// Curated indications (local names in `cond:`).
    pub treats: Vec<String>,
}

/// The curated dictionary plus the ontology's condition vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    pub drugs: Vec<CatalogDrug>,
    /// Condition local names declared in the ontology.
    pub conditions: BTreeSet<String>,
}

fn local(term: &Term, namespace: &str) -> Option<String> {
    match term {
        Term::NamedNode(n) => n.as_str().strip_prefix(namespace).map(str::to_owned),
        _ => None,
    }
}

fn literal(term: &Term) -> Option<String> {
    match term {
        Term::Literal(l) => Some(l.value().to_owned()),
        _ => None,
    }
}

fn select(store: &Store, query: &str) -> anyhow::Result<Vec<oxigraph::sparql::QuerySolution>> {
    match SparqlEvaluator::new()
        .parse_query(query)?
        .on_store(store)
        .execute()?
    {
        QueryResults::Solutions(solutions) => Ok(solutions.collect::<Result<Vec<_>, _>>()?),
        _ => anyhow::bail!("expected SELECT results"),
    }
}

impl Catalog {
    /// Loads the catalog from the bundled ontology and curated dictionary.
    ///
    /// # Errors
    /// Returns an error when the bundled Turtle is invalid.
    pub fn bundled() -> anyhow::Result<Self> {
        Self::from_turtle(ONTOLOGY_TTL, CURATED_DRUGS_TTL)
    }

    /// # Errors
    /// Returns an error when the Turtle cannot be parsed.
    pub fn from_turtle(ontology: &str, curated: &str) -> anyhow::Result<Self> {
        let store = Store::new()?;
        store.load_from_slice(RdfFormat::Turtle, ontology)?;
        store.load_from_slice(RdfFormat::Turtle, curated)?;

        let mut drugs: BTreeMap<String, CatalogDrug> = BTreeMap::new();
        for row in select(
            &store,
            &format!(
                "PREFIX uw: <{UW}> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
                 PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
                 SELECT ?d ?name ?cond WHERE {{
                   ?d a uw:Drug .
                   {{ ?d rdfs:label ?name }} UNION {{ ?d skos:altLabel ?name }}
                   OPTIONAL {{ ?d uw:treats ?cond }}
                 }}"
            ),
        )? {
            let Some(id) = row.get("d").and_then(|t| local(t, DRUG)) else {
                continue;
            };
            let entry = drugs.entry(id.clone()).or_insert_with(|| CatalogDrug {
                id,
                names: Vec::new(),
                treats: Vec::new(),
            });
            if let Some(name) = row.get("name").and_then(literal) {
                let name = name.to_lowercase();
                if !entry.names.contains(&name) {
                    entry.names.push(name);
                }
            }
            if let Some(cond) = row.get("cond").and_then(|t| local(t, COND))
                && !entry.treats.contains(&cond)
            {
                entry.treats.push(cond);
            }
        }
        for d in drugs.values_mut() {
            d.names.sort();
            d.treats.sort();
        }

        let conditions = select(
            &store,
            &format!("PREFIX uw: <{UW}> SELECT ?c WHERE {{ ?c a uw:Condition }}"),
        )?
        .iter()
        .filter_map(|row| row.get("c").and_then(|t| local(t, COND)))
        .collect();

        Ok(Self {
            drugs: drugs.into_values().collect(),
            conditions,
        })
    }

    #[must_use]
    pub fn drug(&self, id: &str) -> Option<&CatalogDrug> {
        self.drugs.iter().find(|d| d.id == id)
    }

    /// Deterministic matcher: a catalog name must appear as a whole word.
    #[must_use]
    pub fn match_text(&self, text: &str) -> Option<&CatalogDrug> {
        let words: Vec<String> = text
            .to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_owned)
            .collect();
        self.drugs
            .iter()
            .find(|d| d.names.iter().any(|n| words.iter().any(|w| w == n)))
    }

    /// Drugs with no curated indication; the LLM is asked about these.
    #[must_use]
    pub fn drugs_without_indication(&self) -> Vec<&CatalogDrug> {
        self.drugs.iter().filter(|d| d.treats.is_empty()).collect()
    }

    /// Generic name -> condition rows for Gold's reference table.
    #[must_use]
    pub fn reference_rows(&self) -> Vec<(String, String)> {
        self.drugs
            .iter()
            .flat_map(|d| d.treats.iter().map(|c| (d.id.clone(), c.clone())))
            .collect()
    }
}

/// LLM answer: which catalog drug a pharmacy text refers to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrugMapping {
    pub text: String,
    /// Catalog drug id, or `None` when the model is unsure.
    pub drug: Option<String>,
    pub confidence: Confidence,
}

/// LLM answer: which ontology condition a drug treats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Indication {
    pub drug: String,
    /// Condition local name, or `None` when no listed condition applies.
    pub condition: Option<String>,
    pub confidence: Confidence,
}

/// Port to a knowledge-extraction model (LM Studio in production).
pub trait KnowledgeExtractor {
    fn model_name(&self) -> String;

    /// # Errors
    /// Returns an error when the model cannot be reached or answers badly.
    fn map_drug_texts(
        &self,
        texts: &[String],
        catalog: &Catalog,
    ) -> anyhow::Result<Vec<DrugMapping>>;

    /// # Errors
    /// Returns an error when the model cannot be reached or answers badly.
    fn propose_indications(
        &self,
        drugs: &[&CatalogDrug],
        conditions: &BTreeSet<String>,
    ) -> anyhow::Result<Vec<Indication>>;
}

/// Validated LLM output, cached on disk so demos are repeatable.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LlmCandidates {
    pub model: String,
    pub mappings: Vec<DrugMapping>,
    pub indications: Vec<Indication>,
    /// Answers dropped by validation, with the reason. Kept for transparency.
    pub rejected: Vec<String>,
}

impl LlmCandidates {
    /// # Errors
    /// Returns an error when the file cannot be read or parsed.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    /// # Errors
    /// Returns an error when the file cannot be written.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    /// Mapping for a pharmacy text accepted under `policy`.
    #[must_use]
    pub fn accepted_mapping(&self, text: &str, policy: TrustPolicy) -> Option<&DrugMapping> {
        self.mappings
            .iter()
            .find(|m| m.text == text && m.drug.is_some() && policy.accepts_llm(m.confidence))
    }

    /// Indications accepted under `policy`.
    pub fn accepted_indications(&self, policy: TrustPolicy) -> impl Iterator<Item = &Indication> {
        self.indications
            .iter()
            .filter(move |i| i.condition.is_some() && policy.accepts_llm(i.confidence))
    }
}

/// Runs the extractor on everything the curated dictionary cannot answer and
/// validates the output against the allow-lists.
///
/// # Errors
/// Returns an error when the extractor fails.
pub fn extract(
    extractor: &dyn KnowledgeExtractor,
    catalog: &Catalog,
    rx_texts: &[String],
) -> anyhow::Result<LlmCandidates> {
    let unmatched: Vec<String> = rx_texts
        .iter()
        .filter(|t| catalog.match_text(t).is_none())
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut out = LlmCandidates {
        model: extractor.model_name(),
        ..LlmCandidates::default()
    };

    if !unmatched.is_empty() {
        for m in extractor.map_drug_texts(&unmatched, catalog)? {
            match &m.drug {
                Some(d) if catalog.drug(d).is_none() => out
                    .rejected
                    .push(format!("mapping '{}' -> unknown drug '{d}'", m.text)),
                _ if !unmatched.contains(&m.text) => out
                    .rejected
                    .push(format!("mapping for unrequested text '{}'", m.text)),
                _ => out.mappings.push(m),
            }
        }
    }

    // Ask about uncurated drugs, plus any drug the mappings pointed to.
    let mut ask: Vec<&CatalogDrug> = catalog.drugs_without_indication();
    ask.sort_by(|a, b| a.id.cmp(&b.id));
    if !ask.is_empty() {
        let asked: BTreeSet<&str> = ask.iter().map(|d| d.id.as_str()).collect();
        for i in extractor.propose_indications(&ask, &catalog.conditions)? {
            match &i.condition {
                _ if !asked.contains(i.drug.as_str()) => out
                    .rejected
                    .push(format!("indication for unrequested drug '{}'", i.drug)),
                Some(c) if !catalog.conditions.contains(c) => out.rejected.push(format!(
                    "indication '{}' -> unknown condition '{c}'",
                    i.drug
                )),
                _ => out.indications.push(i),
            }
        }
    }
    Ok(out)
}

/// Deterministic stand-in for the LLM. Used by tests, CI and `--extractor mock`.
/// Its answers mirror what a capable model is expected to say, including one
/// low-confidence guess that the default threshold rejects.
#[derive(Debug, Default, Clone, Copy)]
pub struct MockExtractor;

impl KnowledgeExtractor for MockExtractor {
    fn model_name(&self) -> String {
        "mock-extractor".to_owned()
    }

    fn map_drug_texts(
        &self,
        texts: &[String],
        _catalog: &Catalog,
    ) -> anyhow::Result<Vec<DrugMapping>> {
        let c = |v| Confidence::new(v).expect("valid");
        Ok(texts
            .iter()
            .map(|t| {
                let lower = t.to_lowercase();
                let (drug, confidence) = if lower.contains("jardiance") {
                    (Some("empagliflozin"), 0.97)
                } else if lower.contains("eliquis") {
                    (Some("apixaban"), 0.96)
                } else if lower.contains("vitamin d") {
                    (Some("cholecalciferol"), 0.92)
                } else {
                    (None, 0.0)
                };
                DrugMapping {
                    text: t.clone(),
                    drug: drug.map(str::to_owned),
                    confidence: c(confidence),
                }
            })
            .collect())
    }

    fn propose_indications(
        &self,
        drugs: &[&CatalogDrug],
        _conditions: &BTreeSet<String>,
    ) -> anyhow::Result<Vec<Indication>> {
        let c = |v| Confidence::new(v).expect("valid");
        Ok(drugs
            .iter()
            .map(|d| {
                let (condition, confidence) = match d.id.as_str() {
                    "empagliflozin" => (Some("Type2Diabetes"), 0.95),
                    "apixaban" => (Some("AtrialFibrillation"), 0.94),
                    "cetirizine" => (Some("Asthma"), 0.35),
                    _ => (None, 0.0),
                };
                Indication {
                    drug: d.id.clone(),
                    condition: condition.map(str::to_owned),
                    confidence: c(confidence),
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conf(v: f64) -> Confidence {
        Confidence::new(v).unwrap()
    }

    #[test]
    fn bundled_catalog_loads() {
        let cat = Catalog::bundled().unwrap();
        let metformin = cat.drug("metformin").unwrap();
        assert_eq!(metformin.names, vec!["glucophage", "metformin"]);
        assert_eq!(metformin.treats, vec!["DiabetesMellitus"]);
        assert!(cat.conditions.contains("Type2Diabetes"));
        assert!(cat.conditions.contains("CardiometabolicCondition"));
        let uncurated: Vec<&str> = cat
            .drugs_without_indication()
            .iter()
            .map(|d| d.id.as_str())
            .collect();
        assert_eq!(
            uncurated,
            vec!["apixaban", "cetirizine", "cholecalciferol", "empagliflozin"]
        );
        assert!(
            cat.reference_rows()
                .contains(&("clopidogrel".to_owned(), "CoronaryArteryDisease".to_owned()))
        );
    }

    #[test]
    fn matcher_uses_whole_words() {
        let cat = Catalog::bundled().unwrap();
        assert_eq!(
            cat.match_text("METFORMIN HCL 500 MG TAB").unwrap().id,
            "metformin"
        );
        assert_eq!(
            cat.match_text("Glucophage XR 500mg").unwrap().id,
            "metformin"
        );
        assert_eq!(cat.match_text("Plavix 75mg").unwrap().id, "clopidogrel");
        assert_eq!(
            cat.match_text("empagliflozin 25mg").unwrap().id,
            "empagliflozin"
        );
        assert!(cat.match_text("Jardiance 10 mg tablet").is_none());
        assert!(cat.match_text("ELIQUIS 5MG").is_none());
        assert!(cat.match_text("metforminx").is_none());
    }

    #[test]
    fn invalid_turtle_is_an_error() {
        assert!(Catalog::from_turtle("not turtle", "").is_err());
    }

    #[test]
    fn extraction_with_mock_validates_and_filters() {
        let cat = Catalog::bundled().unwrap();
        let texts = vec![
            "METFORMIN HCL 500 MG TAB".to_owned(),
            "Jardiance 10 mg tablet".to_owned(),
            "ELIQUIS 5MG".to_owned(),
            "Vitamin D3 2000 IU".to_owned(),
            "Jardiance 10 mg tablet".to_owned(),
        ];
        let out = extract(&MockExtractor, &cat, &texts).unwrap();
        assert_eq!(out.model, "mock-extractor");
        // Only the three unmatched distinct texts are sent.
        assert_eq!(out.mappings.len(), 3);
        assert!(out.rejected.is_empty());

        let curated = TrustPolicy::CuratedOnly;
        let llm = TrustPolicy::CuratedPlusLlm {
            min_confidence: conf(0.8),
        };
        assert!(out.accepted_mapping("ELIQUIS 5MG", curated).is_none());
        assert_eq!(
            out.accepted_mapping("ELIQUIS 5MG", llm)
                .unwrap()
                .drug
                .as_deref(),
            Some("apixaban")
        );
        let accepted: Vec<&str> = out
            .accepted_indications(llm)
            .map(|i| i.drug.as_str())
            .collect();
        assert_eq!(accepted, vec!["apixaban", "empagliflozin"]);
        let permissive = TrustPolicy::CuratedPlusLlm {
            min_confidence: conf(0.3),
        };
        assert_eq!(out.accepted_indications(permissive).count(), 3);
    }

    struct Rogue;
    impl KnowledgeExtractor for Rogue {
        fn model_name(&self) -> String {
            "rogue".into()
        }
        fn map_drug_texts(&self, _: &[String], _: &Catalog) -> anyhow::Result<Vec<DrugMapping>> {
            Ok(vec![
                DrugMapping {
                    text: "ELIQUIS 5MG".into(),
                    drug: Some("unobtainium".into()),
                    confidence: conf(0.99),
                },
                DrugMapping {
                    text: "never asked".into(),
                    drug: None,
                    confidence: conf(0.1),
                },
            ])
        }
        fn propose_indications(
            &self,
            _: &[&CatalogDrug],
            _: &BTreeSet<String>,
        ) -> anyhow::Result<Vec<Indication>> {
            Ok(vec![
                Indication {
                    drug: "apixaban".into(),
                    condition: Some("Vampirism".into()),
                    confidence: conf(0.99),
                },
                Indication {
                    drug: "metformin".into(),
                    condition: Some("Asthma".into()),
                    confidence: conf(0.99),
                },
            ])
        }
    }

    #[test]
    fn extraction_rejects_answers_outside_the_allow_lists() {
        let cat = Catalog::bundled().unwrap();
        let out = extract(&Rogue, &cat, &["ELIQUIS 5MG".to_owned()]).unwrap();
        assert!(out.mappings.is_empty());
        assert!(out.indications.is_empty());
        assert_eq!(out.rejected.len(), 4, "{:?}", out.rejected);
    }

    #[test]
    fn candidates_round_trip_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/cands.json");
        let cat = Catalog::bundled().unwrap();
        let out = extract(&MockExtractor, &cat, &["ELIQUIS 5MG".to_owned()]).unwrap();
        out.save(&path).unwrap();
        assert_eq!(LlmCandidates::load(&path).unwrap(), out);
        assert!(LlmCandidates::load(&dir.path().join("none.json")).is_err());
    }
}
