Your Gold layer knows what a "placed policy" is. It doesn't know what a trust is.

Medallion architecture plus a semantic layer is a great way to get business truth: one definition of "placement rate", conformed dimensions, numbers that every team trusts.

But relationships in a star schema are just join keys. Their meaning (spouse is symmetric, ancestor is transitive, a trust inherits its grantor's interest, a drug implies a condition) lives in SQL that someone writes again for every question. Or it lives nowhere.

So I built a POC to test this. I used synthetic life-insurance new business (illustration → submission → underwriting) and ran the same Silver data through two paths:

🟡 Gold path: dbt + DuckDB, with a MetricFlow semantic layer
🔵 Graph path: Oxigraph, an OWL ontology, and a small Rust reasoner that writes down why it concluded each fact

Same four questions, 1,000 applications:

Q1. Placement rate by channel and product
→ Gold and graph give identical answers. This is Gold's home ground.

Q2. Which applications lack an insurable interest (possible STOLI)?
→ Gold flags 36. The graph flags 12, and those 12 are the real cases.
The other 24 are grandparents and family trusts. The relationship that clears them takes two hops, and Gold can't follow it.

Q3. Which "no" answers contradict the prescription history?
→ Gold finds 8. The graph finds 27.
"Plavix" is a brand name. Coronary artery disease is a kind of heart disease. Gold has every one of these rows, but none of the meaning that connects them.

Q4. Which signed illustrations assume a rate class the applicant can't get?
→ Gold finds 28. The graph finds 55, before the case ever reaches an underwriter.

Where does the LLM fit in? Exactly one place. A local Gemma model proposes the knowledge my curated dictionary lacks ("Jardiance" → empagliflozin → type 2 diabetes). Its answers must follow a JSON schema, are checked against allow-lists, and only count above a confidence threshold. Every conclusion that depends on one is flagged in its explanation. The LLM proposes. The ontology and the rules decide.

My takeaway: semantics gets you to the definition of a number. A triple store is what gets you to relationship truth, and from there to reasoning you can explain.

Could SQL do this? Yes: one recursive CTE per pattern, one question at a time. By then you've rebuilt a graph without a place to say what a relationship means.

Code, data generator and results 👇
https://github.com/neko32/semantic_to_reasoning

#DataArchitecture #KnowledgeGraph #SemanticLayer #Insurance #Underwriting #dbt #RDF #LLM
