use crate::query::{bind, error, quote, Column, Filter, FilterValue, Query};
use rusqlite::{Connection, Result};

fn column_sql(column: &Column, image: Option<(usize, &str)>) -> String {
    let prefix = match image {
        Some((source, image)) if source == column.source => image.to_string(),
        _ => format!("b{}", column.source),
    };
    format!("{prefix}.{}", quote(&column.name))
}

fn filter_sql(filter: &Filter, image: Option<(usize, &str)>) -> String {
    match filter {
        Filter::Compare(left, op, right) => {
            let value = |operand: &FilterValue| match operand {
                FilterValue::Column(c) => column_sql(c, image),
                FilterValue::Integer(n) => n.to_string(),
            };
            format!("({} {op} {})", value(left), value(right))
        }
        Filter::Boolean(left, op, right) => format!(
            "({} {op} {})",
            filter_sql(left, image),
            filter_sql(right, image)
        ),
        Filter::Not(inner) => format!("(NOT {})", filter_sql(inner, image)),
    }
}

fn filter_columns<'a>(filter: &'a Filter, used: &mut Vec<&'a Column>) {
    match filter {
        Filter::Compare(left, _, right) => {
            for value in [left, right] {
                if let FilterValue::Column(c) = value {
                    if !used.contains(&c) {
                        used.push(c);
                    }
                }
            }
        }
        Filter::Boolean(left, _, right) => {
            filter_columns(left, used);
            filter_columns(right, used);
        }
        Filter::Not(inner) => filter_columns(inner, used),
    }
}

// With an OLD/NEW image, only the other source is read, restricted by its join key.
fn contributions(query: &Query, image: Option<(usize, &str)>) -> String {
    let group = column_sql(&query.group, image);
    let value = query
        .factors
        .iter()
        .map(|c| column_sql(c, image))
        .collect::<Vec<_>>()
        .join(" * ");
    let mut tables = query
        .tables
        .iter()
        .enumerate()
        .filter(|(i, _)| image.is_none_or(|(source, _)| source != *i))
        .map(|(i, table)| format!("main.{} AS b{i}", quote(table)))
        .collect::<Vec<_>>()
        .join(", ");
    if let Some((_, alias)) = image {
        tables = format!(
            "__ivm_image AS {alias}{}",
            if tables.is_empty() {
                String::new()
            } else {
                format!(", {tables}")
            }
        );
    }
    let mut predicate = match &query.join {
        Some(keys) => keys
            .iter()
            .map(|pair| {
                format!(
                    "{} = {}",
                    column_sql(&pair[0], image),
                    column_sql(&pair[1], image)
                )
            })
            .collect::<Vec<_>>()
            .join(" AND "),
        None => "1".into(),
    };
    if let Some(filter) = &query.filter {
        predicate = format!("({predicate}) AND {}", filter_sql(filter, image));
    }
    if tables.is_empty() {
        return format!("SELECT {group}, 1, ({value}) WHERE {predicate}");
    }
    format!("SELECT {group}, COUNT(*), SUM({value}) FROM {tables} WHERE {predicate} GROUP BY 1")
}

