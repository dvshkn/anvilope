//! Tests for the generalized config file format ([classifiers.<name>] with
//! per-account classifiers/actions and namespaced class keys).
//!
//! These tests do not require any external services (no Greenmail, no LLM
//! server). They read `tests/data/anvilope.toml` and operate on
//! `tests/data/anvilope.sqlite`, so they must NOT be run concurrently with
//! the integration tests (`cargo itest`).

use anyhow::Result;

use anvilope::*;
use config::{
    AnvilopeConfig,
    RunMode,
};

#[test]
fn test_config_parse_and_validate() -> Result<()> {
    let config = AnvilopeConfig::new(RunMode::Test);
    config.validate()?;

    // accounts
    assert_eq!(config.accounts.len(), 1);
    let acct = &config.accounts[0];
    assert_eq!(acct._key, "greenmail");
    assert_eq!(acct.classifiers, vec!["categories".to_string()]);
    assert_eq!(acct.actions.len(), 2);

    // actions are typed and reference namespaced classes
    let config::Action::Label { name, if_class } = &acct.actions[0];
    assert_eq!(name, "anv_personal");
    assert_eq!(if_class, "categories.personal");
    let config::Action::Label { name, if_class } = &acct.actions[1];
    assert_eq!(name, "anv_marketing");
    assert_eq!(if_class, "categories.spam");

    // classifiers own their classes
    assert_eq!(config.classifiers.len(), 1);
    let classifier = &config.classifiers[0];
    assert_eq!(classifier._key, "categories");
    assert_eq!(classifier.classes.len(), 2);
    assert_eq!(classifier.classes[0]._key, "personal");
    assert_eq!(classifier.classes[1]._key, "spam");

    // namespaced class key helper
    assert_eq!(classifier.class_key(&classifier.classes[0]._key), "categories.personal");
    assert_eq!(classifier.class_key(&classifier.classes[1]._key), "categories.spam");

    Ok(())
}

#[test]
fn test_config_validate_rejects_bad_classifier_reference() -> Result<()> {
    // a config where an account references an unknown classifier must fail validation
    let mut config = AnvilopeConfig::new(RunMode::Test);
    config.accounts[0].classifiers = vec!["nonexistent".to_string()];
    assert!(config.validate().is_err());

    // an account with no classifiers declared must fail validation
    let mut config = AnvilopeConfig::new(RunMode::Test);
    config.accounts[0].classifiers = vec![];
    assert!(config.validate().is_err());

    // an action referencing an unknown class must fail validation
    let mut config = AnvilopeConfig::new(RunMode::Test);
    config.accounts[0].actions = vec![
        config::Action::Label {
            name: "anv_personal".to_string(),
            if_class: "categories.does_not_exist".to_string(),
        },
    ];
    assert!(config.validate().is_err());

    Ok(())
}

#[tokio::test]
async fn test_db_sync_namespaced_classes() -> Result<()> {
    // start from a fresh database
    let config = AnvilopeConfig::new(RunMode::Test);
    std::fs::remove_file(&config.db_path).ok();

    // initialize globals the same way the integration test utils do
    if ANVILOPE_CONFIG.get().is_none() {
        ANVILOPE_CONFIG.get_or_init(|| AnvilopeConfig::new(RunMode::Test));
    }
    if DB_POOL.get().is_none() {
        DB_POOL.get_or_init(|| db::init_pool());
    }
    db::ensure_tables()?;

    // sync the config's prompts into the database
    let (classifier_key_id_map, class_key_id_map) = llm::Llm::sync_prompts().await?;

    // classifier records are keyed by name
    assert_eq!(classifier_key_id_map.len(), 1);
    let classifier_id = *classifier_key_id_map.get("categories").unwrap();

    // class records are keyed with the namespaced "<classifier>.<class>" key
    assert_eq!(class_key_id_map.len(), 2);
    assert!(class_key_id_map.contains_key("categories.personal"));
    assert!(class_key_id_map.contains_key("categories.spam"));

    // verify the database contents directly
    let conn = checkout_db_conn!();
    let (db_classifier_id, db_classifier_key): (i64, String) = conn.query_row(
        "SELECT id, key FROM classifier WHERE archived_at IS NULL",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(db_classifier_id, classifier_id);
    assert_eq!(db_classifier_key, "categories");

    let mut class_keys: Vec<String> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT key FROM class WHERE archived_at IS NULL")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            class_keys.push(row.get(0)?);
        }
    }
    class_keys.sort();
    assert_eq!(class_keys, vec!["categories.personal".to_string(), "categories.spam".to_string()]);

    Ok(())
}
