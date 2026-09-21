//! Owned relational plans lowered from sqlite3-parser. SQLite evaluates scalar expressions.
use crate::catalog::error;
use crate::compile_recursive::recursion_shape;
use rusqlite::{Connection, Result};
use sqlite3_parser::{ast::*, lexer::sql::Parser, Bump, FallibleIterator};
pub(crate) fn sql<T: fmt::ToTokens>(value: &T) -> String {
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
pub(crate) fn field(
    qualifier: String,
    name: String,
    affinity: String,
    collation: String,
    visible: bool,
    unqualified: bool,
) -> Field {
    Field {
        qualifier,
        name,
        affinity,
        visible,
        unqualified,
        position: 0,
        merged_star: false,
        collation,
    }
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
    Fixpoint {
        rules: Vec<Rule>,
    },
}
impl Kind {
    /// The label a `node` span carries, one per variant plus the set operator.
    pub fn label(&self) -> &'static str {
        match self {
            Kind::Input(_) => "input",
            Kind::Map { .. } => "map",
            Kind::Join { .. } => "join",
            Kind::Set("all") => "union_all",
            Kind::Set(_) => "set",
            Kind::Group { window: true, .. } => "window",
            Kind::Group { limit: Some(_), .. } => "group_limit",
            Kind::Group { .. } => "group",
            Kind::Fixpoint { .. } => "fixpoint",
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub enum Occurrence {
    Input(usize),
    Member,
}
/// One UNION term of a recursive CTE. Anchor rules have no `Member` occurrence;
/// step rules have exactly one, which is the SQLite grammar limit.
#[derive(Clone, Debug)]
pub struct Rule {
    pub occurrences: Vec<(Occurrence, usize)>,
    pub head: Vec<String>,
    pub key: String,
    pub predicate: Option<String>,
    pub indexes: Vec<(Occurrence, String)>,
}
impl Rule {
    pub fn member(&self) -> Option<usize> {
        self.occurrences
            .iter()
            .position(|(o, _)| *o == Occurrence::Member)
    }
    pub fn mentions(&self, side: usize) -> bool {
        self.occurrences
            .iter()
            .any(|(o, _)| *o == Occurrence::Input(side))
    }
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
pub(crate) fn name(n: &str) -> String {
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
pub(crate) fn alias(a: &Option<As<'_>>) -> Option<String> {
    a.as_ref().map(|a| match a {
        As::As(n) | As::Elided(n) => name(n.0),
    })
}
pub(crate) fn resolve(e: &Expr<'_>, fields: &[Field]) -> Result<usize> {
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
pub(crate) fn explicit_collation(e: &Expr<'_>) -> Option<String> {
    match e {
        Expr::Collate(_, n) => Some(name(n).to_ascii_uppercase()),
        Expr::Binary(a, _, b) => explicit_collation(a).or_else(|| explicit_collation(b)),
        Expr::Cast { expr, .. } | Expr::Unary(_, expr) => explicit_collation(expr),
        Expr::Parenthesized(es) if es.len() == 1 => explicit_collation(&es[0]),
        _ => None,
    }
}
pub(crate) fn implicit_collation(e: &Expr<'_>, fields: &[Field]) -> Option<String> {
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
pub(crate) fn collation(e: &Expr<'_>, fields: &[Field]) -> String {
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
/// UNION distinct identity in SQL: integral reals fold to integers, other reals
/// and blobs are tagged objects, text is collation-normalized, NULL equals NULL.
pub fn key_sql(parts: &[(String, String)]) -> String {
    let normalized = parts
        .iter()
        .map(|(value, collation)| {
            let x = format!("({})", key_expression(value, collation));
            format!("CASE typeof({x}) WHEN 'blob' THEN json_object('blob',hex({x})) WHEN 'real' THEN CASE WHEN {x}=CAST({x} AS INTEGER) THEN CAST({x} AS INTEGER) ELSE json_object('real',printf('%!.17g',{x})) END WHEN 'text' THEN {x}||'' ELSE {x} END")
        })
        .collect::<Vec<_>>();
    format!("json_array({})", normalized.join(","))
}
pub fn column_reference(index: usize, affinity: &str) -> String {
    if affinity.is_empty() {
        format!("c{index}")
    } else {
        format!("CAST(c{index} AS {affinity})")
    }
}
pub fn expression(e: &Expr<'_>, fields: &[Field], aggregate: bool) -> Result<String> {
    expression_aliases(e, fields, aggregate, &[])
}
pub(crate) fn expression_aliases(
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
                    crate::catalog::quote(&function),
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
pub(crate) fn affinity(declared: &str) -> String {
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
pub(crate) fn expression_affinity(e: &Expr<'_>, fields: &[Field]) -> String {
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
pub(crate) fn has_aggregate(e: &Expr<'_>) -> bool {
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
pub(crate) fn ordinal(e: &Expr<'_>, width: usize) -> Result<Option<usize>> {
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
pub(crate) fn integer_limit(e: &Expr<'_>) -> Result<i64> {
    e.to_string()
        .replace(' ', "")
        .parse()
        .map_err(|_| error("LIMIT/OFFSET requires an integer literal"))
}
pub(crate) fn direction(s: &SortedColumn<'_>) -> &'static str {
    if s.order == Some(SortOrder::Desc) {
        "DESC"
    } else {
        "ASC"
    }
}
pub(crate) fn nulls(s: &SortedColumn<'_>) -> &'static str {
    match s.nulls {
        Some(NullsOrder::First) => " NULLS FIRST",
        Some(NullsOrder::Last) => " NULLS LAST",
        None => "",
    }
}
pub(crate) struct Compiler<'a> {
    pub(crate) db: &'a Connection,
    pub(crate) plan: Plan,
    pub(crate) ctes: Vec<(String, usize)>,
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
    recursion_shape(select)?;
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
