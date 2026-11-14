select party_id, party_type, full_name, date_of_birth
from {{ ref('parties') }}
