select
    trim(product_code)        as product_code,
    trim(product_name)        as product_name,
    upper(trim(product_type)) as product_type
from {{ ref('bronze_products') }}
