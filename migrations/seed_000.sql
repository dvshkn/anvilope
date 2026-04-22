CREATE TABLE class (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL,
    prompt TEXT NOT NULL,
    archived_at INTEGER,
    created_at INTEGER DEFAULT(unixepoch('subsec') * 1000) NOT NULL
);
CREATE INDEX idx_class_key ON class(key);
CREATE INDEX idx_class_archived_at ON class(archived_at);

CREATE TABLE classifier (
    id INTEGER PRIMARY KEY,
    prompt TEXT NOT NULL,
    archived_at INTEGER,
    created_at INTEGER DEFAULT(unixepoch('subsec') * 1000) NOT NULL
);
CREATE INDEX idx_classifier_archived_at ON classifier(archived_at);

CREATE TABLE llm_completion (
    id INTEGER PRIMARY KEY,
    message_id INTEGER NOT NULL,
    classifier_id INTEGER NOT NULL,
    llm_response TEXT NOT NULL,
    endpoint_response TEXT NOT NULL,
    class_id INTEGER,
    class_key TEXT,
    created_at INTEGER DEFAULT(unixepoch('subsec') * 1000) NOT NULL,
    FOREIGN KEY(message_id) REFERENCES message(id),
    FOREIGN KEY(classifier_id) REFERENCES classifier(id),
    FOREIGN KEY(class_id) REFERENCES class(id)
);

CREATE TABLE llm_job (
    id INTEGER PRIMARY KEY,
    message_id INTEGER NOT NULL,
    status TEXT NOT NULL,
    created_at INTEGER DEFAULT(unixepoch('subsec') * 1000) NOT NULL,
    FOREIGN KEY(message_id) REFERENCES message(id)
);
CREATE INDEX idx_llm_job_status ON llm_job(status);

CREATE TABLE message (
    id INTEGER PRIMARY KEY,
    account_key TEXT NOT NULL,
    class_key TEXT,
    from_address TEXT NOT NULL,
    from_name TEXT NOT NULL,
    subject TEXT NOT NULL,
    body_plain TEXT NOT NULL,
    body_html TEXT NOT NULL,
    uid INTEGER NOT NULL,
    uid_validity INTEGER NOT NULL,
    created_at INTEGER DEFAULT(unixepoch('subsec') * 1000) NOT NULL
);
CREATE INDEX idx_message_account_key ON message(account_key);
CREATE INDEX idx_message_class_key ON message(class_key);

CREATE TABLE meta (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    updated_at INTEGER DEFAULT(unixepoch('subsec') * 1000) NOT NULL,
    CONSTRAINT key_unique UNIQUE (key)
);
