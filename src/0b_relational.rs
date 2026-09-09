//! Owned relational plans lowered from sqlite3-parser. SQLite evaluates scalar expressions.
use crate::query::error;
use rusqlite::{Connection, Result};
use sqlite3_parser::{ast::*, lexer::sql::Parser, Bump, FallibleIterator};
fn sql<T: fmt::ToTokens>(value: &T) -> String {
    struct Sql<'a, T>(&'a T);
    impl<T: fmt::ToTokens> std::fmt::Display for Sql<'_, T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.to_fmt(f)
        }
    }
    Sql(value).to_string()
}

#[derive(Clone, Debug)]
pub struct Field {
    pub qualifier: String,
    pub name: String,
    pub affinity: String,
    pub visible: bool,
    pub unqualified: bool,
    pub position: usize,
    pub merged_star: bool,
    pub collation: String,
}
#[derive(Clone, Debug)]
pub enum Kind {
    Input(usize),
    Map {
        expressions: Vec<String>,
        predicate: Option<String>,
    },
    Join {
        left: Vec<usize>,
        right: Vec<usize>,
        mode: &'static str,
        predicate: Option<String>,
    },
    Set(&'static str),
    Group {
        keys: Vec<String>,
        expressions: Vec<String>,
        order: Vec<String>,
        limit: Option<i64>,
        offset: i64,
        having: Option<String>,
        window: bool,
    },
    Reach,
}
#[derive(Clone, Debug)]
pub struct Node {
    pub kind: Kind,
    pub inputs: Vec<usize>,
    pub fields: Vec<Field>,
}
#[derive(Clone, Debug)]
pub struct Source {
    pub name: String,
    pub columns: Vec<String>,
    pub affinities: Vec<String>,
    pub collations: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct Plan {
    pub sources: Vec<Source>,
    pub nodes: Vec<Node>,
    pub output: usize,
    pub names: Vec<String>,
}
fn name(n: &str) -> String {
    if n.starts_with('"') && n.ends_with('"') {
        n[1..n.len() - 1].replace("\"\"", "\"")
    } else if n.starts_with('[') && n.ends_with(']') {
        n[1..n.len() - 1].into()
    } else if n.starts_with('`') && n.ends_with('`') {
        n[1..n.len() - 1].replace("``", "`")
    } else {
        n.into()
    }
}
fn alias(a: &Option<As<'_>>) -> Option<String> {
    a.as_ref().map(|a| match a {
        As::As(n) | As::Elided(n) => name(n.0),
    })
}
fn resolve(e: &Expr<'_>, fields: &[Field]) -> Result<usize> {
    let (q, n) = match e {
        Expr::Id(n) => (None, name(n.0)),
        Expr::Name(n) => (None, name(n.0)),
        Expr::Qualified(q, n) => (Some(name(q.0)), name(n.0)),
        _ => return Err(error("expected column")),
    };
    let found = fields
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            f.name.eq_ignore_ascii_case(&n)
                && q.as_ref()
                    .map_or(f.unqualified, |q| f.qualifier.eq_ignore_ascii_case(q))
        })
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    if found.len() != 1 {
        return Err(error(format!("unknown or ambiguous column {e}")));
    }
    Ok(found[0])
}
fn explicit_collation(e: &Expr<'_>) -> Option<String> {
    match e {
        Expr::Collate(_, n) => Some(name(n).to_ascii_uppercase()),
        Expr::Binary(a, _, b) => explicit_collation(a).or_else(|| explicit_collation(b)),
        Expr::Cast { expr, .. } | Expr::Unary(_, expr) => explicit_collation(expr),
        Expr::Parenthesized(es) if es.len() == 1 => explicit_collation(&es[0]),
        _ => None,
    }
}
fn implicit_collation(e: &Expr<'_>, fields: &[Field]) -> Option<String> {
    if let Ok(i) = resolve(e, fields) {
        return Some(fields[i].collation.clone());
    }
    match e {
        Expr::Cast { expr, .. } | Expr::Unary(UnaryOperator::Positive, expr) => {
            implicit_collation(expr, fields)
        }
        Expr::Parenthesized(es) if es.len() == 1 => implicit_collation(&es[0], fields),
        _ => None,
    }
}
fn collation(e: &Expr<'_>, fields: &[Field]) -> String {
    explicit_collation(e)
        .or_else(|| implicit_collation(e, fields))
        .unwrap_or("BINARY".into())
}
pub fn key_expression(value: &str, collation: &str) -> String {
    match collation {
        "NOCASE" => {
            format!("CASE WHEN typeof({value})='text' THEN lower({value}) ELSE {value} END")
        }
        "RTRIM" => {
            format!("CASE WHEN typeof({value})='text' THEN rtrim({value},' ') ELSE {value} END")
        }
        _ => value.into(),
    }
}
pub fn expression(e: &Expr<'_>, fields: &[Field], aggregate: bool) -> Result<String> {
    expression_aliases(e, fields, aggregate, &[])
}
fn expression_aliases(
    e: &Expr<'_>,
    fields: &[Field],
    aggregate: bool,
    aliases: &[(String, String)],
) -> Result<String> {
    if matches!(e, Expr::Id(_) | Expr::Name(_)) && resolve(e, fields).is_err() {
        let n = match e {
            Expr::Id(n) => name(n.0),
            Expr::Name(n) => name(n.0),
            _ => unreachable!(),
        };
        if let Some((_, value)) = aliases.iter().find(|(a, _)| a.eq_ignore_ascii_case(&n)) {
            return Ok(format!("({value})"));
        }
    }
    let sub = |e| expression_aliases(e, fields, aggregate, aliases);
    Ok(match e {
        Expr::Id(_) | Expr::Name(_) | Expr::Qualified(_, _) => {
            let i = resolve(e, fields)?;
            let v = if fields[i].affinity.is_empty() {
                format!("c{i}")
            } else {
                format!("CAST(c{i} AS {})", fields[i].affinity)
            };
            if fields[i].collation == "BINARY" {
                v
            } else {
                format!("({v} COLLATE {})", fields[i].collation)
            }
        }
        Expr::Collate(e, n) => {
            let c = name(n).to_ascii_uppercase();
            if !["BINARY", "NOCASE", "RTRIM"].contains(&c.as_str()) {
                return Err(error("unsupported expression collation"));
            }
            format!("({} COLLATE {c})", sub(e)?)
        }
        Expr::Literal(_) => e.to_string(),
        Expr::Cast { expr, type_name } => format!(
            "CAST({} AS {})",
            sub(expr)?,
            type_name.as_ref().map(sql).unwrap_or_default()
        ),
        Expr::Case {
            base,
            when_then_pairs,
            else_expr,
        } => format!(
            "CASE {} {} {} END",
            base.map(sub).transpose()?.unwrap_or_default(),
            when_then_pairs
                .iter()
                .map(|(a, b)| Ok(format!("WHEN {} THEN {}", sub(a)?, sub(b)?)))
                .collect::<Result<Vec<_>>>()?
                .join(" "),
            else_expr
                .map(|e| sub(e).map(|s| format!("ELSE {s}")))
                .transpose()?
                .unwrap_or_default()
        ),
        Expr::Like {
            lhs,
            not,
            op,
            rhs,
            escape,
        } => format!(
            "({} {}{} {}{})",
            sub(lhs)?,
            if *not { "NOT " } else { "" },
            sql(op),
            sub(rhs)?,
            escape
                .map(|e| sub(e).map(|s| format!(" ESCAPE {s}")))
                .transpose()?
                .unwrap_or_default()
        ),
        Expr::Binary(l, op, r) => {
            let op = match op {
                Operator::Add => "+",
                Operator::Subtract => "-",
                Operator::Multiply => "*",
                Operator::Divide => "/",
                Operator::Modulus => "%",
                Operator::Equals => "=",
                Operator::NotEquals => "<>",
                Operator::Less => "<",
                Operator::LessEquals => "<=",
                Operator::Greater => ">",
                Operator::GreaterEquals => ">=",
                Operator::And => "AND",
                Operator::Or => "OR",
                Operator::Is => "IS",
                Operator::IsNot => "IS NOT",
                Operator::Concat => "||",
                Operator::BitwiseAnd => "&",
                Operator::BitwiseOr => "|",
                Operator::LeftShift => "<<",
                Operator::RightShift => ">>",
                _ => return Err(error("unsupported binary operator")),
            };
            if ["=", "<>", "<", "<=", ">", ">=", "IS", "IS NOT"].contains(&op) {
                let c = explicit_collation(l)
                    .or_else(|| explicit_collation(r))
                    .or_else(|| implicit_collation(l, fields))
                    .or_else(|| implicit_collation(r, fields))
                    .unwrap_or("BINARY".into());
                format!("(({} COLLATE {c}) {op} {})", sub(l)?, sub(r)?)
            } else {
                format!("({} {op} {})", sub(l)?, sub(r)?)
            }
        }
        Expr::Unary(op, v) => format!(
            "({} {})",
            match op {
                UnaryOperator::Not => "NOT",
                UnaryOperator::Negative => "-",
                UnaryOperator::Positive => "+",
                UnaryOperator::BitwiseNot => "~",
            },
            sub(v)?
        ),
        Expr::Parenthesized(es) if es.len() == 1 => format!("({})", sub(&es[0])?),
        Expr::IsNull(v) => format!("({} IS NULL)", sub(v)?),
        Expr::NotNull(v) => format!("({} IS NOT NULL)", sub(v)?),
        Expr::Between {
            lhs,
            not,
            start,
            end,
        } => format!(
            "({} {}BETWEEN {} AND {})",
            sub(lhs)?,
            if *not { "NOT " } else { "" },
            sub(start)?,
            sub(end)?
        ),
        Expr::InList { lhs, not, rhs } => format!(
            "({} {}IN ({}))",
            sub(lhs)?,
            if *not { "NOT " } else { "" },
            rhs.unwrap_or(&[])
                .iter()
                .map(sub)
                .collect::<Result<Vec<_>>>()?
                .join(",")
        ),
        Expr::FunctionCallStar { name, filter_over }
            if aggregate
                && name.0.eq_ignore_ascii_case("count")
                && filter_over.as_ref().is_none_or(|t| t.over_clause.is_none()) =>
        {
            if let Some(filter) = filter_over.as_ref().and_then(|t| t.filter_clause) {
                format!(
                    "coalesce(sum(CASE WHEN {} THEN __n ELSE 0 END),0)",
                    expression(filter, fields, false)?
                )
            } else {
                "coalesce(sum(__n),0)".into()
            }
        }
        Expr::FunctionCall {
            name,
            distinctness,
            args,
            order_by: None,
            filter_over,
        } if filter_over.as_ref().is_none_or(|t| t.over_clause.is_none()) => {
            let function = name.0.to_ascii_lowercase();
            let args = args.unwrap_or(&[]);
            if aggregate
                && ["count", "sum", "avg", "min", "max"].contains(&function.as_str())
                && args.len() == 1
            {
                let mut value = expression(&args[0], fields, false)?;
                if let Some(filter) = filter_over.as_ref().and_then(|t| t.filter_clause) {
                    value = format!(
                        "CASE WHEN {} THEN {value} ELSE NULL END",
                        expression(filter, fields, false)?
                    );
                }
                if *distinctness == Some(Distinctness::Distinct) {
                    format!("{function}(DISTINCT {value})")
                } else {
                    match function.as_str(){
                    "count"=>format!("coalesce(sum(CASE WHEN {value} IS NULL THEN 0 ELSE __n END),0)"),
                    "sum"=>format!("sum(CASE WHEN typeof({value})='integer' AND typeof(({value})*__n)!='integer' THEN abs(-9223372036854775808) ELSE ({value})*__n END)"),
                    "avg"=>format!("total(({value})*__n)/nullif(sum(CASE WHEN {value} IS NULL THEN 0 ELSE __n END),0)"),
                    _=>format!("{function}({value})"),
                }
                }
            } else if [
                "abs",
                "coalesce",
                "ifnull",
                "nullif",
                "lower",
                "upper",
                "length",
                "substr",
                "substring",
                "round",
                "trim",
                "ltrim",
                "rtrim",
                "replace",
                "instr",
                "unicode",
                "char",
                "hex",
                "typeof",
                "quote",
                "printf",
                "format",
                "sign",
            ]
            .contains(&function.as_str())
            {
                format!(
                    "{function}({})",
                    args.iter().map(sub).collect::<Result<Vec<_>>>()?.join(",")
                )
            } else {
                // bind() checks SQLite's bytecode and registered function flags
                // before any state is installed. Only deterministic scalars
                // reach this branch; SQLite checks their arity and value types.
                format!(
                    "{}({})",
                    crate::query::quote(&function),
                    args.iter().map(sub).collect::<Result<Vec<_>>>()?.join(",")
                )
            }
        }
        Expr::FunctionCall {
            filter_over: Some(tail),
            ..
        } if tail.over_clause.is_some() => {
            return Err(error(
                "window calls must be projected directly or through a FROM subquery",
            ))
        }
        _ => return Err(error(format!("unsupported expression {e}"))),
    })
}
fn pairs(
    e: &Expr<'_>,
    fields: &[Field],
    split: usize,
    l: &mut Vec<usize>,
    r: &mut Vec<usize>,
) -> Result<()> {
    match e {
        Expr::Parenthesized(es) if es.len() == 1 => pairs(&es[0], fields, split, l, r),
        Expr::Binary(a, Operator::And, b) => {
            pairs(a, fields, split, l, r)?;
            pairs(b, fields, split, l, r)
        }
        Expr::Binary(a, Operator::Equals, b) => {
            let (a, b) = (resolve(a, fields)?, resolve(b, fields)?);
            if a < split && b >= split {
                l.push(a);
                r.push(b - split);
            } else if b < split && a >= split {
                l.push(b);
                r.push(a - split);
            } else {
                return Err(error("join equality must connect its inputs"));
            }
            Ok(())
        }
        _ => Err(error("join requires column equality conjunctions")),
    }
}
fn index_pairs(
    e: &Expr<'_>,
    fields: &[Field],
    split: usize,
    l: &mut Vec<usize>,
    r: &mut Vec<usize>,
) {
    match e {
        Expr::Parenthesized(es) if es.len() == 1 => index_pairs(&es[0], fields, split, l, r),
        Expr::Binary(a, Operator::And, b) => {
            index_pairs(a, fields, split, l, r);
            index_pairs(b, fields, split, l, r);
        }
        Expr::Binary(a, Operator::Equals, b) => {
            if let (Ok(a), Ok(b)) = (resolve(a, fields), resolve(b, fields)) {
                if fields[a].affinity == fields[b].affinity
                    && fields[a].collation == fields[b].collation
                {
                    if a < split && b >= split {
                        l.push(a);
                        r.push(b - split);
                    } else if b < split && a >= split {
                        l.push(b);
                        r.push(a - split);
                    }
                }
            }
        }
        _ => {}
    }
}
fn affinity(declared: &str) -> String {
    let declared = declared.to_ascii_uppercase();
    if declared.contains("INT") {
        "INTEGER"
    } else if ["CHAR", "CLOB", "TEXT"]
        .iter()
        .any(|s| declared.contains(s))
    {
        "TEXT"
    } else if ["REAL", "FLOA", "DOUB"]
        .iter()
        .any(|s| declared.contains(s))
    {
        "REAL"
    } else if declared.is_empty() || declared.contains("BLOB") {
        ""
    } else {
        "NUMERIC"
    }
    .into()
}
fn expression_affinity(e: &Expr<'_>, fields: &[Field]) -> String {
    if let Ok(i) = resolve(e, fields) {
        return fields[i].affinity.clone();
    }
    match e {
        Expr::Cast { type_name, .. } => type_name
            .as_ref()
            .map(|t| affinity(t.name))
            .unwrap_or_default(),
        Expr::Collate(e, _) => expression_affinity(e, fields),
        Expr::Parenthesized(es) if es.len() == 1 => expression_affinity(&es[0], fields),
        _ => String::new(),
    }
}
fn has_aggregate(e: &Expr<'_>) -> bool {
    match e {
        Expr::FunctionCall {
            name,
            args,
            filter_over,
            ..
        } => {
            (filter_over.as_ref().is_none_or(|t| t.over_clause.is_none())
                && ["count", "sum", "avg", "min", "max"]
                    .contains(&name.0.to_ascii_lowercase().as_str())
                && args.is_some_and(|a| a.len() == 1))
                || args.unwrap_or(&[]).iter().any(has_aggregate)
        }
        Expr::FunctionCallStar { name, filter_over } => {
            name.0.eq_ignore_ascii_case("count")
                && filter_over.as_ref().is_none_or(|t| t.over_clause.is_none())
        }
        Expr::Binary(a, _, b) => has_aggregate(a) || has_aggregate(b),
        Expr::Unary(_, e)
        | Expr::IsNull(e)
        | Expr::NotNull(e)
        | Expr::Cast { expr: e, .. }
        | Expr::Collate(e, _) => has_aggregate(e),
        Expr::Parenthesized(es) => es.iter().any(has_aggregate),
        Expr::Case {
            base,
            when_then_pairs,
            else_expr,
        } => {
            base.is_some_and(has_aggregate)
                || else_expr.is_some_and(has_aggregate)
                || when_then_pairs
                    .iter()
                    .any(|(a, b)| has_aggregate(a) || has_aggregate(b))
        }
        Expr::Between {
            lhs, start, end, ..
        } => has_aggregate(lhs) || has_aggregate(start) || has_aggregate(end),
        Expr::Like {
            lhs, rhs, escape, ..
        } => has_aggregate(lhs) || has_aggregate(rhs) || escape.is_some_and(has_aggregate),
        Expr::InList { lhs, rhs, .. } => {
            has_aggregate(lhs) || rhs.unwrap_or(&[]).iter().any(has_aggregate)
        }
        _ => false,
    }
}
fn ordinal(e: &Expr<'_>, width: usize) -> Result<Option<usize>> {
    if let Expr::Literal(Literal::Numeric(n)) = e {
        if let Ok(n) = n.parse::<usize>() {
            if n == 0 || n > width {
                return Err(error("output ordinal out of range"));
            }
            return Ok(Some(n - 1));
        }
    }
    Ok(None)
}
fn integer_limit(e: &Expr<'_>) -> Result<i64> {
    e.to_string()
        .replace(' ', "")
        .parse()
        .map_err(|_| error("LIMIT/OFFSET requires an integer literal"))
}
fn direction(s: &SortedColumn<'_>) -> &'static str {
    if s.order == Some(SortOrder::Desc) {
        "DESC"
    } else {
        "ASC"
    }
}
fn nulls(s: &SortedColumn<'_>) -> &'static str {
    match s.nulls {
        Some(NullsOrder::First) => " NULLS FIRST",
        Some(NullsOrder::Last) => " NULLS LAST",
        None => "",
    }
}
struct Compiler<'a> {
    db: &'a Connection,
    plan: Plan,
    ctes: Vec<(String, usize)>,
}
impl Compiler<'_> {
    fn push(&mut self, kind: Kind, inputs: Vec<usize>, fields: Vec<Field>) -> usize {
        let id = self.plan.nodes.len();
        self.plan.nodes.push(Node {
            kind,
            inputs,
            fields,
        });
        id
    }
    fn table(&mut self, t: &SelectTable<'_>) -> Result<usize> {
        if let SelectTable::Sub(from, None) = t {
            return self.from(from);
        }
        let (id, q) = match t {
            SelectTable::Select(s, a) => (self.select(s)?, alias(a).unwrap_or_default()),
            SelectTable::Sub(from, a) => (self.from(from)?, alias(a).unwrap_or_default()),
            SelectTable::Table(t, a, _) => {
                if t.db_name
                    .as_ref()
                    .is_some_and(|n| !name(n.0).eq_ignore_ascii_case("main"))
                {
                    return Err(error("sources must be in main"));
                }
                let table = name(t.name.0);
                let qualifier = alias(a).unwrap_or_else(|| table.clone());
                if let Some((_, id)) = self
                    .ctes
                    .iter()
                    .rev()
                    .find(|(n, _)| n.eq_ignore_ascii_case(&table))
                {
                    (*id, qualifier)
                } else {
                    let actual:String=self.db.query_row("SELECT name FROM pragma_table_list WHERE schema='main' AND type='table' AND name=?1 COLLATE NOCASE",[&table],|r|r.get(0))?;
                    if actual.starts_with("__ivm_") || actual.starts_with("sqlite_") {
                        return Err(error("internal source table"));
                    }
                    if self.db.query_row("SELECT EXISTS(SELECT 1 FROM temp.sqlite_schema WHERE name=?1 COLLATE NOCASE)",[&actual],|r|r.get::<_,bool>(0))?{return Err(error("temporary source shadow"));}
                    let source = if let Some(i) =
                        self.plan.sources.iter().position(|s| s.name == actual)
                    {
                        i
                    } else {
                        let columns=self.db.prepare("SELECT name FROM pragma_table_xinfo(?1,'main') WHERE hidden<>1 ORDER BY cid")?.query_map([&actual],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>>>()?;
                        let affinities=self.db.prepare("SELECT type FROM pragma_table_xinfo(?1,'main') WHERE hidden<>1 ORDER BY cid")?.query_map([&actual],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>>>()?.iter().map(|s|affinity(s)).collect();
                        let mut collations = vec![];
                        for column in &columns {
                            let table_c = std::ffi::CString::new(actual.as_str())
                                .map_err(|e| error(e.to_string()))?;
                            let column_c = std::ffi::CString::new(column.as_str())
                                .map_err(|e| error(e.to_string()))?;
                            let mut collation = std::ptr::null();
                            let rc = unsafe {
                                rusqlite::ffi::sqlite3_table_column_metadata(
                                    self.db.handle(),
                                    c"main".as_ptr(),
                                    table_c.as_ptr(),
                                    column_c.as_ptr(),
                                    std::ptr::null_mut(),
                                    &mut collation,
                                    std::ptr::null_mut(),
                                    std::ptr::null_mut(),
                                    std::ptr::null_mut(),
                                )
                            };
                            if rc != rusqlite::ffi::SQLITE_OK || collation.is_null() {
                                return Err(error("source collation metadata unavailable"));
                            }
                            let collation = unsafe { std::ffi::CStr::from_ptr(collation) }
                                .to_string_lossy()
                                .to_ascii_uppercase();
                            if !["BINARY", "NOCASE", "RTRIM"].contains(&collation.as_str()) {
                                return Err(error("unsupported source collation"));
                            }
                            collations.push(collation);
                        }
                        self.plan.sources.push(Source {
                            name: actual,
                            columns,
                            affinities,
                            collations,
                        });
                        self.plan.sources.len() - 1
                    };
                    let fields = self.plan.sources[source]
                        .columns
                        .iter()
                        .enumerate()
                        .map(|(i, n)| Field {
                            collation: self.plan.sources[source].collations[i].clone(),
                            merged_star: false,
                            position: 0,
                            visible: true,
                            unqualified: true,
                            qualifier: qualifier.clone(),
                            name: n.clone(),
                            affinity: self.plan.sources[source].affinities[i].clone(),
                        })
                        .collect();
                    (self.push(Kind::Input(source), vec![], fields), qualifier)
                }
            }
            _ => return Err(error("unsupported FROM source")),
        };
        let positions = self.plan.nodes[id]
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| f.visible)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        let mut fields = positions
            .iter()
            .map(|i| self.plan.nodes[id].fields[*i].clone())
            .collect::<Vec<_>>();
        for (i, f) in fields.iter_mut().enumerate() {
            f.qualifier = q.clone();
            f.unqualified = true;
            f.position = i;
            f.merged_star = false;
        }
        // Alias nodes share upstream state, while preserving independent column scopes.
        let expressions = positions.iter().map(|i| format!("c{i}")).collect();
        Ok(self.push(
            Kind::Map {
                expressions,
                predicate: None,
            },
            vec![id],
            fields,
        ))
    }
    fn joined(
        &mut self,
        left: usize,
        right: usize,
        on: &Expr<'_>,
        mode: &'static str,
    ) -> Result<usize> {
        let split = self.plan.nodes[left].fields.len();
        let mut fields = self.plan.nodes[left].fields.clone();
        fields.extend(self.plan.nodes[right].fields.clone());
        let (mut l, mut r) = (vec![], vec![]);
        index_pairs(on, &fields, split, &mut l, &mut r);
        let (mut strict_l, mut strict_r) = (vec![], vec![]);
        let pure = pairs(on, &fields, split, &mut strict_l, &mut strict_r).is_ok()
            && strict_l == l
            && strict_r == r;
        let predicate = if pure {
            None
        } else {
            Some(expression(on, &fields, false)?)
        };
        if mode == "semi" || mode == "anti" {
            fields.truncate(split);
        }
        Ok(self.push(
            Kind::Join {
                left: l,
                right: r,
                mode,
                predicate,
            },
            vec![left, right],
            fields,
        ))
    }
    fn from(&mut self, from: &FromClause<'_>) -> Result<usize> {
        let mut id = self.table(from.select.ok_or_else(|| error("source required"))?)?;
        if let Some(joins) = &from.joins {
            for join in joins {
                let right = self.table(&join.table)?;
                let typ = match join.operator {
                    JoinOperator::TypedJoin(Some(t)) => t,
                    _ => JoinType::INNER,
                };
                let mode = match (typ.contains(JoinType::LEFT), typ.contains(JoinType::RIGHT)) {
                    (true, true) => "full",
                    (true, false) => "left",
                    (false, true) => "right",
                    _ => "inner",
                };
                if let Some(JoinConstraint::On(on)) = &join.constraint {
                    id = self.joined(id, right, on, mode)?;
                    continue;
                }
                let split = self.plan.nodes[id].fields.len();
                let mut fields = self.plan.nodes[id].fields.clone();
                fields.extend(self.plan.nodes[right].fields.clone());
                let using = match &join.constraint {
                    Some(JoinConstraint::Using(names)) => {
                        names.iter().map(|n| name(n.0)).collect::<Vec<_>>()
                    }
                    _ if typ.contains(JoinType::NATURAL) => fields[..split]
                        .iter()
                        .filter(|f| {
                            f.visible
                                && fields[split..]
                                    .iter()
                                    .any(|r| r.visible && f.name.eq_ignore_ascii_case(&r.name))
                        })
                        .map(|f| f.name.clone())
                        .collect(),
                    _ => vec![],
                };
                let (mut l, mut r) = (vec![], vec![]);
                for n in using {
                    let find = |fs: &[Field]| -> Result<usize> {
                        let ids = fs
                            .iter()
                            .enumerate()
                            .filter(|(_, f)| f.unqualified && f.name.eq_ignore_ascii_case(&n))
                            .map(|(i, _)| i)
                            .collect::<Vec<_>>();
                        if ids.len() == 1 {
                            Ok(ids[0])
                        } else {
                            Err(error("ambiguous USING column"))
                        }
                    };
                    l.push(find(&fields[..split])?);
                    r.push(find(&fields[split..])?);
                }
                let indexed = l.iter().zip(&r).all(|(a, b)| {
                    fields[*a].affinity == fields[split + b].affinity
                        && fields[*a].collation == fields[split + b].collation
                });
                let value = |i: usize| {
                    if fields[i].affinity.is_empty() {
                        format!("c{i}")
                    } else {
                        format!("CAST(c{i} AS {})", fields[i].affinity)
                    }
                };
                let predicate = if indexed {
                    None
                } else {
                    Some(
                        l.iter()
                            .zip(&r)
                            .map(|(a, b)| {
                                format!(
                                    "({} COLLATE {})={}",
                                    value(*a),
                                    fields[*a].collation,
                                    value(split + b)
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(" AND "),
                    )
                };
                id = self.push(
                    Kind::Join {
                        left: if indexed { l.clone() } else { vec![] },
                        right: if indexed { r.clone() } else { vec![] },
                        mode,
                        predicate,
                    },
                    vec![id, right],
                    fields.clone(),
                );
                let mut expressions = (0..fields.len())
                    .map(|i| format!("c{i}"))
                    .collect::<Vec<_>>();
                // Keep each original qualified key, and expose the SQL USING key
                // separately. FULL JOIN selects the non-NULL side of that key.
                for (a, b) in l.iter().zip(&r) {
                    let mut merged = fields[*a].clone();
                    merged.qualifier.clear();
                    merged.visible = true;
                    merged.unqualified = true;
                    fields[*a].merged_star = true;
                    fields[*a].visible = false;
                    fields[*a].unqualified = false;
                    fields[split + b].visible = false;
                    fields[split + b].unqualified = false;
                    expressions.push(match mode {
                        "full" => format!("coalesce(c{a},c{})", split + b),
                        "right" => format!("c{}", split + b),
                        _ => format!("c{a}"),
                    });
                    fields.push(merged);
                }
                if !l.is_empty() {
                    // SQL star keeps left-side column order, replacing its USING
                    // columns by the merged values; right-side keys are omitted.
                    let mut positions = (0..split)
                        .map(|i| {
                            l.iter()
                                .position(|a| *a == i)
                                .map_or(i, |p| split + self.plan.nodes[right].fields.len() + p)
                        })
                        .collect::<Vec<_>>();
                    positions.extend(
                        (split..split + self.plan.nodes[right].fields.len())
                            .filter(|i| !r.contains(&(i - split))),
                    );
                    positions.extend(
                        (0..fields.len())
                            .filter(|i| !positions.contains(i))
                            .collect::<Vec<_>>(),
                    );
                    let ordered_fields = positions.iter().map(|i| fields[*i].clone()).collect();
                    let ordered_exprs = positions.iter().map(|i| expressions[*i].clone()).collect();
                    id = self.push(
                        Kind::Map {
                            expressions: ordered_exprs,
                            predicate: None,
                        },
                        vec![id],
                        ordered_fields,
                    );
                }
            }
        }
        Ok(id)
    }
    fn predicate(&mut self, id: usize, e: &Expr<'_>) -> Result<usize> {
        if let Expr::Binary(a, Operator::And, b) = e {
            let a = self.predicate(id, a)?;
            return self.predicate(a, b);
        }
        let exists = match e {
            Expr::Exists(s) => Some((*s, "semi")),
            Expr::Unary(UnaryOperator::Not, Expr::Exists(s)) => Some((*s, "anti")),
            _ => None,
        };
        if let Some((s, mode)) = exists {
            let OneSelect::Select {
                from: Some(f),
                where_clause: Some(on),
                group_by: None,
                having: None,
                window_clause: None,
                ..
            } = &s.body.select
            else {
                return Err(error("EXISTS requires a correlated equality"));
            };
            if s.limit.is_some() || s.body.compounds.is_some() || s.with.is_some() {
                return Err(error("EXISTS with LIMIT, compound or WITH unsupported"));
            }
            let right = self.from(f)?;
            // An aggregate SELECT returns a row even for empty input. The
            // existence operator here requires the non-aggregate row shape.
            if let OneSelect::Select { columns, .. } = &s.body.select {
                let mut fields = self.plan.nodes[id].fields.clone();
                fields.extend(self.plan.nodes[right].fields.clone());
                for c in *columns {
                    if let ResultColumn::Expr(e, _) = c {
                        if has_aggregate(e) {
                            return Err(error(
                                "aggregate EXISTS requires scalar-subquery semantics",
                            ));
                        }
                        expression(e, &fields, false)?;
                    }
                }
            }
            return self.joined(id, right, on, mode);
        }
        let fields = self.plan.nodes[id].fields.clone();
        let predicate = Some(expression(e, &fields, false)?);
        let expressions = (0..fields.len()).map(|i| format!("c{i}")).collect();
        Ok(self.push(
            Kind::Map {
                expressions,
                predicate,
            },
            vec![id],
            fields,
        ))
    }
    fn core(
        &mut self,
        s: &OneSelect<'_>,
        order: Option<&[SortedColumn<'_>]>,
        limit: Option<&Limit<'_>>,
    ) -> Result<usize> {
        let OneSelect::Select {
            distinctness,
            columns,
            from: Some(from),
            where_clause,
            group_by,
            having,
            window_clause,
        } = s
        else {
            return Err(error("unsupported SELECT core"));
        };
        let mut id = self.from(from)?;
        if let Some(e) = where_clause {
            id = self.predicate(id, e)?;
        }
        let mut fields = self.plan.nodes[id].fields.clone();
        let aggregate = group_by.is_some()
            || columns
                .iter()
                .any(|c| matches!(c,ResultColumn::Expr(e,_) if has_aggregate(e)))
            || having.as_ref().is_some_and(|e| has_aggregate(e));
        let mut expressions = vec![];
        let mut output_fields = vec![];
        for column in *columns {
            match column {
                ResultColumn::Star | ResultColumn::TableStar(_) => {
                    let mut selected = fields
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| match column {
                            ResultColumn::TableStar(q) => {
                                f.qualifier.eq_ignore_ascii_case(&name(q.0))
                            }
                            _ => f.visible,
                        })
                        .collect::<Vec<_>>();
                    if matches!(column, ResultColumn::TableStar(_)) {
                        selected.sort_by_key(|(_, f)| f.position);
                    }
                    for (i, f) in selected {
                        let i = if matches!(column, ResultColumn::TableStar(_)) && f.merged_star {
                            fields
                                .iter()
                                .position(|m| {
                                    m.visible
                                        && m.unqualified
                                        && m.name.eq_ignore_ascii_case(&f.name)
                                })
                                .unwrap_or(i)
                        } else {
                            i
                        };
                        expressions.push(format!("c{i}"));
                        let mut f = f.clone();
                        f.visible = true;
                        f.unqualified = true;
                        output_fields.push(f);
                    }
                }
                ResultColumn::Expr(e, a) => {
                    let tail = match e {
                        Expr::FunctionCall {
                            filter_over: Some(t),
                            ..
                        }
                        | Expr::FunctionCallStar {
                            filter_over: Some(t),
                            ..
                        } => Some(t),
                        _ => None,
                    };
                    let value = if let Some(over) = tail.and_then(|t| t.over_clause) {
                        if aggregate {
                            return Err(error(
                                "aggregate/window composition requires a FROM subquery",
                            ));
                        }
                        let w = match over {
                            Over::Window(w) => *w,
                            Over::Name(n) => {
                                &window_clause
                                    .unwrap_or(&[])
                                    .iter()
                                    .find(|w| w.name.0.eq_ignore_ascii_case(n.0))
                                    .ok_or_else(|| error("unknown window"))?
                                    .window
                            }
                        };
                        if w.base.is_some() {
                            return Err(error("inherited windows unsupported"));
                        }
                        let keys = w
                            .partition_by
                            .unwrap_or(&[])
                            .iter()
                            .map(|e| {
                                expression(e, &fields, false)
                                    .map(|s| key_expression(&s, &collation(e, &fields)))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        let ordering = w
                            .order_by
                            .unwrap_or(&[])
                            .iter()
                            .map(|s| {
                                Ok(format!(
                                    "{} {}{}",
                                    expression(&s.expr, &fields, false)?,
                                    direction(s),
                                    nulls(s)
                                ))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        let (function, args) = match e {
                            Expr::FunctionCall {
                                name,
                                args,
                                distinctness: None,
                                order_by: None,
                                ..
                            } => (
                                name.0.to_ascii_lowercase(),
                                args.unwrap_or(&[])
                                    .iter()
                                    .map(|e| expression(e, &fields, false))
                                    .collect::<Result<Vec<_>>>()?
                                    .join(","),
                            ),
                            Expr::FunctionCallStar { name, .. } => {
                                (name.0.to_ascii_lowercase(), "*".into())
                            }
                            _ => return Err(error("unsupported window call")),
                        };
                        if ![
                            "row_number",
                            "rank",
                            "dense_rank",
                            "percent_rank",
                            "cume_dist",
                            "ntile",
                            "lag",
                            "lead",
                            "first_value",
                            "last_value",
                            "nth_value",
                            "count",
                            "sum",
                            "avg",
                            "min",
                            "max",
                        ]
                        .contains(&function.as_str())
                        {
                            return Err(error("unsupported window function"));
                        }
                        let filter = tail
                            .and_then(|t| t.filter_clause)
                            .map(|e| {
                                expression(e, &fields, false).map(|v| format!(" FILTER(WHERE {v})"))
                            })
                            .transpose()?
                            .unwrap_or_default();
                        let over = format!(
                            "{} {}",
                            if ordering.is_empty() {
                                String::new()
                            } else {
                                format!("ORDER BY {}", ordering.join(","))
                            },
                            w.frame_clause.as_ref().map(sql).unwrap_or_default()
                        );
                        let mut values = (0..fields.len())
                            .map(|i| format!("c{i}"))
                            .collect::<Vec<_>>();
                        values.push(format!("{function}({args}){filter} OVER({over})"));
                        let value = format!("c{}", fields.len());
                        fields.push(Field {
                            collation: "BINARY".into(),
                            merged_star: false,
                            position: 0,
                            visible: false,
                            unqualified: false,
                            qualifier: String::new(),
                            name: format!("__ivm_window{}", fields.len()),
                            affinity: String::new(),
                        });
                        id = self.push(
                            Kind::Group {
                                keys,
                                expressions: values,
                                order: vec![],
                                limit: None,
                                offset: 0,
                                having: None,
                                window: true,
                            },
                            vec![id],
                            fields.clone(),
                        );
                        value
                    } else {
                        expression(e, &fields, aggregate)?
                    };
                    expressions.push(value);
                    output_fields.push(Field {
                        collation: collation(e, &fields),
                        merged_star: false,
                        position: 0,
                        visible: true,
                        unqualified: true,
                        qualifier: String::new(),
                        affinity: expression_affinity(e, &fields),
                        name: alias(a).unwrap_or_else(|| match e {
                            Expr::Name(n) | Expr::Qualified(_, n) => name(n.0),
                            Expr::Id(n) => name(n.0),
                            _ => e.to_string(),
                        }),
                    });
                }
            }
        }
        let output_width = expressions.len();
        let aliases = output_fields
            .iter()
            .zip(&expressions)
            .map(|(f, e)| (f.name.clone(), e.clone()))
            .collect::<Vec<_>>();
        let keys = group_by
            .unwrap_or(&[])
            .iter()
            .map(|e| {
                if let Some(i) = ordinal(e, expressions.len())? {
                    Ok(key_expression(&expressions[i], &output_fields[i].collation))
                } else {
                    expression_aliases(e, &fields, false, &aliases).map(|v| {
                        let c = if resolve(e, &fields).is_ok() {
                            collation(e, &fields)
                        } else {
                            resolve(e, &output_fields)
                                .map(|i| output_fields[i].collation.clone())
                                .unwrap_or_else(|_| collation(e, &fields))
                        };
                        key_expression(&v, &c)
                    })
                }
            })
            .collect::<Result<Vec<_>>>()?;
        let having = having
            .as_ref()
            .map(|e| expression_aliases(e, &fields, true, &aliases))
            .transpose()?;
        let mut ordering = vec![];
        if limit.is_some() {
            for s in order.unwrap_or(&[]) {
                let existing = ordinal(&s.expr, output_width)?
                    .or_else(|| resolve(&s.expr, &output_fields).ok())
                    .or_else(|| {
                        columns
                            .iter()
                            .position(|c| matches!(c,ResultColumn::Expr(e,_) if *e==s.expr))
                    });
                let index = if let Some(i) = existing {
                    i
                } else {
                    if *distinctness == Some(Distinctness::Distinct) {
                        return Err(error("DISTINCT top-k ordering must refer to an output"));
                    }
                    let value = expression_aliases(&s.expr, &fields, aggregate, &aliases)?;
                    let i = expressions.len();
                    expressions.push(value);
                    output_fields.push(Field {
                        collation: "BINARY".into(),
                        merged_star: false,
                        position: 0,
                        visible: false,
                        unqualified: false,
                        qualifier: String::new(),
                        name: format!("__ivm_order{i}"),
                        affinity: String::new(),
                    });
                    i
                };
                ordering.push(format!("c{index} {}{}", direction(s), nulls(s)));
            }
        }
        id = if aggregate {
            self.push(
                Kind::Group {
                    keys,
                    expressions,
                    order: vec![],
                    limit: None,
                    offset: 0,
                    having,
                    window: false,
                },
                vec![id],
                output_fields,
            )
        } else {
            self.push(
                Kind::Map {
                    expressions,
                    predicate: None,
                },
                vec![id],
                output_fields,
            )
        };
        if *distinctness == Some(Distinctness::Distinct) {
            let fs = self.plan.nodes[id].fields.clone();
            id = self.push(Kind::Set("distinct"), vec![id], fs);
        }
        if let Some(l) = limit {
            let count = integer_limit(&l.expr)?;
            let offset = l
                .offset
                .as_ref()
                .map(integer_limit)
                .transpose()?
                .unwrap_or(0)
                .max(0);
            let fs = self.plan.nodes[id].fields.clone();
            let expressions = (0..output_width).map(|i| format!("c{i}")).collect();
            id = self.push(
                Kind::Group {
                    keys: vec![],
                    expressions,
                    order: ordering,
                    limit: Some(count),
                    offset,
                    having: None,
                    window: false,
                },
                vec![id],
                fs[..output_width].to_vec(),
            );
        }
        Ok(id)
    }
    fn select(&mut self, s: &Select<'_>) -> Result<usize> {
        let mark = self.ctes.len();
        if let Some(with) = &s.with {
            for cte in with.ctes {
                let refers = |t: &SelectTable<'_>| matches!(t,SelectTable::Table(n,_,_) if name(n.name.0).eq_ignore_ascii_case(&name(cte.tbl_name.0)));
                let recursive = with.recursive
                    && cte.select.body.compounds.as_ref().is_some_and(|parts| {
                        parts.iter().any(|part| match &part.select {
                            OneSelect::Select {
                                from: Some(from), ..
                            } => {
                                from.select.is_some_and(refers)
                                    || from
                                        .joins
                                        .as_ref()
                                        .is_some_and(|joins| joins.iter().any(|j| refers(&j.table)))
                            }
                            _ => false,
                        })
                    });
                let mut id = if recursive {
                    self.recursive(cte)?
                } else {
                    self.select(cte.select)?
                };
                if let Some(columns) = cte.columns {
                    let mut fields = self.plan.nodes[id].fields.clone();
                    if columns.len() != fields.len() {
                        return Err(error("CTE column count mismatch"));
                    }
                    for (f, c) in fields.iter_mut().zip(columns) {
                        f.name = name(c.col_name.0);
                    }
                    let expressions = (0..fields.len()).map(|i| format!("c{i}")).collect();
                    id = self.push(
                        Kind::Map {
                            expressions,
                            predicate: None,
                        },
                        vec![id],
                        fields,
                    );
                }
                self.ctes.push((name(cte.tbl_name.0), id));
            }
        }
        let mut id = self.core(
            &s.body.select,
            s.order_by,
            if s.body.compounds.is_some() {
                None
            } else {
                s.limit
            },
        )?;
        if let Some(compounds) = &s.body.compounds {
            for c in compounds {
                let right = self.core(&c.select, None, None)?;
                let fields = self.plan.nodes[id].fields.clone();
                if fields.len() != self.plan.nodes[right].fields.len() {
                    return Err(error("compound column count mismatch"));
                }
                let op = match c.operator {
                    CompoundOperator::Union => "union",
                    CompoundOperator::UnionAll => "all",
                    CompoundOperator::Except => "except",
                    CompoundOperator::Intersect => "intersect",
                };
                id = self.push(Kind::Set(op), vec![id, right], fields);
            }
        }
        if s.body.compounds.is_some() {
            if let Some(l) = s.limit {
                let fields = self.plan.nodes[id].fields.clone();
                let order = s
                    .order_by
                    .unwrap_or(&[])
                    .iter()
                    .map(|s| {
                        let value = if let Some(i) = ordinal(&s.expr, fields.len())? {
                            format!("c{i}")
                        } else {
                            expression(&s.expr, &fields, false)?
                        };
                        Ok(format!("{value} {}{}", direction(s), nulls(s)))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let count = integer_limit(&l.expr)?;
                let offset = l
                    .offset
                    .as_ref()
                    .map(integer_limit)
                    .transpose()?
                    .unwrap_or(0)
                    .max(0);
                let expressions = (0..fields.len()).map(|i| format!("c{i}")).collect();
                id = self.push(
                    Kind::Group {
                        keys: vec![],
                        expressions,
                        order,
                        limit: Some(count),
                        offset,
                        having: None,
                        window: false,
                    },
                    vec![id],
                    fields,
                );
            }
        }
        self.ctes.truncate(mark);
        Ok(id)
    }
    fn recursive(&mut self, cte: &CommonTableExpr<'_>) -> Result<usize> {
        if cte.select.order_by.is_some() || cte.select.limit.is_some() {
            return Err(error("recursive ordering and LIMIT unsupported"));
        }
        let parts = cte
            .select
            .body
            .compounds
            .as_ref()
            .ok_or_else(|| error("recursive UNION required"))?;
        if parts.len() != 1 || parts[0].operator != CompoundOperator::Union {
            return Err(error("recursive reachability requires UNION distinct"));
        }
        let roots = self.core(&cte.select.body.select, None, None)?;
        if self.plan.nodes[roots].fields.len() != 1 {
            return Err(error("recursive reachability requires one node column"));
        }
        let OneSelect::Select {
            distinctness: None,
            columns,
            from: Some(from),
            where_clause: None,
            group_by: None,
            having: None,
            window_clause: None,
        } = &parts[0].select
        else {
            return Err(error("unsupported recursive step"));
        };
        let joins = from
            .joins
            .as_ref()
            .ok_or_else(|| error("recursive step requires a join"))?;
        if joins.iter().any(|j| {
            !matches!(j.operator, JoinOperator::TypedJoin(None))
                && !matches!(j.operator,JoinOperator::TypedJoin(Some(t)) if t==JoinType::INNER)
        }) {
            return Err(error("recursive step requires inner join"));
        }
        if joins.len() != 1 || columns.len() != 1 {
            return Err(error(
                "recursive step requires one edge join and one output",
            ));
        }
        let first = from
            .select
            .ok_or_else(|| error("recursive source missing"))?;
        let second = &joins[0].table;
        let is_recursive = |t: &SelectTable<'_>| matches!(t,SelectTable::Table(n,_,_) if name(n.name.0).eq_ignore_ascii_case(&name(cte.tbl_name.0)));
        let (edge, recursive) = if is_recursive(first) && !is_recursive(second) {
            (second, first)
        } else if is_recursive(second) && !is_recursive(first) {
            (first, second)
        } else {
            return Err(error("recursive step requires one recursive reference"));
        };
        let edges = self.table(edge)?;
        let edge_fields = self.plan.nodes[edges].fields.clone();
        let split = edge_fields.len();
        let SelectTable::Table(_, a, _) = recursive else {
            unreachable!()
        };
        let recursive_name = alias(a).unwrap_or_else(|| name(cte.tbl_name.0));
        let node_name = cte
            .columns
            .and_then(|c| c.first())
            .map(|c| name(c.col_name.0))
            .unwrap_or_else(|| self.plan.nodes[roots].fields[0].name.clone());
        let mut fields = edge_fields.clone();
        fields.push(Field {
            collation: self.plan.nodes[roots].fields[0].collation.clone(),
            merged_star: false,
            position: 0,
            visible: true,
            unqualified: true,
            qualifier: recursive_name,
            name: node_name.clone(),
            affinity: self.plan.nodes[roots].fields[0].affinity.clone(),
        });
        let Some(JoinConstraint::On(on)) = &joins[0].constraint else {
            return Err(error("recursive edge equality required"));
        };
        let (mut left, mut right) = (vec![], vec![]);
        pairs(on, &fields, split, &mut left, &mut right)?;
        if left.len() != 1 {
            return Err(error("recursive node equality required"));
        }
        let ResultColumn::Expr(target, _) = &columns[0] else {
            return Err(error("recursive target column required"));
        };
        let target = resolve(target, &fields)?;
        if target >= split {
            return Err(error("recursive target must come from the edge"));
        }
        if edge_fields[left[0]].affinity != self.plan.nodes[roots].fields[0].affinity
            || edge_fields[target].affinity != self.plan.nodes[roots].fields[0].affinity
        {
            return Err(error("recursive key affinities must match"));
        }
        if edge_fields[left[0]].collation != self.plan.nodes[roots].fields[0].collation
            || edge_fields[target].collation != self.plan.nodes[roots].fields[0].collation
        {
            return Err(error("recursive key collations must match"));
        }
        let mapped = self.push(
            Kind::Map {
                expressions: vec![format!("c{}", left[0]), format!("c{target}")],
                predicate: None,
            },
            vec![edges],
            vec![edge_fields[left[0]].clone(), edge_fields[target].clone()],
        );
        Ok(self.push(
            Kind::Reach,
            vec![roots, mapped],
            vec![Field {
                collation: self.plan.nodes[roots].fields[0].collation.clone(),
                merged_star: false,
                position: 0,
                visible: true,
                unqualified: true,
                qualifier: name(cte.tbl_name.0),
                name: node_name,
                affinity: self.plan.nodes[roots].fields[0].affinity.clone(),
            }],
        ))
    }
}
pub fn bind(db: &Connection, sql: &str) -> Result<Plan> {
    let arena = Bump::new();
    let mut parser = Parser::new(&arena, sql.as_bytes());
    let command = parser
        .next()
        .map_err(|e| error(e.to_string()))?
        .ok_or_else(|| error("SELECT required"))?;
    let Cmd::Stmt(Stmt::Select(select)) = command else {
        return Err(error("one SELECT required"));
    };
    if parser.next().map_err(|e| error(e.to_string()))?.is_some() {
        return Err(error("one SELECT required"));
    }
    let statement = db.prepare(sql)?;
    if statement.parameter_count() != 0 {
        return Err(error("persistent queries cannot contain bind parameters"));
    }
    let names = statement
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let functions = db
        .prepare(&format!("EXPLAIN {sql}"))?
        .query_map([], |r| {
            Ok((r.get::<_, String>(1)?, r.get::<_, Option<String>>(5)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    for (opcode, function) in functions {
        if ![
            "Function", "PureFunc", "AggStep", "AggStep1", "AggValue", "AggFinal",
        ]
        .contains(&opcode.as_str())
        {
            continue;
        }
        let Some(function) = function else {
            continue;
        };
        let name = function.split('(').next().unwrap().to_ascii_lowercase();
        if opcode.starts_with("Agg") {
            if ![
                "count",
                "sum",
                "avg",
                "min",
                "max",
                "row_number",
                "rank",
                "dense_rank",
                "percent_rank",
                "cume_dist",
                "ntile",
                "lag",
                "lead",
                "first_value",
                "last_value",
                "nth_value",
            ]
            .contains(&name.as_str())
            {
                return Err(error(format!("unsupported aggregate {name}")));
            }
        } else {
            let deterministic:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_function_list WHERE name=?1 COLLATE NOCASE AND type='s' AND flags & 2048 != 0) AND NOT EXISTS(SELECT 1 FROM pragma_function_list WHERE name=?1 COLLATE NOCASE AND type='s' AND builtin=0 AND flags & 2048 = 0)",[&name],|r|r.get(0))?;
            if !deterministic
                || [
                    "date",
                    "time",
                    "datetime",
                    "julianday",
                    "unixepoch",
                    "strftime",
                    "timediff",
                ]
                .contains(&name.as_str())
            {
                return Err(error(format!("non-persistent scalar function {name}")));
            }
        }
    }
    if names.iter().enumerate().any(|(i, n)| {
        n.starts_with("__ivm_") || names[..i].iter().any(|other| n.eq_ignore_ascii_case(other))
    }) {
        return Err(error("reserved or duplicate output names"));
    }
    let mut compiler = Compiler {
        db,
        plan: Plan {
            sources: vec![],
            nodes: vec![],
            output: 0,
            names,
        },
        ctes: vec![],
    };
    compiler.plan.output = compiler.select(select)?;
    Ok(compiler.plan)
}
