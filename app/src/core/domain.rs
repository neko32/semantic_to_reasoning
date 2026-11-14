//! Domain newtypes. Validation happens once, at the edge, so the rest of the
//! code can rely on the invariants.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("invalid {kind} '{value}': {reason}")]
    Invalid {
        kind: &'static str,
        value: String,
        reason: &'static str,
    },
}

fn invalid(kind: &'static str, value: &str, reason: &'static str) -> DomainError {
    DomainError::Invalid {
        kind,
        value: value.to_owned(),
        reason,
    }
}

/// Checks `<prefix><6 digits>`.
fn has_id_shape(value: &str, prefixes: &[char]) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if prefixes.contains(&c))
        && value.len() == 7
        && chars.all(|c| c.is_ascii_digit())
}

macro_rules! id_newtype {
    ($name:ident, $kind:literal, [$($prefix:literal),+]) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// # Errors
            /// Returns an error when the value does not look like an id of this kind.
            pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
                let value = value.into();
                if has_id_shape(&value, &[$($prefix),+]) {
                    Ok(Self(value))
                } else {
                    Err(invalid($kind, &value, "expected a letter prefix and 6 digits"))
                }
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = DomainError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_newtype!(PartyId, "party id", ['P', 'T', 'C']);
id_newtype!(SubmissionId, "submission id", ['S']);
id_newtype!(RxId, "prescription id", ['R']);

/// Face amount in whole US dollars. Always positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FaceAmount(u64);

impl FaceAmount {
    /// # Errors
    /// Returns an error for zero.
    pub fn new(dollars: u64) -> Result<Self, DomainError> {
        if dollars == 0 {
            Err(invalid("face amount", "0", "must be positive"))
        } else {
            Ok(Self(dollars))
        }
    }

    #[must_use]
    pub fn dollars(self) -> u64 {
        self.0
    }
}

/// A probability-like score in `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Confidence(f64);

impl Confidence {
    /// # Errors
    /// Returns an error outside `[0, 1]` or for NaN.
    pub fn new(value: f64) -> Result<Self, DomainError> {
        if (0.0..=1.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(invalid(
                "confidence",
                &value.to_string(),
                "must be within [0, 1]",
            ))
        }
    }

    #[must_use]
    pub fn value(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Confidence {
    type Error = DomainError;
    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Confidence> for f64 {
    fn from(value: Confidence) -> Self {
        value.0
    }
}

/// A calendar quarter such as `2026-Q3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quarter {
    year: i32,
    quarter: u8,
}

impl Quarter {
    /// # Errors
    /// Returns an error when `quarter` is not 1..=4.
    pub fn new(year: i32, quarter: u8) -> Result<Self, DomainError> {
        if (1..=4).contains(&quarter) {
            Ok(Self { year, quarter })
        } else {
            Err(invalid("quarter", &quarter.to_string(), "must be 1..=4"))
        }
    }

    /// First day of the quarter, `YYYY-MM-DD`.
    #[must_use]
    pub fn start_date(self) -> String {
        format!("{}-{:02}-01", self.year, (self.quarter - 1) * 3 + 1)
    }

    /// First day after the quarter, `YYYY-MM-DD`.
    #[must_use]
    pub fn end_date_exclusive(self) -> String {
        if self.quarter == 4 {
            format!("{}-01-01", self.year + 1)
        } else {
            format!("{}-{:02}-01", self.year, self.quarter * 3 + 1)
        }
    }

    /// Last day of the quarter, `YYYY-MM-DD`.
    #[must_use]
    pub fn last_date(self) -> String {
        let (month, day) = match self.quarter {
            1 => (3, 31),
            2 => (6, 30),
            3 => (9, 30),
            _ => (12, 31),
        };
        format!("{}-{month:02}-{day:02}", self.year)
    }
}

impl FromStr for Quarter {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || invalid("quarter", s, "expected YYYY-Qn");
        let (year, q) = s.split_once("-Q").ok_or_else(bad)?;
        let year: i32 = year.parse().map_err(|_| bad())?;
        let q: u8 = q.parse().map_err(|_| bad())?;
        Self::new(year, q).map_err(|_| bad())
    }
}

impl fmt::Display for Quarter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-Q{}", self.year, self.quarter)
    }
}

/// Distribution channel (Silver vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Channel {
    Brokerage,
    CareerAgent,
    Direct,
    Bank,
}

