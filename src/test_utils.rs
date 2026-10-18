use rand::{distr::Alphanumeric, rng, RngExt};
use std::env::temp_dir;
use std::path::PathBuf;

pub(crate) fn random_string(len: usize) -> String {
    rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect::<String>()
}

pub(crate) fn temp_file() -> PathBuf {
    temp_dir().join(format!("{}.nut.db", random_string(32)))
}
