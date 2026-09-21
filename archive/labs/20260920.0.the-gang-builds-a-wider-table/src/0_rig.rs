use std::fmt;

use rusqlite::Connection;
use tracing::{info_span, warn};

// View name and shadow names mirror the engine's naming so the replica's SQL
// text is byte-comparable with what `src/2_vtab.rs` and `src/1_maintenance.rs`
// issue today.
pub const VIEW: &str = "v";
pub const FOLD_SPAN: &str = "fold";
pub const CHANGE_SPAN: &str = "fold/change";

pub const STEP_GUARD_PRAGMA: &str = "step/guard/pragma";
pub const STEP_GUARD_TYPES: &str = "step/guard/types";
pub const STEP_DISPATCH: &str = "step/dispatch";
pub const STEP_REFRESH: &str = "step/refresh";
pub const STEP_VALIDATE: &str = "step/validate";
pub const STEP_BUILD: &str = "step/build";
pub const STEP_VALIDITY: &str = "step/validity";
pub const STEP_OVERFLOW: &str = "step/overflow";
pub const STEP_UPSERT: &str = "step/upsert";

pub const STEP_NAMES: [&str; 9] = [
    STEP_GUARD_PRAGMA,
    STEP_GUARD_TYPES,
    STEP_DISPATCH,
    STEP_REFRESH,
    STEP_VALIDATE,
    STEP_BUILD,
    STEP_VALIDITY,
    STEP_OVERFLOW,
    STEP_UPSERT,
];

// Bounds are named so a budget diagnostic says which ceiling stopped the run.
const MAX_TABLES: usize = 16;
const MAX_COLUMNS: usize = 64;
const MAX_JOIN_ARITY: usize = 16;
// Memory budget on the generator: a rig that allocates gigabytes poisons the
// numbers lab 1 reads off it.
const MAX_GENERATED_CELLS: usize = 1 << 20;
// One fold runs inside a single test slot; past this many changes the fold
// risks the 10-second law.
pub const MAX_CHANGES_PER_FOLD: usize = 4096;
// Default fold depth for the measurement binary; tests pass their own.
pub const CHANGES_PER_FOLD: usize = 512;
pub const ROWS_PER_TABLE: usize = 96;
// Join selectivity: a changed key matches ROWS_PER_TABLE / KEY_DOMAIN rows per
// other source, which bounds the contribution cross product.
const KEY_DOMAIN: usize = 64;
// Non-key values stay in 0..=VALUE_SPAN so the M-column product never trips
// the i64 overflow checks the fold is required to exercise, not raise.
const VALUE_SPAN: i64 = 3;

#[derive(Debug)]
pub struct RigError(pub String);

impl fmt::Display for RigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RigError {}

impl From<rusqlite::Error> for RigError {
    fn from(value: rusqlite::Error) -> Self {
        RigError(value.to_string())
    }
}

pub type RigResult<T> = Result<T, RigError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Axes {
    pub tables: usize,
    pub columns: usize,
    pub join_arity: usize,
    pub seed: u64,
}

impl Axes {
    pub fn check(&self) -> RigResult<()> {
        if self.tables < 2 || self.tables > MAX_TABLES {
            return Err(RigError(format!(
                "MAX_TABLES ({MAX_TABLES}): axes.tables {} outside 2..={MAX_TABLES}",
                self.tables
            )));
        }
        if self.columns < 2 || self.columns > MAX_COLUMNS {
            return Err(RigError(format!(
                "MAX_COLUMNS ({MAX_COLUMNS}): axes.columns {} outside 2..={MAX_COLUMNS}",
                self.columns
            )));
        }
        if self.join_arity < 2 || self.join_arity > MAX_JOIN_ARITY {
            return Err(RigError(format!(
                "MAX_JOIN_ARITY ({MAX_JOIN_ARITY}): axes.join_arity {} outside 2..={MAX_JOIN_ARITY}",
                self.join_arity
            )));
        }
        if self.join_arity > self.tables {
            return Err(RigError(format!(
                "axes.join_arity {} exceeds axes.tables {}",
                self.join_arity, self.tables
            )));
        }
        Ok(())
    }
}

