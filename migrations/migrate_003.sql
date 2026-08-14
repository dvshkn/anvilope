ALTER TABLE classifier ADD COLUMN key TEXT NOT NULL DEFAULT '';
CREATE INDEX idx_classifier_key ON classifier(key);
