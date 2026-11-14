//! Deterministic synthetic data generator.
//!
//! Produces a clean [`SilverData`] (used directly by unit tests), messy Bronze
//! CSV files (input to dbt), and a [`GroundTruth`] file that records which
//! scenarios were planted. The expected answers in the ground truth come from
//! a small hand-written oracle in this module, not from the reasoner, so tests
//! compare two independent implementations.
//!
//! Everything is fictional: names, companies, products and amounts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::Context;
use chrono::{Days, NaiveDate};
use rand::seq::SliceRandom;
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

use super::domain::{
    Channel, FaceAmount, PartyId, Question, RateClass, RxId, SubmissionId, SubmissionStatus,
};
use super::silver::{
    CotinineResult, Disclosure, Illustration, Party, PartyType, Product, Relationship,
    RelationshipType, RxFill, SilverData, Submission, TrustRecord,
};

/// Number of submissions generated.
pub const SUBMISSIONS: usize = 1000;

/// Ownership scenarios (drive Q2) and how many submissions get each.
pub const OWNERSHIP_SCENARIOS: [(&str, usize); 11] = [
    ("SPOUSE", 40),
    ("PARENT", 20),
    ("CHILD_OWNS", 12),
    ("GRANDPARENT", 8),
    ("TRUST_SELF", 10),
    ("TRUST_SPOUSE", 6),
    ("KEY_PERSON", 18),
    ("STOLI_INVESTOR", 5),
    ("STOLI_STRANGER", 4),
    ("STOLI_TRUST", 3),
    ("SELF", 0), // fills the rest
];

/// Health scenarios (drive Q3 and Q4) and how many submissions get each.
pub const HEALTH_SCENARIOS: [(&str, usize); 10] = [
    ("ND_GENERIC", 8),
    ("ND_BRAND", 6),
    ("ND_HIERARCHY", 6),
    ("ND_LLM", 7),
    ("HONEST_DISCLOSED", 25),
    ("CARDIO_NO_QUESTION", 8),
    ("NOISE_RX", 30),
    ("SMOKER_HONEST", 12),
    ("SMOKER_HIDDEN", 7),
    ("HEALTHY", 0), // fills the rest
];

/// A prescription text as it appears in the pharmacy feed, plus the oracle's
/// medical knowledge about it.
#[derive(Debug, Clone, Copy)]
pub struct RxTemplate {
    pub text: &'static str,
    /// Condition (local name in `cond:`) the drug implies, if any.
    pub condition: Option<&'static str>,
    /// True when only the LLM candidates link this text to the condition.
    pub needs_llm: bool,
}

const fn rx(text: &'static str, condition: Option<&'static str>, needs_llm: bool) -> RxTemplate {
    RxTemplate {
        text,
        condition,
        needs_llm,
    }
}

const RX_GENERIC: [RxTemplate; 3] = [
    rx("METFORMIN HCL 500 MG TAB", Some("DiabetesMellitus"), false),
    rx("metformin er 750mg", Some("DiabetesMellitus"), false),
    rx("LISINOPRIL 20 MG", Some("Hypertension"), false),
];
const RX_BRAND: [RxTemplate; 2] = [
    rx("Glucophage XR 500mg", Some("DiabetesMellitus"), false),
    rx("Zestril 10mg tablet", Some("Hypertension"), false),
];
const RX_HIERARCHY: [RxTemplate; 2] = [
    rx(
        "clopidogrel bisulfate 75 mg",
        Some("CoronaryArteryDisease"),
        false,
    ),
    rx("Plavix 75mg", Some("CoronaryArteryDisease"), false),
];
const RX_LLM: [RxTemplate; 3] = [
    rx("Jardiance 10 mg tablet", Some("Type2Diabetes"), true),
    rx("empagliflozin 25mg", Some("Type2Diabetes"), true),
    rx("ELIQUIS 5MG", Some("AtrialFibrillation"), true),
];
const RX_CARDIO_NO_QUESTION: [RxTemplate; 1] =
    [rx("atorvastatin 40mg", Some("Hyperlipidemia"), false)];
const RX_NOISE: [RxTemplate; 5] = [
    rx("Vitamin D3 2000 IU", None, false),
    rx("cetirizine 10mg", None, false),
    rx("sertraline 50mg", Some("Depression"), false),
    rx("albuterol HFA inhaler", Some("Asthma"), false),
    rx("levothyroxine 50mcg", Some("Hypothyroidism"), false),
];

