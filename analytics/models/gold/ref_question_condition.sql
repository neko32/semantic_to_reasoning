select upper(trim(question_code)) as question_code, trim(condition_code) as condition_code
from {{ ref('bronze_ref_question_condition') }}
