//! Opening a `PgCatalog` with no server to reach: the refusal names the server and the
//! database and carries no credential. These run on every host.

use contextful_core::run::FailureTag;
use contextful_pg::PgCatalog;

#[test]
fn an_unreachable_server_refuses_the_open_naming_the_server_without_the_password() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let conninfo = format!("host=127.0.0.1 port={port} user=ingest password=canary-secret dbname=catalog connect_timeout=5");
    let Err(failure) = PgCatalog::connect(&conninfo) else { panic!("a closed port accepted the catalog") };
    assert_eq!(failure.tag, FailureTag::Storage);
    assert!(failure.message.contains("Postgres catalog"), "{}", failure.message);
    assert!(failure.message.contains(&format!("127.0.0.1:{port}/catalog")), "{}", failure.message);
    assert!(!failure.message.contains("canary-secret"), "{}", failure.message);
}

#[test]
fn a_malformed_connection_string_refuses_the_open_without_echoing_it() {
    let Err(failure) = PgCatalog::connect("host=db password=canary-secret port=not-a-port") else { panic!("a malformed string opened") };
    assert_eq!(failure.tag, FailureTag::Storage);
    assert!(failure.message.contains("Postgres catalog"), "{}", failure.message);
    assert!(!failure.message.contains("canary-secret"), "{}", failure.message);
}

#[test]
fn a_remote_database_cannot_disable_authenticated_transport() {
    let error = PgCatalog::validate_transport("host=database.example port=5432 user=worker sslmode=disable").unwrap_err();
    assert!(error.message.contains("DatabaseTransportUnprotected"), "{error}");
    assert!(!error.message.contains("canary-secret"), "{error}");
}

// spec: topology.coordinate.postgres-transport@2811fc15
#[test]
fn remote_host_addresses_cannot_hide_behind_absent_or_local_host_names() {
    for connection in [
        "hostaddr=203.0.113.1 sslmode=disable",
        "host=localhost hostaddr=203.0.113.1 sslmode=disable",
        "host=localhost,localhost hostaddr=127.0.0.1,203.0.113.1 sslmode=disable",
    ] {
        let error = PgCatalog::validate_transport(connection).expect_err("remote hostaddr requires authenticated TLS without connecting");
        assert!(error.message.contains("DatabaseTransportUnprotected"), "{error}");
    }
    for connection in [
        "sslmode=disable",
        "host=/tmp sslmode=disable",
        "hostaddr=127.0.0.1 sslmode=disable",
        "host=localhost hostaddr=::1 sslmode=disable",
    ] {
        PgCatalog::validate_transport(connection).expect("local sockets and every loopback address permit local plaintext");
    }
}
