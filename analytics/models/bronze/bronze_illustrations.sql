-- Raw illustrations.csv, every column as text.
select * from {{ bronze_csv('illustrations.csv') }}
