PRAGMA foreign_keys = OFF;

BEGIN TRANSACTION;

CREATE TABLE llm_completion_new (
    id INTEGER PRIMARY KEY,
    message_id INTEGER NOT NULL,
    llm_response TEXT NOT NULL,
    trace TEXT NOT NULL,
    classifier_key TEXT NOT NULL,
    class_key TEXT,
    created_at INTEGER DEFAULT(unixepoch('subsec') * 1000) NOT NULL,
    FOREIGN KEY(message_id) REFERENCES message(id)
);

INSERT INTO llm_completion_new (
    id,
    message_id,
    llm_response,
    trace,
    classifier_key,
    class_key,
    created_at
) SELECT id, message_id, llm_response, trace, "default", class_key, created_at FROM llm_completion;

DROP TABLE llm_completion;
ALTER TABLE llm_completion_new RENAME TO llm_completion;

COMMIT;

PRAGMA foreign_keys = ON;