impl Channel {
    pub const ALL: [Self; 4] = [Self::Brokerage, Self::CareerAgent, Self::Direct, Self::Bank];

    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Brokerage => "BROKERAGE",
            Self::CareerAgent => "CAREER_AGENT",
            Self::Direct => "DIRECT",
            Self::Bank => "BANK",
        }
    }
}

/// Submission status (Silver vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SubmissionStatus {
    InUnderwriting,
    Approved,
    Placed,
    Declined,
    Withdrawn,
}

impl SubmissionStatus {
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::InUnderwriting => "IN_UNDERWRITING",
            Self::Approved => "APPROVED",
            Self::Placed => "PLACED",
            Self::Declined => "DECLINED",
            Self::Withdrawn => "WITHDRAWN",
        }
    }
}

/// Rate class assumed by an illustration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RateClass {
    PreferredPlusNt,
    PreferredNt,
    StandardNt,
    PreferredTobacco,
    StandardTobacco,
}

impl RateClass {
    pub const ALL: [Self; 5] = [
        Self::PreferredPlusNt,
        Self::PreferredNt,
        Self::StandardNt,
        Self::PreferredTobacco,
        Self::StandardTobacco,
    ];

    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::PreferredPlusNt => "PREFERRED_PLUS_NT",
            Self::PreferredNt => "PREFERRED_NT",
            Self::StandardNt => "STANDARD_NT",
            Self::PreferredTobacco => "PREFERRED_TOBACCO",
            Self::StandardTobacco => "STANDARD_TOBACCO",
        }
    }

    /// Local name of the rate-class individual in the ontology.
    #[must_use]
    pub fn iri_local(self) -> &'static str {
        match self {
            Self::PreferredPlusNt => "PreferredPlusNT",
            Self::PreferredNt => "PreferredNT",
            Self::StandardNt => "StandardNT",
            Self::PreferredTobacco => "PreferredTobacco",
            Self::StandardTobacco => "StandardTobacco",
        }
    }

    #[must_use]
    pub fn is_preferred(self) -> bool {
        matches!(
            self,
            Self::PreferredPlusNt | Self::PreferredNt | Self::PreferredTobacco
        )
    }

    #[must_use]
    pub fn is_tobacco(self) -> bool {
        matches!(self, Self::PreferredTobacco | Self::StandardTobacco)
    }

    /// # Errors
    /// Returns an error for an unknown code.
    pub fn from_code(code: &str) -> Result<Self, DomainError> {
        Self::ALL
            .into_iter()
            .find(|rc| rc.code() == code)
            .ok_or_else(|| invalid("rate class", code, "unknown code"))
    }
}

/// Medical questions on the application, and the condition each one asks about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Question {
    Diabetes,
    Heart,
    Hypertension,
}

impl Question {
    pub const ALL: [Self; 3] = [Self::Diabetes, Self::Heart, Self::Hypertension];

    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Diabetes => "DIABETES",
            Self::Heart => "HEART",
            Self::Hypertension => "HYPERTENSION",
        }
    }

    /// Local name of the condition in the `cond:` namespace.
    #[must_use]
    pub fn condition(self) -> &'static str {
        match self {
            Self::Diabetes => "DiabetesMellitus",
            Self::Heart => "HeartDisease",
            Self::Hypertension => "Hypertension",
        }
    }

    /// # Errors
    /// Returns an error for an unknown code.
    pub fn from_code(code: &str) -> Result<Self, DomainError> {
        Self::ALL
            .into_iter()
            .find(|q| q.code() == code)
            .ok_or_else(|| invalid("question", code, "unknown code"))
    }
}

/// Which knowledge the reasoner may use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrustPolicy {
    /// Only the human-curated dictionary.
    CuratedOnly,
    /// Curated knowledge plus LLM candidates at or above the threshold.
    CuratedPlusLlm { min_confidence: Confidence },
}

impl TrustPolicy {
    #[must_use]
    pub fn accepts_llm(self, confidence: Confidence) -> bool {
        match self {
            Self::CuratedOnly => false,
            Self::CuratedPlusLlm { min_confidence } => confidence >= min_confidence,
        }
    }
}

