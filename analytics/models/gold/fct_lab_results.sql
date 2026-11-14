select submission_id, test_code, is_positive
from {{ ref('lab_results') }}
