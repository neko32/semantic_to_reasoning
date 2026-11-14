select
    trim(submission_id)                                    as submission_id,
    upper(trim(test_code))                                 as test_code,
    upper(trim(result)) in ('POS', 'POSITIVE')             as is_positive
from {{ ref('bronze_labs') }}