impl fmt::Display for TrustPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CuratedOnly => f.write_str("curated"),
            Self::CuratedPlusLlm { min_confidence } => {
                write!(
                    f,
                    "curated+llm (confidence >= {:.2})",
                    min_confidence.value()
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_validate_prefix_and_digits() {
        assert!(PartyId::new("P000001").is_ok());
        assert!(PartyId::new("T000001").is_ok());
        assert!(PartyId::new("C123456").is_ok());
        assert!(PartyId::new("S000001").is_err());
        assert!(PartyId::new("P00001").is_err());
        assert!(PartyId::new("P00000A").is_err());
        assert!(SubmissionId::new("S000042").is_ok());
        assert!(RxId::new("R000042").is_ok());
        assert_eq!(SubmissionId::new("S000042").unwrap().to_string(), "S000042");
    }

    #[test]
    fn ids_round_trip_through_serde() {
        let id: PartyId = serde_json::from_str("\"P000007\"").unwrap();
        assert_eq!(id.as_str(), "P000007");
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"P000007\"");
        assert!(serde_json::from_str::<PartyId>("\"X\"").is_err());
        assert_eq!(String::from(id), "P000007");
    }

    #[test]
    fn face_amount_must_be_positive() {
        assert!(FaceAmount::new(0).is_err());
        assert_eq!(FaceAmount::new(250_000).unwrap().dollars(), 250_000);
    }

    #[test]
    fn confidence_range() {
        assert!(Confidence::new(-0.01).is_err());
        assert!(Confidence::new(1.01).is_err());
        assert!(Confidence::new(f64::NAN).is_err());
        assert!((Confidence::new(0.8).unwrap().value() - 0.8).abs() < f64::EPSILON);
        assert!(serde_json::from_str::<Confidence>("1.5").is_err());
        assert!(
            (f64::from(serde_json::from_str::<Confidence>("0.5").unwrap()) - 0.5).abs() < 1e-12
        );
    }

    #[test]
    fn quarter_parsing_and_dates() {
        let q: Quarter = "2026-Q3".parse().unwrap();
        assert_eq!(q.start_date(), "2026-07-01");
        assert_eq!(q.end_date_exclusive(), "2026-10-01");
        assert_eq!(q.last_date(), "2026-09-30");
        assert_eq!(q.to_string(), "2026-Q3");
        let q4: Quarter = "2025-Q4".parse().unwrap();
        assert_eq!(q4.end_date_exclusive(), "2026-01-01");
        assert_eq!(q4.last_date(), "2025-12-31");
        assert_eq!(
            "2026-Q1".parse::<Quarter>().unwrap().last_date(),
            "2026-03-31"
        );
        assert_eq!(
            "2026-Q2".parse::<Quarter>().unwrap().last_date(),
            "2026-06-30"
        );
        assert!("2026-Q5".parse::<Quarter>().is_err());
        assert!("2026Q1".parse::<Quarter>().is_err());
        assert!("abcd-Q1".parse::<Quarter>().is_err());
        assert!("2026-Qx".parse::<Quarter>().is_err());
    }

    #[test]
    fn enum_codes() {
        assert_eq!(Channel::CareerAgent.code(), "CAREER_AGENT");
        assert_eq!(
            Channel::ALL.map(Channel::code),
            ["BROKERAGE", "CAREER_AGENT", "DIRECT", "BANK"]
        );
        for status in [
            SubmissionStatus::InUnderwriting,
            SubmissionStatus::Approved,
            SubmissionStatus::Placed,
            SubmissionStatus::Declined,
            SubmissionStatus::Withdrawn,
        ] {
            assert!(!status.code().is_empty());
        }
        for rc in RateClass::ALL {
            assert_eq!(RateClass::from_code(rc.code()).unwrap(), rc);
            assert!(!rc.iri_local().is_empty());
        }
        assert!(RateClass::from_code("NOPE").is_err());
        assert!(RateClass::PreferredTobacco.is_preferred());
        assert!(RateClass::PreferredTobacco.is_tobacco());
        assert!(!RateClass::StandardNt.is_preferred());
        for q in Question::ALL {
            assert_eq!(Question::from_code(q.code()).unwrap(), q);
        }
        assert_eq!(Question::Heart.condition(), "HeartDisease");
        assert!(Question::from_code("NOPE").is_err());
    }

    #[test]
    fn trust_policy() {
        let c = |v| Confidence::new(v).unwrap();
        assert!(!TrustPolicy::CuratedOnly.accepts_llm(c(1.0)));
        let p = TrustPolicy::CuratedPlusLlm {
            min_confidence: c(0.8),
        };
        assert!(p.accepts_llm(c(0.8)));
        assert!(!p.accepts_llm(c(0.79)));
        assert_eq!(TrustPolicy::CuratedOnly.to_string(), "curated");
        assert_eq!(p.to_string(), "curated+llm (confidence >= 0.80)");
    }

    #[test]
    fn error_message() {
        let err = PartyId::new("bad").unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid party id 'bad': expected a letter prefix and 6 digits"
        );
    }
}
