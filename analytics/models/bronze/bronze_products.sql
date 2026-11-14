-- Raw products.csv, every column as text.
select * from {{ bronze_csv('products.csv') }}
