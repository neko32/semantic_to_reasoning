-- Raw trusts.csv, every column as text.
select * from {{ bronze_csv('trusts.csv') }}
