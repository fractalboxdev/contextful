//! The engine's query functions link into the binary, and a read reaching for an extension
//! refuses (`assurance.build`).

use contextful_context::read::{operator_query, ReadFault};

fn refusal(sql: &str) -> String {
    match operator_query(sql, Default::default()) {
        Err(ReadFault::Refused(r)) => r.identifier().to_string(),
        other => panic!("`{sql}` answered {other:?}"),
    }
}

// spec: assurance.build.linked-query-functions
#[test]
fn columnar_reading_and_statement_serialization_are_statically_linked_and_loaded() {
    let r = operator_query(
        "SELECT extension_name, loaded, install_mode FROM duckdb_extensions() WHERE extension_name IN ('parquet', 'json') ORDER BY 1",
        Default::default(),
    )
    .unwrap();
    let rows: Vec<String> = r.rows.iter().map(|row| row.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("|")).collect();
    assert_eq!(rows, ["\"json\"|true|\"STATICALLY_LINKED\"", "\"parquet\"|true|\"STATICALLY_LINKED\""], "{rows:?}");
    let serialized = operator_query("SELECT json_serialize_sql('SELECT 1') AS tree", Default::default()).unwrap();
    assert!(serialized.rows[0][0].as_str().is_some_and(|t| t.contains("\"error\":false")), "{:?}", serialized.rows);
}

// spec: assurance.build.runtime-extension-load
#[test]
fn an_explicit_load_or_install_refuses() {
    for sql in ["LOAD parquet", "LOAD 'httpfs'", "INSTALL httpfs", "FORCE INSTALL spatial", "UPDATE EXTENSIONS"] {
        assert_eq!(refusal(sql), "ExtensionAutoloadRefused", "{sql}");
    }
}

// spec: assurance.build.runtime-extension-load
#[test]
fn a_function_an_unlinked_extension_provides_refuses_rather_than_autoloading() {
    assert_eq!(refusal("SELECT * FROM sqlite_scan('x.db', 't')"), "ExtensionAutoloadRefused");
    let loaded = operator_query("SELECT count(*) FROM duckdb_extensions() WHERE loaded AND install_mode <> 'STATICALLY_LINKED'", Default::default())
        .unwrap();
    assert_eq!(loaded.rows[0][0].to_string().trim_matches('"'), "0", "no extension loads at run time");
}
