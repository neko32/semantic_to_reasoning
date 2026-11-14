-- Raw rx_fills.csv, every column as text.
select * from {{ bronze_csv('rx_fills.csv') }}