/// The oracle's view of a condition: which application question covers it
/// and whether it is cardiometabolic.
#[must_use]
pub fn oracle_condition(condition: &str) -> (Option<Question>, bool) {
    match condition {
        "DiabetesMellitus" | "Type2Diabetes" => (Some(Question::Diabetes), true),
        "Hypertension" => (Some(Question::Hypertension), true),
        "CoronaryArteryDisease" | "AtrialFibrillation" | "HeartDisease" => {
            (Some(Question::Heart), true)
        }
        "Hyperlipidemia" => (None, true),
        _ => (None, false),
    }
}

/// Expected answer sets, per trust level.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expected {
    pub curated: BTreeSet<String>,
    pub curated_plus_llm: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroundTruth {
    pub seed: u64,
    /// Submission id -> `[ownership scenario, health scenario]`.
    pub scenarios: BTreeMap<String, [String; 2]>,
    pub q2_insurable_interest_not_established: BTreeSet<String>,
    pub q3_potential_non_disclosure: Expected,
    pub q4_requires_reillustration: Expected,
}

impl GroundTruth {
    /// # Errors
    /// Returns an error when the file cannot be read or parsed.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("reading ground truth {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    #[must_use]
    pub fn label(&self, submission_id: &str) -> Option<&[String; 2]> {
        self.scenarios.get(submission_id)
    }
}

/// Health profile planted on one submission.
#[derive(Debug, Clone)]
struct Health {
    label: &'static str,
    rx: Vec<RxTemplate>,
    yes: Vec<Question>,
    cotinine_positive: bool,
    illustration: RateClass,
}

/// Output of the generator.
#[derive(Debug, Clone)]
pub struct Generated {
    pub silver: SilverData,
    pub truth: GroundTruth,
}

struct Gen {
    rng: ChaCha8Rng,
    silver: SilverData,
    next_party: u32,
    next_rx: u32,
}

const GIVEN: [&str; 24] = [
    "Aiko", "Ben", "Chloe", "Daichi", "Elena", "Felix", "Grace", "Haruto", "Ines", "Jonas", "Kana",
    "Liam", "Mei", "Noah", "Olivia", "Priya", "Quinn", "Ren", "Sofia", "Taro", "Uma", "Victor",
    "Wren", "Yuki",
];
const FAMILY: [&str; 20] = [
    "Abernathy",
    "Brightwater",
    "Castellan",
    "Dunmore",
    "Ellery",
    "Fairbourne",
    "Glenhaven",
    "Hollowell",
    "Ishimori",
    "Juniper",
    "Kestrel",
    "Larkspur",
    "Moriyama",
    "Northcott",
    "Oakhurst",
    "Pemberly",
    "Quillfeather",
    "Rosewood",
    "Sakuragi",
    "Thistledown",
];
const EMPLOYERS: [&str; 6] = [
    "Copperleaf Robotics Inc.",
    "Bluefin Logistics Co.",
    "Maple & Pine Architects LLP",
    "Starling Biotech Ltd.",
    "Harbor Light Foods Inc.",
    "Tanuki Systems KK",
];
const INVESTORS: [&str; 3] = [
    "Silverbrook Life Settlement Partners LLC",
    "Ironvale Secondary Capital Fund",
    "Marigold Policy Acquisition Trust Co.",
];

impl Gen {
    fn new(seed: u64) -> Self {
        Self {
            rng: ChaCha8Rng::seed_from_u64(seed),
            silver: SilverData::default(),
            next_party: 1,
            next_rx: 1,
        }
    }

    fn party_id(&mut self, prefix: char) -> PartyId {
        let id = PartyId::new(format!("{prefix}{:06}", self.next_party)).expect("valid id");
        self.next_party += 1;
        id
    }

    fn person(&mut self, birth_year: (i32, i32)) -> PartyId {
        let id = self.party_id('P');
        let given = GIVEN[self.rng.random_range(0..GIVEN.len())];
        let family = FAMILY[self.rng.random_range(0..FAMILY.len())];
        let year = self.rng.random_range(birth_year.0..=birth_year.1);
        let dob = NaiveDate::from_ymd_opt(year, 1, 1)
            .expect("valid date")
            .checked_add_days(Days::new(self.rng.random_range(0..365)))
            .expect("valid date");
        self.silver.parties.push(Party {
            party_id: id.clone(),
            party_type: PartyType::Person,
            full_name: format!("{given} {family}"),
            date_of_birth: Some(dob),
        });
        id
    }

