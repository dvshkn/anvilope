ALTER TABLE llm_completion DROP COLUMN endpoint_response;
ALTER TABLE llm_completion ADD COLUMN trace TEXT NOT NULL DEFAULT "[]";
