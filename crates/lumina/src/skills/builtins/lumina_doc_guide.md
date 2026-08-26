---
name: lumina-doc-guide
description: Reference lumina documentation to create, configure, or explain lumina-specific features like recipes, extensions, sessions, and providers. You MUST fetch relevant lumina docs before answering. You MUST NOT rely on training data or assumptions for any lumina-specific fields, values, names, syntax, or commands.
---

Use this skill when working with **lumina-specific features**:
- Creating or editing recipes
- Configuring extensions or providers
- Explaining how lumina features work
- Any lumina configuration or setup task

Do NOT use this skill for:
- General coding tasks unrelated to lumina
- Running existing recipes (just run them directly)

## Steps (COMPLETE ALL BEFORE RESPONDING)
1. **Load the configured Lumina docs**
   - In a source checkout, read `documentation/static/lumina-docs-map.md` and pages under `documentation/docs/`.
   - In an installed distribution, use only the distributor-provided `LUMINA_DOCS_BASE_URL`. If neither source docs nor a configured URL is available, state that Lumina documentation is unavailable and do not guess.
   - Search the doc map for pages relevant to the user's topic and get the paths for these pages
   - Use the EXACT paths from the doc map. For example:
   - If doc map shows: `docs/guides/sessions/session-management.md`
   - Read the corresponding local file, or fetch that exact path relative to `LUMINA_DOCS_BASE_URL`.
   - Do NOT modify or guess paths.
   - **ONLY fetch paths that are explicitly listed in the doc map - do not guess or infer URLs**
   - Make multiple fetch calls in parallel and save to temp files
   - Use the temp files for subsequent searches instead of re-fetching

2. **Create/modify content**
   - For lumina configuration files:
      - Consult schema/field reference documentation first
      - **Search the fetched docs to extract the complete schema for each element you plan to use**
      - Extract example snippets to understand usage patterns
      - Create your configuration based on reference specs, following example patterns
      - **⚠️ STOP: Before showing the user, verify output content MUST match the schema and reference in the lumina official documentation:**
         - [ ] Field names match exactly as shown in docs
         - [ ] Required fields/properties are present
         - [ ] Value formats match examples (YAML/JSON syntax, data types, etc.)
      - **If ANY verification fails, revise and repeat this step until ALL verifications pass**
      - **DO NOT present unverified output to the user**

3. **MANDATORY VERIFICATION - CHECK ALL THESE ITEMS BEFORE STEP 4**
   Before writing your final answer:
   - [ ] You MUST NOT rely on training data or assumptions for any lumina-specific fields, values, names, syntax, or commands.
   - [ ] **Did you include "How to Use", CLI commands, or usage instructions?**
      - If YES and user didn't ask for it → **REMOVE IT NOW**
      - If YES and user asked for it → verify exact commands from fetched docs before including
   - [ ] List all lumina-specific items in your answer (commands, fields, syntax, values, how to use, explanations, etc.)
   - [ ] For each item, verify it is correct according to the fetched docs. If not found, either fetch the relevant docs NOW and verify, or remove it (if user asked for it, state "I could not find documentation for [X]").

4. **Provide your answer and include a "Verification Completed" section**
   - For EACH lumina-specific item in your response, cite the specific doc file where you verified it

5. **List documentation links**
   - Only include docs actually used
   - Remove `.md` suffix from URLs
   - For local docs, cite the repository path. For hosted docs, cite the configured base URL plus the exact path from the map.