    fn organisation(&mut self, prefix: char, party_type: PartyType, name: String) -> PartyId {
        let id = self.party_id(prefix);
        self.silver.parties.push(Party {
            party_id: id.clone(),
            party_type,
            full_name: name,
            date_of_birth: None,
        });
        id
    }

    fn trust(&mut self, grantor: &PartyId) -> PartyId {
        let name = format!("{} Family Irrevocable Trust", self.surname_of(grantor));
        let id = self.organisation('T', PartyType::Trust, name);
        self.silver.trusts.push(TrustRecord {
            trust_id: id.clone(),
            grantor_id: grantor.clone(),
        });
        id
    }

    fn surname_of(&self, id: &PartyId) -> String {
        self.silver
            .parties
            .iter()
            .find(|p| &p.party_id == id)
            .and_then(|p| p.full_name.split(' ').next_back())
            .unwrap_or("Unknown")
            .to_owned()
    }

    fn relate(&mut self, from: &PartyId, to: &PartyId, relationship: RelationshipType) {
        self.silver.relationships.push(Relationship {
            from: from.clone(),
            to: to.clone(),
            relationship,
        });
    }

    fn spouse_of(&mut self, a: &PartyId) -> PartyId {
        let b = self.person((1955, 1995));
        // Source systems store the pair once, in either direction.
        if self.rng.random_bool(0.5) {
            self.relate(a, &b, RelationshipType::Spouse);
        } else {
            self.relate(&b, a, RelationshipType::Spouse);
        }
        b
    }

