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
