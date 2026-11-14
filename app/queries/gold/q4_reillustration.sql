-- Q4 on Gold: signed illustrations whose assumed rate class the case cannot get.
-- Best effort: tobacco detected by the lab but a non-tobacco class illustrated,
-- or a "yes" to a cardiometabolic question but a preferred class illustrated.
-- What it misses: conditions implied only by prescription history.
select distinct i.submission_id
from gold.fct_illustrations i
join gold.dim_rate_class rc on rc.rate_class_code = i.assumed_rate_class
left join gold.fct_lab_results l
  on l.submission_id = i.submission_id and l.test_code = 'COTININE'
where (not rc.is_tobacco and coalesce(l.is_positive, false))
   or (
     rc.is_preferred
     and exists (
       select 1 from gold.fct_disclosures d
       where d.submission_id = i.submission_id
         and d.answer
         and d.question_code in ('DIABETES', 'HEART', 'HYPERTENSION')
     )
   )
order by 1
