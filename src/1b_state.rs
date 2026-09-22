use crate::{
    catalog::{error, quote},
    statements::{self, Phase},
    relational::{Kind, Occurrence, Plan},
    relational_maintenance::{
        columns, folded, json_key, out_table, table, BULK_MULTIPLICITY_BUDGET,
    },
};
use rusqlite::{types::Value, Connection, Result};

impl Plan {
    /// When every grouping key is projected unchanged, stored output rows can
    /// supply the before-image without aggregating source inputs again.
    pub(crate) fn stored_group_key(&self) -> Option<String> {
        let node = &self.nodes[self.output];
        let Kind::Group { keys, expressions, window: false, .. } = &node.kind else {
            return None;
        };
        let positions = keys.iter().map(|key| expressions.iter().position(|expression| expression == key))
            .collect::<Option<Vec<_>>>()?;
        Some(positions.iter().map(|i| format!("c{i}")).collect::<Vec<_>>().join(","))
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
        let key_width = self.nodes.iter().enumerate().filter_map(|(id,_)| self.native_key_values(id,0).map(|k|k.len())).max().unwrap_or(1);
        let key_columns = (0..key_width).map(|i|format!("k{i}")).collect::<Vec<_>>().join(",");
        statements::batch(db, Phase::Declare, name, &format!(
            "CREATE TABLE main.{}(__i INTEGER PRIMARY KEY,__v TEXT UNIQUE,__node INTEGER,{key_columns})", quote(&dictionary)
        ))?;
        let native_index = fresh_index(db,name,format!("__ivm_{name}_native_keys"))?;
        statements::batch(db,Phase::Declare,name,&format!("CREATE INDEX main.{} ON {}(__node,{key_columns})",quote(&native_index),quote(&dictionary)))?;
        objects.push(("index",native_index));
        objects.push(("table", dictionary));
        let state = format!("{name}_state");
        let result_key = fresh_index(db, name, format!("__ivm_{name}_result_key"))?;
        statements::batch(
            db,
            Phase::Declare,
            name,
            &format!(
                "CREATE TABLE main.{}(__id INTEGER PRIMARY KEY AUTOINCREMENT,__check INTEGER NOT NULL,{}); CREATE INDEX main.{} ON {}({})",
                quote(&state),
                (0..self.names.len())
                    .map(|i| format!("c{i}"))
                    .collect::<Vec<_>>()
                    .join(","),
                quote(&result_key),
                quote(&state),
                crate::native_keys::exact_row_columns(self.names.len(), "").join(",")
            ),
        )?;
        objects.push(("table", state));
        objects.push(("index", result_key));
        if let Some(group_key) = self.stored_group_key().filter(|k| !k.is_empty()) {
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
                let mut expressions = match self.native_key_columns(id,side) {
                    Some(keys) if keys.is_empty() => vec![],
                    Some(keys) => vec![keys.join(",")],
                    None => vec![self.key_sql(id,side).expect("operator key")],
                };
                if let Kind::Group { order, .. } = &node.kind {
                    expressions.extend(order.iter().map(|o| o.replace(" NULLS FIRST", "").replace(" NULLS LAST", "")));
                }
                if let Kind::Fixpoint { rules } = &node.kind {
                    expressions.extend(rules.iter().flat_map(|r| &r.indexes).filter_map(|(occurrence, expression)| {
                        (*occurrence == Occurrence::Input(side)).then(|| expression.clone())
                    }));
                }
                for (position, expression) in expressions.iter().enumerate() {
                    if let Some((source, expression)) = self.source_expression(*input, expression) {
                        let index = fresh_index(db, name, format!("__ivm_{name}_source_{id}_{side}_{position}"))?;
                        statements::batch(db, Phase::Declare, name, &format!(
                            "CREATE INDEX main.{} ON {}({expression})", quote(&index), quote(&self.sources[source].name)
                        ))?;
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
                        Occurrence::Input(_) => continue,
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
                table(name, self.output, 1), self.live_input(db, name, self.output, 0, false, false)
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
    /// Encoded membership keys for sets and recursion. Join/group keys use
    /// native_key_columns and cannot take this encoding path.
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
            Kind::Fixpoint { .. } => {
                json_key((0..width).map(|i| folded(&format!("c{i}"))).collect())
            }
            _ => return None,
        })
    }
    fn fill(&self, db: &Connection, name: &str, id: usize, side: usize) -> Result<()> {
        let node = &self.nodes[id];
        if matches!(node.kind, Kind::Join { mode: "inner", .. } | Kind::Fixpoint { .. }) { return Ok(()); }
        statements::exec(db,Phase::Materialize,name,&self.insert_keys(name,id,side),[])?;
        Ok(())
    }
    /// Read current source rows, optionally restricted to touched keys.
    /// No DDL here: a drain may run inside a trigger program.
    pub(crate) fn source_table(&self, db: &Connection, name: &str, id: usize, side: usize, restricted: bool) -> Result<String> {
        Ok(self.live_input(db, name, id, side, restricted, false))
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
        let k = format!("sqlite_ivm_row_check({})",(0..width).map(|i|format!("o.c{i}")).collect::<Vec<_>>().join(","));
        statements::exec(
            db,
            Phase::Materialize,
            name,
            &format!(
                "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM seq WHERE n<(SELECT coalesce((SELECT max(__m) FROM {out}),0))) INSERT INTO {state}(__check,{}) SELECT {k},o.c0{} FROM {out} o,seq WHERE seq.n<=o.__m",
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