// Executed within xUpdate, where SQLite permits writes to protected shadows.
pub fn maintain(
    db: &Connection,
    name: &str,
    query: &Query,
    source: usize,
    adding: bool,
    payload: &str,
) -> Result<()> {
    let store = quote(&format!("{name}_state"));
    let delta = quote(&format!("{name}_delta"));
    let columns = used_columns(query)
        .into_iter()
        .filter(|c| c.source == source)
        .enumerate()
        .map(|(i, c)| format!("json_extract(?1,'$[{i}]') AS {}", quote(&c.name)))
        .collect::<Vec<_>>()
        .join(",");
    db.execute(&format!("DELETE FROM main.{delta}"), [])?;
    db.execute(
        &format!(
            "WITH __ivm_image AS (SELECT {columns}) INSERT INTO main.{delta} {}",
            contributions(query, Some((source, "image")))
        ),
        [payload],
    )?;
    let invalid: bool = db.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM main.{delta}
        WHERE typeof(g)!='integer' OR typeof(n)!='integer' OR typeof(s)!='integer')"
        ),
        [],
        |r| r.get(0),
    )?;
    if invalid {
        return Err(error("non-integer contribution or overflow"));
    }
    if adding {
        let overflow: bool = db.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM main.{store} a JOIN main.{delta} d ON a.g=d.g
            WHERE typeof(a.n+d.n)!='integer' OR typeof(a.s+d.s)!='integer')"
            ),
            [],
            |r| r.get(0),
        )?;
        if overflow {
            return Err(error("aggregate overflow"));
        }
        db.execute(
            &format!(
                "INSERT INTO main.{store}(g,n,s) SELECT g,n,s FROM main.{delta} WHERE 1
            ON CONFLICT(g) DO UPDATE SET n={store}.n+excluded.n,s={store}.s+excluded.s"
            ),
            [],
        )?;
    } else {
        let missing: bool = db.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM main.{delta} d LEFT JOIN main.{store} a ON a.g=d.g
            WHERE a.g IS NULL OR a.n<d.n)"
            ),
            [],
            |r| r.get(0),
        )?;
        if missing {
            return Err(error("missing contribution"));
        }
        let overflow: bool = db.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM main.{store} a JOIN main.{delta} d ON a.g=d.g
            WHERE a.n>d.n AND typeof(a.s-d.s)!='integer')"
            ),
            [],
            |r| r.get(0),
        )?;
        if overflow {
            return Err(error("aggregate overflow"));
        }
        db.execute(
            &format!(
                "DELETE FROM main.{store} WHERE g IN (SELECT g FROM main.{delta})
            AND n=(SELECT n FROM main.{delta} WHERE g={store}.g)"
            ),
            [],
        )?;
        db.execute(
            &format!(
                "UPDATE main.{store} SET n=n-(SELECT n FROM main.{delta} WHERE g={store}.g),
            s=s-(SELECT s FROM main.{delta} WHERE g={store}.g)
            WHERE g IN (SELECT g FROM main.{delta})"
            ),
            [],
        )?;
    }
    db.execute(&format!("DELETE FROM main.{delta}"), [])?;
    Ok(())
}

pub fn used_columns(query: &Query) -> Vec<&Column> {
    let mut used = vec![&query.group];
    for column in query
        .factors
        .iter()
        .chain(query.join.iter().flatten().flatten())
    {
        if !used.contains(&column) {
            used.push(column);
        }
    }
    if let Some(filter) = &query.filter {
        filter_columns(filter, &mut used);
    }
    used
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name.contains('\0')
        || name.to_ascii_lowercase().starts_with("sqlite_")
        || name.to_ascii_lowercase().starts_with("__ivm_")
    {
        return Err(error(
            "view name must be 1..128 bytes, NUL-free and non-reserved",
        ));
    }
    Ok(())
}

// xCreate owns the surrounding SQLite DDL transaction.
pub fn install(db: &Connection, name: &str, sql: &str) -> Result<Query> {
    validate_name(name)?;
    let query = bind(db, sql)?;
    if query
        .outputs
        .iter()
        .any(|(name, _)| name.to_ascii_lowercase().starts_with("__ivm_"))
    {
        return Err(error("output names beginning __ivm_ are reserved"));
    }
    let settings: bool = db.query_row(
        "SELECT (SELECT recursive_triggers FROM pragma_recursive_triggers)=1
        AND (SELECT trusted_schema FROM pragma_trusted_schema)=1",
        [],
        |r| r.get(0),
    )?;
    if !settings {
        return Err(error(
            "sqlite_ivm requires recursive_triggers=ON and trusted_schema=ON",
        ));
    }
    let used = used_columns(&query);
    for (source, table) in query.tables.iter().enumerate() {
        let checks = used
            .iter()
            .filter(|c| c.source == source)
            .map(|c| format!("typeof({})!='integer'", quote(&c.name)))
            .collect::<Vec<_>>()
            .join(" OR ");
        let invalid: bool = db.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM main.{} WHERE {checks})",
                quote(table)
            ),
            [],
            |r| r.get(0),
        )?;
        if invalid {
            return Err(error("source contains NULL or non-integer values"));
        }
        let foreign_keys: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list(?1,'main'))",
            [table],
            |r| r.get(0),
        )?;
        let user_triggers: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema
            WHERE type='trigger' AND tbl_name=?1 AND substr(name,1,6)!='__ivm_')",
            [table],
            |r| r.get(0),
        )?;
        if foreign_keys || user_triggers {
            return Err(error(
                "sources with foreign keys or user triggers are unsupported",
            ));
        }
    }
    let store = quote(&format!("{name}_state"));
    let delta = quote(&format!("{name}_delta"));
    let mut objects = vec![
        ("table", format!("{name}_state")),
        ("table", format!("{name}_delta")),
    ];
    db.execute_batch(&format!(
        "CREATE TABLE main.{store}(g INTEGER PRIMARY KEY,n INTEGER NOT NULL,s INTEGER NOT NULL);
        CREATE TABLE main.{delta}(g INTEGER PRIMARY KEY,n,s);
        INSERT INTO main.{delta} {}",
        contributions(&query, None)
    ))?;
    let invalid: bool = db.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM main.{delta}
        WHERE typeof(g)!='integer' OR typeof(n)!='integer' OR typeof(s)!='integer')"
        ),
        [],
        |r| r.get(0),
    )?;
    if invalid {
        return Err(error("non-integer initial aggregate or overflow"));
    }
    db.execute_batch(&format!(
        "INSERT INTO main.{store} SELECT * FROM main.{delta}; DELETE FROM main.{delta};"
    ))?;
    objects.extend(create_hooks(db, name, &query, true)?);
    crate::catalog::record(db, name, sql, &query, &used, &objects)?;
    Ok(query)
}