// SplitMix64: integer-only, so the same seed yields the same tables on every
// platform and two labs can never disagree about a fixture.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTable {
    pub name: String,
    // Every row is [k, v1, .., v{columns-1}]: the group key first, then
    // columns-1 value columns. `columns` counts k.
    pub rows: Vec<Vec<i64>>,
}

pub fn generate(axes: &Axes) -> RigResult<Vec<SourceTable>> {
    axes.check()?;
    let cells = axes
        .tables
        .checked_mul(ROWS_PER_TABLE)
        .and_then(|rows| rows.checked_mul(axes.columns))
        .ok_or_else(|| RigError("MAX_GENERATED_CELLS: axes overflow cell count".to_string()))?;
    if cells > MAX_GENERATED_CELLS {
        return Err(RigError(format!(
            "MAX_GENERATED_CELLS ({MAX_GENERATED_CELLS}): request would allocate {cells} cells"
        )));
    }
    let mut rng = SplitMix(axes.seed);
    let mut tables = Vec::with_capacity(axes.tables);
    for table_index in 0..axes.tables {
        let mut rows = Vec::with_capacity(ROWS_PER_TABLE);
        for _ in 0..ROWS_PER_TABLE {
            let mut row = Vec::with_capacity(axes.columns);
            row.push((rng.next() % KEY_DOMAIN as u64) as i64);
            for _ in 1..axes.columns {
                row.push((rng.next() % VALUE_SPAN as u64) as i64);
            }
            rows.push(row);
        }
        tables.push(SourceTable {
            name: format!("t{table_index}"),
            rows,
        });
    }
    Ok(tables)
}

// One maintenance change: a full row image for the view's group source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub row: Vec<i64>,
}

pub fn fold_plan(axes: &Axes, changes: usize) -> RigResult<Vec<Change>> {
    axes.check()?;
    if changes > MAX_CHANGES_PER_FOLD {
        warn!(budget = MAX_CHANGES_PER_FOLD, requested = changes, "MAX_CHANGES_PER_FOLD: fold depth over budget");
        return Err(RigError(format!(
            "MAX_CHANGES_PER_FOLD ({MAX_CHANGES_PER_FOLD}): {changes} changes requested"
        )));
    }
    // Distinct stream from the generator, derived from the same seed, so plan
    // and fixture stay independent but both reproducible.
    let mut rng = SplitMix(axes.seed ^ 0x0DDB1A5E5BAD5EED);
    let mut plan = Vec::with_capacity(changes);
    for _ in 0..changes {
        let mut row = Vec::with_capacity(axes.columns);
        row.push((rng.next() % KEY_DOMAIN as u64) as i64);
        for _ in 1..axes.columns {
            row.push((rng.next() % VALUE_SPAN as u64) as i64);
        }
        plan.push(Change { row });
    }
    Ok(plan)
}

pub struct Schema {
    pub view: String,
    pub state: String,
    pub view_tables: Vec<String>,
    pub columns: usize,
    pub view_query_sql: String,
}

