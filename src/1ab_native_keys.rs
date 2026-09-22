//! Join and group keys stay in SQLite cells, with one indexed column per key.
use crate::{
    columns::substitute_columns,
    relational::{key_expression, Kind, Plan},
    relational_maintenance::{keys_table, out_table},
};

impl Plan {
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
            _ => return None,
        })
    }

    pub(crate) fn native_key_values(&self, id: usize, side: usize) -> Option<Vec<String>> {
        self.native_key_columns(id, side).map(|keys| {
            if keys.is_empty() { vec!["0".into()] } else { keys }
        })
    }

    /// Dictionary columns have no affinity. Unary plus prevents the input
    /// expression from imposing its affinity on an already stored key.
    pub(crate) fn native_key_match(keys: &[String], alias: &str) -> String {
        keys.iter().enumerate().map(|(i,key)| format!("{alias}.k{i} IS (+({key}))"))
            .collect::<Vec<_>>().join(" AND ")
    }

    pub(crate) fn key_lookup(&self, name: &str, id: usize, side: usize) -> String {
        let dict = keys_table(name);
        if let Some(keys) = self.native_key_values(id,side) {
            format!("(SELECT d.__i FROM {dict} d WHERE d.__node={id} AND {})", Self::native_key_match(&keys,"d"))
        } else {
            format!("(SELECT __i FROM {dict} WHERE __v={})",self.key_sql(id,side).expect("operator key"))
        }
    }

    pub(crate) fn insert_keys(&self, name: &str, id: usize, side: usize) -> String {
        let input = self.nodes[id].inputs[side];
        let child = out_table(input,self.nodes[input].fields.len());
        let dict = keys_table(name);
        if let Some(keys) = self.native_key_values(id,side) {
            let columns = (0..keys.len()).map(|i|format!("k{i}")).collect::<Vec<_>>().join(",");
            format!("INSERT INTO {dict}(__node,{columns}) SELECT DISTINCT {id},{} FROM {child} WHERE NOT EXISTS(SELECT 1 FROM {dict} d WHERE d.__node={id} AND {})",keys.join(","),Self::native_key_match(&keys,"d"))
        } else {
            format!("INSERT OR IGNORE INTO {dict}(__v) SELECT {} FROM {child}",self.key_sql(id,side).expect("operator key"))
        }
    }

    /// Drive a source/index probe from each distinct touched key. IS includes
    /// NULL groups; the join operator separately applies SQL '=' semantics.
    pub(crate) fn touched_source(&self, name: &str, id: usize, keys: &[String], source: &str) -> String {
        let dict = keys_table(name);
        let predicate = keys.iter().enumerate().map(|(i,key)| {
            let key = substitute_columns(key, |c|format!("s.c{c}"));
            format!("({key}) IS (+t.k{i})")
        }).collect::<Vec<_>>().join(" AND ");
        format!("(SELECT * FROM {dict} WHERE __node={id} AND __i IN (SELECT __k FROM temp.__ivm_touched)) t CROSS JOIN {source} s ON {predicate}")
    }

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
pub(crate) fn exact_row_columns(width: usize, alias: &str) -> Vec<String> {
    (0..width).flat_map(|i| {
        let c = format!("{alias}c{i}");
        [format!("typeof({c})"),format!("{c} COLLATE BINARY"),
            format!("CASE WHEN typeof({c})='real' THEN sqlite_ivm_real_hex({c}) END")]
    }).collect()
}

pub(crate) fn exact_row_match(width: usize, left: &str, right: &str) -> String {
    exact_row_columns(width,left).iter().zip(exact_row_columns(width,right))
        .map(|(l,r)|format!("({l}) IS ({r})")).collect::<Vec<_>>().join(" AND ")
}
