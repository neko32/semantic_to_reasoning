select submission_id, question_code, answer
from {{ ref('disclosures') }}
