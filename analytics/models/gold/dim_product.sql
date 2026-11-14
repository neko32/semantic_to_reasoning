select product_code, product_name, product_type
from {{ ref('products') }}
