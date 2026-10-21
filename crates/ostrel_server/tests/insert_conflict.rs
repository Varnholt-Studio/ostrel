//! Server side groundwork for AC-31 (D20): no upsert over foreign rows.
//!
//! The server keeps one database connection for all requests (`Server::connection`). These
//! tests drive that connection the way the op path will once it exists: a create carrying an
//! id that is live or tombstoned, in the same or in another model, fails with
//! `DbError::Conflict`; the stored rows stay exactly as they were and a tombstoned row is not
//! brought back, neither by an insert nor by an update. On SQLite the same holds after a
//! restart of the server on the same file.
//!
//! The crafted request tests of AC-31 (an op sent to a running server) need the op path and
//! the WebSocket transport and are not part of this file.

use std::future::Future;
use std::path::PathBuf;
use std::task::{Context, Poll, Waker};

use ostrel_core::FileId;
use ostrel_db::api::{
    Connection, DbError, Driver, MigrationPlan, Query, RowId, Rows, SchemaHash, Value, Write,
};
use ostrel_db_memory::MemoryDriver;
use ostrel_db_sqlite::SqliteDriver;
use ostrel_server::audit::MemoryAuditLog;
use ostrel_server::host::SystemClock;
use ostrel_server::identity::{Credentials, NoTokens};
use ostrel_server::task::{Outcome, Server};
use ostrel_vm::{Image, Limits};

/// Runs a future that never waits. Every future of both drivers is ready on first poll.
fn block_on<T>(f: impl Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("database future was not ready"),
    }
}

/// A SQLite URL for the test `name`, with no database file there yet.
fn fresh_sqlite(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ostrel_server_{name}.db"));
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(p);
    }
    format!("sqlite:{}", path.display())
}

fn image(src: &str) -> Image {
    let file = FileId::from_raw(0);
    let (tokens, lexed) = ostrel_syntax::lex::lex(src, file);
    assert!(lexed.is_empty(), "{lexed:?}");
    let (module, parsed) = ostrel_syntax::parse::parse(src, file, &tokens, &lexed);
    assert!(parsed.is_empty(), "{parsed:?}");
    let (checked, diags) = ostrel_sema::check(&module, file, src);
    assert!(diags.is_empty(), "{diags:?}");
    let program = ostrel_ir::lower(&module, &checked, file).unwrap();
    ostrel_vm::compile(&program).unwrap()
}

const PROGRAM: &str = "fn main()\n  print(\"up\")\n";

/// Starts a server for `PROGRAM` on the database at `url` and checks that it serves.
fn start(driver: &dyn Driver, url: &str) -> Server {
    let db = block_on(driver.connect(url)).unwrap();
    let mut server = Server::new(
        image(PROGRAM),
        Limits::default(),
        db,
        Box::new(MemoryAuditLog::new()),
        Box::new(SystemClock),
        Box::new(NoTokens),
    );
    assert_eq!(
        server.run(&Credentials::anonymous()),
        Ok(Outcome {
            principal: None,
            output: vec!["up".to_owned()],
        })
    );
    server
}

/// Model 1 stands for `Note` of the notes example, model 2 for any other model.
const NOTE: u32 = 1;
const OTHER: u32 = 2;

/// Field 0 is `title` in `Note` and a text field in the other model.
const TITLE: u32 = 0;

/// Live row of `Note`.
const LIVE: u128 = 1;
/// Row of `Note` that was deleted: its id stays taken.
const TOMBSTONED: u128 = 2;
/// Live row of the other model.
const FOREIGN: u128 = 3;
/// Id that no row ever had.
const UNUSED: u128 = 9;

fn rid(n: u128) -> RowId {
    RowId::from_hex(&format!("{n:032x}")).unwrap()
}

fn insert(model: u32, row: u128, title: &str) -> Write {
    Write::Insert {
        model,
        row: rid(row),
        fields: vec![(TITLE, Value::Text(title.to_owned()))],
        collections: vec![],
    }
}

fn plan() -> MigrationPlan {
    MigrationPlan {
        from: None,
        to: SchemaHash([31; 32]),
        schema: "{\"v\":31}".to_owned(),
        enums: vec![],
        steps: vec![],
    }
}

/// Stores one live `Note`, one deleted `Note` and one live row of the other model.
fn seed(db: &mut dyn Connection) {
    block_on(async {
        db.migrate(&plan()).await.unwrap();
        let mut tx = db.begin().await.unwrap();
        tx.apply(&[
            insert(NOTE, LIVE, "kept"),
            insert(NOTE, TOMBSTONED, "dropped"),
            insert(OTHER, FOREIGN, "foreign"),
        ])
        .await
        .unwrap();
        tx.apply(&[Write::Delete {
            model: NOTE,
            row: rid(TOMBSTONED),
            expect_version: 1,
        }])
        .await
        .unwrap();
        tx.commit().await.unwrap();
    });
}

