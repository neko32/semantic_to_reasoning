select
    trim(trust_party_id)   as trust_party_id,
    trim(grantor_party_id) as grantor_party_id
from {{ ref('bronze_trusts') }}