    /// Returns `(insured, owner)` for an ownership scenario.
    fn ownership(&mut self, label: &str) -> (PartyId, PartyId) {
        match label {
            "SPOUSE" => {
                let insured = self.person((1955, 1995));
                let owner = self.spouse_of(&insured);
                (insured, owner)
            }
            "PARENT" => {
                let parent = self.person((1960, 1985));
                let child = self.person((2000, 2015));
                self.relate(&parent, &child, RelationshipType::ParentOf);
                (child, parent)
            }
            "CHILD_OWNS" => {
                let parent = self.person((1940, 1960));
                let child = self.person((1970, 1990));
                self.relate(&parent, &child, RelationshipType::ParentOf);
                (parent, child)
            }
            "GRANDPARENT" => {
                let grandparent = self.person((1940, 1955));
                let parent = self.person((1965, 1980));
                let child = self.person((2000, 2015));
                self.relate(&grandparent, &parent, RelationshipType::ParentOf);
                self.relate(&parent, &child, RelationshipType::ParentOf);
                (child, grandparent)
            }
            "TRUST_SELF" => {
                let insured = self.person((1950, 1980));
                let trust = self.trust(&insured);
                (insured, trust)
            }
            "TRUST_SPOUSE" => {
                let insured = self.person((1950, 1980));
                let spouse = self.spouse_of(&insured);
                let trust = self.trust(&spouse);
                (insured, trust)
            }
            "KEY_PERSON" => {
                let insured = self.person((1960, 1990));
                let name = EMPLOYERS[self.rng.random_range(0..EMPLOYERS.len())].to_owned();
                let company = self.organisation('C', PartyType::Company, name);
                self.relate(&company, &insured, RelationshipType::EmployerOf);
                (insured, company)
            }
            "STOLI_INVESTOR" => {
                let insured = self.person((1940, 1955));
                let name = INVESTORS[self.rng.random_range(0..INVESTORS.len())].to_owned();
                let investor = self.organisation('C', PartyType::Company, name);
                (insured, investor)
            }
            "STOLI_STRANGER" => {
                let insured = self.person((1940, 1955));
                let stranger = self.person((1960, 1990));
                (insured, stranger)
            }
            "STOLI_TRUST" => {
                let insured = self.person((1940, 1955));
                let stranger = self.person((1960, 1990));
                let trust = self.trust(&stranger);
                (insured, trust)
            }
            _ => {
                let insured = self.person((1950, 2000));
                (insured.clone(), insured)
            }
        }
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.rng.random_range(0..items.len())]
    }

    fn healthy_class(&mut self) -> RateClass {
        self.pick(&[
            RateClass::PreferredPlusNt,
            RateClass::PreferredNt,
            RateClass::PreferredNt,
            RateClass::StandardNt,
        ])
    }

    fn health(&mut self, label: &'static str) -> Health {
        let mut h = Health {
            label,
            rx: Vec::new(),
            yes: Vec::new(),
            cotinine_positive: false,
            illustration: self.healthy_class(),
        };
        match label {
            "ND_GENERIC" => h.rx.push(self.pick(&RX_GENERIC)),
            "ND_BRAND" => h.rx.push(self.pick(&RX_BRAND)),
            "ND_HIERARCHY" => h.rx.push(self.pick(&RX_HIERARCHY)),
            "ND_LLM" => h.rx.push(self.pick(&RX_LLM)),
            "HONEST_DISCLOSED" => {
                let all: Vec<RxTemplate> = RX_GENERIC
                    .iter()
                    .chain(&RX_BRAND)
                    .chain(&RX_HIERARCHY)
                    .chain(&RX_LLM)
                    .copied()
                    .collect();
                let t = self.pick(&all);
                h.rx.push(t);
                if let (Some(q), _) = oracle_condition(t.condition.unwrap_or_default()) {
                    h.yes.push(q);
                }
            }
            "CARDIO_NO_QUESTION" => h.rx.push(self.pick(&RX_CARDIO_NO_QUESTION)),
            "NOISE_RX" => h.rx.push(self.pick(&RX_NOISE)),
            "SMOKER_HONEST" => {
                h.cotinine_positive = true;
                h.illustration =
                    self.pick(&[RateClass::PreferredTobacco, RateClass::StandardTobacco]);
            }
            "SMOKER_HIDDEN" => {
                h.cotinine_positive = true;
                h.illustration = self.pick(&[RateClass::PreferredPlusNt, RateClass::PreferredNt]);
            }
            _ => {}
        }
        h
    }

    fn product(&mut self) -> (&'static str, u64) {
        let roll = self.rng.random_range(0..100);
        let (code, (lo, hi, step)) = match roll {
            0..30 => ("TERM20", (5, 60, 50_000)),
            30..50 => ("TERM30", (5, 60, 50_000)),
            50..65 => ("UL_GUARD", (5, 50, 100_000)),
            65..85 => ("IUL_ASCENT", (5, 50, 100_000)),
            _ => ("WL_HERITAGE", (4, 40, 25_000)),
        };
        (code, self.rng.random_range(lo..=hi) * step)
    }

    fn channel(&mut self) -> Channel {
        match self.rng.random_range(0..100) {
            0..40 => Channel::Brokerage,
            40..65 => Channel::CareerAgent,
            65..85 => Channel::Direct,
            _ => Channel::Bank,
        }
    }

    fn status(&mut self, channel: Channel, received: NaiveDate) -> SubmissionStatus {
        let as_of = NaiveDate::from_ymd_opt(2026, 11, 1).expect("valid date");
        if (as_of - received).num_days() < 30 {
            return SubmissionStatus::InUnderwriting;
        }
        let placed = match channel {
            Channel::Brokerage => 0.58,
            Channel::CareerAgent => 0.72,
            Channel::Direct => 0.41,
            Channel::Bank => 0.50,
        };
        if self.rng.random_bool(placed) {
            return SubmissionStatus::Placed;
        }
        match self.rng.random_range(0..100) {
            0..35 => SubmissionStatus::Declined,
            35..80 => SubmissionStatus::Withdrawn,
            _ => SubmissionStatus::Approved,
        }
    }
}

