select
    trim(submission_id)                                    as submission_id,
    upper(trim(question_code))                             as question_code,
    upper(trim(answer)) in ('Y', 'YES', 'TRUE')            as answer
from {{ ref('bronze_disclosures') }}
