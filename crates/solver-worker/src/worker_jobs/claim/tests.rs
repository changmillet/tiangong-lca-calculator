use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::pgbouncer_sqlx::postgres::PgPoolOptions;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

use super::*;
use crate::pgbouncer_sqlx::raw_query;

// Real PostgreSQL plus a simple-protocol TCP fault proxy. Never point this at a
// shared database: the runner creates a dedicated database for this test.
const TEST_DEADLINES: ClaimDeadlines = ClaimDeadlines {
    acquire: Duration::from_millis(300),
    response: Duration::from_millis(300),
    retry: Duration::from_millis(30),
};

#[derive(Clone, Copy, PartialEq)]
enum Fault {
    None,
    ClaimResponse,
    CommitResponse,
    DelayedCommit,
    CommitRollback,
}

struct Proxy {
    port: u16,
    task: JoinHandle<()>,
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn proxy(upstream_port: u16, fault: Fault) -> Proxy {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let armed = Arc::new(AtomicBool::new(true));
    let task = tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            let armed = Arc::clone(&armed);
            tokio::spawn(async move {
                let upstream = TcpStream::connect(("127.0.0.1", upstream_port))
                    .await
                    .unwrap();
                let (mut cr, mut cw) = client.into_split();
                let (mut sr, mut sw) = upstream.into_split();
                let mute = Arc::new(AtomicBool::new(false));
                let mute_reader = Arc::clone(&mute);
                let to_server = async move {
                    let size = cr.read_u32().await?;
                    let mut startup = vec![0; (size - 4) as usize];
                    cr.read_exact(&mut startup).await?;
                    sw.write_u32(size).await?;
                    sw.write_all(&startup).await?;
                    loop {
                        let tag = cr.read_u8().await?;
                        let size = cr.read_u32().await?;
                        let mut body = vec![0; (size - 4) as usize];
                        cr.read_exact(&mut body).await?;
                        let query = String::from_utf8_lossy(&body);
                        let matches = tag == b'Q'
                            && match fault {
                                Fault::ClaimResponse => query.contains("private.worker_claim_jobs"),
                                Fault::CommitResponse
                                | Fault::DelayedCommit
                                | Fault::CommitRollback => query.starts_with("COMMIT"),
                                Fault::None => false,
                            };
                        if matches && armed.swap(false, Ordering::SeqCst) {
                            if fault == Fault::CommitRollback {
                                return Ok(());
                            }
                            mute.store(true, Ordering::SeqCst);
                            if fault == Fault::DelayedCommit {
                                // Forward the old COMMIT only after the caller's
                                // deadline: reconciliation must wait for its lock.
                                sleep(Duration::from_millis(750)).await;
                            }
                        }
                        sw.write_u8(tag).await?;
                        sw.write_u32(size).await?;
                        sw.write_all(&body).await?;
                    }
                    #[allow(unreachable_code)]
                    Ok::<(), std::io::Error>(())
                };
                let to_client = async move {
                    let mut bytes = [0; 8192];
                    loop {
                        let n = sr.read(&mut bytes).await?;
                        if n == 0 {
                            break;
                        }
                        if !mute_reader.load(Ordering::SeqCst) {
                            cw.write_all(&bytes[..n]).await?;
                        }
                    }
                    Ok::<(), std::io::Error>(())
                };
                tokio::select! { _ = to_server => (), _ = to_client => () }
            });
        }
    });
    Proxy { port, task }
}

async fn setup(pool: &PgPool) {
    raw_query(include_str!("fixture.sql"))
        .execute(pool)
        .await
        .unwrap();
}

