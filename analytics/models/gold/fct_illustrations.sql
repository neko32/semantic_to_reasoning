select illustration_id, submission_id, assumed_rate_class
from {{ ref('illustrations') }}
where is_signed
