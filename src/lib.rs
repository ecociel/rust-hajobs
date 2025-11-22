pub mod error;
pub mod executor;
pub mod jobs;
pub mod manager;
pub mod repo;
pub mod schedule;

use serde::{Deserialize, Serialize};
use std::fmt;

pub use jobs::{JobCfg, JobMetadata};
pub use repo::cassandra;

#[derive(PartialEq, Eq, Debug, Clone, Serialize, Deserialize)]
pub struct JobName(pub String);

impl fmt::Display for JobName {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl JobName {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
