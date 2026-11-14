-- One row per party; trimmed names, typed birth dates.
select
    trim(party_id)                                        as party_id,
    upper(trim(party_type))                               as party_type,
    regexp_replace(trim(full_name), '\s+', ' ', 'g')      as full_name,
    {{ parse_date('date_of_birth') }}                     as date_of_birth
from {{ ref('bronze_parties') }}
