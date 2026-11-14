-- Deduplicated submissions with conformed channel and status codes.
with typed as (
    select
        trim(submission_id) as submission_id,
        {{ parse_date('received_date') }} as received_date,
        case
            when lower(trim(channel)) in ('brokerage', 'bga') then 'BROKERAGE'
            when lower(trim(channel)) in ('career agent', 'career', 'career_agent') then 'CAREER_AGENT'
            when lower(trim(channel)) in ('direct', 'dtc', 'direct-online') then 'DIRECT'
            when lower(trim(channel)) in ('bank', 'bancassurance') then 'BANK'
        end as channel,
        trim(product_code) as product_code,
        trim(insured_party_id) as insured_party_id,
        trim(owner_party_id) as owner_party_id,
        cast(try_cast(regexp_replace(face_amount, '[$,\s]', '', 'g') as decimal(18, 2)) as bigint) as face_amount,
        case
            when lower(trim(status)) in ('in underwriting', 'uw', 'pending-uw') then 'IN_UNDERWRITING'
            when lower(trim(status)) in ('approved', 'approved-not-placed') then 'APPROVED'
            when lower(trim(status)) in ('placed', 'issued-paid', 'inforce') then 'PLACED'
            when lower(trim(status)) in ('declined', 'decl') then 'DECLINED'
            when lower(trim(status)) in ('withdrawn', 'nto', 'closed-incomplete') then 'WITHDRAWN'
        end as status
    from {{ ref('bronze_submissions') }}
)
select *
from typed
qualify row_number() over (partition by submission_id order by received_date) = 1
