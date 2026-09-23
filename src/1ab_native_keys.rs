//! Join and group keys stay in SQLite cells, with one indexed column per key.
use crate::{
    columns::substitute_columns,
    relational::{key_expression, Field, Kind, Plan},
    relational_maintenance::keys_table,
};

impl Plan {
    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    pub(crate) fn native_key_columns(&self, id: usize, side: usize) -> Option<Vec<String>> {
        let node = &self.nodes[id];
        Some(match &node.kind {
            Kind::Join { left, right, .. } => {
                let child = &self.nodes[node.inputs[side]];
                (if side == 0 { left } else { right }).iter().map(|i|
                    key_expression(&format!("c{i}"), &child.fields[*i].collation)
                ).collect()
            }
            Kind::Group { keys, .. } => keys.clone(),
            Kind::Set(_) => node.fields.iter().enumerate().map(|(i, field)|
                key_expression(&format!("c{i}"), &field.collation)
            ).collect(),
            _ => return None,
        })
    }

    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    pub(crate) fn native_key_values(&self, id: usize, side: usize) -> Option<Vec<String>> {
        self.native_key_columns(id, side).map(|keys| {
            if keys.is_empty() { vec!["0".into()] } else { keys }
        })
    }

    /// Dictionary columns have no affinity. Unary plus prevents the input
    /// expression from imposing its affinity on an already stored key.
    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    pub(crate) fn native_key_match(keys: &[String], alias: &str) -> String {
        keys.iter().enumerate().map(|(i,key)| format!("{alias}.k{i} IS (+({key}))"))
            .collect::<Vec<_>>().join(" AND ")
    }

    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    pub(crate) fn key_lookup(&self, name: &str, id: usize, side: usize) -> String {
        let dict = keys_table(name);
        let keys = self.native_key_values(id,side).expect("arrangement keys");
        format!("(SELECT d.__i FROM {dict} d WHERE d.__node={id} AND {})", Self::native_key_match(&keys,"d"))
    }

    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    pub(crate) fn insert_keys(&self, name: &str, id: usize, side: usize) -> String {
        let input = self.nodes[id].inputs[side];
        let child = self.out_table(input,self.nodes[input].fields.len());
        let dict = keys_table(name);
        let keys = self.native_key_values(id,side).expect("arrangement keys");
        let columns = (0..keys.len()).map(|i|format!("k{i}")).collect::<Vec<_>>().join(",");
        format!("INSERT OR IGNORE INTO {dict}(__node,{columns}) SELECT DISTINCT {id},{} FROM {child}",keys.join(","))
    }

    /// Drive a source/index probe from each distinct touched key. IS includes
    /// NULL groups; the join operator separately applies SQL '=' semantics.
    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    pub(crate) fn touched_source(&self, name: &str, id: usize, keys: &[String], source: &str) -> String {
        let dict = keys_table(name);
        let predicate = keys.iter().enumerate().map(|(i,key)| {
            let key = substitute_columns(key, |c|format!("s.c{c}"));
            format!("({key}) IS (+t.k{i})")
        }).collect::<Vec<_>>().join(" AND ");
        // Drive the integer primary-key lookup from the changed keys. An IN
        // predicate lets SQLite scan every dictionary key for this node first.
        format!("(SELECT d.* FROM temp.__ivm_touched changed CROSS JOIN {dict} d ON d.__i=changed.__k WHERE d.__node={id}) t CROSS JOIN {source} s ON {predicate}")
    }

    #[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
    pub(crate) fn native_join_match(&self, id: usize) -> String {
        let left = self.native_key_columns(id,0).expect("join keys");
        let right = self.native_key_columns(id,1).expect("join keys");
        if left.is_empty() { return "1".into(); }
        left.iter().zip(right).map(|(l,r)| format!("({})=({})",
            substitute_columns(l, |i|format!("l.c{i}")),
            substitute_columns(&r, |i|format!("r.c{i}")),
        )).collect::<Vec<_>>().join(" AND ")
    }
}

/// Exact bag equality uses separate SQLite values. REAL bits distinguish
/// representations such as negative zero without serializing a whole row.
#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn exact_row_columns(width: usize, alias: &str) -> Vec<String> {
    (0..width).flat_map(|i| {
        let c = format!("{alias}c{i}");
        [format!("typeof({c})"),format!("{c} COLLATE BINARY"),
            format!("CASE WHEN typeof({c})='real' THEN sqlite_ivm_real_hex({c}) END")]
    }).collect()
}

#[tracing::instrument(level = "trace", skip_all, fields(source_file = file!(), source_line = line!()))]
pub(crate) fn exact_row_match(width: usize, left: &str, right: &str) -> String {
    exact_row_columns(width,left).iter().zip(exact_row_columns(width,right))
        .map(|(l,r)|format!("({l}) IS ({r})")).collect::<Vec<_>>().join(" AND ")
}

/// SQLite equality for recursive membership, including NULL, numeric folding,
/// BLOB storage class, and the field's collation. The member columns have no
/// affinity, so IS compares the stored cells without introducing coercion.
pub(crate) fn membership_match(fields: &[Field], left: &str, right: &str) -> String {
    fields.iter().enumerate().map(|(i, field)|
        format!("({left}c{i} COLLATE {}) IS {right}c{i}", field.collation)
    ).collect::<Vec<_>>().join(" AND ")
}

/// Two native SQLite index expressions per cell: a storage-class tag and its
/// value. A tagged integer zero stands in for NULL, so the UNIQUE index treats
/// two NULL cells as equal without conflating NULL with an actual zero.
pub(crate) fn membership_index(fields: &[Field]) -> String {
    if fields.is_empty() { return "(0)".into(); }
    fields.iter().enumerate().flat_map(|(i, field)| [
        format!("(CASE typeof(c{i}) WHEN 'null' THEN 0 WHEN 'integer' THEN 1 WHEN 'real' THEN 1 WHEN 'text' THEN 2 ELSE 3 END)"),
        format!("(CASE WHEN c{i} IS NULL THEN 0 ELSE c{i} END) COLLATE {}", field.collation),
    ]).collect::<Vec<_>>().join(",")
}

/// The dictionary stores normalized cells from many nodes in one table. Each
/// node id and tagged cell tuple has one integer identity, including NULLs.
pub(crate) fn dictionary_membership_index(width: usize) -> String {
    let mut expressions = vec!["__node".to_string()];
    for i in 0..width {
        expressions.push(format!("(CASE typeof(k{i}) WHEN 'null' THEN 0 WHEN 'integer' THEN 1 WHEN 'real' THEN 1 WHEN 'text' THEN 2 ELSE 3 END)"));
        expressions.push(format!("(CASE WHEN k{i} IS NULL THEN 0 ELSE k{i} END) COLLATE BINARY"));
    }
    expressions.join(",")
}
