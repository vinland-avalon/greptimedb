// Copyright 2023 Greptime Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common_catalog::consts::{DEFAULT_CATALOG_NAME, DEFAULT_SCHEMA_NAME};
use common_runtime::Builder as RuntimeBuilder;
use rand::rngs::StdRng;
use rand::Rng;
use rustls::client::{ServerCertVerified, ServerCertVerifier};
use rustls::{Certificate, Error, ServerName};
use servers::auth::UserProviderRef;
use servers::error::Result;
use servers::postgres::PostgresServer;
use servers::server::Server;
use servers::tls::TlsOption;
use table::test_util::MemTable;
use tokio_postgres::{Client, Error as PgError, NoTls, SimpleQueryMessage};

use crate::auth::{DatabaseAuthInfo, MockUserProvider};
use crate::create_testing_instance;

fn create_postgres_server(
    table: MemTable,
    check_pwd: bool,
    tls: TlsOption,
    auth_info: Option<DatabaseAuthInfo>,
) -> Result<Box<dyn Server>> {
    let instance = Arc::new(create_testing_instance(table));
    let io_runtime = Arc::new(
        RuntimeBuilder::default()
            .worker_threads(4)
            .thread_name("postgres-io-handlers")
            .build()
            .unwrap(),
    );
    let user_provider: Option<UserProviderRef> = if check_pwd {
        let mut provider = MockUserProvider::default();
        if let Some(info) = auth_info {
            provider.set_authorization_info(info);
        }
        Some(Arc::new(provider))
    } else {
        None
    };

    Ok(Box::new(PostgresServer::new(
        instance,
        tls,
        io_runtime,
        user_provider,
    )))
}

#[tokio::test]
pub async fn test_start_postgres_server() -> Result<()> {
    let table = MemTable::default_numbers_table();

    let pg_server = create_postgres_server(table, false, Default::default(), None)?;
    let listening = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let result = pg_server.start(listening).await;
    assert!(result.is_ok());

    let result = pg_server.start(listening).await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Postgres server has been started."));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shutdown_pg_server_range() -> Result<()> {
    assert!(test_shutdown_pg_server(false).await.is_ok());
    assert!(test_shutdown_pg_server(true).await.is_ok());
    Ok(())
}

#[tokio::test]
async fn test_schema_validating() -> Result<()> {
    async fn generate_server(auth_info: DatabaseAuthInfo<'_>) -> Result<(Box<dyn Server>, u16)> {
        let table = MemTable::default_numbers_table();
        let postgres_server =
            create_postgres_server(table, true, Default::default(), Some(auth_info))?;
        let listening = "127.0.0.1:5432".parse::<SocketAddr>().unwrap();
        let server_addr = postgres_server.start(listening).await.unwrap();
        let server_port = server_addr.port();
        Ok((postgres_server, server_port))
    }

    common_telemetry::init_default_ut_logging();
    let (pg_server, server_port) = generate_server(DatabaseAuthInfo {
        catalog: "greptime",
        schema: "public",
        username: "greptime",
    })
    .await?;

    let pass = create_plain_connection(server_port, true).await;
    assert!(pass.is_ok());
    let result = pg_server.shutdown().await;
    assert!(result.is_ok());

    let (pg_server, server_port) = generate_server(DatabaseAuthInfo {
        catalog: "greptime",
        schema: "public",
        username: "no_right_user",
    })
    .await?;

    let fail = create_plain_connection(server_port, true).await;
    assert!(fail.is_err());
    let result = pg_server.shutdown().await;
    assert!(result.is_ok());

    Ok(())
}

