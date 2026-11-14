-- Raw relationships.csv, every column as text.
select * from {{ bronze_csv('relationships.csv') }}
