pub mod greenmail;

use anyhow::Result;
use env_logger::{
    Env,
    Target,
};
use log::debug;

use anvilope::*;
use config::{
    AnvilopeConfig,
    RunMode,
};

pub async fn init() -> Result<()> {
    if anvilope::ANVILOPE_CONFIG.get().is_none() {
        env_logger::Builder::from_env(Env::default().default_filter_or("debug"))
            .is_test(true)
            .target(Target::Stdout)
            .init();
        anvilope::ANVILOPE_CONFIG.get_or_init(|| AnvilopeConfig::new(RunMode::Test));

        // delete the test db if it exists so we get a fresh run
        let db_path = anvilope_config!().db_path.to_owned();
        match std::fs::remove_file(db_path) {
            Ok(_) => { debug!("previous test database file removed") },
            Err(_) => { debug!("no test database file found") },
        };
        // NOTE: because of how the database file is being handled here tests
        //       need to be run serially (ie. with --test-threads=1)

        // init a new database
        anvilope::DB_POOL.get_or_init(|| db::init_pool());
        db::ensure_tables();

        // init greenmail user accounts
        greenmail::init().await?;
    }
    Ok(())
}
