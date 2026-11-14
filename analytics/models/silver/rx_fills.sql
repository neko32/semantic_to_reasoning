-- Pharmacy feed. The drug text stays free text: Silver does not know drugs.
select
    trim(rx_id)     as rx_id,
    trim(party_id)  as party_id,
    trim(drug_text) as drug_text
from {{ ref('bronze_rx_fills') }}
