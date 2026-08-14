-- namespace classes by classifier and record which classifier produced
-- each llm job and classification

ALTER TABLE classifier ADD COLUMN key TEXT NOT NULL DEFAULT 'default';
ALTER TABLE class ADD COLUMN classifier_id INTEGER NOT NULL DEFAULT 0;
-- attribute legacy classes to the (now named) legacy classifier, if one exists
UPDATE class SET classifier_id = (SELECT id FROM classifier WHERE key = 'default' ORDER BY id DESC LIMIT 1)
    WHERE classifier_id = 0
    AND EXISTS (SELECT 1 FROM classifier);
-- namespace legacy class keys
UPDATE message SET class_key = 'default.' || class_key WHERE class_key IS NOT NULL;
UPDATE llm_completion SET class_key = 'default.' || class_key WHERE class_key IS NOT NULL;
ALTER TABLE llm_job ADD COLUMN classifier_key TEXT NOT NULL DEFAULT '';
CREATE INDEX idx_class_classifier_key ON class(classifier_id, key);
