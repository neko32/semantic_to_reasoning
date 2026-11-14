{# The upstream systems send ISO, US and day-month-name dates. #}
{% macro parse_date(column) -%}
  cast(try_strptime(trim({{ column }}), ['%Y-%m-%d', '%m/%d/%Y', '%d-%b-%Y']) as date)
{%- endmacro %}
