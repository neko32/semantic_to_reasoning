-- Raw ref_question_condition.csv, every column as text.
select * from {{ bronze_csv('ref_question_condition.csv') }}
