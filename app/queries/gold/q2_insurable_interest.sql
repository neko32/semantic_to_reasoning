-- Q2 on Gold: applications where the owner has no insurable interest in the insured.
-- Best effort with what a star schema offers: the owner is the insured, or a
-- direct relationship row links owner and insured (either direction).
-- What it cannot see without writing a new query per pattern: grandparents
-- (two PARENT_OF hops), trusts (owner -> grantor -> relationship), and any
-- future relationship kind.
select s.submission_id
from gold.fct_submissions s
where s.owner_party_id <> s.insured_party_id
  and not exists (
    select 1
    from gold.bridge_party_relationship r
    where r.relationship in ('SPOUSE', 'PARENT_OF', 'EMPLOYER_OF')
      and (
        (r.from_party_id = s.owner_party_id and r.to_party_id = s.insured_party_id)
        or (r.from_party_id = s.insured_party_id and r.to_party_id = s.owner_party_id)
      )
  )
order by 1
