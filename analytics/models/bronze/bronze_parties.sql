-- Raw parties.csv, every column as text.
select * from {{ bronze_csv('parties.csv') }}
