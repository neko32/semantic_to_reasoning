select
    upper(trim(rate_class_code)) as rate_class_code,
    cast(is_preferred as boolean) as is_preferred,
    cast(is_tobacco as boolean)   as is_tobacco
from {{ ref('bronze_ref_rate_class') }}
