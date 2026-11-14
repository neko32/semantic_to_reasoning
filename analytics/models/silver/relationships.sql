-- Relationship rows exactly as the source stores them (one direction only).
select
    trim(from_party_id)                                   as from_party_id,
    trim(to_party_id)                                     as to_party_id,
    upper(replace(trim(relationship), ' ', '_'))          as relationship
from {{ ref('bronze_relationships') }}
