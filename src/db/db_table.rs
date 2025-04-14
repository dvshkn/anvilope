use anyhow::Result;
use fallible_iterator::FallibleIterator;
use rusqlite::{
    Row,
    Rows
};

/// This is for adding light ORM-esque conveniences when it makes sense
pub trait DbTable {
    const DDL: &'static str;

    fn to_struct(r: &Row) -> Result<Self, rusqlite::Error>
    where
        Self: Sized + for<'a> From<&'a Row<'a>>,
    {
        Ok(Self::from(r))
    }

    fn to_vec(rows: Result<Rows, rusqlite::Error>) -> Result<Vec<Self>>
    where
        Self: Sized + for<'a> From<&'a Row<'a>>,
    {
        Ok(rows?.map(|r| Ok(r.into())).collect()?)
    }
}
