//! Silver-layer records: typed, cleaned, one row per business fact.
//! dbt produces these tables in `DuckDB`; the graph is built from them, so
//! Gold and the graph start from exactly the same data.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::domain::{
    Channel, FaceAmount, PartyId, Question, RateClass, RxId, SubmissionId, SubmissionStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PartyType {
    Person,
    Trust,
    Company,
}

impl PartyType {
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Person => "PERSON",
            Self::Trust => "TRUST",
            Self::Company => "COMPANY",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RelationshipType {
    /// Stored in one direction only, like most source systems do.
    Spouse,
    /// `from` is the parent of `to`.
    ParentOf,
    /// `from` (a company) employs `to` as a key person.
    EmployerOf,
}

impl RelationshipType {
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Spouse => "SPOUSE",
            Self::ParentOf => "PARENT_OF",
            Self::EmployerOf => "EMPLOYER_OF",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Party {
    pub party_id: PartyId,
    pub party_type: PartyType,
    pub full_name: String,
    pub date_of_birth: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    pub from: PartyId,
    pub to: PartyId,
    pub relationship: RelationshipType,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustRecord {
    pub trust_id: PartyId,
    pub grantor_id: PartyId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Product {
    pub product_code: String,
    pub product_name: String,
    pub product_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submission {
    pub submission_id: SubmissionId,
    pub received_date: NaiveDate,
    pub channel: Channel,
    pub product_code: String,
    pub insured: PartyId,
    pub owner: PartyId,
    pub face_amount: FaceAmount,
    pub status: SubmissionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Illustration {
    pub submission_id: SubmissionId,
    pub assumed_rate_class: RateClass,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disclosure {
    pub submission_id: SubmissionId,
    pub question: Question,
    pub answer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RxFill {
    pub rx_id: RxId,
    pub party_id: PartyId,
    pub drug_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CotinineResult {
    pub submission_id: SubmissionId,
    pub positive: bool,
}

/// Everything the graph needs from Silver.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SilverData {
    pub parties: Vec<Party>,
    pub relationships: Vec<Relationship>,
    pub trusts: Vec<TrustRecord>,
    pub products: Vec<Product>,
    pub submissions: Vec<Submission>,
    pub illustrations: Vec<Illustration>,
    pub disclosures: Vec<Disclosure>,
    pub rx_fills: Vec<RxFill>,
    pub cotinine: Vec<CotinineResult>,
}

/// Source of Silver data (`DuckDB` in production, in-memory in tests).
pub trait SilverSource {
    /// # Errors
    /// Returns an error when the Silver tables cannot be read or hold invalid values.
    fn load_silver(&self) -> anyhow::Result<SilverData>;
}

impl SilverSource for SilverData {
    fn load_silver(&self) -> anyhow::Result<SilverData> {
        Ok(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        assert_eq!(PartyType::Trust.code(), "TRUST");
        assert_eq!(PartyType::Person.code(), "PERSON");
        assert_eq!(PartyType::Company.code(), "COMPANY");
        assert_eq!(RelationshipType::ParentOf.code(), "PARENT_OF");
        assert_eq!(RelationshipType::Spouse.code(), "SPOUSE");
        assert_eq!(RelationshipType::EmployerOf.code(), "EMPLOYER_OF");
    }

    #[test]
    fn in_memory_source_returns_a_copy() {
        let data = SilverData::default();
        assert_eq!(data.load_silver().unwrap(), data);
    }
}
