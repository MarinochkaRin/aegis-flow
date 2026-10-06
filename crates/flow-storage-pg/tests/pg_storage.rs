//! Requires AEGIS_TEST_DATABASE_URL pointing at a fresh disposable database
//! initialized using db/migrations/0001_init.sql. Run via scripts/pg-rust-test.sh.

use std::{env, time::Duration};

use flow_domain::{ActivityId, ActivityOutcome, LeaseToken, WorkflowId};
use flow_storage_pg::PgStorage;
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

#[tokio::test]
async fn two_clients_observe_database_fencing_and_parallel_claims() {
    let Ok(dsn) = env::var("AEGIS_TEST_DATABASE_URL") else {
        eprintln!("SKIP: run scripts/pg-rust-test.sh to execute the PostgreSQL integration test");
        return;
    };

    let setup = connect(&dsn).await;
    let single = fixture_activity(&setup).await;
    let a = PgStorage::new(connect(&dsn).await);
    let b = PgStorage::new(connect(&dsn).await);

    // Two independent connections claim the same one eligible activity.
    let (left, right) = tokio::join!(
        a.claim_activity("rust-worker-a", Duration::from_secs(60)),
        b.claim_activity("rust-worker-b", Duration::from_secs(60)),
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert_eq!(left.is_some() as u8 + right.is_some() as u8, 1);
    let claim = left.or(right).unwrap();
    assert_eq!(claim.activity_id, single);
    assert_eq!(claim.attempt.value(), 1);

    // A foreign token cannot renew or finish another worker's lease.
    assert!(
        a.heartbeat_activity(single, LeaseToken::new(), Duration::from_secs(60))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        !b.finish_activity(single, LeaseToken::new(), ActivityOutcome::Success, None)
            .await
            .unwrap()
    );
    assert!(
        a.heartbeat_activity(single, claim.lease_token, Duration::from_secs(60))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        a.finish_activity(single, claim.lease_token, ActivityOutcome::Success, None)
            .await
            .unwrap()
    );
    assert!(
        !a.finish_activity(single, claim.lease_token, ActivityOutcome::Success, None)
            .await
            .unwrap()
    );

    let audit: i64 = setup
        .query_one(
            "SELECT count(*) FROM aegis.events \
             WHERE activity_id = $1 AND event_type = 'ActivityClaimed'",
            &[&single.as_uuid()],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(audit, 1);

    // Separate claims can proceed against separate eligible activities.
    let first = fixture_activity(&setup).await;
    let second = fixture_activity(&setup).await;
    let (left, right) = tokio::join!(
        a.claim_activity("rust-worker-a", Duration::from_secs(60)),
        b.claim_activity("rust-worker-b", Duration::from_secs(60)),
    );
    let left = left.unwrap().expect("worker A must claim an activity");
    let right = right.unwrap().expect("worker B must claim an activity");
    assert_ne!(left.activity_id, right.activity_id);
    assert!([first, second].contains(&left.activity_id));
    assert!([first, second].contains(&right.activity_id));
}