pub fn create_hooks(
    db: &Connection,
    name: &str,
    query: &Query,
    indexes: bool,
) -> Result<Vec<(&'static str, String)>> {
    let mut objects = Vec::new();
    let used = used_columns(query);
    if let Some(keys) = query.join.as_ref().filter(|_| indexes) {
        for (source, table) in query.tables.iter().enumerate() {
            let mut columns = Vec::new();
            for pair in keys {
                let column = quote(&pair[source].name);
                if !columns.contains(&column) {
                    columns.push(column);
                }
            }
            let base = format!("__ivm_{name}_key_{source}");
            let mut index = base.clone();
            // Renamed views retain their source indexes. Reusing their former public
            // name allocates a fresh index name; unowned collisions still fail DDL.
            let has_catalog: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name='__ivm_objects')",
                [],
                |r| r.get(0),
            )?;
            if has_catalog {
                let mut suffix = 0;
                while db.query_row("SELECT EXISTS(SELECT 1 FROM main.__ivm_objects WHERE object_type='index' AND object_name=?1)",[&index],|r|r.get::<_,bool>(0))? {
                    suffix+=1; index=format!("{base}_{suffix}");
                }
            }
            db.execute_batch(&format!(
                "CREATE INDEX main.{} ON {}({})",
                quote(&index),
                quote(table),
                columns.join(",")
            ))?;
            objects.push(("index", index));
        }
    }
    for (source, table) in query.tables.iter().enumerate() {
        for event in ["INSERT", "UPDATE", "DELETE"] {
            let trigger = format!("__ivm_{name}_{source}_{}", event.to_lowercase());
            let mut guard =
                "SELECT CASE WHEN (SELECT recursive_triggers FROM pragma_recursive_triggers)!=1
                THEN RAISE(ABORT,'sqlite_ivm requires recursive_triggers=ON') END;"
                    .to_string();
            if event != "DELETE" {
                let checks = used
                    .iter()
                    .filter(|c| c.source == source)
                    .map(|c| format!("typeof(NEW.{})!='integer'", quote(&c.name)))
                    .collect::<Vec<_>>()
                    .join(" OR ");
                guard.push_str(&format!("SELECT CASE WHEN {checks} THEN RAISE(ABORT,'sqlite_ivm requires integer, non-NULL source values') END;"));
            }
            db.execute_batch(&format!(
                "CREATE TRIGGER main.{} BEFORE {event} ON {} BEGIN {guard} END;",
                quote(&format!("{trigger}_guard")),
                quote(table)
            ))?;
            objects.push(("trigger", format!("{trigger}_guard")));
            let mut body = String::new();
            for (image, adding) in [("OLD", false), ("NEW", true)] {
                if (event == "INSERT" && !adding) || (event == "DELETE" && adding) {
                    continue;
                }
                let values = used
                    .iter()
                    .filter(|c| c.source == source)
                    .map(|c| format!("{image}.{}", quote(&c.name)))
                    .collect::<Vec<_>>()
                    .join(",");
                body.push_str(&format!("INSERT INTO {}(__ivm_source,__ivm_adding,__ivm_row) VALUES({source},{},json_array({values}));",quote(name),i32::from(adding)));
            }
            db.execute_batch(&format!(
                "CREATE TRIGGER main.{} AFTER {event} ON {} BEGIN {body} END;",
                quote(&trigger),
                quote(table)
            ))?;
            objects.push(("trigger", trigger));
        }
    }
    Ok(objects)
}
