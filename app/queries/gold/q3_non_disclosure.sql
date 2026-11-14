-- Q3 on Gold: "no" answers contradicted by prescription history.
-- Best effort: match the generic drug name at the start of the pharmacy text,
-- then join drug -> condition -> question on exact condition codes.
-- What it misses: brand names (Glucophage, Plavix), drugs whose condition is
-- narrower than the question (coronary artery disease vs. "heart disease"),
-- and drugs with no curated indication.
select distinct d.submission_id
from gold.fct_disclosures d
join gold.fct_submissions s on s.submission_id = d.submission_id
join gold.fct_rx_fills rx on rx.party_id = s.insured_party_id
join gold.ref_drug_condition m on lower(rx.drug_text) like m.generic_name || '%'
join gold.ref_question_condition q
  on q.question_code = d.question_code and q.condition_code = m.condition_code
where d.answer = false
order by 1
