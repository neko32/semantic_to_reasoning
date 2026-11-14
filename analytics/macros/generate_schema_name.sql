{# Use the custom schema name as-is (bronze / silver / gold), not "<target>_<custom>". #}
{% macro generate_schema_name(custom_schema_name, node) -%}
  {{ (custom_schema_name or target.schema) | trim }}
{%- endmacro %}
