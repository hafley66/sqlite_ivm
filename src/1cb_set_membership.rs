//! Persistent support counts for UNION and DISTINCT. One representative row
//! per equality key, never a copy of every operator input row.
use crate::{
    catalog::{error, quote},
    native_keys::exact_row_match,
    relational::{Kind, Plan},
    relational_maintenance::{columns, identity_sql, out_table},
    statements::{self, Phase},
};
use rusqlite::{Connection, Result};

pub(crate) struct SetMembershipStatements {
    pub(crate) before: String,
    pub(crate) update: String,
    pub(crate) bad: String,
    pub(crate) representative_removed: String,
    pub(crate) replace_representatives: String,
    pub(crate) remove_empty: String,
    pub(crate) after: String,
    pub(crate) clear_before: String,
}

pub(crate) fn set_membership_table(name: &str, id: usize) -> String {
    format!("main.{}", quote(&format!("{name}_op{id}x0")))
}

impl Plan {
    pub(crate) fn has_set_membership(&self, id: usize) -> bool {
        matches!(self.nodes[id].kind, Kind::Set("union" | "distinct"))
    }

    pub(crate) fn create_set_membership(&self, db: &Connection, name: &str, id: usize) -> Result<String> {
        let width = self.nodes[id].fields.len();
        let table_name = format!("{name}_op{id}x0");
        let cells = (0..width).map(|i|format!("c{i}")).collect::<Vec<_>>().join(",");
        statements::batch(db, Phase::Declare, name, &format!(
            "CREATE TABLE main.{}(__k INTEGER PRIMARY KEY,__n INTEGER NOT NULL,__left INTEGER NOT NULL,__right INTEGER NOT NULL,{cells})",
            quote(&table_name)
        ))?;
        Ok(table_name)
    }

    fn set_membership_inputs(&self, name: &str, id: usize, positive_only: bool) -> String {
        let width = self.nodes[id].fields.len();
        let cols = columns(width);
        self.nodes[id].inputs.iter().enumerate().map(|(side,input)| {
            let child = out_table(*input,width);
            let key = self.key_lookup(name,id,side);
            let identity = identity_sql(width);
            let filter = if positive_only { " WHERE __m<0" } else { "" };
            format!("SELECT {key} AS __k,__m,{cols},{side} AS __side,{identity} AS __identity FROM {child}{filter}")
        }).collect::<Vec<_>>().join(" UNION ALL ")
    }

    pub(crate) fn populate_set_membership(&self, db: &Connection, name: &str, id: usize) -> Result<()> {
        let width = self.nodes[id].fields.len();
        let cols = columns(width);
        let state = set_membership_table(name,id);
        let out = out_table(id,width);
        let inputs = self.set_membership_inputs(name,id,false);
        let projected = (0..width).map(|i|format!("o.c{i}")).collect::<Vec<_>>().join(",");
        let key = self.key_lookup(name,id,0);
        statements::exec(db,Phase::Materialize,name,&format!(
            "WITH input_rows AS ({inputs}), support AS (SELECT __k,sum(__m) AS __n,\
             sum(CASE WHEN __side=0 THEN __m ELSE 0 END) AS __left,\
             sum(CASE WHEN __side=1 THEN __m ELSE 0 END) AS __right FROM input_rows GROUP BY __k) \
             INSERT INTO {state}(__k,__n,__left,__right,{cols}) \
             SELECT support.__k,support.__n,support.__left,support.__right,{projected} \
             FROM support JOIN {out} o ON {key}=support.__k WHERE support.__n>0"
        ),[])?;
        Ok(())
    }

    pub(crate) fn set_membership_statements(&self, name: &str, id: usize) -> SetMembershipStatements {
        let width = self.nodes[id].fields.len();
        let cols = columns(width);
        let state = set_membership_table(name,id);
        let out = out_table(id,width);
        let before = format!("temp.__ivm_before_{width}_{id}");
        let inputs = self.set_membership_inputs(name,id,false);
        let negative = self.set_membership_inputs(name,id,true);
        let candidate_columns = (0..width).map(|i|format!("c{i}")).collect::<Vec<_>>().join(",");
        let assignments = (0..width).map(|i| {
            let key = self.key_lookup(name,id,0);
            format!("c{i}=(SELECT o.c{i} FROM {out} o WHERE {key}={state}.__k LIMIT 1)")
        }).collect::<Vec<_>>().join(",");
        let side_change = (0..width).map(|i|format!(
            "c{i}=CASE WHEN {state}.__left=0 AND excluded.__left>0 THEN excluded.c{i} ELSE {state}.c{i} END"
        )).collect::<Vec<_>>().join(",");
        let exact = exact_row_match(width,"s.","d.");
        SetMembershipStatements {
            before: format!("INSERT INTO {before}({cols},__m) SELECT {cols},1 FROM {state} WHERE __n>0 AND __k IN (SELECT __k FROM temp.__ivm_touched)"),
            update: format!(
                "WITH d AS ({inputs}), ranked AS (SELECT d.*,sum(__m) OVER(PARTITION BY __k) AS __change,\
                 sum(CASE WHEN __side=0 THEN __m ELSE 0 END) OVER(PARTITION BY __k) AS __left_change,\
                 sum(CASE WHEN __side=1 THEN __m ELSE 0 END) OVER(PARTITION BY __k) AS __right_change,\
                 row_number() OVER(PARTITION BY __k ORDER BY (__m<=0),__side,__identity) AS __rank FROM d) \
                 INSERT INTO {state}(__k,__n,__left,__right,{cols}) \
                 SELECT __k,__change,__left_change,__right_change,{candidate_columns} \
                 FROM ranked WHERE __rank=1 \
                 ON CONFLICT(__k) DO UPDATE SET __n={state}.__n+excluded.__n,\
                   __left={state}.__left+excluded.__left,__right={state}.__right+excluded.__right,{side_change}"
            ),
            bad: format!("SELECT EXISTS(SELECT 1 FROM {state} WHERE __k IN (SELECT __k FROM temp.__ivm_touched) AND (typeof(__n)!='integer' OR typeof(__left)!='integer' OR typeof(__right)!='integer' OR __n<0 OR __left<0 OR __right<0 OR __n!=__left+__right))"),
            representative_removed: format!(
                "WITH d AS ({negative}) SELECT EXISTS(SELECT 1 FROM {state} s JOIN d ON s.__k=d.__k WHERE s.__n>0 AND {exact})"
            ),
            replace_representatives: format!("UPDATE {state} SET {assignments} WHERE __n>0 AND __k IN (SELECT __k FROM temp.__ivm_touched)"),
            remove_empty: format!("DELETE FROM {state} WHERE __n=0 AND __k IN (SELECT __k FROM temp.__ivm_touched)"),
            after: format!("INSERT INTO {out}({cols},__m) SELECT {cols},1 FROM {state} WHERE __n>0 AND __k IN (SELECT __k FROM temp.__ivm_touched)"),
            clear_before: format!("DELETE FROM temp.__ivm_before_{width}_{id}"),
        }
    }

    pub(crate) fn check_set_membership(&self, db: &Connection, name: &str, bad: &str) -> Result<()> {
        let invalid: bool = statements::query_cached(db,Phase::Maintain,name,bad,[],|r|r.get(0))?;
        if invalid { Err(error("set support count outside integer domain")) } else { Ok(()) }
    }
}
