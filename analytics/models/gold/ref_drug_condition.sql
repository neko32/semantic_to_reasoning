-- Curated generic drug -> condition (exported by `s2r seed` from the same dictionary the graph uses).
select lower(trim(generic_name)) as generic_name, trim(condition_code) as condition_code
from {{ ref('bronze_ref_drug_condition') }}