async fn enqueue(pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    raw_query(&format!(
        "INSERT INTO private.worker_jobs(id) VALUES ('{id}')"
    ))
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn state(pool: &PgPool, id: Uuid) -> (String, i64) {
    let row = raw_query(&format!(
        "SELECT status, attempt_count FROM private.worker_jobs WHERE id='{id}'"
    ))
    .fetch_one(pool)
    .await
    .unwrap();
    (row.get("status"), row.get("attempt_count"))
}

async fn reproduce_unbounded_legacy_claim(direct: &PgPool, port: u16, database: &str) {
    let id = enqueue(direct).await;
    let proxy = proxy(port, Fault::ClaimResponse).await;
    let url = format!(
        "postgres://postgres@127.0.0.1:{}/{database}?sslmode=disable",
        proxy.port
    );
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let connection = pool.acquire().await.unwrap();
    let mut connection = connection.detach();
    // This is the previous autocommit SELECT with no response deadline. Only
    // the test's external watchdog releases it, after PostgreSQL granted a lease.
    assert!(
        timeout(
            Duration::from_millis(600),
            raw_query("SELECT private.worker_claim_jobs('package','legacy',1,60) AS result")
                .fetch_one(&mut connection)
        )
        .await
        .is_err()
    );
    assert_eq!(state(direct, id).await, ("running".into(), 1));
    drop(connection);
    pool.close().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Sequential scenarios share one disposable database.
#[ignore = "requires dedicated PostgreSQL; run scripts/test_worker_claim_recovery.sh"]
async fn postgres_fault_recovery() {
    let port: u16 = std::env::var("WORKER_CLAIM_TEST_PORT")
        .expect("isolated test port")
        .parse()
        .unwrap();
    let database = std::env::var("WORKER_CLAIM_TEST_DATABASE").expect("dedicated test database");
    assert!(database.starts_with("worker_claim_test_"));
    let url = |port| format!("postgres://postgres@127.0.0.1:{port}/{database}?sslmode=disable");
    let direct = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url(port))
        .await
        .unwrap();
    setup(&direct).await;
    reproduce_unbounded_legacy_claim(&direct, port, &database).await;
    for fault in [
        Fault::None,
        Fault::ClaimResponse,
        Fault::CommitResponse,
        Fault::DelayedCommit,
        Fault::CommitRollback,
    ] {
        raw_query("TRUNCATE private.worker_jobs")
            .execute(&direct)
            .await
            .unwrap();
        let id = enqueue(&direct).await;
        let proxy = proxy(port, fault).await;
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url(proxy.port))
            .await
            .unwrap();
        // An active job's connection must survive discarding the claim socket.
        let mut active = pool.acquire().await.unwrap();
        let result = timeout(
            Duration::from_secs(5),
            claim(&pool, "package", "test", 1, 60, TEST_DEADLINES),
        )
        .await
        .unwrap();
        if fault == Fault::ClaimResponse {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("worker_claim_response_timeout")
            );
            // The next poll can recover immediately without consuming a lease.
            let jobs = claim(&pool, "package", "test", 1, 60, TEST_DEADLINES)
                .await
                .unwrap();
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0].id, id);
        } else if fault == Fault::CommitRollback {
            assert!(result.unwrap().is_empty());
            let jobs = claim(&pool, "package", "test", 1, 60, TEST_DEADLINES)
                .await
                .unwrap();
            assert_eq!(jobs[0].id, id);
        } else {
            let jobs = result.unwrap();
            assert_eq!(jobs.len(), 1);
            assert_eq!(jobs[0].id, id);
        }
        assert_eq!(state(&direct, id).await, ("running".into(), 1));
        raw_query("SELECT 1").execute(&mut *active).await.unwrap();
        drop(active);
        assert!(
            claim(&pool, "package", "test", 1, 60, TEST_DEADLINES)
                .await
                .unwrap()
                .is_empty()
        );
        pool.close().await;
    }

    // Cancellation before COMMIT also discards/rolls back the claim socket.
    raw_query("TRUNCATE private.worker_jobs")
        .execute(&direct)
        .await
        .unwrap();
    let id = enqueue(&direct).await;
    let proxy = proxy(port, Fault::ClaimResponse).await;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url(proxy.port))
        .await
        .unwrap();
    assert!(
        timeout(
            Duration::from_millis(100),
            claim(&pool, "package", "test", 1, 60, DEADLINES)
        )
        .await
        .is_err()
    );
    let jobs = claim(&pool, "package", "test", 1, 60, TEST_DEADLINES)
        .await
        .unwrap();
    assert_eq!(jobs[0].id, id);
    assert_eq!(state(&direct, id).await, ("running".into(), 1));

    // Acquisition has a separate error and must not evict a busy connection.
    let mut held = pool.acquire().await.unwrap();
    let error = claim(&pool, "package", "test", 1, 60, TEST_DEADLINES)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("worker_claim_acquire_timeout"));
    raw_query("SELECT 1").execute(&mut *held).await.unwrap();
    drop(held);

    // Replaced, expired and rolled-back leases may not be executed.
    raw_query(&format!(
        "UPDATE private.worker_jobs SET lease_token=gen_random_uuid() WHERE id='{id}'"
    ))
    .execute(&direct)
    .await
    .unwrap();
    assert!(
        reconcile(&pool, &jobs, TEST_DEADLINES)
            .await
            .unwrap()
            .is_empty()
    );
    raw_query(&format!("UPDATE private.worker_jobs SET lease_token='{}', lease_expires_at=now()-interval '1 second' WHERE id='{id}'", jobs[0].lease_token))
        .execute(&direct).await.unwrap();
    assert!(
        reconcile(&pool, &jobs, TEST_DEADLINES)
            .await
            .unwrap()
            .is_empty()
    );
    raw_query(&format!(
        "UPDATE private.worker_jobs SET status='queued', lease_token=NULL WHERE id='{id}'"
    ))
    .execute(&direct)
    .await
    .unwrap();
    assert!(
        reconcile(&pool, &jobs, TEST_DEADLINES)
            .await
            .unwrap()
            .is_empty()
    );
    pool.close().await;
    direct.close().await;
}
