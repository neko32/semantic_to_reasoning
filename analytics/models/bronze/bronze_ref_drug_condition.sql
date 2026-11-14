-- Raw ref_drug_condition.csv, every column as text.
select * from {{ bronze_csv('ref_drug_condition.csv') }}