pub fn install(db: &Connection, axes: &Axes, tables: &[SourceTable]) -> RigResult<Schema> {
    // The engine refuses to install unless the connection runs with
    // recursive_triggers and trusted_schema on (src/1_maintenance.rs:249), so
    // the replica opens under the same contract the guard step polices.
    db.execute_batch("PRAGMA recursive_triggers=ON; PRAGMA trusted_schema=ON;")?;
    if tables.len() != axes.tables {
        return Err(RigError(format!(
            "install: {} tables generated for axes.tables {}",
            tables.len(),
            axes.tables
        )));
    }
    for table in tables {
        if table.rows.iter().any(|row| row.len() != axes.columns) {
            return Err(RigError(format!(
                "install: table {} has a row whose width is not {}",
                table.name, axes.columns
            )));
        }
    }
    let columns = (1..axes.columns).map(|i| format!("v{i}")).collect::<Vec<_>>();
    for (index, table) in tables.iter().enumerate() {
        let defs = std::iter::once("k INTEGER".to_string())
            .chain(columns.iter().map(|c| format!("{c} INTEGER")))
            .collect::<Vec<_>>()
            .join(",");
        db.execute_batch(&format!("CREATE TABLE main.{}({defs})", table.name))?;
        let mut insert = format!("INSERT INTO main.{} VALUES(", table.name);
        for slot in 0..axes.columns {
            if slot > 0 {
                insert.push(',');
            }
            insert.push_str(&format!("?{}", slot + 1));
        }
        insert.push(')');
        let mut statement = db.prepare(&insert)?;
        for row in &table.rows {
            statement.execute(rusqlite::params_from_iter(row.iter()))?;
        }
        // Join indexes mirror src/1_maintenance.rs:359 for every view source.
        if index < axes.join_arity {
            db.execute_batch(&format!(
                "CREATE INDEX main.__ivm_{VIEW}_key_{index} ON {}(k)",
                table.name
            ))?;
        }
    }
    // The dispatch target. In the engine this is the virtual table with
    // HIDDEN maintenance columns; a plain table with the same column names
    // takes the same INSERT text.
    let mut v_defs = vec![
        "g INTEGER".to_string(),
        "n INTEGER".to_string(),
        "s INTEGER".to_string(),
        "__ivm_source INTEGER".to_string(),
        "__ivm_adding INTEGER".to_string(),
    ];
    for i in 0..axes.columns {
        v_defs.push(format!("__ivm_v{i} INTEGER"));
    }
    db.execute_batch(&format!(
        "CREATE TABLE main.{VIEW}({})", v_defs.join(",")
    ))?;
    db.execute_batch(&format!(
        "CREATE TABLE main.{VIEW}_state(g INTEGER PRIMARY KEY,n INTEGER NOT NULL,s INTEGER NOT NULL)"
    ))?;
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS main.__ivm_views(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE COLLATE NOCASE, query_sql TEXT NOT NULL)",
    )?;
    let view_query_sql = view_query_sql(axes);
    db.execute(
        "INSERT INTO main.__ivm_views(id,name,query_sql) VALUES(1,?1,?2)",
        rusqlite::params![VIEW, view_query_sql],
    )?;
    Ok(Schema {
        view: VIEW.to_string(),
        state: format!("{VIEW}_state"),
        view_tables: (0..axes.join_arity).map(|i| format!("t{i}")).collect(),
        columns: axes.columns,
        view_query_sql,
    })
}

