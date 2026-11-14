{# Reads a Bronze CSV with every column as text: Bronze keeps the source as-is. #}
{% macro bronze_csv(file_name) -%}
  read_csv('{{ var("bronze_dir") }}/{{ file_name }}', header = true, all_varchar = true)
{%- endmacro %}
