select rx_id, party_id, drug_text
from {{ ref('rx_fills') }}