/// Every live row of both models.
fn stored(db: &mut dyn Connection) -> (Rows, Rows) {
    block_on(async {
        (
            db.query(&Query::all(NOTE)).await.unwrap(),
            db.query(&Query::all(OTHER)).await.unwrap(),
        )
    })
}

fn ids(rows: &Rows) -> Vec<RowId> {
    rows.0.iter().map(|r| r.id).collect()
}

/// The checks of AC-31 against the connection of a running server, after `seed`.
fn insert_never_takes_over_a_used_id(db: &mut dyn Connection) {
    let before = stored(db);
    assert_eq!(ids(&before.0), vec![rid(LIVE)]);
    assert_eq!(ids(&before.1), vec![rid(FOREIGN)]);

    // A create with a used id fails in the model the id belongs to and in every other model.
    for model in [NOTE, OTHER] {
        for row in [LIVE, TOMBSTONED, FOREIGN] {
            block_on(async {
                let mut tx = db.begin().await.unwrap();
                assert_eq!(
                    tx.apply(&[insert(model, row, "taken over")]).await,
                    Err(DbError::Conflict),
                    "model {model} row {row}"
                );
                tx.rollback().await.unwrap();
            });
            assert_eq!(stored(db), before, "model {model} row {row}");
        }
    }

    // A conflict anywhere in a batch keeps the whole batch out, also a valid create before it.
    block_on(async {
        let mut tx = db.begin().await.unwrap();
        assert_eq!(
            tx.apply(&[
                insert(NOTE, UNUSED, "new"),
                insert(OTHER, LIVE, "taken over")
            ])
            .await,
            Err(DbError::Conflict)
        );
        tx.rollback().await.unwrap();
    });
    assert_eq!(stored(db), before);

    // An update does not bring a tombstoned row back, in its own model or another one.
    for model in [NOTE, OTHER] {
        block_on(async {
            let mut tx = db.begin().await.unwrap();
            let update = Write::Update {
                model,
                row: rid(TOMBSTONED),
                expect_version: 2,
                fields: vec![(TITLE, Value::Text("back".to_owned()))],
                collections: vec![],
            };
            assert_eq!(
                tx.apply(&[update]).await,
                Err(DbError::NotFound),
                "model {model}"
            );
            tx.rollback().await.unwrap();
        });
        assert_eq!(stored(db), before, "model {model}");
    }

    // Insert and update stay distinct: the live row is changed only by an update, and a
    // create with an id nobody used still succeeds.
    block_on(async {
        let mut tx = db.begin().await.unwrap();
        let update = Write::Update {
            model: NOTE,
            row: rid(LIVE),
            expect_version: 1,
            fields: vec![(TITLE, Value::Text("edited".to_owned()))],
            collections: vec![],
        };
        tx.apply(&[update, insert(NOTE, UNUSED, "new")])
            .await
            .unwrap();
        tx.commit().await.unwrap();
    });
    let (notes, others) = stored(db);
    assert_eq!(others, before.1);
    assert_eq!(ids(&notes), vec![rid(LIVE), rid(UNUSED)]);
    let live = &notes.0[0];
    assert_eq!(live.version, 2);
    assert_eq!(live.fields, vec![(TITLE, Value::Text("edited".to_owned()))]);
}

#[test]
fn insert_over_a_used_id_fails_through_the_server_on_memory() {
    let driver = MemoryDriver::new();
    let mut server = start(&driver, "memory:");
    seed(server.connection());
    insert_never_takes_over_a_used_id(server.connection());
}

#[test]
fn insert_over_a_used_id_fails_through_the_server_on_sqlite() {
    let url = fresh_sqlite("insert_conflict");
    let driver = SqliteDriver::new();
    let mut server = start(&driver, &url);
    seed(server.connection());
    insert_never_takes_over_a_used_id(server.connection());
}

#[test]
fn insert_over_a_used_id_fails_after_a_restart_on_sqlite() {
    let url = fresh_sqlite("insert_conflict_restart");
    let driver = SqliteDriver::new();
    {
        let mut server = start(&driver, &url);
        seed(server.connection());
        // The server and its connection are dropped here: the restart.
    }
    let mut server = start(&driver, &url);
    insert_never_takes_over_a_used_id(server.connection());
}