// The declared view the way the engine would bind it: group on the first
// source's key, product of its value columns, J-way join on k.
fn view_query_sql(axes: &Axes) -> String {
    let factors = (1..axes.columns)
        .map(|i| format!("t0.v{i}"))
        .collect::<Vec<_>>()
        .join(" * ");
    let tables = (0..axes.join_arity)
        .map(|i| format!("t{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let joins = (1..axes.join_arity)
        .map(|i| format!("t0.k = t{i}.k"))
        .collect::<Vec<_>>()
        .join(" AND ");
    format!("SELECT t0.k AS g, count(*), sum({factors}) FROM {tables} WHERE {joins} GROUP BY t0.k")
}

// Engine mode replays each site's prepare policy exactly as the engine pays it
// today: trigger programs and `refresh` are precompiled or cached, the three
// maintenance statements are re-prepared from freshly built strings per row.
// Cached mode prices the ceiling of reusing those three statements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrepareMode {
    Engine,
    Cached,
}

pub struct FoldReport {
    pub changes: usize,
}

pub fn run_fold(db: &Connection, axes: &Axes, plan: &[Change], mode: PrepareMode) -> RigResult<FoldReport> {
    axes.check()?;
    if plan.len() > MAX_CHANGES_PER_FOLD {
        warn!(budget = MAX_CHANGES_PER_FOLD, requested = plan.len(), "MAX_CHANGES_PER_FOLD: fold depth over budget");
        return Err(RigError(format!(
            "MAX_CHANGES_PER_FOLD ({MAX_CHANGES_PER_FOLD}): plan carries {} changes",
            plan.len()
        )));
    }
    let _fold = info_span!(FOLD_SPAN, tables = axes.tables, columns = axes.columns, join_arity = axes.join_arity, changes = plan.len()).entered();

    // Step 1 text: the engine's BEFORE trigger guard, with RAISE rewritten to
    // a 1/0 select because RAISE is legal only inside a trigger program.
    let pragma_sql = "SELECT CASE WHEN (SELECT recursive_triggers FROM pragma_recursive_triggers)!=1 THEN 1 ELSE 0 END";
    // Step 2 text: the engine's per-column typeof guard over the M used
    // columns of the group source.
    let mut type_checks = Vec::with_capacity(axes.columns);
    for slot in 0..axes.columns {
        type_checks.push(format!("typeof(?{})!='integer'", slot + 1));
    }
    let types_sql = format!(
        "SELECT CASE WHEN {} THEN 1 ELSE 0 END",
        type_checks.join(" OR ")
    );
    // Step 3 text: the engine's maintenance INSERT, routed into the vtab
    // dispatch by SQLite. The replica lands it in the plain table.
    let mut hidden = Vec::with_capacity(axes.columns);
    let mut dispatch_values = vec!["?1".to_string(), "?2".to_string()];
    for i in 0..axes.columns {
        hidden.push(format!("__ivm_v{i}"));
        dispatch_values.push(format!("?{}", i + 3));
    }
    let dispatch_sql = format!(
        "INSERT INTO main.{VIEW}(__ivm_source,__ivm_adding,{}) VALUES({})",
        hidden.join(","),
        dispatch_values.join(",")
    );
    // Step 4 text: the engine's per-insert catalog refresh, verbatim.
    let refresh_sql = "SELECT query_sql FROM main.__ivm_views WHERE id=?1";
    let expected_query = view_query_sql(axes);

    let mut pragma = db.prepare_cached(pragma_sql)?;
    let mut types = db.prepare_cached(&types_sql)?;
    let mut dispatch = db.prepare_cached(&dispatch_sql)?;
    let mut refresh = db.prepare_cached(refresh_sql)?;

    let mut image_selects = Vec::with_capacity(axes.columns);
    for slot in 0..axes.columns {
        let name = if slot == 0 { "k".to_string() } else { format!("v{slot}") };
        image_selects.push(format!("?{} AS {name}", slot + 1));
    }
    let image_select = image_selects.join(",");

    for (index, change) in plan.iter().enumerate() {
        let _change = info_span!(CHANGE_SPAN, index = index).entered();

        {
            let _step = info_span!(STEP_GUARD_PRAGMA).entered();
            let abort: i64 = pragma.query_row([], |r| r.get(0))?;
            if abort != 0 {
                return Err(RigError(format!(
                    "{STEP_GUARD_PRAGMA}: recursive_triggers is off; the engine would RAISE ABORT"
                )));
            }
        }
        {
            let _step = info_span!(STEP_GUARD_TYPES).entered();
            let abort: i64 = types.query_row(rusqlite::params_from_iter(change.row.iter()), |r| r.get(0))?;
            if abort != 0 {
                return Err(RigError(format!(
                    "{STEP_GUARD_TYPES}: non-integer image value; the engine would RAISE ABORT"
                )));
            }
        }
        {
            let _step = info_span!(STEP_DISPATCH).entered();
            let mut argv = vec![0i64, 1i64];
            argv.extend(change.row.iter().copied());
            dispatch.execute(rusqlite::params_from_iter(argv.iter()))?;
        }
        {
            let _step = info_span!(STEP_REFRESH).entered();
            let query_sql: String = refresh.query_row([1], |r| r.get(0))?;
            if query_sql != expected_query {
                // The engine rebinds the view here; a replica whose stored SQL
                // drifted is a rig bug, not a fold step.
                return Err(RigError(format!(
                    "{STEP_REFRESH}: stored query_sql drifted from the declared view"
                )));
            }
        }
        {
            let _step = info_span!(STEP_VALIDATE).entered();
            if change.row.len() != axes.columns {
                return Err(RigError(format!(
                    "{STEP_VALIDATE}: image is not {} integers", axes.columns
                )));
            }
        }
        // Step 6: the engine formats the three maintenance statements from
        // scratch on every row; the fold does the same in both modes.
        let _step = info_span!(STEP_BUILD).entered();
        let contrib = contributions(axes);
        let validity_sql = format!(
            "SELECT EXISTS(SELECT 1 FROM (WITH __ivm_image AS (SELECT {image_select}) {contrib}) WHERE typeof(g)!='integer' OR typeof(n)!='integer' OR typeof(s)!='integer')"
        );
        let overflow_sql = format!(
            "SELECT EXISTS(SELECT 1 FROM main.{VIEW}_state a JOIN (WITH __ivm_image AS (SELECT {image_select}) {contrib}) d ON a.g=d.g WHERE typeof(a.n+d.n)!='integer' OR typeof(a.s+d.s)!='integer')"
        );
        let upsert_sql = format!(
            "INSERT INTO main.{VIEW}_state(g,n,s) SELECT * FROM (WITH __ivm_image AS (SELECT {image_select}) {contrib}) WHERE 1 ON CONFLICT(g) DO UPDATE SET n={VIEW}_state.n+excluded.n,s={VIEW}_state.s+excluded.s"
        );
        drop(_step);
        {
            let _step = info_span!(STEP_VALIDITY).entered();
            let invalid: bool = run_query(db, &validity_sql, mode, &change.row)?;
            if invalid {
                return Err(RigError(format!(
                    "{STEP_VALIDITY}: non-integer contribution; the engine would error"
                )));
            }
        }
        {
            let _step = info_span!(STEP_OVERFLOW).entered();
            let overflow: bool = run_query(db, &overflow_sql, mode, &change.row)?;
            if overflow {
                return Err(RigError(format!(
                    "{STEP_OVERFLOW}: aggregate overflow; the engine would error"
                )));
            }
        }
        {
            let _step = info_span!(STEP_UPSERT).entered();
            run_exec(db, &upsert_sql, mode, &change.row)?;
        }
    }
    Ok(FoldReport {
        changes: plan.len(),
    })
}

fn run_query(db: &Connection, sql: &str, mode: PrepareMode, row: &[i64]) -> RigResult<bool> {
    let value: bool = match mode {
        PrepareMode::Engine => {
            let mut statement = db.prepare(sql)?;
            statement.query_row(rusqlite::params_from_iter(row.iter()), |r| r.get(0))?
        }
        PrepareMode::Cached => {
            let mut statement = db.prepare_cached(sql)?;
            statement.query_row(rusqlite::params_from_iter(row.iter()), |r| r.get(0))?
        }
    };
    Ok(value)
}

fn run_exec(db: &Connection, sql: &str, mode: PrepareMode, row: &[i64]) -> RigResult<usize> {
    let changed = match mode {
        PrepareMode::Engine => {
            let mut statement = db.prepare(sql)?;
            statement.execute(rusqlite::params_from_iter(row.iter()))?
        }
        PrepareMode::Cached => {
            let mut statement = db.prepare_cached(sql)?;
            statement.execute(rusqlite::params_from_iter(row.iter()))?
        }
    };
    Ok(changed)
}

// The engine's `contributions()` with the image owned by source 0: the image
// row crosses the other J-1 sources on their join keys, grouped by the image
// key. Mirrors src/1_maintenance.rs:50 with image = Some((0, "image")).
fn contributions(axes: &Axes) -> String {
    let value = (1..axes.columns)
        .map(|i| format!("image.v{i}"))
        .collect::<Vec<_>>()
        .join(" * ");
    let mut tables = vec!["__ivm_image AS image".to_string()];
    for i in 1..axes.join_arity {
        tables.push(format!("main.t{i} AS b{i}"));
    }
    let joins = (1..axes.join_arity)
        .map(|i| format!("image.k = b{i}.k"))
        .collect::<Vec<_>>()
        .join(" AND ");
    format!(
        "SELECT image.k AS g, COUNT(*) AS n, SUM({value}) AS s FROM {} WHERE {joins} GROUP BY 1",
        tables.join(", ")
    )
}
