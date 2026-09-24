//! Run the packet's DD engine from a SQLite commit collector.
//! SQL output tables and a transactional generation counter remain the source
//! of truth after rollback; the DD worker rebuilds only when generations differ.

use frontier_dd_packet::{Change, Engine, Shape};
use sqlite_ext::rusqlite::{functions::FunctionFlags, types::Value, Connection, Error, Result};
use sqlite_ext::{BulkTrigger, Plugin, RowChange};
use std::collections::BTreeSet;

fn error(message: impl Into<String>) -> Error {
    Error::ModuleError(message.into())
}

fn shape_name(shape: Shape) -> &'static str {
    match shape {
        Shape::Access => "access",
        Shape::Group => "team_cost",
    }
}

fn source_rows(db: &Connection, shape: Shape) -> Result<Vec<Change>> {
    let tables: &[(usize, &str)] = match shape {
        Shape::Access => &[(0, "membership"), (1, "permission"), (2, "direct_grant")],
        Shape::Group => &[(3, "job")],
    };
    let mut out = Vec::new();
    for &(table, name) in tables {
        let mut statement = db.prepare(&format!("SELECT * FROM {name}"))?;
        for row in statement.query_map([], |row| {
            Ok(Change {
                table,
                id: row.get(0)?,
                a: row.get(1)?,
                b: row.get(2)?,
                weight: 1,
            })
        })? {
            out.push(row?);
        }
    }
    Ok(out)
}

fn row_changes(batch: &[RowChange]) -> Result<Vec<Change>> {
    batch
        .iter()
        .map(|row| {
            let table = match row.table.as_str() {
                "membership" => 0,
                "permission" => 1,
                "direct_grant" => 2,
                "job" => 3,
                other => return Err(error(format!("unexpected source {other}"))),
            };
            let [Value::Integer(id), Value::Integer(a), Value::Integer(b)] = row.values.as_slice()
            else {
                return Err(error("source rows must have three integer columns"));
            };
            Ok(Change {
                table,
                id: *id,
                a: *a,
                b: *b,
                weight: row.sign.as_integer() as isize,
            })
        })
        .collect()
}

fn visible_rows(db: &Connection, shape: Shape) -> Result<BTreeSet<Vec<i64>>> {
    let name = shape_name(shape);
    let width = if shape == Shape::Access { 2 } else { 3 };
    let mut statement = db.prepare(&format!("SELECT * FROM dd_frontier_{name}"))?;
    let rows = statement
        .query_map([], |row| {
            (0..width)
                .map(|index| row.get(index))
                .collect::<Result<Vec<i64>>>()
        })?
        .collect::<Result<BTreeSet<_>>>()?;
    Ok(rows)
}

fn write_delta(db: &Connection, shape: Shape, row: &[i64], sign: isize) -> Result<()> {
    if sign != 1 && sign != -1 {
        return Err(error(format!(
            "output weight {sign} is outside set semantics"
        )));
    }
    match (shape, sign) {
        (Shape::Access, 1) => {
            db.execute(
                "INSERT INTO dd_frontier_access(person,resource) VALUES(?1,?2)",
                (row[0], row[1]),
            )?;
            db.execute(
                "INSERT INTO dd_frontier_access_delta VALUES(1,?1,?2)",
                (row[0], row[1]),
            )?;
        }
        (Shape::Access, -1) => {
            db.execute(
                "DELETE FROM dd_frontier_access WHERE person=?1 AND resource=?2",
                (row[0], row[1]),
            )?;
            db.execute(
                "INSERT INTO dd_frontier_access_delta VALUES(-1,?1,?2)",
                (row[0], row[1]),
            )?;
        }
        (Shape::Group, 1) => {
            db.execute(
                "INSERT INTO dd_frontier_team_cost(team,jobs,total_cost) VALUES(?1,?2,?3)",
                (row[0], row[1], row[2]),
            )?;
            db.execute(
                "INSERT INTO dd_frontier_team_cost_delta VALUES(1,?1,?2,?3)",
                (row[0], row[1], row[2]),
            )?;
        }
        (Shape::Group, -1) => {
            db.execute(
                "DELETE FROM dd_frontier_team_cost WHERE team=?1 AND jobs=?2 AND total_cost=?3",
                (row[0], row[1], row[2]),
            )?;
            db.execute(
                "INSERT INTO dd_frontier_team_cost_delta VALUES(-1,?1,?2,?3)",
                (row[0], row[1], row[2]),
            )?;
        }
        _ => unreachable!("sign already checked"),
    }
    Ok(())
}

struct Maintain {
    shape: Shape,
    engine: Engine,
    generation: i64,
}

