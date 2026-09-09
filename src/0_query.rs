use rusqlite::{Connection, Error, Result};
use sqlite3_parser::{ast::*, lexer::sql::Parser, Bump, FallibleIterator as _};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub source: usize,
    pub name: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FilterValue {
    Column(Column),
    Integer(i64),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Filter {
    Compare(FilterValue, &'static str, FilterValue),
    Boolean(Box<Filter>, &'static str, Box<Filter>),
    Not(Box<Filter>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Query {
    pub tables: Vec<String>,
    pub join: Option<Vec<[Column; 2]>>,
    pub group: Column,
    // SUM(column) or SUM(column * column).
    pub factors: Vec<Column>,
    pub filter: Option<Filter>,
    pub outputs: Vec<(String, &'static str)>,
}

pub fn error(message: impl Into<String>) -> Error {
    Error::UserFunctionError(Box::new(std::io::Error::other(message.into())))
}

pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn filter_value<'a>(
    expr: &'a Expr<'a>,
    resolve: &impl Fn(&Expr<'a>) -> Result<Column>,
    depth: usize,
) -> Result<FilterValue> {
    if depth >= 64 {
        return Err(error("WHERE nesting exceeds 64 levels"));
    }
    let integer = |text: &str| {
        text.parse::<i64>()
            .map(FilterValue::Integer)
            .map_err(|_| error("WHERE literals must be decimal signed 64-bit integers"))
    };
    match expr {
        Expr::Literal(Literal::Numeric(text)) => integer(text),
        Expr::Unary(op @ (UnaryOperator::Negative | UnaryOperator::Positive), inner) => {
            let Expr::Literal(Literal::Numeric(text)) = inner else {
                return Err(error("WHERE unary signs require an integer literal"));
            };
            integer(&format!(
                "{}{text}",
                if *op == UnaryOperator::Negative {
                    "-"
                } else {
                    "+"
                }
            ))
        }
        Expr::Parenthesized(values) if values.len() == 1 => {
            filter_value(&values[0], resolve, depth + 1)
        }
        _ => resolve(expr).map(FilterValue::Column),
    }
}

fn bind_filter<'a>(
    expr: &'a Expr<'a>,
    resolve: &impl Fn(&Expr<'a>) -> Result<Column>,
    depth: usize,
) -> Result<Filter> {
    if depth >= 64 {
        return Err(error("WHERE nesting exceeds 64 levels"));
    }
    match expr {
        Expr::Parenthesized(values) if values.len() == 1 => {
            bind_filter(&values[0], resolve, depth + 1)
        }
        Expr::Unary(UnaryOperator::Not, inner) => Ok(Filter::Not(Box::new(bind_filter(
            inner,
            resolve,
            depth + 1,
        )?))),
        Expr::Binary(left, op @ (Operator::And | Operator::Or), right) => Ok(Filter::Boolean(
            Box::new(bind_filter(left, resolve, depth + 1)?),
            if *op == Operator::And { "AND" } else { "OR" },
            Box::new(bind_filter(right, resolve, depth + 1)?),
        )),
        Expr::Binary(left, op, right) => {
            let operator = match op {
                Operator::Equals => "=",
                Operator::NotEquals => "<>",
                Operator::Less => "<",
                Operator::LessEquals => "<=",
                Operator::Greater => ">",
                Operator::GreaterEquals => ">=",
                _ => {
                    return Err(error(
                        "WHERE supports integer comparisons with AND, OR and NOT",
                    ))
                }
            };
            Ok(Filter::Compare(
                filter_value(left, resolve, depth + 1)?,
                operator,
                filter_value(right, resolve, depth + 1)?,
            ))
        }
        _ => Err(error(
            "WHERE supports integer comparisons with AND, OR and NOT",
        )),
    }
}

fn bind_join<'a>(
    expr: &'a Expr<'a>,
    resolve: &impl Fn(&Expr<'a>) -> Result<Column>,
    keys: &mut Vec<[Column; 2]>,
    depth: usize,
) -> Result<()> {
    if depth >= 64 {
        return Err(error("JOIN nesting exceeds 64 levels"));
    }
    match expr {
        Expr::Parenthesized(values) if values.len() == 1 => {
            bind_join(&values[0], resolve, keys, depth + 1)
        }
        Expr::Binary(left, Operator::And, right) => {
            bind_join(left, resolve, keys, depth + 1)?;
            bind_join(right, resolve, keys, depth + 1)
        }
        Expr::Binary(left, Operator::Equals, right) => {
            let mut pair = [resolve(left)?, resolve(right)?];
            if pair[0].source == pair[1].source {
                return Err(error("each join equality must connect both source tables"));
            }
            pair.sort_by_key(|c| c.source);
            if !keys.contains(&pair) {
                keys.push(pair);
            }
            Ok(())
        }
        _ => Err(error("join requires column equalities connected by AND")),
    }
}

pub fn bind(db: &Connection, sql: &str) -> Result<Query> {
    if sql.len() > 65_536 || sql.contains('\0') {
        return Err(error("query must be NUL-free and at most 65536 bytes"));
    }
    let arena = Bump::new();
    let mut parser = Parser::new(&arena, sql.as_bytes());
    let command = parser
        .next()
        .map_err(|e| error(e.to_string()))?
        .ok_or_else(|| error("expected SELECT"))?;
    if parser.next().map_err(|e| error(e.to_string()))?.is_some() {
        return Err(error("exactly one SELECT is required"));
    }
    let Cmd::Stmt(Stmt::Select(select)) = command else {
        return Err(error("expected SELECT"));
    };
    if select.with.is_some()
        || select.order_by.is_some()
        || select.limit.is_some()
        || select.body.compounds.is_some()
    {
        return Err(error(
            "WITH, ORDER BY, LIMIT and compound queries are unsupported",
        ));
    }
    let OneSelect::Select {
        distinctness: None,
        columns,
        from: Some(from),
        where_clause,
        group_by: Some(groups),
        having: None,
        window_clause: None,
    } = &select.body.select
    else {
        return Err(error(
            "expected grouped SELECT without DISTINCT, HAVING or WINDOW",
        ));
    };
    if groups.len() != 1 || columns.len() != 3 {
        return Err(error("expected one grouping column, COUNT(*) and SUM"));
    }
    let mut source_nodes = vec![from.select.ok_or_else(|| error("source table required"))?];
    let mut on = None;
    if let Some(joins) = &from.joins {
        if joins.len() != 1 {
            return Err(error("at most two source tables are supported"));
        }
        let joined = &joins[0];
        match joined.operator {
            JoinOperator::TypedJoin(None) => (),
            JoinOperator::TypedJoin(Some(kind)) if kind == JoinType::INNER => (),
            _ => return Err(error("only INNER JOIN ... ON is supported")),
        }
        let Some(JoinConstraint::On(expr)) = &joined.constraint else {
            return Err(error("join requires ON column = column"));
        };
        source_nodes.push(&joined.table);
        on = Some(expr);
    }
    let mut catalog =
        db.prepare("SELECT name FROM pragma_table_list WHERE schema='main' AND type='table'")?;
    let names = catalog
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>>>()?;
    let mut tables = Vec::new();
    let mut qualifiers = Vec::new();
    let mut fields = Vec::new();
    for node in source_nodes {
        let SelectTable::Table(name, alias, None) = node else {
            return Err(error("expected an ordinary source table"));
        };
        if name.db_name.as_ref().is_some_and(|n| n != &Name("main")) {
            return Err(error("source table must be in main"));
        }
        let table = names
            .iter()
            .find(|n| name.name == Name(&quote(n)))
            .ok_or_else(|| error("source must be an ordinary table in main"))?
            .clone();
        if table.to_ascii_lowercase().starts_with("sqlite_")
            || table.to_ascii_lowercase().starts_with("__ivm_")
        {
            return Err(error("internal tables cannot be sources"));
        }
        if tables.contains(&table) {
            return Err(error("self joins are unsupported"));
        }
        let shadowed: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM temp.sqlite_schema WHERE name=?1 COLLATE NOCASE)",
            [&table],
            |r| r.get(0),
        )?;
        if shadowed {
            return Err(error("a temporary object shadows the source table"));
        }
        let qualifier = match alias {
            Some(As::As(n) | As::Elided(n)) => n,
            None => &name.name,
        };
        if qualifiers.contains(qualifier) {
            return Err(error("source aliases must be distinct"));
        }
        qualifiers.push(qualifier.clone());
        let mut statement =
            db.prepare("SELECT name, type, hidden FROM pragma_table_xinfo(?1, 'main')")?;
        fields.push(
            statement
                .query_map([&table], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>>>()?,
        );
        tables.push(table);
    }
    let resolve = |expr: &Expr<'_>| -> Result<Column> {
        let (qualifier, name) = match expr {
            Expr::Name(n) => (None, n.clone()),
            Expr::Id(id) => (None, Name(id.0)),
            Expr::Qualified(q, n) => (Some(q), n.clone()),
            _ => return Err(error("expected a source column")),
        };
        let mut found = None;
        for (source, source_fields) in fields.iter().enumerate() {
            if qualifier.is_some_and(|q| q != &qualifiers[source]) {
                continue;
            }
            for field in source_fields {
                if name != Name(&quote(&field.0)) {
                    continue;
                }
                if found.is_some() {
                    return Err(error("ambiguous source column"));
                }
                if !field.1.eq_ignore_ascii_case("INTEGER") || field.2 != 0 {
                    return Err(error("referenced columns must be ordinary INTEGER columns"));
                }
                found = Some(Column {
                    source,
                    name: field.0.clone(),
                });
            }
        }
        found.ok_or_else(|| error(format!("unknown source column: {}", name.0)))
    };
    let join = if let Some(expr) = on {
        let mut keys = Vec::new();
        bind_join(expr, &resolve, &mut keys, 0)?;
        Some(keys)
    } else {
        None
    };
    let group = resolve(&groups[0])?;
    let filter = where_clause
        .map(|expr| bind_filter(expr, &resolve, 0))
        .transpose()?;
    let statement = db.prepare(sql)?;
    let output_names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|name| name.to_string())
        .collect();
    let mut outputs = Vec::new();
    let mut factors = Vec::new();
    for (index, result) in columns.iter().enumerate() {
        let ResultColumn::Expr(expr, _) = result else {
            return Err(error("SELECT * is unsupported"));
        };
        let stored = match expr {
            Expr::FunctionCallStar {
                name,
                filter_over: None,
            } if Name(name.0) == Name("count") => "n",
            Expr::FunctionCall {
                name,
                distinctness: None,
                args: Some(args),
                order_by: None,
                filter_over: None,
            } if Name(name.0) == Name("sum") && args.len() == 1 => {
                factors = match &args[0] {
                    Expr::Binary(left, Operator::Multiply, right) => {
                        vec![resolve(left)?, resolve(right)?]
                    }
                    expr => vec![resolve(expr)?],
                };
                "s"
            }
            _ if resolve(expr)? == group => "g",
            _ => return Err(error("expected grouping column, COUNT(*) and SUM")),
        };
        if outputs.iter().any(|(name, kind): &(String, &str)| {
            *kind == stored || name.eq_ignore_ascii_case(&output_names[index])
        }) {
            return Err(error(
                "duplicate output names or aggregate roles are unsupported",
            ));
        }
        outputs.push((output_names[index].clone(), stored));
    }
    if factors.is_empty() || !outputs.iter().any(|(_, kind)| *kind == "n") {
        return Err(error("COUNT(*) and SUM are required"));
    }
    Ok(Query {
        tables,
        join,
        group,
        factors,
        filter,
        outputs,
    })
}
