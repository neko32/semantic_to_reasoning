-- Grain: one submission. Business truth for new-business reporting.
select
    s.submission_id,
    s.received_date,
    s.channel,
    s.product_code,
    p.product_type,
    s.insured_party_id,
    s.owner_party_id,
    s.face_amount,
    s.status,
    case when s.status = 'PLACED' then 1 else 0 end as is_placed
from {{ ref('submissions') }} s
join {{ ref('products') }} p using (product_code)