impl BulkTrigger for Maintain {
    fn on_batch(&mut self, db: &Connection, batch: &[RowChange]) -> Result<()> {
        let name = shape_name(self.shape);
        let generation: i64 = db.query_row(
            "SELECT generation FROM dd_frontier_catalog WHERE name=?1",
            [name],
            |row| row.get(0),
        )?;
        db.execute_batch(&format!("DELETE FROM dd_frontier_{name}_delta"))?;
        if generation == self.generation {
            let changes = self.engine.apply(row_changes(batch)?).map_err(error)?;
            for (row, sign) in changes.iter().filter(|(_, sign)| *sign < 0) {
                write_delta(db, self.shape, row, *sign)?;
            }
            for (row, sign) in changes.iter().filter(|(_, sign)| *sign > 0) {
                write_delta(db, self.shape, row, *sign)?;
            }
        } else {
            // The previous xSync changed DD, then SQLite rolled back its own
            // tables. Rehydrate DD from the source rows visible in this batch.
            self.engine = Engine::new(self.shape);
            self.engine
                .apply(source_rows(db, self.shape)?)
                .map_err(error)?;
            let desired: BTreeSet<_> = self.engine.snapshot().map_err(error)?.into_iter().collect();
            let current = visible_rows(db, self.shape)?;
            for row in current.difference(&desired) {
                write_delta(db, self.shape, row, -1)?;
            }
            for row in desired.difference(&current) {
                write_delta(db, self.shape, row, 1)?;
            }
        }
        self.generation = generation + 1;
        db.execute(
            "UPDATE dd_frontier_catalog SET generation=?1 WHERE name=?2",
            (self.generation, name),
        )?;
        Ok(())
    }
}

fn install_case(db: &Connection, shape: Shape) -> Result<()> {
    let name = shape_name(shape);
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS dd_frontier_catalog(name TEXT PRIMARY KEY,generation INTEGER NOT NULL)",
    )?;
    let already: i64 = db.query_row(
        "SELECT count(*) FROM dd_frontier_catalog WHERE name=?1",
        [name],
        |row| row.get(0),
    )?;
    if already != 0 {
        return Err(error(format!("{name} is already installed")));
    }
    match shape {
        Shape::Access => db.execute_batch(
            "CREATE TABLE dd_frontier_access(person INTEGER NOT NULL,resource INTEGER NOT NULL,PRIMARY KEY(person,resource));
             CREATE TABLE dd_frontier_access_delta(__sign INTEGER NOT NULL,person INTEGER NOT NULL,resource INTEGER NOT NULL);",
        )?,
        Shape::Group => db.execute_batch(
            "CREATE TABLE dd_frontier_team_cost(team INTEGER NOT NULL PRIMARY KEY,jobs INTEGER NOT NULL,total_cost INTEGER NOT NULL);
             CREATE TABLE dd_frontier_team_cost_delta(__sign INTEGER NOT NULL,team INTEGER NOT NULL,jobs INTEGER NOT NULL,total_cost INTEGER NOT NULL);",
        )?,
    }
    db.execute(
        "INSERT INTO dd_frontier_catalog(name,generation) VALUES(?1,0)",
        [name],
    )?;
    let engine = Engine::new(shape);
    for (row, sign) in engine.apply(source_rows(db, shape)?).map_err(error)? {
        write_delta(db, shape, &row, sign)?;
    }
    let tables: &[&str] = match shape {
        Shape::Access => &["membership", "permission", "direct_grant"],
        Shape::Group => &["job"],
    };
    sqlite_ext::watch(
        db,
        &format!("dd_watch_{name}"),
        tables,
        Maintain {
            shape,
            engine,
            generation: 0,
        },
    )?;
    Ok(())
}

fn install(db: &Connection) -> Result<()> {
    db.create_scalar_function(
        c"dd_frontier_install",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let db = unsafe { ctx.get_connection()? };
            let shape = match ctx.get_raw(0).as_str()? {
                "access" => Shape::Access,
                "team_cost" => Shape::Group,
                other => return Err(error(format!("unsupported frontier {other}"))),
            };
            install_case(&db, shape)?;
            Ok(1_i64)
        },
    )?;
    db.create_scalar_function(
        c"dd_frontier_drop",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DIRECTONLY,
        |ctx| {
            let db = unsafe { ctx.get_connection()? };
            let name = match ctx.get_raw(0).as_str()? {
                "access" => "access",
                "team_cost" => "team_cost",
                other => return Err(error(format!("unsupported frontier {other}"))),
            };
            db.execute_batch(&format!(
                "DROP TABLE dd_watch_{name}; DROP TABLE dd_frontier_{name}_delta; DROP TABLE dd_frontier_{name};"
            ))?;
            db.execute("DELETE FROM dd_frontier_catalog WHERE name=?1", [name])?;
            Ok(1_i64)
        },
    )
}

const PLUGIN: Plugin = Plugin::new(
    "frontier_dd_ext",
    env!("CARGO_PKG_VERSION"),
    "warn",
    install,
);
sqlite_ext::sqlite_extension!(sqlite3_frontier_dd_ext_init, PLUGIN);
