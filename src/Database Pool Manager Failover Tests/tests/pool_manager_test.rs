use db_pool_manager::db::pool_manager::{PoolManager, QueryType, PoolError};
use sqlx::{PgPool, Row};
use testcontainers::{clients::Cli, Container};
use testcontainers_modules::postgres::Postgres;
use tokio::time::{sleep, Duration};

struct TestEnvironment<'a> {
    primary: Container<'a, Postgres>,
    replica: Container<'a, Postgres>,
    primary_url: String,
    replica_url: String,
}

impl<'a> TestEnvironment<'a> {
    async fn new(docker: &'a Cli) -> Self {
        let primary = docker.run(Postgres::default());
        let replica = docker.run(Postgres::default());

        let primary_port = primary.get_host_port_ipv4(5432);
        let replica_port = replica.get_host_port_ipv4(5432);

        let primary_url = format!(
            "postgres://postgres:postgres@localhost:{}/postgres",
            primary_port
        );
        let replica_url = format!(
            "postgres://postgres:postgres@localhost:{}/postgres",
            replica_port
        );

        // Wait for databases to be ready
        sleep(Duration::from_secs(2)).await;

        Self {
            primary,
            replica,
            primary_url,
            replica_url,
        }
    }

    async fn setup_test_data(&self) -> Result<(), sqlx::Error> {
        let primary_pool = PgPool::connect(&self.primary_url).await?;
        
        sqlx::query("CREATE TABLE IF NOT EXISTS test_table (id SERIAL PRIMARY KEY, value TEXT)")
            .execute(&primary_pool)
            .await?;
        
        sqlx::query("INSERT INTO test_table (value) VALUES ('test_data')")
            .execute(&primary_pool)
            .await?;

        primary_pool.close().await;

        // Replicate data to replica
        let replica_pool = PgPool::connect(&self.replica_url).await?;
        
        sqlx::query("CREATE TABLE IF NOT EXISTS test_table (id SERIAL PRIMARY KEY, value TEXT)")
            .execute(&replica_pool)
            .await?;
        
        sqlx::query("INSERT INTO test_table (value) VALUES ('test_data')")
            .execute(&replica_pool)
            .await?;

        replica_pool.close().await;

        Ok(())
    }
}

#[tokio::test]
async fn test_read_query_routes_to_replica() {
    let docker = Cli::default();
    let env = TestEnvironment::new(&docker).await;
    env.setup_test_data().await.unwrap();

    let pool_manager = PoolManager::new(
        &env.primary_url,
        vec![env.replica_url.clone()],
        5,
    )
    .await
    .unwrap();

    // Execute a read query
    let result = pool_manager
        .execute_query(QueryType::Read, |pool| {
            Box::pin(async move {
                let row = sqlx::query("SELECT value FROM test_table WHERE id = 1")
                    .fetch_one(pool)
                    .await?;
                let value: String = row.get("value");
                Ok(value)
            })
        })
        .await
        .unwrap();

    assert_eq!(result, "test_data");
    
    // Verify replica is being used
    let healthy_replicas = pool_manager.get_healthy_replica_count().await;
    assert_eq!(healthy_replicas, 1);
}

#[tokio::test]
async fn test_write_query_routes_to_primary() {
    let docker = Cli::default();
    let env = TestEnvironment::new(&docker).await;
    env.setup_test_data().await.unwrap();

    let pool_manager = PoolManager::new(
        &env.primary_url,
        vec![env.replica_url.clone()],
        5,
    )
    .await
    .unwrap();

    // Execute a write query
    let result = pool_manager
        .execute_query(QueryType::Write, |pool| {
            Box::pin(async move {
                sqlx::query("INSERT INTO test_table (value) VALUES ('new_data')")
                    .execute(pool)
                    .await?;
                Ok(())
            })
        })
        .await;

    assert!(result.is_ok());

    // Verify data was written to primary
    let primary_pool = PgPool::connect(&env.primary_url).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM test_table")
        .fetch_one(&primary_pool)
        .await
        .unwrap();
    
    assert_eq!(count, 2);
    primary_pool.close().await;
}

#[tokio::test]
async fn test_failover_on_replica_failure() {
    let docker = Cli::default();
    let env = TestEnvironment::new(&docker).await;
    env.setup_test_data().await.unwrap();

    let pool_manager = PoolManager::new(
        &env.primary_url,
        vec![env.replica_url.clone()],
        5,
    )
    .await
    .unwrap();

    // Stop the replica container to simulate failure
    drop(env.replica);
    sleep(Duration::from_secs(1)).await;

    // Read query should failover to primary
    let result = pool_manager
        .execute_query(QueryType::Read, |pool| {
            Box::pin(async move {
                let row = sqlx::query("SELECT value FROM test_table WHERE id = 1")
                    .fetch_one(pool)
                    .await?;
                let value: String = row.get("value");
                Ok(value)
            })
        })
        .await
        .unwrap();

    assert_eq!(result, "test_data");
}

#[tokio::test]
async fn test_pool_health_checks() {
    let docker = Cli::default();
    let env = TestEnvironment::new(&docker).await;

    let pool_manager = PoolManager::new(
        &env.primary_url,
        vec![env.replica_url.clone()],
        5,
    )
    .await
    .unwrap();

    // Initial health check - all should be healthy
    let health = pool_manager.check_health().await.unwrap();
    assert!(health.primary_healthy);
    assert_eq!(health.healthy_replicas, 1);
    assert_eq!(health.total_replicas, 1);

    // Stop replica and check health again
    drop(env.replica);
    sleep(Duration::from_secs(1)).await;

    let health = pool_manager.check_health().await.unwrap();
    assert!(health.primary_healthy);
    assert_eq!(health.healthy_replicas, 0);
    assert_eq!(health.total_replicas, 1);
}

#[tokio::test]
async fn test_concurrent_query_routing() {
    let docker = Cli::default();
    let env = TestEnvironment::new(&docker).await;
    env.setup_test_data().await.unwrap();

    let pool_manager = PoolManager::new(
        &env.primary_url,
        vec![env.replica_url.clone()],
        10,
    )
    .await
    .unwrap();

    let pool_manager = std::sync::Arc::new(pool_manager);

    // Spawn multiple concurrent read queries
    let mut handles = vec![];
    for i in 0..20 {
        let pm = pool_manager.clone();
        let handle = tokio::spawn(async move {
            pm.execute_query(QueryType::Read, |pool| {
                Box::pin(async move {
                    let row = sqlx::query("SELECT value FROM test_table WHERE id = 1")
                        .fetch_one(pool)
                        .await?;
                    let value: String = row.get("value");
                    Ok(value)
                })
            })
            .await
        });
        handles.push(handle);
    }

    // Spawn concurrent write queries
    for i in 0..10 {
        let pm = pool_manager.clone();
        let handle = tokio::spawn(async move {
            pm.execute_query(QueryType::Write, |pool| {
                Box::pin(async move {
                    sqlx::query("INSERT INTO test_table (value) VALUES ('concurrent_data')")
                        .execute(pool)
                        .await?;
                    Ok(())
                })
            })
            .await
        });
        handles.push(handle);
    }

    // Wait for all queries to complete
    for handle in handles {
        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }

    // Verify all writes completed
    let primary_pool = PgPool::connect(&env.primary_url).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM test_table")
        .fetch_one(&primary_pool)
        .await
        .unwrap();
    
    assert_eq!(count, 11); // 1 initial + 10 concurrent writes
    primary_pool.close().await;
}
