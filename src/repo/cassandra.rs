use crate::repo::cassandra::ErrorKind::{
    BindError, ColumnError, ConnectError, CreateKeySpaceError, CreateTableError, DBAuthError,
    ExecuteError, InvalidTimeStamp,
};
use cassandra_cpp::{AsRustType, BindRustType, Cluster, Session};
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;
use std::time::{Duration};
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use crate::JobName;
use tokio::sync::Mutex;
use crate::cassandra::ErrorKind::{CustomError, RowAlreadyExists};
use crate::repo::Repo;
use gethostname::gethostname;
use crate::jobs::JobStatus;
use crate::schedule::JobSchedule;


#[derive(Clone, Debug)]
pub struct TheRepository {
    session: Session,
    hostname: String,
}

impl TheRepository {
    pub async fn new(
        uri: String,
        credentials: Option<(String, String)>,
    ) -> Result<Self, RepoError> {
        let (user, password) = credentials.ok_or_else(|| RepoError {
            target: uri.clone(),
            kind: ErrorKind::InvalidCredentialsError,
        })?;

        let mut cluster = Cluster::default();

        cluster
            .set_credentials(user.as_str(), password.as_str())
            .map_err(|e| RepoError {
                target: uri.clone(),
                kind: DBAuthError(e.into()),
            })?;

        cluster
            .set_contact_points(uri.as_str())
            .map_err(|e| RepoError {
                target: uri.clone(),
                kind: ConnectError(e.into()),
            })?;

        let session = cluster.connect().await.map_err(|e| RepoError {
            target: uri.clone(),
            kind: ConnectError(e.into()),
        })?;
        let hostname = gethostname().into_string().unwrap_or_else(|_| "unknown-host".to_string());

        Ok(Self { session, hostname })
    }

}

