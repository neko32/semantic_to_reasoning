-- Raw ref_rate_class.csv, every column as text.
select * from {{ bronze_csv('ref_rate_class.csv') }}
