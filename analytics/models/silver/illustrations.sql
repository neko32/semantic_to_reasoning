select
    trim(illustration_id)                                  as illustration_id,
    trim(submission_id)                                    as submission_id,
    upper(replace(trim(assumed_rate_class), ' ', '_'))     as assumed_rate_class,
    upper(trim(signed)) in ('Y', 'YES', 'TRUE')            as is_signed
from {{ ref('bronze_illustrations') }}
