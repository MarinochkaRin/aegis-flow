//! Requires AEGIS_TEST_DATABASE_URL pointing at a fresh disposable database
//! initialized using db/migrations/0001_init.sql. Run via scripts/worker-rust-test.sh.
use serial_test::serial;
use std::{env, time::Duration};

use async_trait::async_trait;
use flow_domain::{ActivityId, ActivityOutcome, WorkflowId};
use flow_storage_pg::{ClaimedActivity, PgStorage};
use flow_worker::{ActivityExecutor, RunResult, Worker, WorkerConfig};
use tokio_postgres::{Client, NoTls};

async fn connect(dsn: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(dsn, NoTls)
        .await
        .expect("connect to disposable test database");
    tokio::spawn(async move {
        connection.await.expect("PostgreSQL connection failed");
    });
    client
}

async fn fixture_activity(client: &Client) -> ActivityId {
    let workflow = WorkflowId::new();
    let activity = ActivityId::new();
    client
        .execute(
            "INSERT INTO aegis.workflows(id, state) VALUES ($1, 'Running')",
            &[&workflow.as_uuid()],
        )
        .await
        .unwrap();
    client
        .execute(
            "INSERT INTO aegis.activities(id, workflow_id, effect_policy) \
             VALUES ($1, $2, 'SafeToRetry')",
            &[&activity.as_uuid(), &workflow.as_uuid()],
        )
        .await
        .unwrap();
    activity
}

struct DelayedExecutor {
    delay: Duration,
    outcome: ActivityOutcome,
}

#[async_trait]
impl ActivityExecutor for DelayedExecutor {
    async fn execute(&self, _activity: ClaimedActivity) -> ActivityOutcome {
        tokio::time::sleep(self.delay).await;
        self.outcome
    }
}

#[tokio::test]
#[serial]
async fn heartbeat_keeps_a_long_running_activity_owned_until_success() {
    let Ok(dsn) = env::var("AEGIS_TEST_DATABASE_URL") else {
        eprintln!("SKIP: run scripts/worker-rust-test.sh to execute the worker PostgreSQL test");
        return;
    };

    let setup = connect(&dsn).await;
    let activity = fixture_activity(&setup).await;
    let worker = Worker::new(
        PgStorage::new(connect(&dsn).await),
        DelayedExecutor {
            delay: Duration::from_millis(260),
            outcome: ActivityOutcome::Success,
        },
        WorkerConfig::new(
            "worker-integration",
            Duration::from_millis(120),
            Duration::from_millis(30),
            Duration::from_millis(50),
        )
        .unwrap(),
    );

    assert_eq!(
        worker.run_once().await.unwrap(),
        RunResult::Finished {
            activity_id: activity,
            outcome: ActivityOutcome::Success,
        }
    );

    let row = setup
        .query_one(
            "SELECT state, attempt, worker_id, lease_token, lease_until \
             FROM aegis.activities WHERE id = $1",
            &[&activity.as_uuid()],
        )
        .await
        .unwrap();
    let state: String = row.get("state");
    let attempt: i64 = row.get("attempt");
    let worker_id: Option<String> = row.get("worker_id");
    let lease_token: Option<uuid::Uuid> = row.get("lease_token");
    let lease_until: Option<chrono::DateTime<chrono::Utc>> = row.get("lease_until");

    assert_eq!(state, "Succeeded");
    assert_eq!(attempt, 1);
    assert!(worker_id.is_none());
    assert!(lease_token.is_none());
    assert!(lease_until.is_none());
}

#[tokio::test]
#[serial]
async fn retryable_failure_is_scheduled_using_database_time() {
    let Ok(dsn) = env::var("AEGIS_TEST_DATABASE_URL") else {
        eprintln!("SKIP: run scripts/worker-rust-test.sh to execute the worker PostgreSQL test");
        return;
    };

    let setup = connect(&dsn).await;
    let activity = fixture_activity(&setup).await;
    let worker = Worker::new(
        PgStorage::new(connect(&dsn).await),
        DelayedExecutor {
            delay: Duration::ZERO,
            outcome: ActivityOutcome::RetryableFailure,
        },
        WorkerConfig::new(
            "worker-retry",
            Duration::from_secs(1),
            Duration::from_millis(200),
            Duration::from_millis(75),
        )
        .unwrap(),
    );

    assert!(matches!(
        worker.run_once().await.unwrap(),
        RunResult::Finished {
            activity_id: id,
            outcome: ActivityOutcome::RetryableFailure,
        } if id == activity
    ));

    let row = setup
        .query_one(
            "SELECT state, available_at > updated_at AS delayed \
             FROM aegis.activities WHERE id = $1",
            &[&activity.as_uuid()],
        )
        .await
        .unwrap();
    let state: String = row.get("state");
    let delayed: bool = row.get("delayed");
    assert_eq!(state, "RetryWaiting");
    assert!(delayed);
}
