-- Raw submissions.csv, every column as text.
select * from {{ bronze_csv('submissions.csv') }}
