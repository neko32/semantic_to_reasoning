Your Gold layer knows what a "placed policy" is. It doesn't know what a trust is.

A semantic layer gives you business truth: one definition of every metric. But in a star schema, relationships are just join keys. Their meaning never makes it into the data.

I tested this on synthetic life-insurance applications: same Silver data, two paths.
🟡 dbt + DuckDB + MetricFlow
🔵 Triple store + OWL ontology + a Rust reasoner that explains every conclusion

Results on 1,000 applications:
• Placement rate by channel: identical. Gold's home ground.
• Missing insurable interest: Gold flags 36, graph 12. The extra 24 are grandparents and family trusts, and clearing them takes a two-hop chain.
• "No" answers contradicted by Rx history: Gold 8, graph 27 (brand names, condition hierarchy).
• Illustrations assuming an unreachable rate class: Gold 28, graph 55.

A local Gemma model fills gaps in the drug dictionary. Its answers are schema-checked and kept only above a confidence threshold, and every conclusion that relies on one is flagged.

Semantics gets you to the definition. Relationships get you to reasoning.

https://github.com/neko32/semantic_to_reasoning

#KnowledgeGraph #SemanticLayer #DataArchitecture #Insurance
