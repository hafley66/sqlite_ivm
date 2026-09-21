use crate::{
    query::{error, quote},
    relational::{Kind, Occurrence, Plan},
    relational_maintenance::{
        columns, folded, json_key, keys_table, out_table, plain, table, BULK_MULTIPLICITY_BUDGET,
    },
};
use rusqlite::{types::Value, Connection, Result};

impl Plan {
    pub fn create_state(&self, db: &Connection, name: &str) -> Result<Vec<(&'static str, String)>> {
        let mut objects = vec![];
        let dictionary = format!("{name}_keys");
        db.execute_batch(&format!(
            "CREATE TABLE main.{}(__i INTEGER PRIMARY KEY,__v TEXT NOT NULL UNIQUE)",
            quote(&dictionary)
        ))?;
        objects.push(("table", dictionary));
        let state = format!("{name}_state");
        db.execute_batch(&format!(
            "CREATE TABLE main.{}(__key TEXT NOT NULL,{}); CREATE INDEX main.{} ON {}(__key)",
            quote(&state),
            (0..self.names.len())
                .map(|i| format!("c{i}"))
                .collect::<Vec<_>>()
                .join(","),
            quote(&format!("__ivm_{name}_result_key")),
            quote(&state)
        ))?;
        objects.push(("table", state));
        objects.push(("index", format!("__ivm_{name}_result_key")));
        for (id, node) in self.nodes.iter().enumerate() {
            if matches!(node.kind, Kind::Input(_) | Kind::Map { .. }) {
                continue;
            }
            for (side, input) in node.inputs.iter().enumerate() {
                let t = format!("{name}_op{id}_{side}");
                let n = self.nodes[*input].fields.len();
                db.execute_batch(&format!("CREATE TABLE main.{}(__k INTEGER NOT NULL,__r INTEGER NOT NULL,__n INTEGER NOT NULL,{}); CREATE INDEX main.{} ON {}(__k); CREATE INDEX main.{} ON {}(__r)",quote(&t),columns(n),quote(&format!("__ivm_{name}_op{id}_{side}_key")),quote(&t),quote(&format!("__ivm_{name}_op{id}_{side}_row")),quote(&t)))?;
                objects.push(("table", t));
                objects.push(("index", format!("__ivm_{name}_op{id}_{side}_key")));
                objects.push(("index", format!("__ivm_{name}_op{id}_{side}_row")));
                if let Kind::Group {
                    order,
                    limit: Some(_),
                    ..
                } = &node.kind
                {
                    if !order.is_empty() {
                        let index = format!("__ivm_{name}_op{id}_{side}_order");
                        db.execute_batch(&format!(
                            "CREATE INDEX main.{} ON {}(__k,{})",
                            quote(&index),
                            quote(&format!("{name}_op{id}_{side}")),
                            order
                                .iter()
                                .map(|o| o.replace(" NULLS FIRST", "").replace(" NULLS LAST", ""))
                                .collect::<Vec<_>>()
                                .join(",")
                        ))?;
                        objects.push(("index", index));
                    }
                }
            }
            if let Kind::Fixpoint { rules } = &node.kind {
                let member = node.inputs.len();
                for side in [member, member + 1] {
                    let t = format!("{name}_op{id}_{side}");
                    // AUTOINCREMENT keeps rowids monotone after the delete pass
                    // removes the newest members, so `rowid>lo` still means new.
                    db.execute_batch(&format!(
                        "CREATE TABLE main.{}(__id INTEGER PRIMARY KEY AUTOINCREMENT,__k TEXT NOT NULL UNIQUE,{})",
                        quote(&t),
                        columns(node.fields.len())
                    ))?;
                    objects.push(("table", t));
                }
                let mut created = vec![];
                for (occurrence, expression) in rules.iter().flat_map(|r| &r.indexes) {
                    let side = match occurrence {
                        Occurrence::Input(side) => *side,
                        Occurrence::Member => member,
                    };
                    if created.contains(&(side, expression)) {
                        continue;
                    }
                    let index = format!("__ivm_{name}_fix{id}_{}", created.len());
                    db.execute_batch(&format!(
                        "CREATE INDEX main.{} ON {}({expression})",
                        quote(&index),
                        quote(&format!("{name}_op{id}_{side}"))
                    ))?;
                    objects.push(("index", index));
                    created.push((side, expression));
                }
            }
        }
        Ok(objects)
    }
    pub fn populate(&self, db: &Connection, name: &str) -> Result<()> {
        for source in &self.sources {
            let bad = source
                .columns
                .iter()
                .zip(&source.affinities)
                .filter_map(|(column, affinity)| {
                    let c = quote(column);
                    match affinity.as_str() {
                        "TEXT" => Some(format!("typeof({c}) IN ('blob','integer','real')")),
                        "INTEGER" => Some(format!("typeof({c}) IN ('blob','text','real')")),
                        "REAL" | "NUMERIC" => Some(format!("typeof({c}) IN ('blob','text')")),
                        _ => None,
                    }
                })
                .collect::<Vec<_>>();
            if bad.is_empty() {
                continue;
            }
            let invalid: bool = db.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM main.{} WHERE {})",
                    quote(&source.name),
                    bad.join(" OR ")
                ),
                [],
                |r| r.get(0),
            )?;
            if invalid {
                return Err(error("source value does not conform to declared affinity"));
            }
        }
        for (id, node) in self.nodes.iter().enumerate() {
            let out = out_table(id, node.fields.len());
            db.execute(
                &format!(
                    "CREATE TABLE IF NOT EXISTS {out}({},__m)",
                    columns(node.fields.len())
                ),
                [],
            )?;
            db.execute(&format!("DELETE FROM {out}"), [])?;
        }
        let mut reads = vec![0usize; self.nodes.len()];
        reads[self.output] += 1;
        for node in &self.nodes {
            match &node.kind {
                Kind::Input(_) => {}
                Kind::Map { .. } => reads[node.inputs[0]] += 1,
                Kind::Set(op) if *op == "all" => reads[node.inputs[0]] += 1,
                _ => {
                    for side in 0..node.inputs.len() {
                        reads[node.inputs[side]] += 1;
                    }
                }
            }
        }
        for id in 0..self.nodes.len() {
            let node = &self.nodes[id];
            let direct = match &node.kind {
                Kind::Map { .. } => false,
                Kind::Set(op) if *op == "all" => false,
                _ => true,
            };
            if direct {
                for side in 0..node.inputs.len() {
                    self.fill(db, name, id, side)?;
                    self.exhaust(db, &mut reads, node.inputs[side])?;
                }
            }
            self.materialize(db, name, id, false)?;
            if !direct {
                self.exhaust(db, &mut reads, node.inputs[0])?;
            }
            if id == self.output {
                self.write_state(db, name, id)?;
                self.exhaust(db, &mut reads, id)?;
            }
        }
        Ok(())
    }
    fn exhaust(&self, db: &Connection, reads: &mut [usize], child: usize) -> Result<()> {
        reads[child] -= 1;
        if reads[child] == 0 {
            let node = &self.nodes[child];
            db.execute(
                &format!("DELETE FROM {}", out_table(child, node.fields.len())),
                [],
            )?;
        }
        Ok(())
    }
    /// The `__k` composite of one input side of an arrangement node, as SQL
    /// over that side's `c<i>` columns. None for nodes without arrangements.
    pub(crate) fn key_sql(&self, id: usize, side: usize) -> Option<String> {
        let node = &self.nodes[id];
        let child_node = &self.nodes[node.inputs[side]];
        let width = child_node.fields.len();
        Some(match &node.kind {
            Kind::Set(_) => json_key(
                (0..node.fields.len())
                    .map(|i| {
                        folded(&crate::relational::key_expression(
                            &format!("c{i}"),
                            &node.fields[i].collation,
                        ))
                    })
                    .collect(),
            ),
            Kind::Join { left, right, .. } => {
                let positions = if side == 0 { left } else { right };
                json_key(
                    positions
                        .iter()
                        .map(|i| {
                            folded(&crate::relational::key_expression(
                                &format!("c{i}"),
                                &child_node.fields[*i].collation,
                            ))
                        })
                        .collect(),
                )
            }
            Kind::Group { keys, .. } => json_key(keys.iter().map(|e| folded(e)).collect()),
            Kind::Fixpoint { .. } => {
                json_key((0..width).map(|i| folded(&format!("c{i}"))).collect())
            }
            _ => return None,
        })
    }
    fn fill(&self, db: &Connection, name: &str, id: usize, side: usize) -> Result<()> {
        let node = &self.nodes[id];
        let child = node.inputs[side];
        let child_node = &self.nodes[child];
        let width = child_node.fields.len();
        let out = out_table(child, width);
        let t = table(name, id, side);
        let r = json_key((0..width).map(|i| plain(&format!("c{i}"))).collect());
        let Some(k) = self.key_sql(id, side) else {
            return Ok(());
        };
        let dict = keys_table(name);
        db.execute(
            &format!("INSERT OR IGNORE INTO {dict}(__v) SELECT {k} FROM {out}"),
            [],
        )?;
        let k = format!("(SELECT __i FROM {dict} WHERE __v={k})");
        // No UNIQUE target is left to upsert against. Rows sharing a composite
        // are equal in every column, so the grouped select keeps the same row.
        db.execute(
            &format!(
                "INSERT INTO {t}(__k,__r,__n,{cols}) SELECT {k},sqlite_ivm_hash(__ivm_r),__ivm_n,{cols} FROM (SELECT {r} AS __ivm_r,sum(__m) AS __ivm_n,{cols} FROM {out} GROUP BY {r})",
                cols = columns(width)
            ),
            [],
        )?;
        let bad: bool = db.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {t} WHERE typeof(__n)!='integer' OR __n<0)"),
            [],
            |r| r.get(0),
        )?;
        if bad {
            return Err(error("arrangement multiplicity overflow"));
        }
        Ok(())
    }
    /// The arrangement a bulk read scans: the whole table when populating, or a
    /// subquery over the touched keys when draining a batch. The subquery names
    /// `rowid` explicitly so the Set representative select can read it. No
    /// DDL here: a drain runs inside a trigger program.
    pub(crate) fn source_table(&self, db: &Connection, name: &str, id: usize, side: usize, restricted: bool) -> Result<String> {
        let full = table(name, id, side);
        if !restricted {
            return Ok(full);
        }
        let _ = db;
        Ok(format!(
            "(SELECT rowid AS rowid,* FROM {full} WHERE __k IN (SELECT __k FROM temp.__ivm_touched))"
        ))
    }
    fn write_state(&self, db: &Connection, name: &str, id: usize) -> Result<()> {
        let width = self.nodes[id].fields.len();
        let out = out_table(id, width);
        let state = format!("main.{}", quote(&format!("{name}_state")));
        let peak: i64 = db.query_row(
            &format!("SELECT coalesce(max(__m),0) FROM {out}"),
            [],
            |r| r.get(0),
        )?;
        if peak > BULK_MULTIPLICITY_BUDGET {
            return Err(error("result multiplicity expansion exceeds budget"));
        }
        let k = json_key((0..width).map(|i| plain(&format!("o.c{i}"))).collect());
        db.execute(
            &format!(
                "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<(SELECT coalesce((SELECT max(__m) FROM {out}),0))) INSERT INTO {state}(__key,{}) SELECT {k},o.c0{} FROM {out} o,seq WHERE seq.n<=o.__m",
                columns(width),
                (1..width).map(|i| format!(",o.c{i}")).collect::<String>()
            ),
            [],
        )?;
        Ok(())
    }
    /// Rejects a source row whose values do not fit its declared affinities.
    /// Runs at the trigger, before staging, so the statement fails, not the
    /// commit.
    pub fn validate(&self, source: usize, row: &[Value]) -> Result<()> {
        for (value, affinity) in row.iter().zip(&self.sources[source].affinities) {
            let valid = match value {
                Value::Null => true,
                Value::Blob(_) => affinity.is_empty(),
                Value::Text(_) => affinity == "TEXT" || affinity.is_empty(),
                Value::Integer(_) => affinity != "TEXT",
                Value::Real(_) => affinity != "TEXT" && affinity != "INTEGER",
            };
            if !valid {
                return Err(error("source value does not conform to declared affinity"));
            }
        }
        Ok(())
    }
    /// The collector that stages this view's source rows between a trigger
    /// firing and the drain. Its shadow table is one of the view's objects.
    pub fn collector(&self, name: &str) -> sqlite_bulk_trigger::Collector {
        let width = self.sources.iter().map(|s| s.columns.len()).max().unwrap_or(0);
        sqlite_bulk_trigger::Collector::new(format!("{name}_staged"), width)
    }
}