#[async_trait]
impl Repo for TheRepository {
    async fn create_job(
        &self,
        name: &JobName,
        backoff_duration: Duration,
        check_interval: Duration,
        last_run: DateTime<Utc>,
        lock_ttl: Duration,
        retry_attempts: u32,
        max_retries: u32,
        schedule: JobSchedule,
        state: Arc<Mutex<Vec<u8>>>,
        status: JobStatus,
    ) -> Result<(), RepoError> {
        let mut statement = self
            .session
            .statement("INSERT INTO job.jobs (name, backoff_duration, check_interval, last_run, lock_ttl, max_retries, retry_attempts, schedule, state, status) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?);");

        statement.bind(0, name.0.as_str()).map_err(|e| RepoError {
            target: name.0.clone(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        let backoff_secs = backoff_duration.as_secs() as i64;
        statement.bind(1, backoff_secs).map_err(|e| RepoError {
            target: "backoff_duration".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        let check_interval_secs = check_interval.as_secs() as i64;
        statement.bind(2, check_interval_secs).map_err(|e| RepoError {
            target: "check_interval".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        let last_run_epoch = last_run.naive_utc().and_utc().timestamp();
        statement.bind(3, last_run_epoch).map_err(|e| RepoError {
            target: "last_run".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        let lock_ttl_secs = lock_ttl.as_secs() as i32;
        statement.bind(4, lock_ttl_secs).map_err(|e| RepoError {
            target: "lock_ttl".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        statement.bind(5, max_retries as i32).map_err(|e| RepoError {
            target: "max_retries".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        statement.bind(6, retry_attempts as i32).map_err(|e| RepoError {
            target: "retry_attempts".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;
        statement.bind(7, schedule.to_string().as_str()).map_err(|e| RepoError {
            target: "schedule".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        let state_bytes = state.lock().await.clone();
        statement.bind(8, state_bytes).map_err(|e| RepoError {
            target: "state".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        statement.bind(9, status.to_string().as_str()).map_err(|e| RepoError {
            target: "status".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        let result = statement.execute().await.map_err(|e| RepoError {
            target: "job.jobs".to_string(),
            kind: ErrorKind::ExecuteError(e.into()),
        })?;

        if let Some(row) = result.first_row() {
            let app: bool = row.get_by_name("[applied]").map_err(|e| RepoError {
                target: "jobs - [applied] status".to_string(),
                kind: ExecuteError(e.into()),
            })?;

            if !app {
                return Err(RepoError {
                    target: name.0.clone(),
                    kind: RowAlreadyExists,
                });
            }
        }

        Ok(())
    }

    async fn update_lock_ttl(&self, name: &str, ttl: Duration) -> Result<(), RepoError> {
        let query = "
        UPDATE job.jobs
        SET lock_ttl = ?
        WHERE name = ? IF lock_status = 'LOCKED'";

        let mut statement = self.session.statement(query);
        let ttl_seconds = ttl.as_secs() as i32;
        statement.bind(0, ttl_seconds).map_err(|e| RepoError {
            target: "lock_ttl".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;
        statement.bind(1, name).map_err(|e| RepoError {
            target: "name".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;
        let result = statement.execute().await.map_err(|e| RepoError {
            target: "job.jobs".to_string(),
            kind: ErrorKind::ExecuteError(e.into()),
        })?;

        if let Some(row) = result.first_row() {
            let app: bool = row.get_by_name("[applied]").map_err(|e| RepoError {
                target: "jobs - [applied] status".to_string(),
                kind: ExecuteError(e.into()),
            })?;

            if !app {
                return Err(RepoError {
                    target: name.parse().unwrap(),
                    kind: RowAlreadyExists,
                });
            }
        }

        Ok(())
    }

    //TODO: Rename the query if required or choose state or status ???
    async fn save_and_commit_state(&self, name: &JobName, status: JobStatus, state: Vec<u8>, last_run: DateTime<Utc>) -> Result<(), RepoError> {
        let mut statement = self.session.statement(
            "SELECT name FROM job.jobs WHERE name = ?;",
        );
        statement.bind(0, name.0.as_str()).map_err(|e| {
            RepoError {
                target: name.0.clone(),
                kind: ErrorKind::BindError(e.into()),
            }
        })?;

        let result = statement.execute().await.map_err(|e| {
            RepoError {
                target: "job.jobs".to_string(),
                kind: ErrorKind::ExecuteError(e.into()),
            }
        })?;

        if result.first_row().is_some() {
            let mut update_statement = self.session.statement(
                "UPDATE job.jobs SET state = ? ,status = ?, last_run = ? WHERE name = ?;",
            );
            update_statement.bind(0, state).map_err(|e| {
                RepoError {
                    target: "state".to_string(),
                    kind: ErrorKind::BindError(e.into()),
                }
            })?;
            let status_str = status.to_string();
            update_statement.bind(1, status_str.as_str()).map_err(|e| {
                RepoError {
                    target: "status".to_string(),
                    kind: ErrorKind::BindError(e.into()),
                }
            })?;
            //let last_run_timestamp = last_run.timestamp_millis() as i64;
            let last_run = Utc::now().timestamp_millis();
            update_statement.bind(2, last_run).map_err(|e| {
                RepoError {
                    target: last_run.to_string(),
                    kind: ErrorKind::BindError(e.into()),
                }
            })?;
            update_statement.bind(3, name.0.as_str()).map_err(|e| {
                RepoError {
                    target: name.0.clone(),
                    kind: ErrorKind::BindError(e.into()),
                }
            })?;


            update_statement.execute().await.map_err(|e| {
                RepoError {
                    target: "job.jobs".to_string(),
                    kind: ErrorKind::ExecuteError(e.into()),
                }
            })?;
        } else {
            eprintln!("Job not found. Cannot update state for non-existing job: {:?}", name);
            return Err(RepoError {
                target: name.0.clone(),
                kind: ErrorKind::NotFound,
            });
        }

        Ok(())
    }
    async fn acquire_lock(&self, name: &str) -> Result<bool, RepoError> {
        let exists_query = "SELECT name, owner ,lock_status FROM job.jobs WHERE name = ?";
        let mut check_statement = self.session.statement(exists_query);
        check_statement.bind(0, name).map_err(|e| RepoError {
            target: "name".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        let exists_result = check_statement.execute().await.map_err(|e| RepoError {
            target: "job.jobs".to_string(),
            kind: ErrorKind::ExecuteError(e.into()),
        })?;

        if let Some(row) = exists_result.first_row() {
            let lock_status: Option<String> = row.get_by_name("lock_status").ok();

            if lock_status.as_deref() == Some("LOCKED") {
                return Ok(true);
            } else {
                let update_query = "UPDATE job.jobs SET lock_status = 'LOCKED', owner = ? ,lock_timestamp = toTimestamp(now()) WHERE name = ? IF lock_status  IN ('UNLOCKED', null)";
                let mut update_statement = self.session.statement(update_query);
                update_statement.bind(0, self.hostname.as_str()).map_err(|e| RepoError {
                    target: "owner".to_string(),
                    kind: ErrorKind::BindError(e.into()),
                })?;
                update_statement.bind(1, name).map_err(|e| RepoError {
                    target: "name".to_string(),
                    kind: ErrorKind::BindError(e.into()),
                })?;

                let update_result = update_statement.execute().await.map_err(|e| RepoError {
                    target: "job.jobs".to_string(),
                    kind: ErrorKind::ExecuteError(e.into()),
                })?;

                if let Some(row) = update_result.first_row() {
                    let applied: bool = row.get_by_name("[applied]").map_err(|e| RepoError {
                        target: "job.jobs - [applied] status".to_string(),
                        kind: ErrorKind::ColumnError(e.into()),
                    })?;
                    return Ok(applied);
                }
            }
        } else {
            let insert_query = "INSERT INTO job.jobs (name, owner, lock_status, lock_timestamp) VALUES (?, ?, ?, toTimestamp(now())) IF NOT EXISTS";
            let mut insert_statement = self.session.statement(insert_query);

            insert_statement.bind(0, name).map_err(|e| RepoError {
                target: "name".to_string(),
                kind: ErrorKind::BindError(e.into()),
            })?;
            insert_statement.bind(1, self.hostname.as_str()).map_err(|e| RepoError {
                target: "owner".to_string(),
                kind: ErrorKind::BindError(e.into()),
            })?;

            insert_statement.bind(2, "LOCKED").map_err(|e| RepoError {
                target: "lock_status".to_string(),
                kind: ErrorKind::BindError(e.into()),
            })?;

            let insert_result = insert_statement.execute().await.map_err(|e| RepoError {
                target: "job.jobs".to_string(),
                kind: ErrorKind::ExecuteError(e.into()),
            })?;

            if let Some(row) = insert_result.first_row() {
                let applied: bool = row.get_by_name("[applied]").map_err(|e| RepoError {
                    target: "job.jobs - [applied] status".to_string(),
                    kind: ErrorKind::ColumnError(e.into()),
                })?;
                return Ok(applied);
            }
        }

        Err(RepoError {
            target: name.to_string(),
            kind: ErrorKind::AcquireLockFailed("Failed to acquire lock due to unexpected error".to_string()),
        })
    }

    async fn release_lock(&self, name: &str) -> Result<(), RepoError> {
        let query = "UPDATE job.jobs
                SET owner = NULL, lock_status = 'UNLOCKED'
                WHERE name = ?
                IF lock_status = 'LOCKED' AND owner != NULL";

        let mut statement = self.session.statement(query);
        statement.bind(0, name).map_err(|e| RepoError {
            target: "name".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        let result = statement.execute().await.map_err(|e| RepoError {
            target: "job.jobs".to_string(),
            kind: ErrorKind::ExecuteError(e.into()),
        })?;

        if let Some(row) = result.first_row() {
            let applied: bool = row.get_by_name("[applied]").map_err(|e| RepoError {
                target: "job.jobs - [applied] status".to_string(),
                kind: ErrorKind::BindError(e.into()),
            })?;

            if applied {
                return Ok(());
            }
        }
        let query_null_case = "UPDATE job.jobs
                SET owner = NULL, lock_status = 'UNLOCKED'
                WHERE name = ?
                IF lock_status = NULL AND owner = NULL";

        let mut statement_null = self.session.statement(query_null_case);
        statement_null.bind(0, name).map_err(|e| RepoError {
            target: "name".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        let result_null = statement_null.execute().await.map_err(|e| RepoError {
            target: "job.jobs".to_string(),
            kind: ErrorKind::ExecuteError(e.into()),
        })?;

        if let Some(row) = result_null.first_row() {
            let applied: bool = row.get_by_name("[applied]").map_err(|e| RepoError {
                target: "job.jobs - [applied] status".to_string(),
                kind: ErrorKind::BindError(e.into()),
            })?;

            if applied {
                return Ok(());
            }
        }
        Err(RepoError {
            target: name.to_string(),
            kind: ErrorKind::AcquireLockFailed(
                "Failed to release lock: lock_status did not match.".to_string(),
            ),
        })
    }


    async fn get_last_run_time(&self, job_name: &str) -> Result<Option<i64>, RepoError> {
        let query = "SELECT last_run FROM job.jobs WHERE name = ?";

        let mut statement = self.session.statement(query);
        statement.bind(0, job_name).map_err(|e| RepoError {
            target: "job_name".to_string(),
            kind: ErrorKind::BindError(e.into()),
        })?;

        let result = statement.execute().await.map_err(|e| RepoError {
            target: "job.jobs".to_string(),
            kind: ErrorKind::ExecuteError(e.into()),
        })?;

        if let Some(row) = result.first_row() {
            let last_run_timestamp: i64 = row.get_by_name("last_run").map_err(|e| RepoError {
                target: "job.jobs - last_run".to_string(),
                kind: ErrorKind::ColumnError(e.into()),
            })?;

            let last_run = match Utc.timestamp_opt(last_run_timestamp, 0) {
                chrono::LocalResult::Single(dt) => dt,
                _ => {
                    return Err(RepoError {
                        target: "job.jobs - last_run".to_string(),
                        kind: ErrorKind::CustomError("Invalid timestamp".to_string()),
                    });
                }
            };
            return Ok(Some(last_run.timestamp_millis()));
        }
        Ok(None)
    }
}

#[derive(Debug)]
pub enum LoadError {
    InnerDBError(String),
    InvalidTimestamp(String),
}

impl Display for LoadError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::InnerDBError(msg) => write!(f, "Cassandra error: {}", msg),
            LoadError::InvalidTimestamp(msg) => write!(f, "Invalid timestamp: {}", msg),
        }
    }
}

impl Error for LoadError {}

impl From<cassandra_cpp::Error> for LoadError {
    fn from(error: cassandra_cpp::Error) -> Self {
        LoadError::InnerDBError(error.to_string())
    }
}

#[derive(Debug)]
pub struct CassandraErrorKind(String);



impl From<cassandra_cpp::Error> for CassandraErrorKind {
    fn from(error: cassandra_cpp::Error) -> Self {
        CassandraErrorKind(format!("{}", error))
    }
}


pub struct RepoError {
    target: String,
    kind: ErrorKind,
}

#[derive(Debug)]
pub enum ErrorKind {
    ConnectError(CassandraErrorKind),
    DBAuthError(CassandraErrorKind),
    InvalidCredentialsError,
    InvalidTimeStamp,
    CreateKeySpaceError(CassandraErrorKind),
    CreateTableError(CassandraErrorKind),
    BindError(CassandraErrorKind),
    ExecuteError(CassandraErrorKind),
    ColumnError(CassandraErrorKind),
    RowAlreadyExists,
    InvalidConfig(String),
    AcquireLockFailed(String),
    //UpdateLockTtlFailed(CassandraErrorKind),
    CustomError(String),
    NotFound,
}



impl Display for RepoError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            ConnectError(err) => write!(
                f,
                "Failed to connect or initialize repo for database - {}: {}",
                self.target, err
            ),
            DBAuthError(err) => write!(
                f,
                "Authentication/credentials error for database - {}: {}",
                self.target, err
            ),
            ErrorKind::InvalidCredentialsError => write!(
                f,
                "Invalid credentials provided for database - {}",
                self.target
            ),
            CreateKeySpaceError(err) => write!(f, "Error creating keyspace: {}", err),
            CreateTableError(err) => write!(f, "Error creating table: {}", err),
            BindError(err) => write!(f, "error in binding type:{}-{}", self.target, err),
            ExecuteError(err) => write!(f, "error in executing query:{}-{}", self.target, err),
            ColumnError(err) => write!(f, "error in fetching column:{}-{}", self.target, err),
            InvalidTimeStamp => write!(f, "Invalid timestamp:{}", self.target),
            //UpdateLockTtlFailed => write!(f,"Failed to update lock to ttl"),
            CustomError(err) => write!(f,"Error: {}", err),
            ErrorKind::RowAlreadyExists => write!(
                f,
                "Row with the given ID already exists in the database: {}",
                self.target
            ),
            ErrorKind::InvalidConfig(msg) => write!(
                f,
                "Invalid configuration for target: {}. Reason: {}",
                self.target, msg
            ),
            ErrorKind::AcquireLockFailed(msg) => write!(
                f,
                "Lock acquire failed: {}. Reason: {}",
                self.target,msg
            ),
            ErrorKind::NotFound => write!(
                f,
                "The requested item was not found in the database: {}",
                self.target
            ),
        }
    }
}

impl Debug for RepoError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RepoError {{ target: {}, kind: {:?} }}",
            self.target, self.kind
        )
    }
}

impl Error for RepoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        None
    }
}

impl Display for CassandraErrorKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Error for CassandraErrorKind {}