fn expand(scenarios: &[(&'static str, usize)], total: usize) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = scenarios
        .iter()
        .flat_map(|(label, n)| std::iter::repeat_n(*label, *n))
        .collect();
    let filler = scenarios.last().expect("non-empty").0;
    out.resize(total, filler);
    out
}

fn products() -> Vec<Product> {
    [
        ("TERM20", "Evergreen Term 20", "TERM"),
        ("TERM30", "Evergreen Term 30", "TERM"),
        ("UL_GUARD", "Guardian Universal Life", "UL"),
        ("IUL_ASCENT", "Ascent Indexed UL", "IUL"),
        ("WL_HERITAGE", "Heritage Whole Life", "WL"),
    ]
    .into_iter()
    .map(|(code, name, kind)| Product {
        product_code: code.to_owned(),
        product_name: name.to_owned(),
        product_type: kind.to_owned(),
    })
    .collect()
}

/// The oracle: computes the expected Q3/Q4 membership from a health profile.
fn oracle(h: &Health, with_llm: bool) -> (bool, bool) {
    let evidence: Vec<(Option<Question>, bool)> =
        h.rx.iter()
            .filter(|t| with_llm || !t.needs_llm)
            .filter_map(|t| t.condition)
            .map(oracle_condition)
            .collect();
    let non_disclosure = evidence
        .iter()
        .any(|(q, _)| q.is_some_and(|q| !h.yes.contains(&q)));
    let preferred_ineligible = !h.yes.is_empty() || evidence.iter().any(|(_, cardio)| *cardio);
    let reillustrate = (h.illustration.is_preferred() && preferred_ineligible)
        || (!h.illustration.is_tobacco() && h.cotinine_positive);
    (non_disclosure, reillustrate)
}

/// Generates the full synthetic dataset.
///
/// # Panics
/// Never in practice: ids and dates are built from constants known to be valid.
#[must_use]
pub fn generate(seed: u64) -> Generated {
    let mut g = Gen::new(seed);
    g.silver.products = products();

    let mut ownership = expand(&OWNERSHIP_SCENARIOS, SUBMISSIONS);
    let mut health = expand(&HEALTH_SCENARIOS, SUBMISSIONS);
    ownership.shuffle(&mut g.rng);
    health.shuffle(&mut g.rng);

    let mut truth = GroundTruth {
        seed,
        ..GroundTruth::default()
    };
    let start = NaiveDate::from_ymd_opt(2025, 10, 1).expect("valid date");

    for (i, (own_label, health_label)) in ownership.into_iter().zip(health).enumerate() {
        let sid = SubmissionId::new(format!("S{:06}", i + 1)).expect("valid id");
        let (insured, owner) = g.ownership(own_label);
        let h = g.health(health_label);
        let received = start + Days::new(g.rng.random_range(0..396));
        let channel = g.channel();
        let status = g.status(channel, received);
        let (product, face) = g.product();

        g.silver.submissions.push(Submission {
            submission_id: sid.clone(),
            received_date: received,
            channel,
            product_code: product.to_owned(),
            insured: insured.clone(),
            owner,
            face_amount: FaceAmount::new(face).expect("positive"),
            status,
        });
        g.silver.illustrations.push(Illustration {
            submission_id: sid.clone(),
            assumed_rate_class: h.illustration,
        });
        for q in Question::ALL {
            g.silver.disclosures.push(Disclosure {
                submission_id: sid.clone(),
                question: q,
                answer: h.yes.contains(&q),
            });
        }
        g.silver.cotinine.push(CotinineResult {
            submission_id: sid.clone(),
            positive: h.cotinine_positive,
        });
        for t in &h.rx {
            let rx_id = RxId::new(format!("R{:06}", g.next_rx)).expect("valid id");
            g.next_rx += 1;
            g.silver.rx_fills.push(RxFill {
                rx_id,
                party_id: insured.clone(),
                drug_text: t.text.to_owned(),
            });
        }

        let key = sid.to_string();
        if own_label.starts_with("STOLI") {
            truth
                .q2_insurable_interest_not_established
                .insert(key.clone());
        }
        for (with_llm, q3, q4) in [
            (
                false,
                &mut truth.q3_potential_non_disclosure.curated,
                &mut truth.q4_requires_reillustration.curated,
            ),
            (
                true,
                &mut truth.q3_potential_non_disclosure.curated_plus_llm,
                &mut truth.q4_requires_reillustration.curated_plus_llm,
            ),
        ] {
            let (nd, reill) = oracle(&h, with_llm);
            if nd {
                q3.insert(key.clone());
            }
            if reill {
                q4.insert(key.clone());
            }
        }
        truth
            .scenarios
            .insert(key, [own_label.to_owned(), h.label.to_owned()]);
    }

    // Unrelated married couples: noise in the relationship table.
    for _ in 0..60 {
        let a = g.person((1950, 1995));
        g.spouse_of(&a);
    }

    Generated {
        silver: g.silver,
        truth,
    }
}

/// Reference rows that Gold gets as dbt sources (from the curated dictionary).
#[derive(Debug, Clone, Default)]
pub struct ReferenceData {
    /// `(generic name, condition local name)`
    pub drug_condition: Vec<(String, String)>,
}

/// Writes messy Bronze CSV files, reference CSVs and the ground truth.
///
/// # Errors
/// Returns an error when a file cannot be written.
#[allow(clippy::too_many_lines)] // one block per CSV file
pub fn write_bronze(
    generated: &Generated,
    reference: &ReferenceData,
    dir: &Path,
    seed: u64,
) -> anyhow::Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut mess = Messer {
        rng: ChaCha8Rng::seed_from_u64(seed ^ 0x5eed_b0b0),
    };
    let s = &generated.silver;

    let mut w = writer(dir, "parties.csv")?;
    w.write_record(["party_id", "party_type", "full_name", "date_of_birth"])?;
    for p in &s.parties {
        let dob = p.date_of_birth.map(|d| mess.date(d)).unwrap_or_default();
        w.write_record([
            p.party_id.as_str(),
            &mess.case(p.party_type.code()),
            &mess.name(&p.full_name),
            &dob,
        ])?;
    }
    w.flush()?;

    let mut w = writer(dir, "relationships.csv")?;
    w.write_record(["from_party_id", "to_party_id", "relationship"])?;
    for r in &s.relationships {
        let code = match r.relationship {
            RelationshipType::Spouse => mess.pick(&["SPOUSE", "Spouse", "spouse"]),
            RelationshipType::ParentOf => mess.pick(&["PARENT_OF", "Parent Of", "parent_of"]),
            RelationshipType::EmployerOf => mess.pick(&["EMPLOYER_OF", "Employer Of"]),
        };
        w.write_record([r.from.as_str(), r.to.as_str(), code])?;
    }
    w.flush()?;

    let mut w = writer(dir, "trusts.csv")?;
    w.write_record(["trust_party_id", "grantor_party_id"])?;
    for t in &s.trusts {
        w.write_record([t.trust_id.as_str(), t.grantor_id.as_str()])?;
    }
    w.flush()?;

    let mut w = writer(dir, "products.csv")?;
    w.write_record(["product_code", "product_name", "product_type"])?;
    for p in &s.products {
        w.write_record([&p.product_code, &p.product_name, &p.product_type])?;
    }
    w.flush()?;

    let mut w = writer(dir, "submissions.csv")?;
    w.write_record([
        "submission_id",
        "received_date",
        "channel",
        "product_code",
        "insured_party_id",
        "owner_party_id",
        "face_amount",
        "status",
    ])?;
    for (i, sub) in s.submissions.iter().enumerate() {
        let record = [
            sub.submission_id.to_string(),
            mess.date(sub.received_date),
            mess.channel(sub.channel).to_owned(),
            sub.product_code.clone(),
            sub.insured.to_string(),
            sub.owner.to_string(),
            mess.money(sub.face_amount.dollars()),
            mess.status(sub.status).to_owned(),
        ];
        w.write_record(&record)?;
        // A few exact duplicates, as re-sent by an upstream system.
        if i % 200 == 7 {
            w.write_record(&record)?;
        }
    }
    w.flush()?;

    let mut w = writer(dir, "illustrations.csv")?;
    w.write_record([
        "illustration_id",
        "submission_id",
        "assumed_rate_class",
        "signed",
    ])?;
    for (i, ill) in s.illustrations.iter().enumerate() {
        let class = mess.rate_class(ill.assumed_rate_class);
        w.write_record([
            format!("ILL{:06}", i + 1),
            ill.submission_id.to_string(),
            class,
            mess.pick(&["Y", "yes", "TRUE"]).to_owned(),
        ])?;
    }
    w.flush()?;

    let mut w = writer(dir, "disclosures.csv")?;
    w.write_record(["submission_id", "question_code", "answer"])?;
    for d in &s.disclosures {
        let answer = if d.answer {
            mess.pick(&["Y", "yes", "TRUE"])
        } else {
            mess.pick(&["N", "no", "FALSE"])
        };
        w.write_record([
            d.submission_id.as_str(),
            &mess.case(d.question.code()),
            answer,
        ])?;
    }
    w.flush()?;

    let mut w = writer(dir, "labs.csv")?;
    w.write_record(["submission_id", "test_code", "result"])?;
    for c in &s.cotinine {
        let result = if c.positive {
            mess.pick(&["POS", "positive", "Positive"])
        } else {
            mess.pick(&["NEG", "negative", "Negative"])
        };
        w.write_record([c.submission_id.as_str(), "COTININE", result])?;
    }
    w.flush()?;

    let mut w = writer(dir, "rx_fills.csv")?;
    w.write_record(["rx_id", "party_id", "drug_text"])?;
    for r in &s.rx_fills {
        w.write_record([r.rx_id.as_str(), r.party_id.as_str(), &r.drug_text])?;
    }
    w.flush()?;

    let mut w = writer(dir, "ref_drug_condition.csv")?;
    w.write_record(["generic_name", "condition_code"])?;
    for (drug, condition) in &reference.drug_condition {
        w.write_record([drug, condition])?;
    }
    w.flush()?;

    let mut w = writer(dir, "ref_question_condition.csv")?;
    w.write_record(["question_code", "condition_code"])?;
    for q in Question::ALL {
        w.write_record([q.code(), q.condition()])?;
    }
    w.flush()?;

    let mut w = writer(dir, "ref_rate_class.csv")?;
    w.write_record(["rate_class_code", "is_preferred", "is_tobacco"])?;
    for rc in RateClass::ALL {
        w.write_record([
            rc.code(),
            &rc.is_preferred().to_string(),
            &rc.is_tobacco().to_string(),
        ])?;
    }
    w.flush()?;

    fs::write(
        dir.join("_ground_truth.json"),
        serde_json::to_string_pretty(&generated.truth)?,
    )?;
    Ok(())
}

fn writer(dir: &Path, name: &str) -> anyhow::Result<csv::Writer<fs::File>> {
    let path = dir.join(name);
    csv::Writer::from_path(&path).with_context(|| format!("writing {}", path.display()))
}

/// Makes clean values look like they came from several upstream systems.
struct Messer {
    rng: ChaCha8Rng,
}

impl Messer {
    fn pick<'a>(&mut self, options: &[&'a str]) -> &'a str {
        options[self.rng.random_range(0..options.len())]
    }

    fn date(&mut self, d: NaiveDate) -> String {
        match self.rng.random_range(0..3) {
            0 => d.format("%Y-%m-%d").to_string(),
            1 => d.format("%m/%d/%Y").to_string(),
            _ => d.format("%d-%b-%Y").to_string(),
        }
    }

    fn case(&mut self, s: &str) -> String {
        match self.rng.random_range(0..3) {
            0 => s.to_owned(),
            1 => s.to_lowercase(),
            _ => format!(" {s} "),
        }
    }

    fn name(&mut self, s: &str) -> String {
        match self.rng.random_range(0..4) {
            0 => s.to_uppercase(),
            1 => format!("{s}  "),
            _ => s.to_owned(),
        }
    }

    fn money(&mut self, dollars: u64) -> String {
        let grouped = group_thousands(dollars);
        match self.rng.random_range(0..3) {
            0 => dollars.to_string(),
            1 => format!("${grouped}"),
            _ => format!("{grouped}.00"),
        }
    }

    fn channel(&mut self, c: Channel) -> &'static str {
        match c {
            Channel::Brokerage => self.pick(&["Brokerage", "BGA", "brokerage "]),
            Channel::CareerAgent => self.pick(&["Career Agent", "CAREER", "career_agent"]),
            Channel::Direct => self.pick(&["Direct", "DTC", "direct-online"]),
            Channel::Bank => self.pick(&["Bank", "Bancassurance", "BANK"]),
        }
    }

    fn status(&mut self, s: SubmissionStatus) -> &'static str {
        match s {
            SubmissionStatus::InUnderwriting => self.pick(&["In Underwriting", "UW", "pending-uw"]),
            SubmissionStatus::Approved => self.pick(&["Approved", "APPROVED-NOT-PLACED"]),
            SubmissionStatus::Placed => self.pick(&["Placed", "ISSUED-PAID", "inforce"]),
            SubmissionStatus::Declined => self.pick(&["Declined", "DECL"]),
            SubmissionStatus::Withdrawn => self.pick(&["Withdrawn", "NTO", "closed-incomplete"]),
        }
    }

    fn rate_class(&mut self, rc: RateClass) -> String {
        match self.rng.random_range(0..3) {
            0 => rc.code().to_owned(),
            1 => rc.code().to_lowercase(),
            _ => rc.code().replace('_', " "),
        }
    }
}

fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic() {
        let a = generate(42);
        let b = generate(42);
        assert_eq!(a.silver, b.silver);
        assert_eq!(a.truth, b.truth);
        assert_ne!(generate(7).silver, a.silver);
    }

    #[test]
    fn planted_counts_match_the_scenario_table() {
        let g = generate(42);
        assert_eq!(g.silver.submissions.len(), SUBMISSIONS);
        assert_eq!(g.truth.scenarios.len(), SUBMISSIONS);
        let count = |idx: usize, label: &str| {
            g.truth
                .scenarios
                .values()
                .filter(|l| l[idx] == label)
                .count()
        };
        for (label, n) in OWNERSHIP_SCENARIOS.iter().filter(|(l, _)| *l != "SELF") {
            assert_eq!(count(0, label), *n, "{label}");
        }
        for (label, n) in HEALTH_SCENARIOS.iter().filter(|(l, _)| *l != "HEALTHY") {
            assert_eq!(count(1, label), *n, "{label}");
        }
        assert_eq!(g.truth.q2_insurable_interest_not_established.len(), 12);
    }

    #[test]
    fn llm_knowledge_only_adds_findings() {
        let t = generate(42).truth;
        let q3 = &t.q3_potential_non_disclosure;
        let q4 = &t.q4_requires_reillustration;
        assert!(q3.curated.is_subset(&q3.curated_plus_llm));
        assert!(q4.curated.is_subset(&q4.curated_plus_llm));
        // ND_LLM cases are found only with LLM knowledge.
        assert_eq!(q3.curated_plus_llm.len() - q3.curated.len(), 7);
        assert_eq!(q3.curated.len(), 8 + 6 + 6);
    }

    #[test]
    fn oracle_rules() {
        let base = Health {
            label: "T",
            rx: vec![],
            yes: vec![],
            cotinine_positive: false,
            illustration: RateClass::PreferredNt,
        };
        assert_eq!(oracle(&base, true), (false, false));
        let smoker = Health {
            cotinine_positive: true,
            ..base.clone()
        };
        assert_eq!(oracle(&smoker, false), (false, true));
        let honest = Health {
            rx: vec![RX_GENERIC[0]],
            yes: vec![Question::Diabetes],
            ..base.clone()
        };
        assert_eq!(oracle(&honest, false), (false, true));
        let hidden_llm = Health {
            rx: vec![RX_LLM[2]],
            illustration: RateClass::StandardNt,
            ..base.clone()
        };
        assert_eq!(oracle(&hidden_llm, false), (false, false));
        assert_eq!(oracle(&hidden_llm, true), (true, false));
        let statin = Health {
            rx: vec![RX_CARDIO_NO_QUESTION[0]],
            ..base
        };
        assert_eq!(oracle(&statin, false), (false, true));
    }

    #[test]
    fn oracle_condition_table() {
        assert_eq!(
            oracle_condition("Type2Diabetes"),
            (Some(Question::Diabetes), true)
        );
        assert_eq!(
            oracle_condition("AtrialFibrillation"),
            (Some(Question::Heart), true)
        );
        assert_eq!(oracle_condition("Hyperlipidemia"), (None, true));
        assert_eq!(oracle_condition("Asthma"), (None, false));
    }

    #[test]
    fn thousands_grouping() {
        assert_eq!(group_thousands(5), "5");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(1_250_000), "1,250,000");
    }

    #[test]
    fn bronze_files_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let g = generate(1);
        let reference = ReferenceData {
            drug_condition: vec![("metformin".into(), "DiabetesMellitus".into())],
        };
        write_bronze(&g, &reference, dir.path(), 1).unwrap();
        for f in [
            "parties.csv",
            "relationships.csv",
            "trusts.csv",
            "products.csv",
            "submissions.csv",
            "illustrations.csv",
            "disclosures.csv",
            "labs.csv",
            "rx_fills.csv",
            "ref_drug_condition.csv",
            "ref_question_condition.csv",
            "ref_rate_class.csv",
            "_ground_truth.json",
        ] {
            assert!(dir.path().join(f).exists(), "{f}");
        }
        let subs = fs::read_to_string(dir.path().join("submissions.csv")).unwrap();
        // header + 1000 rows + 5 duplicates
        assert_eq!(subs.lines().count(), 1 + SUBMISSIONS + 5);
        let truth = GroundTruth::load(&dir.path().join("_ground_truth.json")).unwrap();
        assert_eq!(truth, g.truth);
        assert!(truth.label("S000001").is_some());
        assert!(GroundTruth::load(&dir.path().join("missing.json")).is_err());
    }
}
