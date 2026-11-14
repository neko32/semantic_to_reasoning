-- Raw disclosures.csv, every column as text.
select * from {{ bronze_csv('disclosures.csv') }}
