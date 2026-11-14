//! `DuckDB` adapter: reads Silver for the graph and runs the Gold SQL.
//! The connection is read-only; dbt owns writes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::NaiveDate;
use duckdb::{AccessMode, Config, Connection};

use crate::core::domain::{
    Channel, FaceAmount, PartyId, Question, RateClass, RxId, SubmissionId, SubmissionStatus,
};
use crate::core::questions::GoldStore;
use crate::core::silver::{
    CotinineResult, Disclosure, Illustration, Party, PartyType, Product, Relationship,
    RelationshipType, RxFill, SilverData, SilverSource, Submission, TrustRecord,
};

/// A `DuckDB` warehouse file produced by `dbt build`.
pub struct DuckDbWarehouse {
    conn: Connection,
    path: PathBuf,
}

impl std::fmt::Debug for DuckDbWarehouse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DuckDbWarehouse")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl DuckDbWarehouse {
    /// Opens the warehouse read-only.
    ///
    /// # Errors
    /// Returns an error when the file is missing or cannot be opened.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        anyhow::ensure!(
            path.exists(),
            "warehouse {} not found: run `s2r seed` and `dbt build` first",
            path.display()
        );
        let config = Config::default().access_mode(AccessMode::ReadOnly)?;
        let conn = Connection::open_with_flags(path, config)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Self {
            conn,
            path: path.to_owned(),
        })
    }

    /// Wraps an existing connection (used by tests).
    #[must_use]
    pub fn from_connection(conn: Connection) -> Self {
        Self {
            conn,
            path: PathBuf::from(":memory:"),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn rows(&self, sql: &str, width: usize) -> anyhow::Result<Vec<Vec<Option<String>>>> {
        let mut stmt = self
            .conn
            .prepare(sql)
            .with_context(|| format!("preparing:\n{sql}"))?;
        let rows = stmt.query_map([], |row| {
            (0..width)
                .map(|i| row.get::<_, Option<String>>(i))
                .collect::<Result<Vec<_>, _>>()
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

fn req(value: Option<&Option<String>>, what: &str) -> anyhow::Result<String> {
    value
        .and_then(Clone::clone)
        .with_context(|| format!("missing {what}"))
}

fn date(value: &str) -> anyhow::Result<NaiveDate> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d").with_context(|| format!("bad date '{value}'"))
}

fn flag(value: &str) -> anyhow::Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => anyhow::bail!("bad boolean '{other}'"),
    }
}

fn channel(code: &str) -> anyhow::Result<Channel> {
    Channel::ALL
        .into_iter()
        .find(|c| c.code() == code)
        .with_context(|| format!("unknown channel '{code}'"))
}

fn status(code: &str) -> anyhow::Result<SubmissionStatus> {
    [
        SubmissionStatus::InUnderwriting,
        SubmissionStatus::Approved,
        SubmissionStatus::Placed,
        SubmissionStatus::Declined,
        SubmissionStatus::Withdrawn,
    ]
    .into_iter()
    .find(|s| s.code() == code)
    .with_context(|| format!("unknown status '{code}'"))
}

fn party_type(code: &str) -> anyhow::Result<PartyType> {
    [PartyType::Person, PartyType::Trust, PartyType::Company]
        .into_iter()
        .find(|t| t.code() == code)
        .with_context(|| format!("unknown party type '{code}'"))
}

fn relationship(code: &str) -> anyhow::Result<RelationshipType> {
    [
        RelationshipType::Spouse,
        RelationshipType::ParentOf,
        RelationshipType::EmployerOf,
    ]
    .into_iter()
    .find(|t| t.code() == code)
    .with_context(|| format!("unknown relationship '{code}'"))
}

impl SilverSource for DuckDbWarehouse {
    #[allow(clippy::too_many_lines)]
    fn load_silver(&self) -> anyhow::Result<SilverData> {
        let mut s = SilverData::default();
        for r in self.rows(
            "select party_id, party_type, full_name, cast(date_of_birth as varchar)
             from silver.parties order by party_id",
            4,
        )? {
            s.parties.push(Party {
                party_id: PartyId::new(req(r.first(), "party_id")?)?,
                party_type: party_type(&req(r.get(1), "party_type")?)?,
                full_name: req(r.get(2), "full_name")?,
                date_of_birth: r.get(3).cloned().flatten().map(|d| date(&d)).transpose()?,
            });
        }
        for r in self.rows(
            "select from_party_id, to_party_id, relationship from silver.relationships
             order by 1, 2, 3",
            3,
        )? {
            s.relationships.push(Relationship {
                from: PartyId::new(req(r.first(), "from_party_id")?)?,
                to: PartyId::new(req(r.get(1), "to_party_id")?)?,
                relationship: relationship(&req(r.get(2), "relationship")?)?,
            });
        }
        for r in self.rows(
            "select trust_party_id, grantor_party_id from silver.trusts order by 1",
            2,
        )? {
            s.trusts.push(TrustRecord {
                trust_id: PartyId::new(req(r.first(), "trust_party_id")?)?,
                grantor_id: PartyId::new(req(r.get(1), "grantor_party_id")?)?,
            });
        }
        for r in self.rows(
            "select product_code, product_name, product_type from silver.products order by 1",
            3,
        )? {
            s.products.push(Product {
                product_code: req(r.first(), "product_code")?,
                product_name: req(r.get(1), "product_name")?,
                product_type: req(r.get(2), "product_type")?,
            });
        }
        for r in self.rows(
            "select submission_id, cast(received_date as varchar), channel, product_code,
                    insured_party_id, owner_party_id, cast(face_amount as varchar), status
             from silver.submissions order by 1",
            8,
        )? {
            let face: u64 = req(r.get(6), "face_amount")?
                .parse()
                .context("face_amount")?;
            s.submissions.push(Submission {
                submission_id: SubmissionId::new(req(r.first(), "submission_id")?)?,
                received_date: date(&req(r.get(1), "received_date")?)?,
                channel: channel(&req(r.get(2), "channel")?)?,
                product_code: req(r.get(3), "product_code")?,
                insured: PartyId::new(req(r.get(4), "insured_party_id")?)?,
                owner: PartyId::new(req(r.get(5), "owner_party_id")?)?,
                face_amount: FaceAmount::new(face)?,
                status: status(&req(r.get(7), "status")?)?,
            });
        }
        for r in self.rows(
            "select submission_id, assumed_rate_class from silver.illustrations
             where is_signed order by 1",
            2,
        )? {
            s.illustrations.push(Illustration {
                submission_id: SubmissionId::new(req(r.first(), "submission_id")?)?,
                assumed_rate_class: RateClass::from_code(&req(r.get(1), "assumed_rate_class")?)?,
            });
        }
        for r in self.rows(
            "select submission_id, question_code, cast(answer as varchar)
             from silver.disclosures order by 1, 2",
            3,
        )? {
            s.disclosures.push(Disclosure {
                submission_id: SubmissionId::new(req(r.first(), "submission_id")?)?,
                question: Question::from_code(&req(r.get(1), "question_code")?)?,
                answer: flag(&req(r.get(2), "answer")?)?,
            });
        }
        for r in self.rows(
            "select submission_id, cast(is_positive as varchar) from silver.lab_results
             where test_code = 'COTININE' order by 1",
            2,
        )? {
            s.cotinine.push(CotinineResult {
                submission_id: SubmissionId::new(req(r.first(), "submission_id")?)?,
                positive: flag(&req(r.get(1), "is_positive")?)?,
            });
        }
        for r in self.rows(
            "select rx_id, party_id, drug_text from silver.rx_fills order by 1",
            3,
        )? {
            s.rx_fills.push(RxFill {
                rx_id: RxId::new(req(r.first(), "rx_id")?)?,
                party_id: PartyId::new(req(r.get(1), "party_id")?)?,
                drug_text: req(r.get(2), "drug_text")?,
            });
        }
        Ok(s)
    }
}

impl GoldStore for DuckDbWarehouse {
    fn submission_ids(&self, sql: &str) -> anyhow::Result<BTreeSet<String>> {
        Ok(self
            .rows(sql, 1)?
            .into_iter()
            .filter_map(|r| r.into_iter().next().flatten())
            .collect())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::seed::generate;
    use std::fmt::Write as _;

    fn sql_str(v: &str) -> String {
        format!("'{}'", v.replace('\'', "''"))
    }

    /// Loads clean Silver data into an in-memory `DuckDB` with the same schema
    /// as the dbt models, plus the Gold tables the queries use.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn in_memory_warehouse(s: &SilverData) -> DuckDbWarehouse {
        let conn = Connection::open_in_memory().unwrap();
        populate(&conn, s);
        DuckDbWarehouse::from_connection(conn)
    }

    /// Writes the Silver/Gold test schema into `conn`.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn populate(conn: &Connection, s: &SilverData) {
        conn.execute_batch(
            "create schema silver; create schema gold;
             create table silver.parties(party_id varchar, party_type varchar, full_name varchar, date_of_birth date);
             create table silver.relationships(from_party_id varchar, to_party_id varchar, relationship varchar);
             create table silver.trusts(trust_party_id varchar, grantor_party_id varchar);
             create table silver.products(product_code varchar, product_name varchar, product_type varchar);
             create table silver.submissions(submission_id varchar, received_date date, channel varchar, product_code varchar,
                 insured_party_id varchar, owner_party_id varchar, face_amount bigint, status varchar);
             create table silver.illustrations(illustration_id varchar, submission_id varchar, assumed_rate_class varchar, is_signed boolean);
             create table silver.disclosures(submission_id varchar, question_code varchar, answer boolean);
             create table silver.lab_results(submission_id varchar, test_code varchar, is_positive boolean);
             create table silver.rx_fills(rx_id varchar, party_id varchar, drug_text varchar);
             create table gold.ref_drug_condition(generic_name varchar, condition_code varchar);
             create table gold.ref_question_condition(question_code varchar, condition_code varchar);
             create table gold.dim_rate_class(rate_class_code varchar, is_preferred boolean, is_tobacco boolean);",
        )
        .unwrap();
        // One multi-row INSERT per table: row-by-row statements are slow in DuckDB.
        let mut tables: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        let mut push = |table: &str, values: Vec<String>| {
            tables
                .entry(table.to_owned())
                .or_default()
                .push(format!("({})", values.join(", ")));
        };
        for p in &s.parties {
            push(
                "silver.parties",
                vec![
                    sql_str(p.party_id.as_str()),
                    sql_str(p.party_type.code()),
                    sql_str(&p.full_name),
                    p.date_of_birth
                        .map_or("null".into(), |d| format!("date '{d}'")),
                ],
            );
        }
        for r in &s.relationships {
            push(
                "silver.relationships",
                vec![
                    sql_str(r.from.as_str()),
                    sql_str(r.to.as_str()),
                    sql_str(r.relationship.code()),
                ],
            );
        }
        for t in &s.trusts {
            push(
                "silver.trusts",
                vec![sql_str(t.trust_id.as_str()), sql_str(t.grantor_id.as_str())],
            );
        }
        for p in &s.products {
            push(
                "silver.products",
                vec![
                    sql_str(&p.product_code),
                    sql_str(&p.product_name),
                    sql_str(&p.product_type),
                ],
            );
        }
        for x in &s.submissions {
            push(
                "silver.submissions",
                vec![
                    sql_str(x.submission_id.as_str()),
                    format!("date '{}'", x.received_date),
                    sql_str(x.channel.code()),
                    sql_str(&x.product_code),
                    sql_str(x.insured.as_str()),
                    sql_str(x.owner.as_str()),
                    x.face_amount.dollars().to_string(),
                    sql_str(x.status.code()),
                ],
            );
        }
        for i in &s.illustrations {
            push(
                "silver.illustrations",
                vec![
                    sql_str(&format!("ILL-{}", i.submission_id)),
                    sql_str(i.submission_id.as_str()),
                    sql_str(i.assumed_rate_class.code()),
                    "true".into(),
                ],
            );
        }
        for d in &s.disclosures {
            push(
                "silver.disclosures",
                vec![
                    sql_str(d.submission_id.as_str()),
                    sql_str(d.question.code()),
                    d.answer.to_string(),
                ],
            );
        }
        for c in &s.cotinine {
            push(
                "silver.lab_results",
                vec![
                    sql_str(c.submission_id.as_str()),
                    "'COTININE'".into(),
                    c.positive.to_string(),
                ],
            );
        }
        for r in &s.rx_fills {
            push(
                "silver.rx_fills",
                vec![
                    sql_str(r.rx_id.as_str()),
                    sql_str(r.party_id.as_str()),
                    sql_str(&r.drug_text),
                ],
            );
        }
        let catalog = crate::core::knowledge::Catalog::bundled().unwrap();
        for (drug, cond) in catalog.reference_rows() {
            push(
                "gold.ref_drug_condition",
                vec![sql_str(&drug), sql_str(&cond)],
            );
        }
        for q in Question::ALL {
            push(
                "gold.ref_question_condition",
                vec![sql_str(q.code()), sql_str(q.condition())],
            );
        }
        for rc in RateClass::ALL {
            push(
                "gold.dim_rate_class",
                vec![
                    sql_str(rc.code()),
                    rc.is_preferred().to_string(),
                    rc.is_tobacco().to_string(),
                ],
            );
        }
        let mut batch = String::new();
        for (table, rows) in &tables {
            let _ = writeln!(batch, "insert into {table} values {};", rows.join(", "));
        }
        conn.execute_batch(&batch).unwrap();
        // Gold views mirroring the dbt models.
        conn.execute_batch(
            "create view gold.fct_submissions as
               select s.*, p.product_type, case when s.status = 'PLACED' then 1 else 0 end as is_placed
               from silver.submissions s join silver.products p using (product_code);
             create view gold.bridge_party_relationship as select * from silver.relationships;
             create view gold.fct_illustrations as select illustration_id, submission_id, assumed_rate_class from silver.illustrations where is_signed;
             create view gold.fct_disclosures as select * from silver.disclosures;
             create view gold.fct_lab_results as select * from silver.lab_results;
             create view gold.fct_rx_fills as select * from silver.rx_fills;",
        )
        .unwrap();
    }

    #[test]
    fn silver_round_trips_through_duckdb() {
        let g = generate(3);
        let wh = in_memory_warehouse(&g.silver);
        let loaded = wh.load_silver().unwrap();
        assert_eq!(loaded.submissions.len(), g.silver.submissions.len());
        assert_eq!(loaded.parties.len(), g.silver.parties.len());
        assert_eq!(loaded.rx_fills, {
            let mut v = g.silver.rx_fills.clone();
            v.sort_by(|a, b| a.rx_id.cmp(&b.rx_id));
            v
        });
        let mut expected = g.silver.submissions.clone();
        expected.sort_by(|a, b| a.submission_id.cmp(&b.submission_id));
        assert_eq!(loaded.submissions, expected);
        assert_eq!(loaded.cotinine.len(), g.silver.cotinine.len());
        assert_eq!(loaded.trusts.len(), g.silver.trusts.len());
        assert_eq!(loaded.illustrations.len(), g.silver.illustrations.len());
        assert_eq!(loaded.disclosures.len(), g.silver.disclosures.len());
        assert_eq!(loaded.relationships.len(), g.silver.relationships.len());
        assert_eq!(loaded.products.len(), 5);
        assert_eq!(wh.path(), Path::new(":memory:"));
    }

    #[test]
    fn open_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("none.duckdb");
        assert!(
            DuckDbWarehouse::open(&missing)
                .unwrap_err()
                .to_string()
                .contains("s2r seed")
        );
        // A real file opens read-only.
        let path = dir.path().join("w.duckdb");
        Connection::open(&path)
            .unwrap()
            .execute_batch("create table t(x int)")
            .unwrap();
        let wh = DuckDbWarehouse::open(&path).unwrap();
        assert!(
            wh.submission_ids("select 'S000001'")
                .unwrap()
                .contains("S000001")
        );
        assert!(wh.submission_ids("insert into t values (1)").is_err());
        assert!(wh.load_silver().is_err());
    }

    #[test]
    fn converters_reject_bad_values() {
        assert!(date("2026/01/01").is_err());
        assert!(flag("maybe").is_err());
        assert!(channel("X").is_err());
        assert!(status("X").is_err());
        assert!(party_type("X").is_err());
        assert!(relationship("X").is_err());
        assert!(req(None, "x").is_err());
        assert!(req(Some(&None), "x").is_err());
        assert_eq!(status("PLACED").unwrap(), SubmissionStatus::Placed);
    }
}
