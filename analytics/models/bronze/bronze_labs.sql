-- Raw labs.csv, every column as text.
select * from {{ bronze_csv('labs.csv') }}
