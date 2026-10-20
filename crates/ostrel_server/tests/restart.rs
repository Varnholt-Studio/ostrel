//! Groundwork for AC-10: data of the app survives a restart of the server on a SQLite file.
//!
//! A server is started on a database file, data is committed through its connection and a
//! request runs; then the server is dropped and a new one is started on the same file. The
//! schema, the rows, a tombstone and the op log must all be there again.
//!
//! The full acceptance of AC-10 (data written by requests of a running app, the server
//! process stopped and started again) is T154; this test checks the server side of it with
//! the VM entries that exist today.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::task::{Context, Poll, Waker};

use ostrel_core::FileId;
use ostrel_db::api::{
    Connection, DbError, Driver, Hlc, MigrationPlan, NewOp, OpId, Query, ReplicaId, RowId,
    SchemaHash, ServerSeq, Value, Write,
};
use ostrel_db_sqlite::SqliteDriver;
use ostrel_server::audit::MemoryAuditLog;
use ostrel_server::host::SystemClock;
use ostrel_server::identity::{Credentials, NoTokens};
use ostrel_server::task::{Outcome, Server};
use ostrel_vm::{Image, Limits};

/// Runs a future that never waits. Every future of the SQLite driver is ready on first poll.
fn block_on<T>(f: impl Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("sqlite future was not ready"),
    }
}

/// A path for the database file of the test `name`, with no file there yet.
fn fresh_file(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("ostrel_server_{name}.db"));
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut p = path.clone().into_os_string();
        p.push(suffix);
        let _ = std::fs::remove_file(p);
    }
    path
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

/// Starts a server for `PROGRAM` on the SQLite file at `path`.
fn start(path: &Path) -> Server {
    let url = format!("sqlite:{}", path.display());
    let db = block_on(SqliteDriver::new().connect(&url)).unwrap();
    Server::new(
        image(PROGRAM),
        Limits::default(),
        db,
        Box::new(MemoryAuditLog::new()),
        Box::new(SystemClock),
        Box::new(NoTokens),
    )
}

fn plan() -> MigrationPlan {
    MigrationPlan {
        from: None,
        to: SchemaHash([7; 32]),
        schema: "{\"v\":7}".to_owned(),
        enums: vec![],
        steps: vec![],
    }
}

fn rid(n: u128) -> RowId {
    RowId::from_hex(&format!("{n:032x}")).unwrap()
}

fn insert(row: u128, title: &str) -> Write {
    Write::Insert {
        model: 1,
        row: rid(row),
        fields: vec![(0, Value::Text(title.to_owned()))],
        collections: vec![],
    }
}

fn serves_requests(server: &mut Server) {
    assert_eq!(
        server.run(&Credentials::anonymous()),
        Ok(Outcome {
            principal: None,
            output: vec!["up".to_owned()],
        })
    );
}

#[test]
fn restart_on_the_same_sqlite_file_keeps_schema_rows_and_ops() {
    let path = fresh_file("restart");
    let before = {
        let mut server = start(&path);
        serves_requests(&mut server);
        let db: &mut dyn Connection = server.connection();
        block_on(async {
            db.migrate(&plan()).await.unwrap();
            let mut tx = db.begin().await.unwrap();
            tx.apply(&[insert(1, "first"), insert(2, "second")])
                .await
                .unwrap();
            tx.apply(&[Write::Delete {
                model: 1,
                row: rid(2),
                expect_version: 1,
            }])
            .await
            .unwrap();
            let op = NewOp {
                id: OpId {
                    replica: ReplicaId(5),
                    seq: 1,
                },
                hlc: Hlc::new(11, 0, ReplicaId(5)).unwrap(),
                model: 1,
                row: rid(1),
                body: b"{}".to_vec(),
            };
            assert_eq!(tx.append_ops(&[op]).await, Ok(ServerSeq(1)));
            tx.commit().await.unwrap();
            db.query(&Query::all(1)).await.unwrap()
        })
        // The server and its connection are dropped here: the restart.
    };
    assert_eq!(before.0.len(), 1);

    let mut server = start(&path);
    serves_requests(&mut server);
    let db = server.connection();
    block_on(async {
        let schema = db.applied_schema().await.unwrap().unwrap();
        assert_eq!(schema.hash, SchemaHash([7; 32]));
        assert_eq!(db.query(&Query::all(1)).await, Ok(before));
        let mut tx = db.begin().await.unwrap();
        // The tombstone of row 2 still holds its id (AC-31) and the op log continues.
        assert_eq!(
            tx.apply(&[insert(2, "again")]).await,
            Err(DbError::Conflict)
        );
        tx.rollback().await.unwrap();
        let mut tx = db.begin().await.unwrap();
        assert_eq!(tx.append_ops(&[]).await, Ok(ServerSeq(1)));
        let ops = tx.ops_since(ServerSeq(0), 9).await.unwrap();
        assert_eq!(ops.len(), 1);
        tx.rollback().await.unwrap();
    });
}
