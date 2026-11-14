-- Party-to-party links, one row per source record (direction as stored).
select from_party_id, to_party_id, relationship
from {{ ref('relationships') }}