// #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shutdown_pg_server(with_pwd: bool) -> Result<()> {
    common_telemetry::init_default_ut_logging();

    let table = MemTable::default_numbers_table();
    let postgres_server = create_postgres_server(table, with_pwd, Default::default(), None)?;
    let result = postgres_server.shutdown().await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Postgres server is not started."));

    let listening = "127.0.0.1:5432".parse::<SocketAddr>().unwrap();
    let server_addr = postgres_server.start(listening).await.unwrap();
    let server_port = server_addr.port();

    let mut join_handles = vec![];
    for _ in 0..2 {
        join_handles.push(tokio::spawn(async move {
            for _ in 0..1000 {
                match create_plain_connection(server_port, with_pwd).await {
                    Ok(connection) => {
                        match connection
                            .simple_query("SELECT uint32s FROM numbers LIMIT 1")
                            .await
                        {
                            Ok(rows) => {
                                let result_text = unwrap_results(&rows)[0];
                                let result: i32 = result_text.parse().unwrap();
                                assert_eq!(result, 0);
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                            Err(e) => {
                                return Err(e);
                            }
                        }
                    }
                    Err(e) => {
                        return Err(e);
                    }
                }
            }
            Ok(())
        }))
    }

    tokio::time::sleep(Duration::from_millis(100)).await;
    let result = postgres_server.shutdown().await;
    assert!(result.is_ok());

    for handle in join_handles.iter_mut() {
        let result = handle.await.unwrap();
        assert!(result.is_err());
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_query_pg_concurrently() -> Result<()> {
    let server_port = start_test_server(Default::default()).await?;

    let threads = 4;
    let expect_executed_queries_per_worker = 300;
    let mut join_handles = vec![];
    for _i in 0..threads {
        join_handles.push(tokio::spawn(async move {
            let mut rand: StdRng = rand::SeedableRng::from_entropy();

            let mut client = create_plain_connection(server_port, false).await.unwrap();

            for _k in 0..expect_executed_queries_per_worker {
                let expected: u32 = rand.gen_range(0..100);
                let result: u32 = unwrap_results(
                    client
                        .simple_query(&format!(
                            "SELECT uint32s FROM numbers WHERE uint32s = {expected}"
                        ))
                        .await
                        .unwrap()
                        .as_ref(),
                )[0]
                .parse()
                .unwrap();
                assert_eq!(result, expected);

                // 1/100 chance to reconnect
                let should_recreate_conn = expected == 1;
                if should_recreate_conn {
                    client = create_plain_connection(server_port, false).await.unwrap();
                }
            }
            expect_executed_queries_per_worker
        }))
    }
    let mut total_pending_queries = threads * expect_executed_queries_per_worker;
    for handle in join_handles.iter_mut() {
        total_pending_queries -= handle.await.unwrap();
    }
    assert_eq!(0, total_pending_queries);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_server_secure_prefer_client_plain() -> Result<()> {
    common_telemetry::init_default_ut_logging();
    do_simple_query_with_secure_server(servers::tls::TlsMode::Prefer, false, false).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_server_secure_prefer_client_plain_with_pkcs8_priv_key() -> Result<()> {
    common_telemetry::init_default_ut_logging();
    do_simple_query_with_secure_server(servers::tls::TlsMode::Prefer, false, true).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_server_secure_require_client_secure() -> Result<()> {
    common_telemetry::init_default_ut_logging();
    do_simple_query_with_secure_server(servers::tls::TlsMode::Require, true, false).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_server_secure_require_client_secure_with_pkcs8_priv_key() -> Result<()> {
    common_telemetry::init_default_ut_logging();
    do_simple_query_with_secure_server(servers::tls::TlsMode::Require, true, true).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_server_secure_require_client_plain() -> Result<()> {
    common_telemetry::init_default_ut_logging();

    let server_tls = TlsOption {
        mode: servers::tls::TlsMode::Require,
        cert_path: "tests/ssl/server.crt".to_owned(),
        key_path: "tests/ssl/server-rsa.key".to_owned(),
    };
    let server_port = start_test_server(server_tls).await?;
    let r = create_plain_connection(server_port, false).await;
    assert!(r.is_err());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_server_secure_require_client_plain_with_pkcs8_priv_key() -> Result<()> {
    common_telemetry::init_default_ut_logging();

    let server_tls = TlsOption {
        mode: servers::tls::TlsMode::Require,
        cert_path: "tests/ssl/server.crt".to_owned(),
        key_path: "tests/ssl/server-pkcs8.key".to_owned(),
    };
    let server_port = start_test_server(server_tls).await?;
    let r = create_plain_connection(server_port, false).await;
    assert!(r.is_err());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_using_db() -> Result<()> {
    let server_port = start_test_server(TlsOption::default()).await?;

    let client = create_connection_with_given_db(server_port, "testdb").await;
    assert!(client.is_err());

    let client = create_connection_without_db(server_port).await;
    assert!(client.is_err());

    let client = create_connection_with_given_db(server_port, DEFAULT_SCHEMA_NAME)
        .await
        .unwrap();
    let result = client.simple_query("SELECT uint32s FROM numbers").await;
    assert!(result.is_ok());

    let client = create_connection_with_given_catalog_schema(
        server_port,
        DEFAULT_CATALOG_NAME,
        DEFAULT_SCHEMA_NAME,
    )
    .await;
    assert!(client.is_ok());

    let client =
        create_connection_with_given_catalog_schema(server_port, "notfound", DEFAULT_SCHEMA_NAME)
            .await;
    assert!(client.is_err());

    let client =
        create_connection_with_given_catalog_schema(server_port, DEFAULT_CATALOG_NAME, "notfound")
            .await;
    assert!(client.is_err());
    Ok(())
}

async fn start_test_server(server_tls: TlsOption) -> Result<u16> {
    common_telemetry::init_default_ut_logging();
    let table = MemTable::default_numbers_table();
    let pg_server = create_postgres_server(table, false, server_tls, None)?;
    let listening = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let server_addr = pg_server.start(listening).await.unwrap();
    Ok(server_addr.port())
}

async fn do_simple_query(server_tls: TlsOption, client_tls: bool) -> Result<()> {
    let server_port = start_test_server(server_tls).await?;

    if !client_tls {
        let client = create_plain_connection(server_port, false).await.unwrap();
        let result = client.simple_query("SELECT uint32s FROM numbers").await;
        assert!(result.is_ok());
    } else {
        let client = create_secure_connection(server_port, false).await.unwrap();
        let result = client.simple_query("SELECT uint32s FROM numbers").await;
        assert!(result.is_ok());
    }

    Ok(())
}

async fn create_secure_connection(
    port: u16,
    with_pwd: bool,
) -> std::result::Result<Client, PgError> {
    let url = if with_pwd {
        format!(
            "sslmode=require host=127.0.0.1 port={port} user=greptime password=greptime connect_timeout=2, dbname={DEFAULT_SCHEMA_NAME}",
        )
    } else {
        format!("host=127.0.0.1 port={port} connect_timeout=2 dbname={DEFAULT_SCHEMA_NAME}")
    };

    let mut config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    config
        .dangerous()
        .set_certificate_verifier(Arc::new(AcceptAllVerifier {}));

    let tls = tokio_postgres_rustls::MakeRustlsConnect::new(config);
    let (client, conn) = tokio_postgres::connect(&url, tls).await.expect("connect");

    tokio::spawn(conn);
    Ok(client)
}

async fn create_plain_connection(
    port: u16,
    with_pwd: bool,
) -> std::result::Result<Client, PgError> {
    let url = if with_pwd {
        format!(
            "host=127.0.0.1 port={port} user=greptime password=greptime connect_timeout=2 dbname={DEFAULT_SCHEMA_NAME}",
        )
    } else {
        format!("host=127.0.0.1 port={port} connect_timeout=2 dbname={DEFAULT_SCHEMA_NAME}")
    };
    let (client, conn) = tokio_postgres::connect(&url, NoTls).await?;
    tokio::spawn(conn);
    Ok(client)
}

async fn create_connection_with_given_db(
    port: u16,
    db: &str,
) -> std::result::Result<Client, PgError> {
    let url = format!("host=127.0.0.1 port={port} connect_timeout=2 dbname={db}");
    let (client, conn) = tokio_postgres::connect(&url, NoTls).await?;
    tokio::spawn(conn);
    Ok(client)
}

async fn create_connection_with_given_catalog_schema(
    port: u16,
    catalog: &str,
    schema: &str,
) -> std::result::Result<Client, PgError> {
    let url = format!("host=127.0.0.1 port={port} connect_timeout=2 dbname={catalog}-{schema}");
    let (client, conn) = tokio_postgres::connect(&url, NoTls).await?;
    tokio::spawn(conn);
    Ok(client)
}

async fn create_connection_without_db(port: u16) -> std::result::Result<Client, PgError> {
    let url = format!("host=127.0.0.1 port={port} connect_timeout=2");
    let (client, conn) = tokio_postgres::connect(&url, NoTls).await?;
    tokio::spawn(conn);
    Ok(client)
}

fn resolve_result(resp: &SimpleQueryMessage, col_index: usize) -> Option<&str> {
    match resp {
        SimpleQueryMessage::Row(r) => r.get(col_index),
        _ => None,
    }
}

fn unwrap_results(resp: &[SimpleQueryMessage]) -> Vec<&str> {
    resp.iter().filter_map(|m| resolve_result(m, 0)).collect()
}

struct AcceptAllVerifier {}
impl ServerCertVerifier for AcceptAllVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &Certificate,
        _intermediates: &[Certificate],
        _server_name: &ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: SystemTime,
    ) -> std::result::Result<ServerCertVerified, Error> {
        Ok(ServerCertVerified::assertion())
    }
}

async fn do_simple_query_with_secure_server(
    server_tls_mode: servers::tls::TlsMode,
    client_tls: bool,
    is_pkcs8_priv_key: bool,
) -> Result<()> {
    let server_tls = TlsOption {
        mode: server_tls_mode,
        cert_path: "tests/ssl/server.crt".to_owned(),
        key_path: {
            if is_pkcs8_priv_key {
                "tests/ssl/server-pkcs8.key".to_owned()
            } else {
                "tests/ssl/server-rsa.key".to_owned()
            }
        },
    };

    do_simple_query(server_tls, client_tls).await
}
