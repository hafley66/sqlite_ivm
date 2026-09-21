use crate::{
    catalog::{error, quote},
    statements::{self, Phase},
    relational::{Kind, Occurrence, Plan},
    relational_maintenance::{
        columns, folded, json_key, keys_table, out_table, plain, table, BULK_MULTIPLICITY_BUDGET,
    },
};
use rusqlite::{types::Value, Connection, Result};

impl Plan {
    /// When every grouping key is projected unchanged, stored output rows can
    /// supply the before-image without aggregating the input arrangement again.
    pub(crate) fn stored_group_key(&self) -> Option<String> {
        let node = &self.nodes[self.output];
        let Kind::Group { keys, expressions, window: false, limit: None, .. } = &node.kind else {
            return None;
        };
        let positions = keys.iter().map(|key| expressions.iter().position(|expression| expression == key))
            .collect::<Option<Vec<_>>>()?;
        Some(json_key(positions.iter().map(|i| folded(&format!("c{i}"))).collect()))
    }
    pub fn create_state(&self, db: &Connection, name: &str) -> Result<Vec<(&'static str, String)>> {
        // A recreated view must not collide with index names a rename retained:
        // allocate a fresh suffix while the name is recorded for another view.
        fn fresh_index(db: &Connection, name: &str, base: String) -> Result<String> {
            let mut index = base.clone();
            let recorded: bool = statements::query(
                db,
                Phase::Declare,
                name,
                "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema WHERE name='__ivm_objects')",
                [],
                |r| r.get(0),
            )?;
            if !recorded {
                return Ok(index);
            }
            let mut suffix = 0;
            while statements::query(
                db,
                Phase::Declare,
                name,
                "SELECT EXISTS(SELECT 1 FROM main.__ivm_objects WHERE object_type='index' AND object_name=?1)",
                [&index],
                |r| r.get::<_, bool>(0),
            )? {
                suffix += 1;
                index = format!("{base}_{suffix}");
            }
            Ok(index)
        }
        let mut objects = vec![];
        let dictionary = format!("{name}_keys");
        statements::batch(
            db,
            Phase::Declare,
            name,
            &format!(
                "CREATE TABLE main.{}(__i INTEGER PRIMARY KEY,__v TEXT NOT NULL UNIQUE)",
                quote(&dictionary)
            ),
        )?;
        objects.push(("table", dictionary));
        let state = format!("{name}_state");
        let result_key = fresh_index(db, name, format!("__ivm_{name}_result_key"))?;
        statements::batch(
            db,
            Phase::Declare,
            name,
            &format!(
                "CREATE TABLE main.{}(__key TEXT NOT NULL,{}); CREATE INDEX main.{} ON {}(__key)",
                quote(&state),
                (0..self.names.len())
                    .map(|i| format!("c{i}"))
                    .collect::<Vec<_>>()
                    .join(","),
                quote(&result_key),
                quote(&state)
            ),
        )?;
        objects.push(("table", state));
        objects.push(("index", result_key));
        if let Some(group_key) = self.stored_group_key() {
            let index = fresh_index(db, name, format!("__ivm_{name}_result_group"))?;
            statements::batch(db, Phase::Declare, name, &format!(
                "CREATE INDEX main.{} ON {}({group_key})", quote(&index), quote(&format!("{name}_state"))
            ))?;
            objects.push(("index", index));
        }
        if let Some(sums) = crate::relational_program::aggregate_sum_inputs(self) {
            let metadata = format!("{name}_op{}x1", self.output);
            let columns = sums.iter().map(|(i,_)|format!("nn{i} INTEGER NOT NULL")).collect::<Vec<_>>().join(",");
            statements::batch(db, Phase::Declare, name, &format!("CREATE TABLE main.{}(__k INTEGER PRIMARY KEY,__safe INTEGER NOT NULL,{columns})", quote(&metadata)))?;
            objects.push(("table", metadata));
        }
        for (id, node) in self.nodes.iter().enumerate() {
            if matches!(node.kind, Kind::Input(_) | Kind::Map { .. }) {
                continue;
            }
            for (side, input) in node.inputs.iter().enumerate() {
                let t = format!("{name}_op{id}x{side}");
                let n = self.nodes[*input].fields.len();
                let key = fresh_index(db, name, format!("__ivm_{name}_op{id}_{side}_key"))?;
                let row = fresh_index(db, name, format!("__ivm_{name}_op{id}_{side}_row"))?;
                statements::batch(db, Phase::Declare, name, &format!("CREATE TABLE main.{}(__k INTEGER NOT NULL,__r INTEGER NOT NULL,__n INTEGER NOT NULL,{}); CREATE INDEX main.{} ON {}(__k); CREATE UNIQUE INDEX main.{} ON {}(__r,{})",quote(&t),columns(n),quote(&key),quote(&t),quote(&row),quote(&t),crate::relational_maintenance::identity_sql(n)))?;
                objects.push(("table", t));
                objects.push(("index", key));
                objects.push(("index", row));
                if let Kind::Group {
                    order,
                    limit: Some(_),
                    ..
                } = &node.kind
                {
                    if !order.is_empty() {
                        let index =
                            fresh_index(db, name, format!("__ivm_{name}_op{id}_{side}_order"))?;
                        statements::batch(
                            db,
                            Phase::Declare,
                            name,
                            &format!(
                                "CREATE INDEX main.{} ON {}(__k,{})",
                                quote(&index),
                                quote(&format!("{name}_op{id}x{side}")),
                                order
                                    .iter()
                                    .map(|o| o.replace(" NULLS FIRST", "").replace(" NULLS LAST", ""))
                                    .collect::<Vec<_>>()
                                    .join(",")
                            ),
                        )?;
                        objects.push(("index", index));
                    }
                }
            }
            if let Kind::Fixpoint { rules } = &node.kind {
                let member = node.inputs.len();
                for side in [member, member + 1] {
                    let t = format!("{name}_op{id}x{side}");
                    // removes the newest members, so `rowid>lo` still means new.
                    statements::batch(
                        db,
                        Phase::Declare,
                        name,
                        &format!(
                            "CREATE TABLE main.{}(__id INTEGER PRIMARY KEY AUTOINCREMENT,__k TEXT NOT NULL UNIQUE,{})",
                            quote(&t),
                            columns(node.fields.len())
                        ),
                    )?;
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
                    let index = fresh_index(db, name, format!("__ivm_{name}_fix{id}_{}", created.len()))?;
                    statements::batch(
                        db,
                        Phase::Declare,
                        name,
                        &format!(
                            "CREATE INDEX main.{} ON {}({expression})",
                            quote(&index),
                            quote(&format!("{name}_op{id}x{side}"))
                        ),
                    )?;
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
            let invalid: bool = statements::query(
                db,
                Phase::Declare,
                name,
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
            statements::exec(
                db,
                Phase::Declare,
                name,
                &format!(
                    "CREATE TABLE IF NOT EXISTS {out}({},__m)",
                    columns(node.fields.len())
                ),
                [],
            )?;
            statements::exec(db, Phase::Declare, name, &format!("DELETE FROM {out}"), [])?;
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
                    self.exhaust(db, name, &mut reads, node.inputs[side])?;
                }
            }
            self.materialize(db, name, id, false)?;
            if !direct {
                self.exhaust(db, name, &mut reads, node.inputs[0])?;
            }
            if id == self.output {
                self.write_state(db, name, id)?;
                self.exhaust(db, name, &mut reads, id)?;
            }
        }
        if let Some(sums) = crate::relational_program::aggregate_sum_inputs(self) {
            let columns = sums.iter().map(|(i,_)|format!("nn{i}")).collect::<Vec<_>>().join(",");
            let values = sums.iter().map(|(_,v)|format!("sum(CASE WHEN ({v}) IS NULL THEN 0 ELSE __n END)")).collect::<Vec<_>>().join(",");
            let safe = sums.iter().map(|(_,v)|format!("min(typeof({v}) IN ('integer','null') AND coalesce(abs(CAST(({v}) AS REAL)),0)<=1000000)")).collect::<Vec<_>>().join(" AND ");
            statements::exec(db, Phase::Materialize, name, &format!(
                "INSERT INTO {}(__k,__safe,{columns}) SELECT __k,sum(__n)<=1000000 AND {safe},{values} FROM {} GROUP BY __k",
                table(name, self.output, 1), table(name, self.output, 0)
            ), [])?;
        }
        Ok(())
    }
    fn exhaust(&self, db: &Connection, name: &str, reads: &mut [usize], child: usize) -> Result<()> {
        reads[child] -= 1;
        if reads[child] == 0 {
            let node = &self.nodes[child];
            statements::exec(
                db,
                Phase::Materialize,
                name,
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
        statements::exec(
            db,
            Phase::Materialize,
            name,
            &format!("INSERT OR IGNORE INTO {dict}(__v) SELECT {k} FROM {out}"),
            [],
        )?;
        let k = format!("(SELECT __i FROM {dict} WHERE __v={k})");
        // No UNIQUE target is left to upsert against. Rows sharing a composite
        // are equal in every column, so the grouped select keeps the same row.
        statements::exec(
            db,
            Phase::Materialize,
            name,
            &format!(
                "INSERT INTO {t}(__k,__r,__n,{cols}) SELECT {k},sqlite_ivm_hash(__ivm_r),__ivm_n,{cols} FROM (SELECT {r} AS __ivm_r,sum(__m) AS __ivm_n,{cols} FROM {out} GROUP BY {r})",
                cols = columns(width)
            ),
            [],
        )?;
        let bad: bool = statements::query(
            db,
            Phase::Materialize,
            name,
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
        let peak: i64 = statements::query(
            db,
            Phase::Materialize,
            name,
            &format!("SELECT coalesce(max(__m),0) FROM {out}"),
            [],
            |r| r.get(0),
        )?;
        if peak > BULK_MULTIPLICITY_BUDGET {
            return Err(error("result multiplicity expansion exceeds budget"));
        }
        let k = json_key((0..width).map(|i| plain(&format!("o.c{i}"))).collect());
        statements::exec(
            db,
            Phase::Materialize,
            name,
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
        sqlite_bulk_trigger::Collector::new(name, width)
    }
}
